//! Captures Azure DevOps review evidence without changing the pull request.

use std::collections::HashSet;
use std::num::NonZeroU64;

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
    let account_properties =
        properties(field(&connection.payload, "authenticatedUser")?.get("properties"))?;
    if let Some(account) = account_properties
        .get("Account")
        .filter(|value| !value.is_null())
    {
        text(account)?;
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
    for name in [
        "title",
        "description",
        "sourceRefName",
        "targetRefName",
        "mergeStatus",
        "mergeId",
    ] {
        optional_text(&pr, name)?;
    }
    for name in [
        "lastMergeSourceCommit",
        "lastMergeTargetCommit",
        "lastMergeCommit",
    ] {
        if let Some(commit) = optional_object(&pr, name)? {
            optional_text(commit, "commitId")?;
        }
    }
    let iterations = list(transport, &format!("{path}/iterations"))?;
    let reviewers = list(transport, &format!("{path}/reviewers"))?;
    let threads = list(transport, &format!("{path}/threads"))?;

    let mut revision_ids = HashSet::new();
    let mut latest: Option<(NonZeroU64, Value)> = None;
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
        let id = service_id(field(thread, "id")?)?;
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

/// Keeps optional provider prose verbatim while rejecting non-string values.
fn optional_text<'a>(node: &'a Value, name: &str) -> Result<Option<&'a str>, AppError> {
    node.get(name)
        .filter(|value| !value.is_null())
        .map(text)
        .transpose()
}

/// Rejects malformed optional objects before reading their nested evidence.
fn optional_object<'a>(node: &'a Value, name: &str) -> Result<Option<&'a Value>, AppError> {
    match node.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value)),
        _ => Err(fail("service", format!("Invalid object {name}"))),
    }
}

/// Iteration ordinals are ordered numerically even if represented as strings.
fn iteration_id(value: &Value) -> Result<NonZeroU64, AppError> {
    NonZeroU64::new(ordinal(value)?)
        .ok_or_else(|| fail("service", "Invalid Azure DevOps iteration ID"))
}

/// Thread comparisons may refer to the common-commit baseline before iteration one.
fn ordinal(value: &Value) -> Result<u64, AppError> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .ok_or_else(|| fail("service", "Invalid Azure DevOps iteration ordinal"))
}

/// Detects numeric identity aliases without changing the provider's displayed ID.
fn service_id(value: &Value) -> Result<String, AppError> {
    let id = identifier(value)?;
    if id.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = iteration_id(value)?;
        return Ok(number.to_string());
    }
    Ok(id)
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
    for name in ["sourceRefCommit", "targetRefCommit", "commonRefCommit"] {
        if let Some(commit) = optional_object(node, name)? {
            optional_text(commit, "commitId")?;
        }
    }
    optional_text(node, "reason")?;
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
    for name in [
        "rightFileStart",
        "rightFileEnd",
        "leftFileStart",
        "leftFileEnd",
    ] {
        if let Some(position) = optional_object(node, name)? {
            if let Some(line) = position.get("line").filter(|line| !line.is_null()) {
                ordinal(line)?;
            }
            if let Some(offset) = position.get("offset").filter(|offset| !offset.is_null()) {
                ordinal(offset)?;
            }
        }
    }
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
fn opening_index<'a>(
    comments: impl ExactSizeIterator<Item = &'a Value>,
) -> Result<Option<usize>, AppError> {
    if comments.len() == 0 {
        return Ok(None);
    }
    let count = comments.len();
    let mut lowest: Option<(u64, usize)> = None;
    let mut ids = HashSet::new();
    let mut ordinals = HashSet::new();
    for (index, comment) in comments.enumerate() {
        let id = service_id(field(comment, "id")?)?;
        if !ids.insert(id.clone()) {
            return Err(fail("service", "Duplicate thread comment ID"));
        }
        let number = id.parse::<u64>().ok().filter(|number| *number > 0);
        if number.is_none() && count > 1 {
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
    latest: NonZeroU64,
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
    let thread_context = optional_object(node, "pullRequestThreadContext")?;
    let iteration_context = thread_context
        .map(|context| optional_object(context, "iterationContext"))
        .transpose()?
        .flatten();
    let anchor_revision = iteration_context
        .and_then(|context| context.get("secondComparingIteration"))
        .filter(|value| !value.is_null())
        .map(ordinal)
        .transpose()?;
    if let Some(context) = thread_context
        && let Some(value) = context
            .get("changeTrackingId")
            .filter(|value| !value.is_null())
    {
        ordinal(value)?;
    }
    let nodes = array(field(node, "comments")?)?;
    let opener = opening_index(nodes.iter())?;
    let mut comments = Vec::new();
    let mut surviving = Vec::new();
    for (index, comment) in nodes.iter().enumerate() {
        if optional_bool(comment, "isDeleted")? {
            continue;
        }
        surviving.push(comment);
        comments.push(convert_comment(
            comment,
            &url,
            (opener == Some(index)).then_some(properties),
        )?);
    }
    let opening_id = opening_index(surviving.iter().copied())?
        .and_then(|index| surviving.get(index))
        .and_then(|v| v.get("id"));
    let published = canonical_time(node.get("publishedDate"))?;
    Ok(json!({
        "id": id, "url": url, "publishedUtc": published,
        "lastUpdatedUtc": canonical_time(node.get("lastUpdatedDate"))?,
        "eventUtc": published, "rawStatus": node.get("status"), "resolution": resolution,
        "kind": kind, "isOutdated": anchor_revision.is_some_and(|id| id < latest.get()),
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
        .filter(|v| {
            v.as_u64() != Some(0) && v.as_str().and_then(|id| id.parse().ok()) != Some(0_u64)
        });
    if let Some(parent) = parent {
        service_id(parent)?;
    }
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
    use std::cell::{RefCell, RefMut};
    use std::collections::{BTreeMap, VecDeque};
    use std::io::Result as StdIoResult;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, from_str, json, to_vec};
    use ss_ai_runtime::Failure;
    use url::Url;

    use super::{collect, list as collect_pages};
    use crate::clock::timestamp;
    use crate::target::{Platform, Target};
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, classification};

    /// Scripts independent endpoints while preserving their page order.
    struct Recording {
        endpoints: BTreeMap<String, &'static str>,
        replies: RefCell<BTreeMap<&'static str, VecDeque<Reply>>>,
        requests: RefCell<Vec<String>>,
    }

    /// Associates an expected continuation with its response and next token.
    type Reply = (Option<&'static str>, Value, Option<&'static str>);

    impl Recording {
        fn new(target: &Target, replies: BTreeMap<&'static str, VecDeque<Reply>>) -> Self {
            let collection = target
                .collection_url
                .as_deref()
                .unwrap()
                .trim_end_matches('/');
            let pr = format!("{collection}/{}", target.pr_api());
            Self {
                endpoints: BTreeMap::from([
                    (format!("{collection}/_apis/connectionData"), "connection"),
                    (pr.clone(), "pr"),
                    (format!("{pr}/iterations"), "iterations"),
                    (format!("{pr}/reviewers"), "reviewers"),
                    (format!("{pr}/threads"), "threads"),
                ]),
                replies: RefCell::new(replies),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn page(&self, endpoint: &str, index: usize) -> RefMut<'_, Reply> {
            RefMut::map(self.replies.borrow_mut(), |replies| {
                &mut replies.get_mut(endpoint).unwrap()[index]
            })
        }

        fn assert_complete(&self) {
            assert!(
                self.replies.borrow().values().all(VecDeque::is_empty),
                "unconsumed endpoint responses"
            );
        }
    }

    impl Process for Recording {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> StdIoResult<ProcessOutput> {
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
            let mut parsed = Url::parse(url).unwrap();
            let tokens: Vec<_> = parsed
                .query_pairs()
                .filter(|(key, _)| key == "continuationToken")
                .map(|(_, value)| value.into_owned())
                .collect();
            parsed.set_query(None);
            let endpoint = self
                .endpoints
                .get(parsed.as_str())
                .expect("unexpected endpoint");
            let (expected, payload, continuation) = self
                .replies
                .borrow_mut()
                .get_mut(endpoint)
                .expect("unscripted endpoint")
                .pop_front()
                .expect("unexpected extra request");
            assert_eq!(
                tokens.as_slice(),
                expected.as_slice(),
                "unexpected continuation for {url}"
            );
            self.requests.borrow_mut().push(url.to_owned());
            let mut headers = BTreeMap::new();
            if let Some(continuation) = continuation {
                headers.insert("x-ms-continuationtoken".to_owned(), continuation.to_owned());
            }
            Ok(HttpResponse::new(200, headers, to_vec(&payload).unwrap()))
        }
    }

    /// Simulates a refused later page without involving a live service.
    struct RefusedPage<'a>(&'a Recording);

    impl Http for RefusedPage<'_> {
        fn send(
            &self,
            method: &str,
            url: &str,
            token: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            assert_eq!(method, "GET");
            assert!(body.is_none());
            if url.contains("continuationToken=next%2Btoken") {
                let mut parsed = Url::parse(url).unwrap();
                let tokens: Vec<_> = parsed
                    .query_pairs()
                    .filter(|(key, _)| key == "continuationToken")
                    .map(|(_, value)| value.into_owned())
                    .collect();
                parsed.set_query(None);
                assert_eq!(self.0.endpoints.get(parsed.as_str()), Some(&"iterations"));
                assert_eq!(tokens, ["next+token"]);
                assert_eq!(
                    self.0.replies.borrow()["iterations"][0].0,
                    Some("next+token")
                );
                return Ok(HttpResponse::new(
                    403,
                    BTreeMap::new(),
                    br#"{"message":"page access denied"}"#.to_vec(),
                ));
            }
            self.0.send(method, url, token, body)
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
        Recording::new(
            &target(),
            BTreeMap::from([
                (
                    "connection",
                    VecDeque::from([(
                        None,
                        json!({"authenticatedUser": {"id": "account", "subjectDescriptor": "aad.account"}}),
                        None,
                    )]),
                ),
                (
                    "pr",
                    VecDeque::from([(
                        None,
                        json!({"pullRequestId": 7, "createdBy": {"id": "author"}, "title": "Unicode λ", "description": null, "sourceRefName": "refs/heads/topic", "targetRefName": "refs/heads/main"}),
                        None,
                    )]),
                ),
                (
                    "iterations",
                    VecDeque::from([
                        (None, list(json!([iteration(2)])), Some("next+token")),
                        (Some("next+token"), list(json!([iteration(1)])), None),
                    ]),
                ),
                (
                    "reviewers",
                    VecDeque::from([(
                        None,
                        list(json!([{"id":"reviewer", "isRequired":true}])),
                        None,
                    )]),
                ),
                (
                    "threads",
                    VecDeque::from([(
                        None,
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
                    )]),
                ),
            ]),
        )
    }

    /// Successful capture must consume every scripted evidence page.
    fn collect_complete(
        transport: &mut Transport<'_>,
        target: &Target,
        recording: &Recording,
    ) -> Value {
        let result = collect(transport, target).unwrap();
        recording.assert_complete();
        result
    }

    #[test]
    fn independent_endpoints_allow_either_order_with_ordered_pages() {
        let target = target();
        for endpoints in [["reviewers", "threads"], ["threads", "reviewers"]] {
            let recording = Recording::new(
                &target,
                BTreeMap::from([
                    (
                        "reviewers",
                        VecDeque::from([
                            (None, list(json!(["reviewer-1"])), Some("reviewers-next")),
                            (Some("reviewers-next"), list(json!(["reviewer-2"])), None),
                        ]),
                    ),
                    (
                        "threads",
                        VecDeque::from([
                            (None, list(json!(["thread-1"])), Some("threads-next")),
                            (Some("threads-next"), list(json!(["thread-2"])), None),
                        ]),
                    ),
                ]),
            );
            let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
            for endpoint in endpoints {
                let values =
                    collect_pages(&mut transport, &format!("{}/{endpoint}", target.pr_api()))
                        .unwrap();
                let expected = if endpoint == "reviewers" {
                    json!(["reviewer-1", "reviewer-2"])
                } else {
                    json!(["thread-1", "thread-2"])
                };
                assert_eq!(json!(values), expected);
            }
            recording.assert_complete();
        }
    }

    #[test]
    #[should_panic(expected = "unexpected continuation")]
    fn rejects_out_of_order_endpoint_page() {
        let recording = recording();
        let endpoint = format!(
            "{}/{}/iterations?continuationToken=next%2Btoken",
            target().collection_url.unwrap(),
            target().pr_api()
        );
        recording
            .send("GET", &endpoint, "fixture-token", None)
            .unwrap();
    }

    #[test]
    fn captures_paginated_latest_iteration_and_actionable_discussion() {
        let target = target();
        let recording = recording();
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
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
        recording.page("iterations", 1).2 = Some("next+token");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.page("iterations", 0).1["value"][0]["commonRefCommit"] = Value::Null;
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn deleted_opener_does_not_transfer_property_evidence() {
        let target = target();
        let recording = recording();
        recording.page("threads", 0).1["value"][1]["comments"][1]["isDeleted"] = json!(true);
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        assert_eq!(result["threads"][0]["openingCommentId"], 2);
        assert_eq!(
            result["threads"][0]["comments"][0]["authorship"],
            "indeterminate"
        );
    }

    #[test]
    fn selects_surviving_openers_without_changing_comment_order() {
        let target = target();
        let recording = recording();
        recording.page("threads", 0).1["value"][1]["comments"] = json!([
            {"id": 10, "author": {"id": "a"}, "content": "ten"},
            {"id": 1, "isDeleted": true},
            {"id": "02", "author": {"id": "a"}, "content": "two"},
            {"id": 5, "author": {"id": "a"}, "content": "five"}
        ]);
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        let thread = &result["threads"][0];
        assert_eq!(thread["openingCommentId"], "02");
        assert_eq!(thread["comments"][0]["id"], 10);
        assert_eq!(thread["comments"][1]["id"], "02");
        assert_eq!(thread["comments"][2]["id"], 5);
        assert_eq!(thread["comments"][1]["authorship"], "indeterminate");
    }

    #[test]
    fn handles_lone_opaque_and_empty_surviving_comments() {
        let target = target();
        for deleted in [false, true] {
            let recording = recording();
            recording.page("threads", 0).1["value"][1]["comments"] = json!([
                {"id": "opaque/id", "isDeleted": deleted, "author": {"id": "a"}, "content": "text"}
            ]);
            let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
            let result = collect_complete(&mut transport, &target, &recording);
            let thread = &result["threads"][0];
            if deleted {
                assert_eq!(thread["openingCommentId"], Value::Null);
                assert_eq!(thread["comments"], json!([]));
            } else {
                assert_eq!(thread["openingCommentId"], "opaque/id");
                assert_eq!(thread["comments"][0]["authorship"], "agent");
            }
        }
    }

    #[test]
    fn validates_deleted_comment_ids_before_omitting_them() {
        let target = target();
        for deleted_id in [json!("01"), json!("opaque"), json!(0), Value::Null] {
            let recording = recording();
            recording.page("threads", 0).1["value"][1]["comments"] = json!([
                {"id": 1, "author": {"id": "a"}},
                {"id": deleted_id, "isDeleted": true}
            ]);
            let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
            collect(&mut transport, &target).unwrap_err();
        }
    }

    #[test]
    fn preserves_iteration_id_representations_and_baseline_comparisons() {
        let target = target();
        let recording = recording();
        {
            recording.page("iterations", 0).1["value"][0]["id"] = json!("002");
            recording.page("threads", 0).1["value"][1]["pullRequestThreadContext"]["iterationContext"]
                ["secondComparingIteration"] = json!("0");
        }
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        assert_eq!(result["revisions"][0]["id"], "002");
        assert_eq!(result["pullRequest"]["reviewedSourceCommit"], "source-2");
        assert_eq!(result["threads"][0]["isOutdated"], true);

        for invalid_id in [json!(0), json!("0")] {
            let recording = self::recording();
            recording.page("iterations", 0).1["value"][0]["id"] = invalid_id;
            let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
            collect(&mut transport, &target).unwrap_err();
        }
    }

    #[test]
    fn preserves_opaque_thread_ids_and_baseline_anchors() {
        let target = target();
        let recording = recording();
        {
            let mut page = recording.page("threads", 0);
            let thread = &mut page.1["value"][1];
            thread["id"] = json!("opaque/id");
            thread["pullRequestThreadContext"]["iterationContext"]["secondComparingIteration"] =
                json!(0);
            thread["publishedDate"] = json!("2026-01-01T00:00:00");
        }
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
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
    fn retains_left_and_right_positions_and_change_tracking_identity() {
        let target = target();
        let recording = recording();
        {
            let mut page = recording.page("threads", 0);
            let thread = &mut page.1["value"][1];
            thread["threadContext"] = json!({
                "filePath": "/renamed.rs",
                "leftFileStart": {"line": 7, "offset": 1},
                "leftFileEnd": {"line": 9, "offset": 2}
            });
            thread["pullRequestThreadContext"]["iterationContext"]["secondComparingIteration"] =
                json!(2);
            thread["comments"][0]["parentCommentId"] = json!("00");
        }
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        let thread = &result["threads"][0];
        assert_eq!(thread["context"]["filePath"], "renamed.rs");
        assert_eq!(thread["context"]["side"], "left");
        assert_eq!(thread["context"]["originalStartLine"], 7);
        assert_eq!(thread["context"]["originalEndLine"], 9);
        assert_eq!(thread["context"]["startLine"], Value::Null);
        assert_eq!(thread["anchorKey"], 42);
        assert_eq!(thread["isOutdated"], false);
        assert_eq!(thread["comments"][0]["parentId"], Value::Null);
    }

    #[test]
    fn rejects_missing_or_ambiguous_collections_and_comment_order() {
        let target = target();
        let recording = recording();
        recording.page("reviewers", 0).1 = json!({});
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.page("threads", 0).1["value"][1]["comments"][0]["id"] = json!("opaque");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn follows_reviewer_and_thread_pages_and_preserves_reply_evidence() {
        let target = target();
        let recording = recording();
        {
            let mut replies = recording.replies.borrow_mut();
            let reviewers = replies.get_mut("reviewers").unwrap();
            reviewers[0].2 = Some("reviewers-next");
            reviewers.push_back((
                Some("reviewers-next"),
                list(json!([{"id":"other", "isContainer":true}])),
                None,
            ));
            let threads = replies.get_mut("threads").unwrap();
            threads[0].2 = Some("threads-next");
            threads[0].1["value"][1]["comments"][0]["parentCommentId"] = json!(1);
            threads.push_back((
                Some("threads-next"),
                list(json!([{"id":10, "comments":[
                    {"id":1, "author":{"id":"b"}, "content":" \n  β\r\n\n"}
                ]}])),
                None,
            ));
        }
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        recording.assert_complete();
        assert_eq!(result["reviewers"].as_array().unwrap().len(), 2);
        assert_eq!(result["threads"].as_array().unwrap().len(), 2);
        assert_eq!(result["threads"][0]["comments"][0]["parentId"], 1);
        assert_eq!(
            result["threads"][1]["comments"][0]["content"],
            " \n  β\r\n\n"
        );
    }

    #[test]
    fn refuses_incomplete_later_pages_and_numeric_identity_aliases() {
        let target = target();
        let recording = recording();
        recording.page("iterations", 1).1 = json!({"count":1,"value":[]});
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.page("threads", 0).1["value"][1]["comments"][0]["id"] = json!("01");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();

        let recording = self::recording();
        recording.page("threads", 0).1["value"][1]["id"] = json!("0");
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn propagates_a_later_page_refusal_instead_of_returning_partial_data() {
        let target = target();
        let recording = recording();
        let http = RefusedPage(&recording);
        let mut transport = Transport::new(&target, None, &recording, &http, &TestClock);
        let error = collect(&mut transport, &target).unwrap_err();
        assert_eq!(classification(&error), "permissionDenied");
        let failure = error.find_source::<Failure>().expect("classified refusal");
        let diagnostic = failure.to_string();
        assert!(diagnostic.contains("403"));
        assert!(diagnostic.contains("page access denied"));
        assert_eq!(recording.requests.borrow().len(), 3);
    }

    #[test]
    fn rejects_malformed_commit_anchor_parent_and_prose_evidence() {
        let target = target();
        for (page, pointer, value) in [
            ("pr", "/description", json!({"unexpected":"object"})),
            (
                "iterations",
                "/value/0/targetRefCommit",
                json!("not a commit"),
            ),
            (
                "threads",
                "/value/1/threadContext/leftFileStart",
                json!("not a position"),
            ),
            (
                "threads",
                "/value/1/threadContext/rightFileEnd/line",
                json!([]),
            ),
            (
                "threads",
                "/value/1/pullRequestThreadContext/iterationContext",
                json!("not a comparison"),
            ),
            (
                "threads",
                "/value/1/pullRequestThreadContext/changeTrackingId",
                json!([]),
            ),
            ("threads", "/value/1/comments/0/parentCommentId", json!({})),
        ] {
            let recording = recording();
            *recording.page(page, 0).1.pointer_mut(pointer).unwrap() = value;
            let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
            assert!(collect(&mut transport, &target).is_err(), "{pointer}");
        }
    }

    #[test]
    fn refuses_incomplete_authenticated_account_property() {
        let target = target();
        let recording = recording();
        recording.page("connection", 0).1["authenticatedUser"]["properties"] =
            json!({"Account": {"$type": "String"}});
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        collect(&mut transport, &target).unwrap_err();
    }

    #[test]
    fn shared_provider_fixture_retains_unicode_and_reviewed_commits() {
        let fixture: Value = from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/azure-devops.json"
        ))
        .unwrap();
        let responses = &fixture["responses"];
        let target = Target::pull_request(
            "https://example.visualstudio.com/Example%20Project/_git/example%20repo/pullrequest/4242",
            Some(Platform::AzureDevOps),
        ).unwrap();
        let recording = Recording::new(
            &target,
            BTreeMap::from([
                (
                    "connection",
                    VecDeque::from([(None, responses["connectionData"].clone(), None)]),
                ),
                (
                    "pr",
                    VecDeque::from([(None, responses["pullRequest"].clone(), None)]),
                ),
                (
                    "iterations",
                    VecDeque::from([(None, responses["iterations"].clone(), None)]),
                ),
                (
                    "reviewers",
                    VecDeque::from([(None, responses["reviewers"].clone(), None)]),
                ),
                (
                    "threads",
                    VecDeque::from([(None, responses["threads"].clone(), None)]),
                ),
            ]),
        );
        let mut transport = Transport::new(&target, None, &recording, &recording, &TestClock);
        let result = collect_complete(&mut transport, &target, &recording);
        let expected: Value = from_str(include_str!(
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
