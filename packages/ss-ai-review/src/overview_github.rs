//! Publishes and verifies the frozen initial overview through GitHub.

use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use ohno::AppError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::Process;
use crate::error::fail;
use crate::json::{array, boolean, field, text};
use crate::target::{Platform, Target};
use crate::transport::Transport;

/// Restricts provider operations to concrete GitHub pull requests.
fn check_target(transport: &Transport<'_>, target: &Target) -> Result<(), AppError> {
    if transport.platform != Platform::GitHub
        || target.platform != Platform::GitHub
        || target.pull_request_id == 0
        || target.organization.trim().is_empty()
        || target.repository.trim().is_empty()
    {
        return Err(fail("invalidInput", "A GitHub pull request is required"));
    }
    Ok(())
}

/// Checks current PR state immediately before a publication effect.
pub(crate) fn is_open(transport: &mut Transport<'_>, target: &Target) -> Result<bool, AppError> {
    check_target(transport, target)?;
    let response = transport.request(&target.pr_api(), "GET", None)?;
    match text(field(&response, "state")?)? {
        "open" => Ok(true),
        "closed" => Ok(false),
        _ => Err(fail("service", "Unexpected GitHub pull request state")),
    }
}

/// Creates the unlisted Gist once using the frozen Markdown bytes.
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The provider interface consistently accepts exclusive transport access"
)]
pub(crate) fn upload(
    transport: &mut Transport<'_>,
    target: &Target,
    snapshot: &Value,
) -> Result<Value, AppError> {
    check_target(transport, target)?;
    let path = text(field(snapshot, "markdownPath")?)?;
    let bytes = fs::read(path)?;
    upload_bytes(transport, snapshot, &bytes)
}

/// Keeps the file boundary separate from the single non-idempotent request.
fn upload_bytes(
    transport: &Transport<'_>,
    snapshot: &Value,
    bytes: &[u8],
) -> Result<Value, AppError> {
    check_hash(snapshot, bytes)?;
    let content = str::from_utf8(bytes).map_err(|error| {
        crate::error::caused_by(
            "invalidState",
            "Frozen overview is not UTF-8".to_owned(),
            error,
        )
    })?;
    let run = text(field(snapshot, "runId")?)?;
    // This metadata budget permits compatible identifiers beyond generated UUIDs
    // without treating the bound as a Gist provider restriction.
    if run.is_empty() || run.len() > 128 {
        return Err(fail("invalidState", "Invalid overview run identifier"));
    }
    let body = json!({"public":false, "description":format!("ss-review initial overview {run}"),
        "files":{"overview.md":{"content":content}}});
    let response = transport.github_write_once_with_refusal_evidence("gists", &body)?;
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| gist_id(id))
        .ok_or_else(|| {
            fail(
                "ambiguousWrite",
                "Gist creation returned no usable identifier",
            )
        })?;
    // Keep an acknowledged ID even when ancillary URL evidence is absent.
    Ok(json!({"id":id, "url":bounded_url(response.get("html_url"))}))
}

/// Checks unlisted status and exact revision bytes before returning a rendered link.
pub(crate) fn verify_artifact(
    transport: &mut Transport<'_>,
    target: &Target,
    snapshot: &Value,
    payload: &Value,
) -> Result<Value, AppError> {
    check_target(transport, target)?;
    let parent = Path::new(text(field(snapshot, "markdownPath")?)?)
        .parent()
        .ok_or_else(|| fail("invalidState", "Overview path has no parent"))?;
    verify_artifact_with(transport, snapshot, payload, |transport, id, revision| {
        read_revision(transport.process(), parent, id, revision)
    })
}

/// Separates revision retrieval from API interpretation for injected offline tests.
fn verify_artifact_with(
    transport: &mut Transport<'_>,
    snapshot: &Value,
    payload: &Value,
    read_revision: impl FnOnce(&Transport<'_>, &str, &str) -> Result<Vec<u8>, AppError>,
) -> Result<Value, AppError> {
    let id = text(field(payload, "id")?)?;
    if !gist_id(id) {
        return Err(fail("invalidState", "Invalid Gist identifier"));
    }
    let gist = transport.request(&format!("gists/{id}"), "GET", None)?;
    if boolean(field(&gist, "public")?)? {
        return Err(fail("service", "The report Gist is not unlisted"));
    }
    if text(field(&gist, "id")?)? != id {
        return Err(fail("service", "GitHub returned another Gist"));
    }
    let file = field(field(&gist, "files")?, "overview.md")?;
    let history = array(field(&gist, "history")?)?;
    let revision = text(field(
        history
            .first()
            .ok_or_else(|| fail("service", "Gist has no revision"))?,
        "version",
    )?)?;
    if !revision_id(revision) {
        return Err(fail("service", "Invalid Gist revision"));
    }
    if boolean(field(file, "truncated")?)? {
        let bytes = read_revision(transport, id, revision)?;
        check_hash(snapshot, &bytes)?;
    } else {
        check_hash(snapshot, text(field(file, "content")?)?.as_bytes())?;
    }
    let url = gist_url(text(field(&gist, "html_url")?)?, id)?;
    Ok(json!({"id":id, "revision":revision, "url":format!("{url}/{revision}")}))
}

/// Creates the PR conversation comment without replaying a refused or uncertain write.
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The provider interface consistently accepts exclusive transport access"
)]
pub(crate) fn comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<Value, AppError> {
    check_target(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let response = transport.github_write_once_with_refusal_evidence(
        &format!(
            "{}/issues/{}/comments",
            target.repository_api(),
            target.pull_request_id
        ),
        &json!({"body":body}),
    )?;
    let id = comment_id(response.get("id")).ok_or_else(|| {
        fail(
            "ambiguousWrite",
            "Comment creation returned no usable identifier",
        )
    })?;
    Ok(json!({"id":id.get(), "url":bounded_url(response.get("html_url"))}))
}

/// Requires the complete posted body to match the intended comment exactly.
pub(crate) fn verify_comment(
    transport: &mut Transport<'_>,
    target: &Target,
    payload: &Value,
) -> Result<(), AppError> {
    check_target(transport, target)?;
    let body = text(field(payload, "body")?)?;
    let id = comment_id(field(payload, "comment")?.get("id"))
        .ok_or_else(|| fail("invalidState", "Invalid comment identifier"))?;
    let comment = transport.request(
        &format!("{}/issues/comments/{id}", target.repository_api()),
        "GET",
        None,
    )?;
    if comment_id(comment.get("id")) != Some(id) || text(field(&comment, "body")?)? != body {
        return Err(fail("service", "GitHub comment read-back does not match"));
    }
    Ok(())
}

/// Allows only bounded GitHub Gist IDs in routes and Git URLs.
fn gist_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Git revisions must be complete lowercase object IDs, not revision expressions.
fn revision_id(id: &str) -> bool {
    (40..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Comment database IDs remain bounded numeric route segments.
fn comment_id(value: Option<&Value>) -> Option<NonZeroU64> {
    let value = value?;
    let id = value.as_u64().or_else(|| value.as_str()?.parse().ok())?;
    NonZeroU64::new(id)
}

/// Ancillary response URLs are bounded before they enter a persisted acknowledgement.
fn bounded_url(value: Option<&Value>) -> Option<&str> {
    value?
        .as_str()
        .filter(|url| !url.is_empty() && url.len() <= 4096)
}

/// Only a credential-free rendered Gist URL for the requested ID can be linked.
fn gist_url(value: &str, id: &str) -> Result<String, AppError> {
    if value.len() > 4096 {
        return Err(fail("service", "GitHub returned an unexpected Gist URL"));
    }
    let url = Url::parse(value).map_err(|error| {
        crate::error::caused_by("service", "Invalid Gist URL".to_owned(), error)
    })?;
    let path = url.path().trim_end_matches('/');
    let parts: Vec<_> = path.split('/').skip(1).collect();
    if url.scheme() != "https"
        || url.host_str() != Some("gist.github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || parts.len() != 2
        || parts.first().is_none_or(|owner| {
            owner.is_empty()
                || !owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || parts.get(1) != Some(&id)
    {
        return Err(fail("service", "GitHub returned an unexpected Gist URL"));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Compares the exact report bytes with the frozen lowercase SHA-256 digest.
fn check_hash(snapshot: &Value, bytes: &[u8]) -> Result<(), AppError> {
    let expected = text(field(snapshot, "markdownHash")?)?;
    if expected != format!("{:x}", Sha256::digest(bytes)) {
        return Err(fail(
            "service",
            "Overview bytes do not match the frozen SHA-256",
        ));
    }
    Ok(())
}

/// Removes only the private directory created for this invocation.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        _ = fs::remove_dir_all(&self.0);
    }
}

/// Fetches complete truncated content by immutable revision without credentials.
#[expect(
    clippy::create_dir,
    reason = "Exclusive creation prevents reusing or deleting another invocation's directory"
)]
fn read_revision(
    process: &dyn Process,
    parent: &Path,
    id: &str,
    revision: &str,
) -> Result<Vec<u8>, AppError> {
    if !gist_id(id) || !revision_id(revision) {
        return Err(fail("service", "Invalid Gist or revision identifier"));
    }
    let path = parent.join(format!(".overview-gist-{}", Uuid::new_v4()));
    fs::create_dir(&path)?;
    let scratch = Scratch(path);
    git_revision(process, &scratch.0, id, revision)
}

/// Reads Git stdout as bytes so the final newline remains part of the digest.
fn git_revision(
    process: &dyn Process,
    directory: &Path,
    id: &str,
    revision: &str,
) -> Result<Vec<u8>, AppError> {
    // Isolated Git configuration prevents inherited credentials or URL rewrites.
    let common = [
        "--no-pager",
        "--no-replace-objects",
        "-c",
        "credential.helper=",
        "-c",
        "http.followRedirects=false",
    ];
    let clone = common
        .iter()
        .map(|value| (*value).to_owned())
        .chain([
            "clone".to_owned(),
            "--bare".to_owned(),
            "--quiet".to_owned(),
            format!("https://gist.github.com/{id}.git"),
            "repo.git".to_owned(),
        ])
        .collect::<Vec<_>>();
    if !process.isolated_git(directory, &clone)?.success {
        return Err(fail("service", "Unable to clone the unlisted Gist"));
    }
    let show = common
        .iter()
        .map(|value| (*value).to_owned())
        .chain([
            "--git-dir".to_owned(),
            "repo.git".to_owned(),
            "show".to_owned(),
            format!("{revision}:overview.md"),
        ])
        .collect::<Vec<_>>();
    let output = process.isolated_git(directory, &show)?;
    if !output.success {
        return Err(fail("service", "Unable to read the complete Gist revision"));
    }
    Ok(output.stdout)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests inspect bounded requests and JSON fixtures"
)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, from_slice, json};
    use sha2::{Digest, Sha256};

    use super::{
        comment, comment_id, git_revision, is_open, upload_bytes, verify_artifact_with,
        verify_comment,
    };
    use crate::target::Target;
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, classification};

    type Request = (&'static str, &'static str, Option<Value>, Value, bool);
    type GitResponse = (Vec<String>, Vec<u8>, bool);

    /// Matches every request and returns controlled GitHub or Git evidence.
    struct Script {
        requests: RefCell<VecDeque<Request>>,
        git: RefCell<VecDeque<GitResponse>>,
    }

    impl Process for Script {
        fn execute(
            &self,
            program: &str,
            arguments: &[String],
            input: &[u8],
        ) -> io::Result<ProcessOutput> {
            assert_eq!(program, "gh");
            let (path, method, body, response, success) = self
                .requests
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(arguments[1], path);
            assert_eq!(arguments[3], method);
            assert_eq!(
                body,
                if input.is_empty() {
                    None
                } else {
                    Some(from_slice(input).unwrap())
                }
            );
            let response = if success {
                response.to_string()
            } else {
                let status = response
                    .get("__status")
                    .and_then(Value::as_u64)
                    .unwrap_or(500);
                format!("HTTP/2 {status}\r\n\r\n{response}")
            };
            Ok(ProcessOutput::new(
                success,
                response.into_bytes(),
                Vec::new(),
            ))
        }

        fn isolated_git(
            &self,
            directory: &Path,
            arguments: &[String],
        ) -> io::Result<ProcessOutput> {
            assert_eq!(directory, Path::new("owned"));
            let (expected, output, success) = self
                .git
                .borrow_mut()
                .pop_front()
                .expect("unexpected git invocation");
            assert_eq!(arguments, expected);
            Ok(ProcessOutput::new(success, output, Vec::new()))
        }
    }

    impl Http for Script {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            panic!("GitHub uses gh")
        }
    }

    struct NoWait;
    impl Clock for NoWait {
        fn now(&self) -> Timestamp {
            "2026-01-01T00:00:00Z".parse().unwrap()
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("no retry expected")
        }
    }

    fn script(requests: Vec<Request>) -> Script {
        Script {
            requests: RefCell::new(requests.into()),
            git: RefCell::new(VecDeque::new()),
        }
    }

    fn snapshot(bytes: &[u8]) -> Value {
        json!({"runId":"run", "markdownHash":format!("{:x}", Sha256::digest(bytes))})
    }

    fn gist(content: &str, truncated: bool) -> Value {
        json!({"id":"abc123","public":false,"html_url":"https://gist.github.com/alice/abc123",
            "history":[{"version":"a".repeat(40)}],
            "files":{"overview.md":{"content":content,"truncated":truncated}}})
    }

    #[test]
    fn comment_ids_preserve_positive_numeric_routes() {
        for value in [json!(1), json!("1"), json!(u64::MAX)] {
            assert_ne!(comment_id(Some(&value)).unwrap().get(), 0);
        }
        for value in [
            json!(0),
            json!("0"),
            json!(-1),
            json!("18446744073709551616"),
        ] {
            assert!(comment_id(Some(&value)).is_none());
        }
    }

    #[test]
    fn publishes_exact_unicode_and_verifies_complete_comment() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let bytes = "Mermaid é 🐙\r\nend\n".as_bytes();
        let body = "[Copilot speaking]\n\n[overview](https://gist.github.com/alice/abc123)";
        let fixture = script(vec![
            (
                "repos/acme/repo/pulls/42",
                "GET",
                None,
                json!({"state":"open"}),
                true,
            ),
            (
                "gists",
                "POST",
                Some(
                    json!({"public":false, "description":"ss-review initial overview run",
                "files":{"overview.md":{"content":str::from_utf8(bytes).unwrap()}}}),
                ),
                json!({"id":"abc123","html_url":"https://gist.github.com/alice/abc123"}),
                true,
            ),
            (
                "gists/abc123",
                "GET",
                None,
                gist(str::from_utf8(bytes).unwrap(), false),
                true,
            ),
            (
                "repos/acme/repo/issues/42/comments",
                "POST",
                Some(json!({"body":body})),
                json!({"id":123,"html_url":"https://github.com/acme/repo/issues/42#issuecomment-123"}),
                true,
            ),
            (
                "repos/acme/repo/issues/comments/123",
                "GET",
                None,
                json!({"id":123,"body":body}),
                true,
            ),
        ]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        assert!(is_open(&mut transport, &target).unwrap());
        let receipt = upload_bytes(&transport, &snapshot(bytes), bytes).unwrap();
        let verified =
            verify_artifact_with(&mut transport, &snapshot(bytes), &receipt, |_, _, _| {
                panic!("not truncated")
            })
            .unwrap();
        assert_eq!(
            verified["url"],
            format!("https://gist.github.com/alice/abc123/{}", "a".repeat(40))
        );
        let comment = comment(&mut transport, &target, &json!({"body":body})).unwrap();
        verify_comment(
            &mut transport,
            &target,
            &json!({"body":body, "comment":comment}),
        )
        .unwrap();
        assert!(fixture.requests.borrow().is_empty());
    }

    #[test]
    fn truncated_content_uses_exact_revision_and_exact_bytes() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let bytes = "complete é\r\nlast\n".as_bytes();
        for result in [bytes.to_vec(), bytes[..bytes.len() - 1].to_vec()] {
            let fixture = script(vec![(
                "gists/abc123",
                "GET",
                None,
                gist("partial", true),
                true,
            )]);
            let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
            let valid = result == bytes;
            let result = verify_artifact_with(
                &mut transport,
                &snapshot(bytes),
                &json!({"id":"abc123"}),
                |_, id, revision| {
                    assert_eq!(id, "abc123");
                    assert_eq!(revision, "a".repeat(40));
                    Ok(result)
                },
            );
            assert_eq!(result.is_ok(), valid);
        }
    }

    #[test]
    fn revision_fetch_disables_helpers_and_preserves_stdout() {
        let fixture = script(vec![]);
        let prefix = [
            "--no-pager",
            "--no-replace-objects",
            "-c",
            "credential.helper=",
            "-c",
            "http.followRedirects=false",
        ];
        let args = |suffix: Vec<String>| {
            prefix
                .iter()
                .map(|value| (*value).to_owned())
                .chain(suffix)
                .collect()
        };
        fixture.git.borrow_mut().extend([
            (
                args(vec![
                    "clone".into(),
                    "--bare".into(),
                    "--quiet".into(),
                    "https://gist.github.com/abc123.git".into(),
                    "repo.git".into(),
                ]),
                vec![],
                true,
            ),
            (
                args(vec![
                    "--git-dir".into(),
                    "repo.git".into(),
                    "show".into(),
                    format!("{}:overview.md", "a".repeat(40)),
                ]),
                b"content\r\n".to_vec(),
                true,
            ),
        ]);
        assert_eq!(
            git_revision(&fixture, Path::new("owned"), "abc123", &"a".repeat(40)).unwrap(),
            b"content\r\n"
        );
        assert!(fixture.git.borrow().is_empty());
    }

    #[test]
    fn rejects_untrusted_or_incomplete_readback() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let bytes = b"frozen\n";
        let mut variants = vec![];
        for url in [
            "http://gist.github.com/alice/abc123",
            "https://user:password@gist.github.com/alice/abc123",
            "https://gist.github.com.evil.example/alice/abc123",
            "https://gist.github.com/alice/other",
            "https://gist.github.com/alice/abc123?query=1",
            "https://gist.github.com:444/alice/abc123",
        ] {
            let mut value = gist("frozen\n", false);
            value["html_url"] = json!(url);
            variants.push(value);
        }
        let mut public = gist("frozen\n", false);
        public["public"] = json!(true);
        variants.push(public);
        let mut truncated = gist("frozen\n", false);
        truncated["files"]["overview.md"]["truncated"] = Value::Null;
        variants.push(truncated);
        let mut wrong_id = gist("frozen\n", false);
        wrong_id["id"] = json!("deaf");
        variants.push(wrong_id);
        let mut bad_revision = gist("frozen\n", false);
        bad_revision["history"][0]["version"] = json!("HEAD");
        variants.push(bad_revision);
        variants.push(gist("frozen", false));
        for value in variants {
            let fixture = script(vec![("gists/abc123", "GET", None, value, true)]);
            let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
            verify_artifact_with(
                &mut transport,
                &snapshot(bytes),
                &json!({"id":"abc123"}),
                |_, _, _| panic!("not truncated"),
            )
            .unwrap_err();
        }
    }

    #[test]
    fn write_failure_is_not_replayed_and_acknowledgement_retains_id() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let bytes = b"content";
        let expected = json!({"public":false,"description":"ss-review initial overview run","files":{"overview.md":{"content":"content"}}});
        let fixture = script(vec![(
            "gists",
            "POST",
            Some(expected.clone()),
            json!({"message":"server error"}),
            false,
        )]);
        let transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        let error = upload_bytes(&transport, &snapshot(bytes), bytes).unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert!(fixture.requests.borrow().is_empty());
        let fixture = script(vec![(
            "gists",
            "POST",
            Some(expected),
            json!({"id":"abc123","html_url":null}),
            true,
        )]);
        let transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        assert_eq!(
            upload_bytes(&transport, &snapshot(bytes), bytes).unwrap(),
            json!({"id":"abc123","url":null})
        );
    }

    #[test]
    fn rejects_incomplete_or_modified_comment_and_unusable_ids() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        for response in [
            json!({"id":12,"body":"full"}),
            json!({"id":13,"body":"full\n"}),
        ] {
            let fixture = script(vec![(
                "repos/acme/repo/issues/comments/12",
                "GET",
                None,
                response,
                true,
            )]);
            let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
            verify_comment(
                &mut transport,
                &target,
                &json!({"body":"full\n","comment":{"id":12}}),
            )
            .unwrap_err();
        }
        let fixture = script(vec![(
            "repos/acme/repo/issues/42/comments",
            "POST",
            Some(json!({"body":"text"})),
            json!({"id":0}),
            true,
        )]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        assert_eq!(
            classification(&comment(&mut transport, &target, &json!({"body":"text"})).unwrap_err()),
            "ambiguousWrite"
        );
    }

    #[test]
    fn comment_failure_is_not_replayed_and_closed_state_is_distinct() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let fixture = script(vec![
            (
                "repos/acme/repo/pulls/42",
                "GET",
                None,
                json!({"state":"closed"}),
                true,
            ),
            (
                "repos/acme/repo/issues/42/comments",
                "POST",
                Some(json!({"body":"text"})),
                json!({"message":"server error"}),
                false,
            ),
        ]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        assert!(!is_open(&mut transport, &target).unwrap());
        let error = comment(&mut transport, &target, &json!({"body":"text"})).unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert!(fixture.requests.borrow().is_empty());
    }

    #[test]
    fn both_writes_preserve_explicit_rate_refusal_without_replay() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let bytes = b"content";
        let fixture = script(vec![
            (
                "gists",
                "POST",
                Some(
                    json!({"public":false,"description":"ss-review initial overview run",
                    "files":{"overview.md":{"content":"content"}}}),
                ),
                json!({"__status":429}),
                false,
            ),
            (
                "repos/acme/repo/issues/42/comments",
                "POST",
                Some(json!({"body":"text"})),
                json!({"__status":429}),
                false,
            ),
        ]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        assert_eq!(
            classification(&upload_bytes(&transport, &snapshot(bytes), bytes).unwrap_err()),
            "explicitRateLimited"
        );
        assert_eq!(
            classification(&comment(&mut transport, &target, &json!({"body":"text"})).unwrap_err()),
            "explicitRateLimited"
        );
        assert!(fixture.requests.borrow().is_empty());
    }

    #[test]
    fn malformed_gist_id_and_frozen_bytes_never_reach_remote() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let fixture = script(vec![]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &NoWait);
        upload_bytes(&transport, &snapshot(b"expected"), b"changed").unwrap_err();
        for id in ["../other", "", "a".repeat(65).as_str(), "ABC"] {
            verify_artifact_with(
                &mut transport,
                &snapshot(b"expected"),
                &json!({"id":id}),
                |_, _, _| panic!("invalid identifier"),
            )
            .unwrap_err();
        }
        assert!(fixture.requests.borrow().is_empty());
    }
}
