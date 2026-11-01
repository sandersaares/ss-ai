//! Files tracker issues independently of the reviewed pull request.

use ohno::AppError;
use serde_json::{Value, json};
use url::Url;

use crate::error::fail;
use crate::target::Platform;
use crate::tracker_target::TrackerTarget;
use crate::transport::Transport;

const REPOSITORY: &str = "query($owner:String!,$repo:String!){
 repository(owner:$owner,name:$repo){id nameWithOwner hasIssuesEnabled isArchived viewerPermission}}";
const CREATE: &str = "mutation($repositoryId:ID!,$title:String!,$body:String!){
 createIssue(input:{repositoryId:$repositoryId,title:$title,body:$body}){
 issue{id number url title body}}}";
const READ: &str = "query($issueId:ID!){
 node(id:$issueId){... on Issue{id number url title body}}}";

/// Validates the independently selected destination before any issue is created.
pub(crate) fn state(transport: &Transport<'_>, target: &TrackerTarget) -> Result<Value, AppError> {
    check_transport(transport)?;
    target.validate()?;
    if target.platform != Platform::GitHub {
        return Err(fail(
            "invalidInput",
            "A GitHub tracker repository is required",
        ));
    }
    let repository_name = target
        .repository
        .as_deref()
        .ok_or_else(|| fail("invalidInput", "A GitHub tracker repository is required"))?;

    let response = transport.graphql(
        REPOSITORY,
        &json!({"owner":target.organization,"repo":repository_name}),
    )?;
    let repository = node(&response, "/data/repository", "service")?;
    let id = required(repository, "id", "service")?;
    let name = required(repository, "nameWithOwner", "service")?;
    let expected = format!("{}/{}", target.organization, repository_name);
    if !name.eq_ignore_ascii_case(&expected) {
        return Err(fail(
            "service",
            "GitHub returned a different tracker repository",
        ));
    }
    let archived = boolean(repository, "isArchived")?;
    let enabled = boolean(repository, "hasIssuesEnabled")?;
    let permission = required(repository, "viewerPermission", "service")?;
    if !matches!(
        permission,
        "READ" | "TRIAGE" | "WRITE" | "MAINTAIN" | "ADMIN" | "NONE"
    ) {
        return Err(fail(
            "service",
            "GitHub returned an unknown repository permission",
        ));
    }
    if archived || !enabled || permission == "NONE" {
        return Err(fail(
            "permissionDenied",
            "The GitHub repository does not permit issue creation",
        ));
    }

    Ok(json!({"platform":"gitHub","repositoryId":id,"trackerName":name}))
}

/// Creates one issue without replaying an uncertain mutation.
pub(crate) fn create(
    transport: &Transport<'_>,
    state: &Value,
    item: &Value,
) -> Result<Value, AppError> {
    check_transport(transport)?;
    if required(state, "platform", "invalidInput")? != "gitHub" {
        return Err(fail("invalidInput", "Expected GitHub tracker state"));
    }
    let repository_id = required(state, "repositoryId", "invalidInput")?;
    required(state, "trackerName", "invalidInput")?;
    let title = string(item, "title", "invalidInput")?;
    if title.trim().is_empty() {
        return Err(fail("invalidInput", "Issue title must not be empty"));
    }
    let body = string(item, "body", "invalidInput")?;
    let response = transport.graphql_mutation(
        CREATE,
        &json!({"repositoryId":repository_id,"title":title,"body":body}),
    )?;
    // Missing or malformed receipt fields cannot establish that no write occurred.
    record(
        node(&response, "/data/createIssue/issue", "ambiguousWrite")?,
        "ambiguousWrite",
    )
}

/// Reads the created issue by its opaque identifier for exact body verification.
pub(crate) fn read(transport: &Transport<'_>, receipt: &Value) -> Result<Value, AppError> {
    check_transport(transport)?;
    let id = required(receipt, "itemNodeId", "invalidInput")?;
    let response = transport.graphql(READ, &json!({"issueId":id}))?;
    let result = record(node(&response, "/data/node", "service")?, "service")?;
    if required(&result, "itemNodeId", "service")? != id {
        return Err(fail("service", "GitHub returned a different issue"));
    }
    Ok(result)
}

/// Prevents a caller from accidentally routing GitHub queries through ADO.
fn check_transport(transport: &Transport<'_>) -> Result<(), AppError> {
    if transport.platform != Platform::GitHub {
        return Err(fail("invalidInput", "A GitHub transport is required"));
    }
    Ok(())
}

/// Requires the requested GraphQL object instead of accepting null or partial data.
fn node<'a>(value: &'a Value, path: &str, kind: &'static str) -> Result<&'a Value, AppError> {
    value
        .pointer(path)
        .filter(|value| value.is_object())
        .ok_or_else(|| fail(kind, format!("Missing GitHub object {path}")))
}

/// Keeps empty content distinct from a missing or incorrectly typed field.
fn string<'a>(value: &'a Value, name: &str, kind: &'static str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| fail(kind, format!("Missing or invalid {name}")))
}

/// Requires a nonempty identity or metadata string.
fn required<'a>(value: &'a Value, name: &str, kind: &'static str) -> Result<&'a str, AppError> {
    let value = string(value, name, kind)?;
    if value.trim().is_empty() {
        return Err(fail(kind, format!("Missing or invalid {name}")));
    }
    Ok(value)
}

/// Refuses missing capability evidence instead of assuming it is enabled.
fn boolean(value: &Value, name: &str) -> Result<bool, AppError> {
    value
        .get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| fail("service", format!("Missing or invalid {name}")))
}

/// Normalizes complete issue evidence without modifying the stored prose.
fn record(issue: &Value, kind: &'static str) -> Result<Value, AppError> {
    let id = required(issue, "id", kind)?;
    let number = issue
        .get("number")
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| fail(kind, "Missing or invalid issue number"))?;
    let url = required(issue, "url", kind)?;
    let parsed = Url::parse(url)
        .map_err(|error| crate::error::caused_by(kind, "Invalid issue URL".to_owned(), error))?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(fail(kind, "Invalid issue URL"));
    }
    let title = string(issue, "title", kind)?;
    let body = string(issue, "body", kind)?;
    Ok(
        json!({"itemId":number.to_string(),"itemNodeId":id,"url":url,
        "title":title,"body":body,"descriptionFormat":"Markdown"}),
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    reason = "The tests index bounded fixtures with known fields and lengths"
)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};

    use super::{CREATE, READ, REPOSITORY, create, read, state};
    use crate::error::fail;
    use crate::target::Target;
    use crate::tracker_target::{TrackerTarget, TrackerTargetInput};
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, classification};

    /// Captures every request and returns only the scripted provider evidence.
    struct Script {
        replies: RefCell<VecDeque<ProcessOutput>>,
        requests: RefCell<Vec<Value>>,
    }

    impl Script {
        fn new(replies: Vec<ProcessOutput>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl Process for Script {
        fn execute(
            &self,
            program: &str,
            arguments: &[String],
            input: &[u8],
        ) -> io::Result<ProcessOutput> {
            assert_eq!(program, "gh");
            assert_eq!(arguments.get(1).map(String::as_str), Some("graphql"));
            self.requests
                .borrow_mut()
                .push(serde_json::from_slice(input).unwrap());
            Ok(self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request"))
        }
    }

    /// The GitHub adapter has no Azure HTTP effects or wall-clock waits.
    struct NoEffects;

    impl Http for NoEffects {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Err(fail("service", "Unexpected HTTP request"))
        }
    }

    impl Clock for NoEffects {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            Err(fail("service", "Unexpected wait"))
        }
    }

    fn response(value: &Value) -> ProcessOutput {
        ProcessOutput::new(true, format!("HTTP/2 200\n\n{value}").into_bytes(), vec![])
    }

    fn target() -> TrackerTarget {
        TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://github.com/acme/tracker"),
            ..Default::default()
        })
        .unwrap()
    }

    fn transport_target() -> Target {
        Target::repository("https://github.com/acme/tracker").unwrap()
    }

    fn repository() -> Value {
        json!({"id":"repo-id","nameWithOwner":"Acme/Tracker",
            "hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"READ"})
    }

    fn issue() -> Value {
        json!({"id":"issue-id","number":42,"url":"https://github.com/acme/tracker/issues/42",
            "title":"é 🌍","body":"first\r\n第二\0end\n"})
    }

    #[test]
    fn creates_and_reads_exact_utf8_against_the_tracker_repository() {
        let script = Script::new(vec![
            response(&json!({"data":{"repository":repository()}})),
            response(&json!({"data":{"createIssue":{"issue":issue()}}})),
            response(&json!({"data":{"node":issue()}})),
        ]);
        let target = target();
        let transport = Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
        let state = state(&transport, &target).unwrap();
        assert_eq!(
            state,
            json!({"platform":"gitHub","repositoryId":"repo-id","trackerName":"Acme/Tracker"})
        );
        let item = json!({"title":"é 🌍","body":"first\r\n第二\0end\n"});
        let receipt = create(&transport, &state, &item).unwrap();
        assert_eq!(
            receipt,
            json!({"itemId":"42","itemNodeId":"issue-id",
            "url":"https://github.com/acme/tracker/issues/42","title":item["title"],
            "body":item["body"],"descriptionFormat":"Markdown"})
        );
        assert_eq!(read(&transport, &receipt).unwrap(), receipt);
        let requests = script.requests.borrow();
        assert_eq!(
            requests[0],
            json!({"query":REPOSITORY,"variables":{"owner":"acme","repo":"tracker"}})
        );
        assert_eq!(
            requests[1],
            json!({"query":CREATE,"variables":{
            "repositoryId":"repo-id","title":item["title"],"body":item["body"]}})
        );
        assert_eq!(
            requests[2],
            json!({"query":READ,"variables":{"issueId":"issue-id"}})
        );
    }

    #[test]
    fn preflight_rejects_mismatched_or_incomplete_capabilities() {
        for (field, value) in [
            ("nameWithOwner", json!("other/tracker")),
            ("id", json!("")),
            ("isArchived", json!(true)),
            ("hasIssuesEnabled", json!(false)),
            ("hasIssuesEnabled", json!("true")),
            ("viewerPermission", json!("NONE")),
            ("viewerPermission", Value::Null),
            ("viewerPermission", json!("UNKNOWN")),
        ] {
            let mut repository = repository();
            repository[field] = value;
            let script = Script::new(vec![response(&json!({"data":{"repository":repository}}))]);
            let target = target();
            let transport =
                Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
            state(&transport, &target).expect_err(field);
            assert_eq!(script.requests.borrow().len(), 1);
        }
    }

    #[test]
    fn incomplete_creation_receipts_and_lost_replies_are_ambiguous_without_replay() {
        for (field, value) in [
            ("id", Value::Null),
            ("number", json!("42")),
            ("number", json!(0)),
            ("url", json!("not a URL")),
            ("title", Value::Null),
            ("body", json!(123)),
        ] {
            let mut issue = issue();
            issue[field] = value;
            let script = Script::new(vec![response(
                &json!({"data":{"createIssue":{"issue":issue}}}),
            )]);
            let transport =
                Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
            let error = create(
                &transport,
                &json!({"platform":"gitHub","repositoryId":"r","trackerName":"a/b"}),
                &json!({"title":"title","body":"body"}),
            )
            .unwrap_err();
            assert_eq!(classification(&error), "ambiguousWrite", "{field}");
            assert_eq!(script.requests.borrow().len(), 1);
        }
        let script = Script::new(vec![ProcessOutput::new(
            false,
            vec![],
            b"connection lost".to_vec(),
        )]);
        let transport = Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
        let error = create(
            &transport,
            &json!({"platform":"gitHub","repositoryId":"r","trackerName":"a/b"}),
            &json!({"title":"title","body":"body"}),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert_eq!(script.requests.borrow().len(), 1);

        let script = Script::new(vec![response(&json!({
            "data":{"createIssue":{"issue":issue()}},
            "errors":[{"message":"partial response"}]
        }))]);
        let transport = Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
        let error = create(
            &transport,
            &json!({"platform":"gitHub","repositoryId":"r","trackerName":"a/b"}),
            &json!({"title":"title","body":"body"}),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert_eq!(script.requests.borrow().len(), 1);
    }

    #[test]
    fn read_rejects_wrong_issue_and_malformed_evidence() {
        let mut malformed = issue();
        malformed["body"] = Value::Null;
        for (requested, returned) in [
            ("different", json!({"data":{"node":issue()}})),
            ("issue-id", json!({"data":{"node":null}})),
            ("issue-id", json!({"data":{"node":malformed}})),
        ] {
            let script = Script::new(vec![response(&returned)]);
            let transport =
                Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
            let error = read(&transport, &json!({"itemNodeId":requested})).unwrap_err();
            assert_eq!(classification(&error), "service");
        }
    }

    #[test]
    fn read_exposes_stored_changes_for_the_caller_to_verify() {
        let mut stored = issue();
        stored["body"] = json!("changed\n");
        let script = Script::new(vec![response(&json!({"data":{"node":stored}}))]);
        let transport = Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
        let result = read(&transport, &json!({"itemNodeId":"issue-id"})).unwrap();
        assert_eq!(result["body"], "changed\n");
    }

    #[test]
    fn invalid_inputs_never_issue_requests() {
        let script = Script::new(vec![]);
        let target = target();
        let transport = Transport::new(&transport_target(), None, &script, &NoEffects, &NoEffects);
        create(
            &transport,
            &json!({"platform":"gitHub","repositoryId":"r","trackerName":"a/b"}),
            &json!({"title":"title","body":null}),
        )
        .unwrap_err();
        read(&transport, &json!({"itemNodeId":null})).unwrap_err();
        let mut other = target.clone();
        other.organization = "other/org".into();
        state(&transport, &other).unwrap_err();
        other = target;
        other.repository = None;
        state(&transport, &other).unwrap_err();
        assert!(script.requests.borrow().is_empty());
    }
}
