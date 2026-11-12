//! Publishes the frozen overview through Azure DevOps attachment and thread APIs.

use std::fs;
use std::num::NonZeroU32;

use ohno::AppError;
use serde_json::{Value, from_slice, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::error::fail;
use crate::json::{array, field, text};
use crate::target::{Platform, Target};
use crate::transport::Transport;

/// Caps untrusted routing components while allowing long organization names.
const MAX_ROUTE_LENGTH: usize = 4096;

/// Checks whether publication still targets an active pull request.
pub(crate) fn is_open(transport: &mut Transport<'_>, target: &Target) -> Result<bool, AppError> {
    let path = route(transport, target)?;
    let response = transport.request(&path, "GET", None)?;
    let status = text(field(&response, "status")?)?;
    match status {
        "active" => Ok(true),
        "abandoned" | "completed" => Ok(false),
        _ => Err(fail("service", "Unexpected Azure pull request status")),
    }
}

/// Uploads the exact frozen archive once and returns its attachment receipt.
pub(crate) fn upload(
    transport: &mut Transport<'_>,
    target: &Target,
    snapshot: &Value,
) -> Result<Value, AppError> {
    route(transport, target)?;
    attachment_name(snapshot)?;
    let archive_path = text(field(snapshot, "archivePath")?)?;
    if archive_path.is_empty() {
        return Err(fail("invalidInput", "Missing frozen overview archive path"));
    }
    let bytes = fs::read(archive_path)?;
    upload_bytes(transport, target, snapshot, &bytes)
}

/// Keeps file reads separate from the byte-preserving publication boundary.
fn upload_bytes(
    transport: &mut Transport<'_>,
    target: &Target,
    snapshot: &Value,
    bytes: &[u8],
) -> Result<Value, AppError> {
    let path = route(transport, target)?;
    let name = attachment_name(snapshot)?;
    check_hash(bytes, snapshot)?;
    let path = format!("{path}/attachments/{name}");
    let response = transport.ado_binary(&path, "POST", Some(bytes), &[], "7.1")?;
    if response
        .headers
        .get("x-ms-continuationtoken")
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Err(fail(
            "ambiguousWrite",
            "Unexpected Azure attachment upload continuation",
        ));
    }
    // A successful HTTP write with malformed metadata still may have created
    // an attachment, so it must not be interpreted as a retryable failure.
    let attachment: Value = from_slice(&response.body).map_err(|error| {
        crate::error::caused_by(
            "ambiguousWrite",
            "Azure attachment upload returned invalid JSON".to_owned(),
            error,
        )
    })?;
    let id = field(&attachment, "id")
        .and_then(|id| attachment_id(id, &name).map(|()| id.clone()))
        .map_err(|error| {
            crate::error::caused_by(
                "ambiguousWrite",
                "Azure attachment upload returned an invalid ID",
                error.into_std_error(),
            )
        })?;
    Ok(json!({
        "id": id,
        "url": attachment_url(target, &path)?,
        "fileName": name,
    }))
}

/// Verifies downloaded bytes and the receipt before the comment can link them.
pub(crate) fn verify_artifact(
    transport: &mut Transport<'_>,
    target: &Target,
    snapshot: &Value,
    payload: &Value,
) -> Result<Value, AppError> {
    let path = route(transport, target)?;
    let name = attachment_name(snapshot)?;
    attachment_id(field(payload, "id")?, &name)?;
    if text(field(payload, "fileName")?)? != name {
        return Err(fail(
            "invalidInput",
            "Overview attachment filename mismatch",
        ));
    }
    let path = format!("{path}/attachments/{name}");
    if text(field(payload, "url")?)? != attachment_url(target, &path)? {
        return Err(fail("invalidInput", "Overview attachment URL mismatch"));
    }
    let response = transport.ado_binary(&path, "GET", None, &[], "7.1")?;
    if response
        .headers
        .get("x-ms-continuationtoken")
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Err(fail("service", "Unexpected Azure attachment continuation"));
    }
    check_hash(&response.body, snapshot)?;
    Ok(payload.clone())
}

/// Creates a separate resolved, unanchored informational thread once.
pub(crate) fn comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<Value, AppError> {
    let path = route(transport, target)?;
    let body = text(field(payload, "body")?)?;
    if body.is_empty() {
        return Err(fail("invalidInput", "Overview comment body is empty"));
    }
    let response = transport.ado_write_with_rate_limit_retries(
        &format!("{path}/threads"),
        "POST",
        &json!({
            "status": "fixed",
            "comments": [{ "parentCommentId": 0, "commentType": "text", "content": body }],
        }),
    )?;
    let id = field(&response, "id")
        .and_then(thread_id)
        .map_err(|error| {
            crate::error::caused_by(
                "ambiguousWrite",
                "Azure thread creation returned an invalid ID",
                error.into_std_error(),
            )
        })?;
    Ok(json!({ "id": id.get(), "url": thread_url(target, id)? }))
}

/// Confirms the complete root comment and resolved status without modifying it.
pub(crate) fn verify_comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<(), AppError> {
    let path = route(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let id = thread_id(field(field(payload, "comment")?, "id")?)?;
    let response = transport.request(&format!("{path}/threads/{id}"), "GET", None)?;
    let comments = array(field(&response, "comments")?)?;
    let first = comments
        .first()
        .ok_or_else(|| fail("service", "Azure overview thread has no root comment"))?;
    if text(field(first, "content")?)? != body {
        return Err(fail(
            "service",
            "Azure overview comment body does not match",
        ));
    }
    let status = field(&response, "status")?;
    if status.as_str() != Some("fixed") && status.as_u64() != Some(2) {
        return Err(fail(
            "service",
            "Azure overview thread is not resolved; do not repost it",
        ));
    }
    Ok(())
}

/// Restricts requests and constructed links to the resolved collection and PR.
fn route(transport: &Transport<'_>, target: &Target) -> Result<String, AppError> {
    if transport.platform != Platform::AzureDevOps
        || target.platform != Platform::AzureDevOps
        || target.pull_request_id == 0
        || target
            .project
            .as_deref()
            .is_none_or(|s| s.trim().is_empty() || matches!(s, "." | ".."))
        || target.repository.trim().is_empty()
        || matches!(target.repository.as_str(), "." | "..")
        // Bound both provider routes and returned browser links before a write.
        || target.pr_api().len() > MAX_ROUTE_LENGTH
    {
        return Err(fail(
            "invalidInput",
            "A resolved Azure pull request is required",
        ));
    }
    let target_collection = collection(target)?;
    if transport
        .ado_collection()
        .map(|url| url.trim_end_matches('/'))
        != Some(target_collection)
    {
        return Err(fail(
            "invalidInput",
            "Azure transport and pull request collections differ",
        ));
    }
    Ok(target.pr_api())
}

/// Restricts links to an authenticated HTTPS collection without decorations.
fn collection(target: &Target) -> Result<&str, AppError> {
    let collection = target
        .collection_url
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "Missing Azure collection URL"))?;
    let url = Url::parse(collection).map_err(|error| {
        crate::error::caused_by("invalidInput", "Invalid Azure collection URL", error)
    })?;
    if collection.len() > MAX_ROUTE_LENGTH
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str().trim_end_matches('/') != collection.trim_end_matches('/')
    {
        return Err(fail("invalidInput", "Invalid Azure collection URL"));
    }
    Ok(collection.trim_end_matches('/'))
}

/// Derives attachment routing from bounded frozen identifiers, never from a URL.
fn attachment_name(snapshot: &Value) -> Result<String, AppError> {
    let run = text(field(snapshot, "runId")?)?;
    let hash = text(field(snapshot, "archiveHash")?)?;
    if !lower_hex(run, 32) || !lower_hex(hash, 64) {
        return Err(fail("invalidInput", "Invalid frozen overview identifiers"));
    }
    Ok(format!("ss-review-{run}-{hash}.zip"))
}

/// Allows only the canonical lowercase hexadecimal representation.
fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Compares raw bytes to the immutable archive digest.
fn check_hash(bytes: &[u8], snapshot: &Value) -> Result<(), AppError> {
    let expected = text(field(snapshot, "archiveHash")?)?;
    if !lower_hex(expected, 64) || format!("{:x}", Sha256::digest(bytes)) != expected {
        return Err(fail(
            "service",
            "Azure attachment does not match the frozen archive",
        ));
    }
    Ok(())
}

/// Accepts only the generated filename used to route this attachment.
fn attachment_id(value: &Value, name: &str) -> Result<(), AppError> {
    if value.as_str() == Some(name) {
        return Ok(());
    }
    Err(fail("service", "Invalid Azure attachment identifier"))
}

/// Restricts thread identifiers to the positive integer range accepted by ADO.
fn thread_id(value: &Value) -> Result<NonZeroU32, AppError> {
    let parsed = value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .or_else(|| {
            let id = value.as_str()?;
            if id.is_empty() || id.len() > 10 || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            id.parse::<u32>().ok()
        });
    parsed
        .and_then(NonZeroU32::new)
        .ok_or_else(|| fail("service", "Invalid Azure thread identifier"))
}

/// Constructs the same versioned attachment URL used for authenticated reads.
fn attachment_url(target: &Target, path: &str) -> Result<String, AppError> {
    Ok(format!("{}/{path}?api-version=7.1", collection(target)?))
}

/// Builds the browser link from validated coordinates rather than service data.
fn thread_url(target: &Target, id: NonZeroU32) -> Result<String, AppError> {
    collection(target)?;
    let mut canonical = target.clone();
    canonical.refresh_url();
    Ok(format!("{}?discussionId={id}", canonical.web_url))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use serde_json::to_vec;

    use super::*;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput};

    /// Retains the expected request and its scripted response.
    type Reply = (String, String, Option<Value>, Vec<u8>);

    /// Supplies a scripted transport so no test contacts a provider.
    struct Mock {
        replies: RefCell<VecDeque<Reply>>,
    }

    impl Mock {
        fn new(replies: Vec<(&str, &str, Option<Value>, Vec<u8>)>) -> Self {
            Self {
                replies: RefCell::new(
                    replies
                        .into_iter()
                        .map(|(method, path, body, response)| {
                            (method.to_owned(), path.to_owned(), body, response)
                        })
                        .collect(),
                ),
            }
        }

        fn response(&self, method: &str, url: &str, body: Option<&[u8]>) -> HttpResponse {
            let (expected_method, expected_url, expected_body, response) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(method, expected_method);
            assert_eq!(url, expected_url);
            match expected_body {
                Some(expected) => {
                    assert_eq!(from_slice::<Value>(body.unwrap()).unwrap(), expected);
                }
                None => assert!(body.is_none()),
            }
            HttpResponse::new(200, BTreeMap::new(), response)
        }
    }

    impl Process for Mock {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "az");
            Ok(ProcessOutput::new(true, b"token".to_vec(), Vec::new()))
        }
    }

    impl Http for Mock {
        fn send(
            &self,
            method: &str,
            url: &str,
            _: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Ok(self.response(method, url, body))
        }

        fn send_binary(
            &self,
            method: &str,
            url: &str,
            _: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Ok(self.response(method, url, body))
        }
    }

    /// Rejects any unexpected retry without consulting a real clock.
    struct NoWait;

    impl Clock for NoWait {
        fn now(&self) -> Timestamp {
            "2026-01-01T00:00:00Z".parse().unwrap()
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("unexpected retry")
        }
    }

    fn target() -> Target {
        Target::pull_request(
            "https://dev.azure.com/org/Project/_git/repo/pullrequest/7",
            None,
        )
        .unwrap()
    }

    fn snapshot(bytes: &[u8]) -> Value {
        json!({
            "runId": "0123456789abcdef0123456789abcdef",
            "archiveHash": format!("{:x}", Sha256::digest(bytes)),
        })
    }

    #[test]
    fn uploads_exact_frozen_bytes_once() {
        /// Captures the binary write independently of JSON handling.
        struct Upload<'a> {
            bytes: &'a [u8],
            url: &'a str,
            response: Vec<u8>,
            calls: Cell<usize>,
        }

        impl Http for Upload<'_> {
            fn send(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: Option<&[u8]>,
            ) -> Result<HttpResponse, AppError> {
                panic!("unexpected JSON request")
            }

            fn send_binary(
                &self,
                method: &str,
                url: &str,
                _: &str,
                body: Option<&[u8]>,
            ) -> Result<HttpResponse, AppError> {
                assert_eq!(method, "POST");
                assert_eq!(url, self.url);
                assert_eq!(body, Some(self.bytes));
                self.calls.set(self.calls.get() + 1);
                Ok(HttpResponse::new(
                    200,
                    BTreeMap::new(),
                    self.response.clone(),
                ))
            }
        }

        let target = target();
        let bytes = b"PK\x03\x04\0\xff";
        let snapshot = snapshot(bytes);
        let name = attachment_name(&snapshot).unwrap();
        let path = format!("{}/attachments/{name}", target.pr_api());
        let url = attachment_url(&target, &path).unwrap();
        let mock = Mock::new(Vec::new());
        let http = Upload {
            bytes,
            url: &url,
            response: format!(r#"{{"id":"{name}"}}"#).into_bytes(),
            calls: Cell::new(0),
        };
        let mut transport = Transport::new(&target, None, &mock, &http, &NoWait);
        upload_bytes(&mut transport, &target, &snapshot, b"wrong").unwrap_err();
        assert_eq!(http.calls.get(), 0);
        assert_eq!(
            upload_bytes(&mut transport, &target, &snapshot, bytes).unwrap(),
            json!({"id": name, "url": url, "fileName": name})
        );
        assert_eq!(http.calls.get(), 1);
    }

    #[test]
    fn verifies_exact_binary_archive_and_scoped_receipt() {
        // A leading HTML-like byte sequence remains archive data at this boundary.
        let bytes = b"<binary \0\xff";
        let snapshot = snapshot(bytes);
        let target = target();
        let path = format!(
            "{}/attachments/{}",
            target.pr_api(),
            attachment_name(&snapshot).unwrap()
        );
        let url = attachment_url(&target, &path).unwrap();
        let name = attachment_name(&snapshot).unwrap();
        let receipt = json!({"id": name, "url": url, "fileName": name});
        let mock = Mock::new(vec![("GET", &url, None, bytes.to_vec())]);
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        assert_eq!(
            verify_artifact(&mut transport, &target, &snapshot, &receipt).unwrap(),
            receipt
        );
        assert!(mock.replies.borrow().is_empty());

        let mock = Mock::new(vec![("GET", &url, None, b"different".to_vec())]);
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        verify_artifact(&mut transport, &target, &snapshot, &receipt).unwrap_err();
        assert!(mock.replies.borrow().is_empty());

        let mock = Mock::new(Vec::new());
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        let mut altered = receipt.clone();
        *altered.get_mut("url").unwrap() = json!("https://other.invalid/file");
        verify_artifact(&mut transport, &target, &snapshot, &altered).unwrap_err();
    }

    #[test]
    fn creates_and_checks_resolved_unanchored_thread() {
        let target = target();
        let url = format!(
            "https://dev.azure.com/org/{}/threads?api-version=7.1",
            target.pr_api()
        );
        let body = "prefix\n\n[overview](https://example.test/)";
        let expected = json!({
            "status": "fixed",
            "comments": [{"parentCommentId": 0, "commentType": "text", "content": body}]
        });
        let mock = Mock::new(vec![(
            "POST",
            &url,
            Some(expected),
            br#"{"id":5}"#.to_vec(),
        )]);
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        let receipt = comment(&mut transport, &target, &json!({"body": body})).unwrap();
        assert_eq!(
            receipt,
            json!({"id": 5, "url": format!("{}?discussionId=5", target.web_url)})
        );
        assert!(mock.replies.borrow().is_empty());

        let url = format!(
            "https://dev.azure.com/org/{}/threads/5?api-version=7.1",
            target.pr_api()
        );
        for (status, content, expected) in [
            (json!("fixed"), body, true),
            (json!(2), body, true),
            (json!("active"), body, false),
            (json!("fixed"), "prefix", false),
        ] {
            let response = json!({"status": status, "comments": [{"content": content}]});
            let mock = Mock::new(vec![("GET", &url, None, to_vec(&response).unwrap())]);
            let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
            assert_eq!(
                verify_comment(
                    &mut transport,
                    &target,
                    &json!({"body": body, "comment": receipt})
                )
                .is_ok(),
                expected
            );
            assert!(mock.replies.borrow().is_empty());
        }
    }

    #[test]
    fn refuses_unbounded_identifiers_and_malformed_receipts() {
        let snapshot = snapshot(b"archive");
        let mut invalid = snapshot.clone();
        *invalid.get_mut("runId").unwrap() = json!("../bad");
        attachment_name(&invalid).unwrap_err();
        invalid = snapshot.clone();
        *invalid.get_mut("archiveHash").unwrap() = json!("f".repeat(65));
        attachment_name(&invalid).unwrap_err();
        for id in [json!(0), json!(-1), json!("../../1"), json!("4294967296")] {
            thread_id(&id).unwrap_err();
        }
        for id in [json!(1), json!("1"), json!(u32::MAX)] {
            assert_ne!(thread_id(&id).unwrap().get(), 0);
        }
        attachment_id(
            &json!("../attachment"),
            &attachment_name(&snapshot).unwrap(),
        )
        .unwrap_err();
        check_hash(b"archive", &snapshot).unwrap();
        check_hash(b"other", &snapshot).unwrap_err();
    }

    #[test]
    fn checks_pull_request_status_and_transport_scope() {
        let target = target();
        let url = format!(
            "https://dev.azure.com/org/{}?api-version=7.1",
            target.pr_api()
        );
        for (status, expected) in [
            ("active", Some(true)),
            ("completed", Some(false)),
            ("abandoned", Some(false)),
            ("unexpected", None),
        ] {
            let response = json!({"status": status});
            let mock = Mock::new(vec![("GET", &url, None, to_vec(&response).unwrap())]);
            let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
            assert_eq!(is_open(&mut transport, &target).ok(), expected);
            assert!(mock.replies.borrow().is_empty());
        }

        let mock = Mock::new(Vec::new());
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        let mut other = target.clone();
        other.collection_url = Some("https://dev.azure.com/other".to_owned());
        is_open(&mut transport, &other).unwrap_err();
    }

    #[test]
    fn malformed_thread_acknowledgement_is_not_replayed() {
        let target = target();
        let url = format!(
            "https://dev.azure.com/org/{}/threads?api-version=7.1",
            target.pr_api()
        );
        let body = "overview";
        let expected = json!({
            "status": "fixed",
            "comments": [{"parentCommentId": 0, "commentType": "text", "content": body}]
        });
        let mock = Mock::new(vec![(
            "POST",
            &url,
            Some(expected),
            br#"{"id":"../5"}"#.to_vec(),
        )]);
        let mut transport = Transport::new(&target, None, &mock, &mock, &NoWait);
        let error = comment(&mut transport, &target, &json!({"body": body})).unwrap_err();
        assert_eq!(crate::classification(&error), "ambiguousWrite");
        assert!(mock.replies.borrow().is_empty());
    }
}
