//! Captures Azure DevOps review evidence without changing the pull request.

use std::collections::HashSet;

use ohno::AppError;
use serde_json::{Map, Value, json};

use crate::clock::timestamp;
use crate::error::fail;
use crate::identity::Identity;
use crate::json::{array, field, identifier, text};
use crate::target::{Platform, Target, escape};
use crate::transport::{Transport, ado_items};

/// Reads the provider's complete discussion and iteration evidence.
pub(crate) fn collect(transport: &mut Transport<'_>, target: &Target) -> Result<Value, AppError> {
    if target.platform != Platform::AzureDevOps
        || transport.platform != Platform::AzureDevOps
        || target.pull_request_id == 0
        || target
            .collection_url
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(fail(
            "invalidInput",
            "A resolved Azure DevOps PR is required",
        ));
    }

    let mut target = target.clone();
    if target
        .project
        .as_deref()
        .is_none_or(|s| s.trim().is_empty())
        || target.repository.trim().is_empty()
    {
        let response = transport.request(
            &format!("_apis/git/pullrequests/{}", target.pull_request_id),
            "GET",
            None,
        )?;
        let repository = field(&response, "repository")?;
        let name = required_text(repository, "name")?;
        let project = required_text(field(repository, "project")?, "name")?;
        if !target.repository.trim().is_empty() && !target.repository.eq_ignore_ascii_case(name)
            || target
                .project
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .is_some_and(|existing| !existing.eq_ignore_ascii_case(project))
        {
            return Err(fail(
                "service",
                "Resolved Azure DevOps target contradicts supplied coordinates",
            ));
        }
        name.clone_into(&mut target.repository);
        target.project = Some(project.to_owned());
    }
    target.refresh_url();

    let connection = transport.ado("_apis/connectionData", "GET", None, &[], "7.1-preview")?;
    if connection.continuation.is_some() {
        return Err(fail("service", "Unexpected connection-data continuation"));
    }
    let identity = Identity::from_connection(&connection.payload)?;
    if identity.id.is_none() {
        return Err(fail(
            "authentication",
            "Azure DevOps did not identify the account",
        ));
    }
    identity.validate()?;

    let path = target.pr_api();
    let pr = transport.request(&path, "GET", None)?;
    if identifier(field(&pr, "pullRequestId")?)? != target.pull_request_id.to_string() {
        return Err(fail(
            "service",
            "Azure DevOps returned a different pull request",
        ));
    }
    let iterations = list(transport, &format!("{path}/iterations"))?;
    let reviewers = list(transport, &format!("{path}/reviewers"))?;
    let threads = list(transport, &format!("{path}/threads"))?;

    let mut revision_ids = HashSet::new();
    let mut latest: Option<(u64, Value)> = None;
    let mut revisions = Vec::new();
    for iteration in &iterations {
        let id = iteration_id(field(iteration, "id")?)?;
        if !revision_ids.insert(id) {
            return Err(fail("service", "Duplicate Azure DevOps iteration"));
        }
        let revision = revision(iteration)?;
        if latest.as_ref().is_none_or(|(current, _)| id > *current) {
            latest = Some((id, revision.clone()));
        }
        revisions.push(revision);
    }
    let (latest_id, latest) = latest.ok_or_else(|| fail("service", "No Azure DevOps iteration"))?;
    let source = required_text(&latest, "sourceCommit")?;
    let base = required_text(&latest, "mergeBaseCommit")?;

    let mut converted_reviewers = Vec::new();
    let mut reviewer_ids = HashSet::new();
    for reviewer in &reviewers {
        let reviewer_identity = Identity::from_account(Platform::AzureDevOps, reviewer)?;
        let reviewer_id = reviewer_identity
            .id
            .as_ref()
            .ok_or_else(|| fail("service", "Azure DevOps reviewer lacks an identifier"))?;
        if !reviewer_ids.insert(reviewer_id.to_lowercase()) {
            return Err(fail("service", "Duplicate Azure DevOps reviewer"));
        }
        converted_reviewers.push(json!({
            "identity": reviewer_identity,
            "isRequired": optional_bool(reviewer, "isRequired")?,
            "isTeam": optional_bool(reviewer, "isContainer")?
        }));
    }
    let mut converted_threads = Vec::new();
    let mut thread_ids = HashSet::new();
    for thread in &threads {
        if optional_bool(thread, "isDeleted")? {
            continue;
        }
        let properties = properties(thread.get("properties"))?;
        if properties
            .get("CodeReviewThreadType")
            .filter(|value| !value.is_null())
            .map(text)
            .transpose()?
            .is_some_and(|s| s.eq_ignore_ascii_case("VoteUpdate"))
        {
            continue;
        }
        let id = identifier(field(thread, "id")?)?;
        if !thread_ids.insert(id) {
            return Err(fail("service", "Duplicate Azure DevOps thread"));
        }
        converted_threads.push(convert_thread(thread, &target, &properties, latest_id)?);
    }
    Ok(json!({
        "target": target,
        "authenticatedIdentity": identity,
        "pullRequest": {
            "id": field(&pr, "pullRequestId")?,
            "title": pr.get("title"),
            "description": pr.get("description"),
            "isDraft": optional_bool(&pr, "isDraft")?,
            "author": Identity::from_account(Platform::AzureDevOps, field(&pr, "createdBy")?)?,
            "createdUtc": canonical_time(pr.get("creationDate"))?,
            "sourceRef": pr.get("sourceRefName"),
            "targetRef": pr.get("targetRefName"),
            "reviewedSourceCommit": source,
            "reviewedMergeBaseCommit": base,
            "mergeState": {
                "status": pr.get("mergeStatus"),
                "mergeId": pr.get("mergeId"),
                "lastMergeSourceCommit": pr.pointer("/lastMergeSourceCommit/commitId"),
                "lastMergeTargetCommit": pr.pointer("/lastMergeTargetCommit/commitId"),
                "lastMergeCommit": pr.pointer("/lastMergeCommit/commitId")
            }
        },
        "reviewers": converted_reviewers,
        "revisions": revisions,
        "threads": converted_threads
    }))
}

/// Follows each continuation token and rejects cycles instead of truncating evidence.
fn list(transport: &mut Transport<'_>, path: &str) -> Result<Vec<Value>, AppError> {
    let mut token: Option<String> = None;
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    loop {
        let query = token
            .as_ref()
            .map(|value| vec![("continuationToken", value.clone())])
            .unwrap_or_default();
        let page = transport.ado(path, "GET", None, &query, "7.1")?;
        result.extend(ado_items(&page.payload)?);
        match page.continuation {
            None => return Ok(result),
            Some(next) if seen.insert(next.clone()) => token = Some(next),
            Some(_) => return Err(fail("service", "Azure DevOps continuation token repeated")),
        }
    }
}

/// Distinguishes absent optional flags from malformed service data.
fn optional_bool(node: &Value, name: &str) -> Result<bool, AppError> {
    match node.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(fail("service", format!("Invalid Boolean {name}"))),
    }
}

/// Keeps missing commit and routing evidence from looking like an empty value.
fn required_text<'a>(node: &'a Value, name: &str) -> Result<&'a str, AppError> {
    let value = text(field(node, name)?)?;
    if value.trim().is_empty() {
        return Err(fail("service", format!("Empty {name}")));
    }
    Ok(value)
}

/// Iteration ordinals are ordered numerically even if represented as strings.
fn iteration_id(value: &Value) -> Result<u64, AppError> {
    let id = ordinal(value)?;
    if id == 0 {
        return Err(fail("service", "Invalid Azure DevOps iteration ID"));
    }
    Ok(id)
}

/// Thread comparisons may refer to the common-commit baseline before iteration one.
fn ordinal(value: &Value) -> Result<u64, AppError> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .ok_or_else(|| fail("service", "Invalid Azure DevOps iteration ordinal"))
}

/// Produces the same UTC representation as the canonical PowerShell document.
fn canonical_time(value: Option<&Value>) -> Result<Option<String>, AppError> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = text(value)?;
    if value.trim().is_empty() {
        return Ok(None);
    }
    // Azure's offset-free timestamps represent UTC, not the machine's local zone.
    let value = if value.ends_with(|c: char| c.is_ascii_digit())
        && !value.contains('+')
        && !value.get(10..).is_some_and(|suffix| suffix.contains('-'))
    {
        format!("{value}Z")
    } else {
        value.to_owned()
    };
    let utc = timestamp(&value)?.to_string();
    let core = utc
        .strip_suffix('Z')
        .ok_or_else(|| fail("service", "Timestamp is not UTC"))?;
    let (whole, fraction) = core.split_once('.').unwrap_or((core, ""));
    let mut fraction = fraction.to_owned();
    fraction.truncate(7);
    while fraction.len() < 7 {
        fraction.push('0');
    }
    Ok(Some(format!("{whole}.{fraction}Z")))
}

/// Common ref commit is the iteration's merge base, not the target branch head.
fn revision(node: &Value) -> Result<Value, AppError> {
    Ok(json!({
        "id": field(node, "id")?,
        "createdUtc": canonical_time(node.get("createdDate"))?,
        "reason": node.get("reason"),
        "sourceCommit": node.pointer("/sourceRefCommit/commitId"),
        "targetCommit": node.pointer("/targetRefCommit/commitId"),
        "mergeBaseCommit": node.pointer("/commonRefCommit/commitId")
    }))
}

/// Removes scalar property envelopes while retaining structured values.
fn properties(node: Option<&Value>) -> Result<Map<String, Value>, AppError> {
    match node {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| {
                let value = if value.get("$type").is_some() {
                    value
                        .get("$value")
                        .cloned()
                        .ok_or_else(|| fail("service", "Incomplete property envelope"))?
                } else {
                    value.clone()
                };
                Ok((key.clone(), value))
            })
            .collect(),
        _ => Err(fail("service", "Invalid Azure DevOps property bag")),
    }
}

/// Preserves both sides of the anchor because removed lines exist only on the left.
fn context(node: Option<&Value>) -> Result<Value, AppError> {
    let Some(node) = node.filter(|node| !node.is_null()) else {
        return Ok(Value::Null);
    };
    if !node.is_object() {
        return Err(fail("service", "Invalid thread context"));
    }
    let file = match node.get("filePath") {
        None | Some(Value::Null) => String::new(),
        Some(value) => text(value)?
            .trim()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_owned(),
    };
    let right = node.get("rightFileStart").is_some_and(|v| !v.is_null())
        || node.get("rightFileEnd").is_some_and(|v| !v.is_null());
    let left = node.get("leftFileStart").is_some_and(|v| !v.is_null())
        || node.get("leftFileEnd").is_some_and(|v| !v.is_null());
    if file.is_empty() && !right && !left {
        return Ok(Value::Null);
    }
    Ok(json!({
        "filePath": file,
        "side": if right { Some("right") } else if left { Some("left") } else { None },
        "startLine": node.pointer("/rightFileStart/line"),
        "endLine": node.pointer("/rightFileEnd/line"),
        "originalStartLine": node.pointer("/leftFileStart/line"),
        "originalEndLine": node.pointer("/leftFileEnd/line")
    }))
}

/// The first comment owns thread-level machine-authorship evidence.
fn opening_index(comments: &[Value]) -> Result<Option<usize>, AppError> {
    if comments.is_empty() {
        return Ok(None);
    }
    let mut lowest: Option<(u64, usize)> = None;
    let mut ids = HashSet::new();
    let mut ordinals = HashSet::new();
    for (index, comment) in comments.iter().enumerate() {
        let id = identifier(field(comment, "id")?)?;
        if !ids.insert(id.clone()) {
            return Err(fail("service", "Duplicate thread comment ID"));
        }
        let number = id.parse::<u64>().ok().filter(|number| *number > 0);
        if number.is_none() && comments.len() > 1 {
            return Err(fail("service", "Cannot establish thread opening comment"));
        }
        if let Some(number) = number {
            if !ordinals.insert(number) {
                return Err(fail("service", "Ambiguous thread comment order"));
            }
            if lowest.is_none_or(|(current, _)| number < current) {
                lowest = Some((number, index));
            }
        }
    }
    Ok(Some(lowest.map_or(0, |(_, index)| index)))
}

/// Records actionable discussion, including unknown system activity.
fn convert_thread(
    node: &Value,
    target: &Target,
    properties: &Map<String, Value>,
    latest: u64,
) -> Result<Value, AppError> {
    let id = field(node, "id")?;
    let url = format!(
        "{}?discussionId={}",
        target.web_url,
        escape(&identifier(id)?)
    );
    let status = node
        .get("status")
        .filter(|value| !value.is_null())
        .map(text)
        .transpose()?
        .unwrap_or_default();
    let resolution = if ["fixed", "closed", "byDesign", "wontFix"]
        .iter()
        .any(|s| status.eq_ignore_ascii_case(s))
    {
        "resolved"
    } else if ["active", "pending"]
        .iter()
        .any(|s| status.eq_ignore_ascii_case(s))
    {
        "active"
    } else {
        "indeterminate"
    };
    let thread_type = properties
        .get("CodeReviewThreadType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = if thread_type.is_empty() {
        "text"
    } else if thread_type.eq_ignore_ascii_case("RefUpdate") {
        "refUpdate"
    } else if thread_type.eq_ignore_ascii_case("ReviewersUpdate") {
        "reviewersUpdated"
    } else if thread_type.eq_ignore_ascii_case("PolicyStatusUpdate") {
        "policy"
    } else {
        "system"
    };
    let anchor_revision = node
        .pointer("/pullRequestThreadContext/iterationContext/secondComparingIteration")
        .filter(|value| !value.is_null())
        .map(ordinal)
        .transpose()?;
    let nodes = array(field(node, "comments")?)?;
    let opener = opening_index(nodes)?;
    let mut comments = Vec::new();
    let mut surviving = Vec::new();
    for (index, comment) in nodes.iter().enumerate() {
        if optional_bool(comment, "isDeleted")? {
            continue;
        }
        surviving.push(comment.clone());
        comments.push(convert_comment(
            comment,
            &url,
            (opener == Some(index)).then_some(properties),
        )?);
    }
    let opening_id = opening_index(&surviving)?
        .and_then(|index| surviving.get(index))
        .and_then(|v| v.get("id"));
    let published = canonical_time(node.get("publishedDate"))?;
    Ok(json!({
        "id": id, "url": url, "publishedUtc": published,
        "lastUpdatedUtc": canonical_time(node.get("lastUpdatedDate"))?,
        "eventUtc": published, "rawStatus": node.get("status"), "resolution": resolution,
        "kind": kind, "isOutdated": anchor_revision.is_some_and(|id| id < latest),
        "context": context(node.get("threadContext"))?,
        "anchorKey": node.pointer("/pullRequestThreadContext/changeTrackingId"),
        "properties": properties, "openingCommentId": opening_id,
        "decision": null, "comments": comments
    }))
}

/// A person's account does not prove a person composed a particular comment.
fn convert_comment(
    node: &Value,
    url: &str,
    properties: Option<&Map<String, Value>>,
) -> Result<Value, AppError> {
    let author = Identity::from_account(Platform::AzureDevOps, field(node, "author")?)?;
    let content = node
        .get("content")
        .filter(|value| !value.is_null())
        .map(text)
        .transpose()?;
    let mut evidence: Vec<String> = author
        .evidence
        .iter()
        .map(|item| format!("identity:{item}"))
        .collect();
    let mut agent = author.kind == "service";
    if let Some(properties) = properties {
        for key in properties.keys() {
            let leaf = key.rsplit('.').next().unwrap_or(key);
            if [
                "LLMModel",
                "Model",
                "Focus_Area",
                "Intent",
                "PromptVersionQualifier",
                "SeverityThreshold",
                "GitOpsUserAgent",
                "ExtensionName",
            ]
            .iter()
            .any(|marker| leaf.eq_ignore_ascii_case(marker))
            {
                agent = true;
                evidence.push(format!("property:{leaf}"));
            }
        }
    }
    if content.is_some_and(|s| s.trim_start().starts_with("[Copilot speaking]")) {
        agent = true;
        evidence.push("content:agentMarker".to_owned());
    }
    let published = canonical_time(node.get("publishedDate"))?;
    let edited = canonical_time(node.get("lastContentUpdatedDate"))?;
    let parent = node
        .get("parentCommentId")
        .filter(|v| !v.is_null())
        .filter(|v| v.as_u64() != Some(0) && v.as_str() != Some("0"));
    Ok(json!({
        "id": field(node, "id")?, "url": url, "parentId": parent,
        "author": author, "content": content, "publishedUtc": published,
        "lastEditedUtc": edited, "isEdited": edited.is_some() && edited != published,
        "authorship": if agent { "agent" } else { "indeterminate" }, "authorshipEvidence": evidence
    }))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::needless_pass_by_value,
        reason = "Test fixtures use bounded JSON values and owned construction"
    )]
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};

    use super::collect;
    use crate::clock::timestamp;
    use crate::target::Target;
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput};

    /// Ordered responses verify that every provider request remains a read.
    struct Recording {
        replies: RefCell<VecDeque<(&'static str, Value, Option<&'static str>)>>,
        requests: RefCell<Vec<String>>,
    }

    impl Process for Recording {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "az");
            Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
        }
    }

    impl Http for Recording {
        fn send(
            &self,
            method: &str,
            url: &str,
            _: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            assert_eq!(method, "GET");
            assert!(body.is_none());
            let (expected, payload, continuation) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert!(url.contains(expected), "{url} does not contain {expected}");
            self.requests.borrow_mut().push(url.to_owned());
            let mut headers = BTreeMap::new();
            if let Some(continuation) = continuation {
                headers.insert("x-ms-continuationtoken".to_owned(), continuation.to_owned());
            }
            Ok(HttpResponse::new(
                200,
                headers,
                serde_json::to_vec(&payload).unwrap(),
            ))
        }
    }

    struct TestClock;
    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            timestamp("2026-01-01T00:00:00Z").unwrap()
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("unexpected retry")
        }
    }

    fn target() -> Target {
        Target::pull_request(
            "https://dev.azure.com/org/proj/_git/repo/pullrequest/7",
            None,
        )
        .unwrap()
    }

    fn iteration(id: u32) -> Value {
        json!({"id": id, "sourceRefCommit": {"commitId": format!("source-{id}")},
            "targetRefCommit": {"commitId": "target"}, "commonRefCommit": {"commitId": format!("base-{id}")}})
    }

    fn list(values: Value) -> Value {
        json!({"count": values.as_array().unwrap().len(), "value": values})
    }

    fn recording() -> Recording {
        Recording {
            replies: RefCell::new(VecDeque::from([
                (
                    "_apis/connectionData",
                    json!({"authenticatedUser": {"id": "account", "subjectDescriptor": "aad.account"}}),
                    None,
                ),
                (
                    "pullRequests/7?",
                    json!({"pullRequestId": 7, "createdBy": {"id": "author"}, "title": "Unicode λ", "sourceRefName": "refs/heads/topic", "targetRefName": "refs/heads/main"}),
                    None,
                ),
                (
                    "/iterations?",
                    list(json!([iteration(2)])),
                    Some("next+token"),
                ),
                (
                    "continuationToken=next%2Btoken",
                    list(json!([iteration(1)])),
                    None,
                ),
                (
                    "/reviewers?",
                    list(json!([{"id":"reviewer", "isRequired":true}])),
                    None,
                ),
                (
                    "/threads?",
                    list(json!([
                        {"id": 8, "properties":{"CodeReviewThreadType":{"$type":"String","$value":"VoteUpdate"}}, "comments":[]},
                        {"id": 9, "status":"active", "publishedDate":"2026-01-01T01:00:00+01:00",
                         "properties":{"review.LLMModel":{"$type":"String","$value":"model"}},
                         "threadContext":{"filePath":"/src/a.rs","leftFileStart":{"line":2},"rightFileEnd":{"line":5}},
                         "pullRequestThreadContext":{"changeTrackingId":42,"iterationContext":{"secondComparingIteration":1}},
                         "comments":[
                            {"id":2,"parentCommentId":0,"author":{"id":"a","descriptor":"aad.person"},"content":"Reply"},
                            {"id":1,"parentCommentId":0,"author":{"id":"a","descriptor":"aad.person"},"content":"Opening\nλ"},
                            {"id":3,"isDeleted":true,"author":{"id":"a"},"content":"removed"}
                         ]}
                    ])),
                    None,
                ),
            ])),
            requests: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn captures_paginated_latest_iteration_and_actionable_discussion() {
        let target = target();
        let recording = recording();
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect(&mut transport, &target).unwrap();
        assert!(recording.replies.borrow().is_empty());
        assert_eq!(result["pullRequest"]["reviewedSourceCommit"], "source-2");
        assert_eq!(result["pullRequest"]["reviewedMergeBaseCommit"], "base-2");
        assert_eq!(result["revisions"].as_array().unwrap().len(), 2);
        assert_eq!(result["threads"].as_array().unwrap().len(), 1);
        let thread = &result["threads"][0];
        assert_eq!(thread["publishedUtc"], "2026-01-01T00:00:00.0000000Z");
        assert_eq!(thread["anchorKey"], 42);
        assert_eq!(thread["context"]["originalStartLine"], 2);
        assert_eq!(thread["context"]["endLine"], 5);
        assert_eq!(thread["openingCommentId"], 1);
        assert_eq!(thread["isOutdated"], true);
        assert_eq!(thread["comments"][0]["authorship"], "indeterminate");
        assert_eq!(thread["comments"][1]["authorship"], "agent");
        assert_eq!(thread["comments"][1]["content"], "Opening\nλ");
    }

    #[test]
    fn rejects_nonadvancing_pagination_and_missing_merge_base() {
        let target = target();
        let recording = recording();
        recording.replies.borrow_mut()[3].2 = Some("next+token");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.replies.borrow_mut()[2].1["value"][0]["commonRefCommit"] = Value::Null;
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn deleted_opener_does_not_transfer_property_evidence() {
        let target = target();
        let recording = recording();
        recording.replies.borrow_mut()[5].1["value"][1]["comments"][1]["isDeleted"] = json!(true);
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect(&mut transport, &target).unwrap();
        assert_eq!(result["threads"][0]["openingCommentId"], 2);
        assert_eq!(
            result["threads"][0]["comments"][0]["authorship"],
            "indeterminate"
        );
    }

    #[test]
    fn preserves_opaque_thread_ids_and_baseline_anchors() {
        let target = target();
        let recording = recording();
        {
            let mut replies = recording.replies.borrow_mut();
            let thread = &mut replies[5].1["value"][1];
            thread["id"] = json!("opaque/id");
            thread["pullRequestThreadContext"]["iterationContext"]["secondComparingIteration"] =
                json!(0);
            thread["publishedDate"] = json!("2026-01-01T00:00:00");
        }
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect(&mut transport, &target).unwrap();
        assert_eq!(result["threads"][0]["id"], "opaque/id");
        assert!(
            result["threads"][0]["url"]
                .as_str()
                .unwrap()
                .ends_with("?discussionId=opaque%2Fid")
        );
        assert_eq!(result["threads"][0]["isOutdated"], true);
        assert_eq!(
            result["threads"][0]["publishedUtc"],
            "2026-01-01T00:00:00.0000000Z"
        );
    }

    #[test]
    fn rejects_missing_or_ambiguous_collections_and_comment_order() {
        let target = target();
        let recording = recording();
        recording.replies.borrow_mut()[4].1 = json!({});
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.replies.borrow_mut()[5].1["value"][1]["comments"][0]["id"] = json!("opaque");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn shared_provider_fixture_retains_unicode_and_reviewed_commits() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
        ))
        .unwrap();
        let responses = &fixture["responses"];
        let recording = Recording {
            replies: RefCell::new(VecDeque::from([
                (
                    "_apis/connectionData",
                    responses["connectionData"].clone(),
                    None,
                ),
                ("pullRequests/4242?", responses["pullRequest"].clone(), None),
                ("/iterations?", responses["iterations"].clone(), None),
                ("/reviewers?", responses["reviewers"].clone(), None),
                ("/threads?", responses["threads"].clone(), None),
            ])),
            requests: RefCell::new(Vec::new()),
        };
        let target = Target::pull_request(
            "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242",
            Some(crate::target::Platform::AzureDevOps),
        ).unwrap();
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect(&mut transport, &target).unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/azure-devops-context.json"
        ))
        .unwrap();
        assert_eq!(result, expected);
        assert_eq!(
            result["pullRequest"]["description"],
            "Review café — “design” 🤖"
        );
        assert_eq!(
            result["pullRequest"]["reviewedSourceCommit"],
            responses["iterations"]["value"][0]["sourceRefCommit"]["commitId"]
        );
        assert_eq!(
            result["pullRequest"]["reviewedMergeBaseCommit"],
            responses["iterations"]["value"][0]["commonRefCommit"]["commitId"]
        );
        assert_eq!(
            result["threads"][0]["comments"][0]["content"],
            "Alpha — “gamma” 🤖 café"
        );
        assert_eq!(result["threads"][0]["context"]["filePath"], "src/café.rs");
    }
}
