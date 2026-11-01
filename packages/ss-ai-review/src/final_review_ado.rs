//! Azure DevOps effects for final review publication.
//!
//! ADO publishes each comment immediately and has no draft review. The caller
//! must journal intent before each effect and retain the returned identifiers.

use std::collections::{HashMap, HashSet};

use ohno::AppError;
use serde_json::{Map, Value, json};

use crate::error::fail;
use crate::identity::Identity;
use crate::json::{array, field, identifier, text};
use crate::target::{Platform, Target, escape};
use crate::transport::{Transport, ado_items};

/// Resolves incomplete coordinates before constructing repository-scoped routes.
fn resolve(transport: &mut Transport<'_>, target: &Target) -> Result<Target, AppError> {
    if transport.platform != Platform::AzureDevOps
        || target.platform != Platform::AzureDevOps
        || target.pull_request_id == 0
        || target.collection_url.as_deref().is_none_or(str::is_empty)
        || transport.ado_collection() != target.collection_url.as_deref()
    {
        return Err(fail(
            "invalidInput",
            "A matching Azure DevOps target is required",
        ));
    }
    let mut resolved = target.clone();
    if resolved.project.as_deref().is_none_or(str::is_empty) || resolved.repository.is_empty() {
        let response = transport.request(
            &format!("_apis/git/pullrequests/{}", target.pull_request_id),
            "GET",
            None,
        )?;
        let repository = field(&response, "repository")?;
        let name = text(field(repository, "name")?)?;
        let project = text(field(field(repository, "project")?, "name")?)?;
        if name.trim().is_empty()
            || project.trim().is_empty()
            || !resolved.repository.is_empty() && !resolved.repository.eq_ignore_ascii_case(name)
            || resolved
                .project
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_some_and(|s| !s.eq_ignore_ascii_case(project))
        {
            return Err(fail(
                "service",
                "Resolved Azure DevOps coordinates disagree",
            ));
        }
        name.clone_into(&mut resolved.repository);
        resolved.project = Some(project.to_owned());
    }
    resolved.refresh_url();
    Ok(resolved)
}

/// Follows all list pages and rejects continuation cycles.
fn list(transport: &mut Transport<'_>, path: &str) -> Result<Vec<Value>, AppError> {
    let mut result = Vec::new();
    let mut token: Option<String> = None;
    let mut seen = HashSet::new();
    loop {
        let query = token
            .as_ref()
            .map(|s| vec![("continuationToken", s.clone())])
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

/// Obtains the authenticated account from the preview connection endpoint.
fn account(transport: &mut Transport<'_>) -> Result<Identity, AppError> {
    let page = transport.ado("_apis/connectionData", "GET", None, &[], "7.1-preview")?;
    if page.continuation.is_some() {
        return Err(fail("service", "Unexpected connection-data continuation"));
    }
    let identity = Identity::from_connection(&page.payload)?;
    identity.validate()?;
    if identity.id.is_none() {
        return Err(fail(
            "authentication",
            "Azure DevOps did not identify the account",
        ));
    }
    Ok(identity)
}

/// Parses an iteration or service ordinal without accepting zero.
fn positive(value: &Value) -> Result<u64, AppError> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| fail("service", "Invalid Azure DevOps identifier"))
}

/// Captures the live iteration and account before publication.
pub(crate) fn state(transport: &mut Transport<'_>, target: &Target) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let path = target.pr_api();
    let pr = transport.request(&path, "GET", None)?;
    if identifier(field(&pr, "pullRequestId")?)? != target.pull_request_id.to_string() {
        return Err(fail(
            "service",
            "Azure DevOps returned a different pull request",
        ));
    }
    let mut latest: Option<(u64, Value)> = None;
    let mut ids = HashSet::new();
    for iteration in list(transport, &format!("{path}/iterations"))? {
        let id = positive(field(&iteration, "id")?)?;
        if !ids.insert(id) {
            return Err(fail("service", "Duplicate Azure DevOps iteration"));
        }
        if latest.as_ref().is_none_or(|(current, _)| id > *current) {
            latest = Some((id, iteration));
        }
    }
    let (_, latest) = latest.ok_or_else(|| fail("service", "No Azure DevOps iteration"))?;
    let source = text(field(field(&latest, "sourceRefCommit")?, "commitId")?)?;
    let base = text(field(field(&latest, "commonRefCommit")?, "commitId")?)?;
    if source.trim().is_empty() || base.trim().is_empty() {
        return Err(fail("service", "Iteration commit is empty"));
    }
    let status = text(field(&pr, "status")?)?;
    let draft = match pr.get("isDraft") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(fail("service", "Invalid pull request draft status")),
    };
    let identity = account(transport)?;
    Ok(json!({
        "platform": "azureDevOps", "pullRequestNodeId": null,
        "url": target.web_url, "status": status, "isActive": status.eq_ignore_ascii_case("active"),
        "isDraft": draft, "sourceCommit": source, "mergeBaseCommit": base,
        "identity": identity.unique_name.unwrap_or_default(), "pendingReviewId": null,
        "iterationId": field(&latest, "id")?
    }))
}

/// Normalizes ADO paths to repository-relative form.
fn canonical_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim()
        .trim_start_matches('/')
        .to_owned()
}

/// Reads the cumulative changed-file identities for the selected iteration.
pub(crate) fn file_identity(
    transport: &mut Transport<'_>,
    target: &Target,
    state: &Value,
    paths: &[String],
) -> Result<Value, AppError> {
    if paths.is_empty() {
        return Ok(json!({}));
    }
    let target = resolve(transport, target)?;
    let iteration = positive(field(state, "iterationId")?)?;
    let path = format!("{}/iterations/{iteration}/changes", target.pr_api());
    let mut skip = 0_u64;
    let mut found = HashMap::new();
    loop {
        // Match the provider reader's bounded page size and body cursor.
        let mut query = vec![("$top", "1000".to_owned())];
        if skip > 0 {
            query.push(("$skip", skip.to_string()));
        }
        let page = transport.ado(&path, "GET", None, &query, "7.1")?;
        if page.continuation.is_some() {
            return Err(fail("service", "Unexpected change-list continuation"));
        }
        for change in array(field(&page.payload, "changeEntries")?)? {
            let Some(item_path) = change.pointer("/item/path").and_then(Value::as_str) else {
                continue;
            };
            let Some(id) = change.get("changeTrackingId").filter(|v| !v.is_null()) else {
                continue;
            };
            positive(id)?;
            let canonical = canonical_path(item_path);
            if !canonical.is_empty() {
                // PowerShell's ordered dictionary compares keys case-insensitively.
                found.insert(canonical.to_lowercase(), id.clone());
            }
        }
        let Some(next) = page.payload.get("nextSkip").filter(|v| !v.is_null()) else {
            break;
        };
        let next = next
            .as_i64()
            .or_else(|| next.as_str()?.parse().ok())
            .ok_or_else(|| fail("service", "Invalid change-list cursor"))?;
        if next <= 0 {
            break;
        }
        let next = u64::try_from(next)?;
        if next <= skip {
            return Err(fail("service", "Change-list cursor did not advance"));
        }
        skip = next;
    }
    let mut result = Map::new();
    let mut missing = Vec::new();
    for path in paths {
        let canonical = canonical_path(path);
        if let Some(id) = found.get(&canonical.to_lowercase()) {
            result.insert(path.clone(), id.clone());
        } else {
            missing.push(canonical);
        }
    }
    if !missing.is_empty() {
        return Err(fail(
            "service",
            format!(
                "Files absent from iteration {iteration}: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(Value::Object(result))
}

/// Creates the local review handle; ADO has no pending review resource.
pub(crate) fn start(
    transport: &mut Transport<'_>,
    target: &Target,
    state: &Value,
) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let iteration = field(state, "iterationId")?;
    let id = positive(iteration)?;
    Ok(
        json!({"reviewId": format!("iteration/{id}"), "state": "unpublished",
        "iterationId": iteration, "target": target, "published": [],
        "summaryThreadId": null, "summaryCommentId": null}),
    )
}

/// Builds the comment shape shared by replies and opening comments.
fn comment(body: &str, parent: Option<&Value>) -> Result<Value, AppError> {
    let parent = match parent.filter(|v| !v.is_null() && v.as_str() != Some("")) {
        None => 0,
        Some(value) => positive(value)?,
    };
    Ok(json!({"parentCommentId": parent, "content": body, "commentType": "text"}))
}

/// Builds a thread request with an iteration-bound file or line anchor.
fn thread_request(
    body: &str,
    anchor: Option<&Value>,
    tracking: Option<&Value>,
    iteration: Option<&Value>,
    status: &str,
) -> Result<Value, AppError> {
    let mut request = Map::new();
    request.insert("comments".to_owned(), json!([comment(body, None)?]));
    request.insert("status".to_owned(), json!(status));
    let Some(anchor) = anchor.filter(|v| !v.is_null()) else {
        return Ok(Value::Object(request));
    };
    let path = canonical_path(text(field(anchor, "path")?)?);
    if path.is_empty() {
        return Err(fail("invalidInput", "Empty anchor path"));
    }
    let tracking = tracking
        .filter(|v| !v.is_null())
        .ok_or_else(|| fail("invalidInput", "Missing change tracking ID"))?;
    positive(tracking)?;
    let iteration = iteration.ok_or_else(|| fail("invalidInput", "Missing iteration ID"))?;
    positive(iteration)?;
    let mut context = Map::new();
    context.insert("filePath".to_owned(), json!(format!("/{path}")));
    if text(field(anchor, "kind")?)? == "line" {
        let end = positive(field(anchor, "endLine")?)?;
        let start = anchor
            .get("startLine")
            .filter(|v| !v.is_null())
            .map(positive)
            .transpose()?
            .unwrap_or(end);
        if start > end {
            return Err(fail("invalidInput", "Invalid line range"));
        }
        let side = anchor
            .get("side")
            .and_then(Value::as_str)
            .unwrap_or("right");
        let (start_name, end_name) = match side {
            "left" => ("leftFileStart", "leftFileEnd"),
            "right" => ("rightFileStart", "rightFileEnd"),
            _ => return Err(fail("invalidInput", "Invalid anchor side")),
        };
        // ADO needs the shortest nonempty column range for whole-line anchors.
        context.insert(start_name.to_owned(), json!({"line": start, "offset": 1}));
        context.insert(end_name.to_owned(), json!({"line": end, "offset": 2}));
    }
    request.insert("threadContext".to_owned(), Value::Object(context));
    request.insert("pullRequestThreadContext".to_owned(), json!({"changeTrackingId": tracking,
        "iterationContext": {"firstComparingIteration": iteration, "secondComparingIteration": iteration}}));
    Ok(Value::Object(request))
}

/// A successful thread creation must identify both the thread and its opener.
fn thread_receipt(response: &Value) -> Result<Value, AppError> {
    let thread_id = response
        .get("id")
        .and_then(|v| identifier(v).ok())
        .filter(|id| positive(&json!(id)).is_ok())
        .ok_or_else(|| fail("ambiguousWrite", "Thread creation has no thread receipt"))?;
    let comments = response
        .get("comments")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| fail("ambiguousWrite", "Thread creation has no comment receipt"))?;
    let comment_ids = comments
        .iter()
        .map(|comment| {
            let id = comment.get("id").ok_or_else(|| {
                fail(
                    "ambiguousWrite",
                    "Thread creation has a comment without an ID",
                )
            })?;
            let number = positive(id).map_err(|error| {
                crate::error::caused_by(
                    "ambiguousWrite",
                    "Thread creation has an invalid comment ID".to_owned(),
                    error.into_std_error(),
                )
            })?;
            Ok((number, id))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let comment_id = comment_ids
        .into_iter()
        .min_by_key(|(number, _)| *number)
        .and_then(|(_, id)| identifier(id).ok())
        .ok_or_else(|| {
            fail(
                "ambiguousWrite",
                "Thread creation has no opening comment ID",
            )
        })?;
    Ok(json!({"threadId": thread_id, "commentId": comment_id}))
}

/// Publishes one finding and retains its exact thread and comment identifiers.
pub(crate) fn add(
    transport: &mut Transport<'_>,
    target: &Target,
    review: &mut Value,
    operation: &Value,
) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    // Validate the handle before issuing an irreversible request.
    let iteration = field(review, "iterationId")?.clone();
    positive(&iteration)?;
    let published = review
        .get_mut("published")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| fail("invalidState", "Review has no publication receipts"))?;
    let body = text(field(operation, "body")?)?;
    let name = text(field(operation, "operation")?)?;
    let route = format!("{}/threads", target.pr_api());
    let written = match name {
        "reply" => {
            let thread = identifier(field(operation, "threadId")?)?;
            positive(&json!(thread))?;
            let request = comment(body, operation.get("commentId"))?;
            let response = transport.request(
                &format!("{route}/{}/comments", escape(&thread)),
                "POST",
                Some(&request),
            )?;
            let id = response
                .get("id")
                .and_then(|v| identifier(v).ok())
                .filter(|s| positive(&json!(s)).is_ok())
                .ok_or_else(|| fail("ambiguousWrite", "Reply has no comment receipt"))?;
            json!({"threadId": thread, "commentId": id})
        }
        "newLineThread" | "newFileThread" => {
            let anchor = field(operation, "anchor")?;
            let expected = if name == "newLineThread" {
                "line"
            } else {
                "file"
            };
            if text(field(anchor, "kind")?)? != expected {
                return Err(fail("invalidInput", "Operation and anchor kind disagree"));
            }
            let request = thread_request(
                body,
                Some(anchor),
                operation.get("changeTrackingId"),
                Some(&iteration),
                "active",
            )?;
            let response = transport.request(&route, "POST", Some(&request))?;
            thread_receipt(&response)?
        }
        _ => {
            return Err(fail(
                "invalidInput",
                "Unknown Azure DevOps publication operation",
            ));
        }
    };
    published.push(written.clone());
    Ok(written)
}

/// Records approval while retaining an existing reviewer's required status.
pub(crate) fn approve(transport: &mut Transport<'_>, target: &Target) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let identity = account(transport)?;
    let id = identity
        .id
        .as_deref()
        .ok_or_else(|| fail("authentication", "Approval requires an account ID"))?;
    let path = format!("{}/reviewers", target.pr_api());
    let mut required = false;
    for reviewer in list(transport, &path)? {
        if Identity::from_account(Platform::AzureDevOps, &reviewer)?.may_match(&identity) {
            required = match reviewer.get("isRequired") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(value)) => *value,
                _ => return Err(fail("service", "Invalid reviewer required status")),
            };
            break;
        }
    }
    let body = json!({"id": id, "vote": 10, "isRequired": required});
    let response = transport.request(&format!("{path}/{}", escape(id)), "PUT", Some(&body))?;
    if response
        .get("id")
        .and_then(|v| identifier(v).ok())
        .is_none_or(|s| !s.eq_ignore_ascii_case(id))
        || response.get("vote").and_then(Value::as_i64) != Some(10)
        || response.get("isRequired").and_then(Value::as_bool) != Some(required)
    {
        return Err(fail(
            "ambiguousWrite",
            "Approval response does not establish the requested vote",
        ));
    }
    Ok(response)
}

/// Confirms the recorded vote on the authenticated reviewer's current entry.
pub(crate) fn verify_approval(
    transport: &mut Transport<'_>,
    target: &Target,
    receipt: &Value,
) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let id = receipt
        .get("id")
        .and_then(|value| identifier(value).ok())
        .ok_or_else(|| fail("invalidState", "Approval receipt has no account identifier"))?;
    let required = receipt
        .get("isRequired")
        .and_then(Value::as_bool)
        .ok_or_else(|| fail("invalidState", "Approval receipt has no required status"))?;
    if receipt.get("vote").and_then(Value::as_i64) != Some(10) {
        return Err(fail(
            "invalidState",
            "Approval receipt has no approving vote",
        ));
    }

    // The account acquiring the readback must be the one that cast the vote.
    let identity = account(transport)?;
    if identity
        .id
        .as_deref()
        .is_none_or(|current| !current.eq_ignore_ascii_case(&id))
    {
        return Err(fail(
            "authentication",
            "Approval account changed before readback",
        ));
    }
    let path = format!("{}/reviewers/{}", target.pr_api(), escape(&id));
    let reviewer = transport.request(&path, "GET", None)?;
    if reviewer
        .get("id")
        .and_then(|value| identifier(value).ok())
        .is_none_or(|current| !current.eq_ignore_ascii_case(&id))
        || reviewer.get("vote").and_then(Value::as_i64) != Some(10)
        || reviewer.get("isRequired").and_then(Value::as_bool) != Some(required)
    {
        return Err(fail(
            "service",
            "Approval readback does not match the recorded vote",
        ));
    }
    Ok(reviewer)
}

/// Posts the summary after the independently journaled approval, if any.
pub(crate) fn complete(
    transport: &mut Transport<'_>,
    target: &Target,
    review: &mut Value,
    summary: &str,
    _approval: bool,
) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let review_id = identifier(field(review, "reviewId")?)?;
    if review.get("published").and_then(Value::as_array).is_none() {
        return Err(fail("invalidState", "Invalid review handle"));
    }
    let review = review
        .as_object_mut()
        .ok_or_else(|| fail("invalidState", "Invalid review handle"))?;
    let request = thread_request(summary, None, None, None, "closed")?;
    let response = transport.request(
        &format!("{}/threads", target.pr_api()),
        "POST",
        Some(&request),
    )?;
    let receipt = thread_receipt(&response)?;
    let thread_id = text(field(&receipt, "threadId")?)?;
    let comment_id = field(&receipt, "commentId")?;
    review.insert("summaryThreadId".to_owned(), json!(thread_id));
    review.insert("summaryCommentId".to_owned(), comment_id.clone());
    review.insert("state".to_owned(), json!("published"));
    Ok(json!({"reviewId": review_id, "state": "published",
        "url": thread_url(&target, thread_id)}))
}

/// Builds a browser link to one discussion.
fn thread_url(target: &Target, id: &str) -> String {
    format!("{}?discussionId={}", target.web_url, escape(id))
}

/// Returns a visible comment and its position in the provider's thread response.
fn find_comment<'a>(
    thread: Option<&'a Value>,
    id: &str,
) -> Result<Option<(usize, &'a Value)>, AppError> {
    let Some(thread) = thread else {
        return Ok(None);
    };
    if deleted(thread)? {
        return Ok(None);
    }
    let mut matching = None;
    for (position, comment) in array(field(thread, "comments")?)?.iter().enumerate() {
        if comment
            .get("id")
            .and_then(|v| identifier(v).ok())
            .as_deref()
            == Some(id)
            && matching.replace((position, comment)).is_some()
        {
            return Err(fail("service", "Duplicate comment identifier in thread"));
        }
    }
    if let Some((_, comment)) = matching
        && deleted(comment)?
    {
        return Ok(None);
    }
    Ok(matching)
}

/// Treats malformed deletion evidence as an error rather than visibility.
fn deleted(node: &Value) -> Result<bool, AppError> {
    match node.get("isDeleted") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(fail("service", "Invalid deletion status")),
    }
}

/// Reads only this review's recorded receipts from the complete discussion.
fn verify_core(
    transport: &mut Transport<'_>,
    target: &Target,
    review: &Value,
    include_summary: bool,
) -> Result<Value, AppError> {
    let target = resolve(transport, target)?;
    let mut threads = HashMap::new();
    for thread in list(transport, &format!("{}/threads", target.pr_api()))? {
        let id = identifier(field(&thread, "id")?)?;
        if threads.insert(id, thread).is_some() {
            return Err(fail("service", "Duplicate thread identifier"));
        }
    }
    // The provider exposes a thread-grouped collection, not a globally ordered
    // sequence of comments. Receipt order organizes this result but is not
    // evidence of the order in which the provider published those comments.
    let published = array(field(review, "published")?)?;
    let mut comments = Vec::new();
    for entry in published {
        let thread_id = identifier(field(entry, "threadId")?)?;
        let id = identifier(field(entry, "commentId")?)?;
        let thread = threads.get(&thread_id);
        let found = find_comment(thread, &id)?;
        let comment = found.map(|(_, comment)| comment);
        let comment_position = found.map(|(position, _)| position);
        let context = thread
            .and_then(|t| t.get("threadContext"))
            .filter(|v| v.is_object());
        let path = context
            .and_then(|c| c.get("filePath"))
            .and_then(Value::as_str)
            .map(canonical_path)
            .unwrap_or_default();
        let line_and_side = context.and_then(|c| {
            [
                ("/rightFileEnd/line", "RIGHT"),
                ("/rightFileStart/line", "RIGHT"),
                ("/leftFileEnd/line", "LEFT"),
                ("/leftFileStart/line", "LEFT"),
            ]
            .iter()
            .find_map(|(path, side)| {
                c.pointer(path)
                    .filter(|v| !v.is_null())
                    .map(|line| (line, *side))
            })
        });
        let line = line_and_side.map(|(line, _)| line);
        let side = line_and_side.map(|(_, side)| side);
        let start_line = context.and_then(|context| match side {
            Some("RIGHT") => context.pointer("/rightFileStart/line"),
            Some("LEFT") => context.pointer("/leftFileStart/line"),
            _ => None,
        });
        let end_line = context.and_then(|context| match side {
            Some("RIGHT") => context.pointer("/rightFileEnd/line"),
            Some("LEFT") => context.pointer("/leftFileEnd/line"),
            _ => None,
        });
        let subject = if line.is_some() {
            "LINE"
        } else if !path.is_empty() {
            "FILE"
        } else {
            "PULL_REQUEST"
        };
        let visible = comment.is_some();
        // A receipt identifies the expected thread, not proof the thread is visible.
        let actual_thread_id = if visible {
            thread
                .map(|thread| identifier(field(thread, "id")?))
                .transpose()?
        } else {
            None
        };
        comments.push(json!({"id": id, "threadId": actual_thread_id,
            "threadCommentPosition": comment_position, "path": path, "line": line,
            "startLine": start_line, "endLine": end_line, "side": side,
            "subjectType": subject, "state": if visible {"published"} else {"unpublished"},
            "rawStatus": thread.and_then(|t| t.get("status")).and_then(Value::as_str).unwrap_or_default(),
            "isPublished": visible, "body": comment.and_then(|c| c.get("content")).and_then(Value::as_str).unwrap_or_default()}));
    }
    let summary_thread = if include_summary {
        review
            .get("summaryThreadId")
            .and_then(|v| identifier(v).ok())
    } else {
        None
    };
    let summary_id = if include_summary {
        review
            .get("summaryCommentId")
            .and_then(|v| identifier(v).ok())
    } else {
        None
    };
    let summary = match (summary_thread.as_deref(), summary_id.as_deref()) {
        (Some(thread), Some(id)) => {
            find_comment(threads.get(thread), id)?.map(|(_, comment)| comment)
        }
        _ => None,
    };
    let visible = summary.is_some();
    Ok(json!({"reviewId": identifier(field(review, "reviewId")?)?,
        "state": if visible {"published"} else {"unpublished"},
        "url": summary_thread.as_deref().map(|id| thread_url(&target, id)).unwrap_or_default(),
        "body": summary.and_then(|c| c.get("content")).and_then(Value::as_str).unwrap_or_default(),
        "isPublished": visible, "commentsOrder": "unavailable", "comments": comments}))
}

/// Verifies findings before publishing the summary.
pub(crate) fn verify_findings(
    transport: &mut Transport<'_>,
    target: &Target,
    review: &Value,
) -> Result<Value, AppError> {
    verify_core(transport, target, review, false)
}

/// Verifies the summary and each recorded finding.
pub(crate) fn verify(
    transport: &mut Transport<'_>,
    target: &Target,
    review: &Value,
) -> Result<Value, AppError> {
    verify_core(transport, target, review, true)
}

/// Reopens a discussion after its reply has been published.
pub(crate) fn reopen(
    transport: &mut Transport<'_>,
    target: &Target,
    thread_id: &str,
) -> Result<(), AppError> {
    let target = resolve(transport, target)?;
    positive(&json!(thread_id))?;
    let response = transport.request(
        &format!("{}/threads/{}", target.pr_api(), escape(thread_id)),
        "PATCH",
        Some(&json!({"status":"active"})),
    )?;
    if response
        .get("id")
        .and_then(|v| identifier(v).ok())
        .as_deref()
        != Some(thread_id)
        || response
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|s| !s.eq_ignore_ascii_case("active"))
    {
        return Err(fail(
            "ambiguousWrite",
            "Reopen response lacks a matching receipt",
        ));
    }
    Ok(())
}

/// Refuses to claim that already published ADO comments were discarded.
pub(crate) fn discard(
    _transport: &mut Transport<'_>,
    _target: &Target,
    review: &Value,
) -> Result<(), AppError> {
    let id = identifier(field(review, "reviewId")?)?;
    Err(fail(
        "invalidState",
        format!(
            "Azure DevOps has no draft review to discard for {id}; comments are already published"
        ),
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::assertions_on_result_states,
        reason = "The fixtures have explicit bounded shapes and test refusal outcomes"
    )]

    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Map, Value, json};

    use super::{
        add, approve, complete, discard, file_identity, reopen, start, state, verify,
        verify_approval, verify_findings,
    };
    use crate::target::Target;
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, classification};

    /// Matches every request against a scripted, in-process provider.
    type Step = (&'static str, String, Option<Value>, Value);

    struct Fixture {
        steps: RefCell<VecDeque<Step>>,
    }

    impl Fixture {
        fn new(steps: Vec<Step>) -> Self {
            Self {
                steps: RefCell::new(steps.into()),
            }
        }

        fn done(&self) {
            assert!(self.steps.borrow().is_empty());
        }
    }

    impl Process for Fixture {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "az");
            Ok(ProcessOutput::new(true, b"token".to_vec(), vec![]))
        }
    }

    impl Clock for Fixture {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("unexpected retry")
        }
    }

    impl Http for Fixture {
        fn send(
            &self,
            method: &str,
            url: &str,
            token: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            assert_eq!(token, "token");
            let (expected_method, expected_url, expected_body, response) = self
                .steps
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(method, expected_method);
            assert_eq!(url, expected_url);
            let actual = body.map(|bytes| serde_json::from_slice::<Value>(bytes).unwrap());
            assert_eq!(actual, expected_body);
            Ok(HttpResponse::new(
                200,
                BTreeMap::new(),
                serde_json::to_vec(&response).unwrap(),
            ))
        }
    }

    fn target() -> Target {
        Target::pull_request(
            "https://dev.azure.com/org/proj/_git/repo/pullrequest/7",
            None,
        )
        .unwrap()
    }

    fn url(path: &str, query: &str) -> String {
        format!("https://dev.azure.com/org/{path}?{query}api-version=7.1")
    }

    fn connection_url() -> String {
        "https://dev.azure.com/org/_apis/connectionData?api-version=7.1-preview".to_owned()
    }

    fn list(values: Value) -> Value {
        let count = values.as_array().unwrap().len();
        Value::Object(Map::from_iter([
            ("count".to_owned(), json!(count)),
            ("value".to_owned(), values),
        ]))
    }

    fn connection() -> Value {
        json!({"authenticatedUser": {"id": "account", "subjectDescriptor":"aad.account",
            "properties":{"Account":{"$type":"String", "$value":"user@example.test"}}}})
    }

    fn review() -> Value {
        json!({"reviewId":"iteration/3", "iterationId":3, "published":[],
            "summaryThreadId":null, "summaryCommentId":null, "state":"unpublished"})
    }

    #[test]
    fn state_and_change_pages_use_iteration_identity() {
        let pr = "proj/_apis/git/repositories/repo/pullRequests/7";
        let steps = vec![
            (
                "GET",
                url(pr, ""),
                None,
                json!({"pullRequestId":7,"status":"active","isDraft":true}),
            ),
            (
                "GET",
                url(&format!("{pr}/iterations"), ""),
                None,
                list(
                    json!([{"id":2,"sourceRefCommit":{"commitId":"old"},"commonRefCommit":{"commitId":"oldbase"}},
                    {"id":3,"sourceRefCommit":{"commitId":"head"},"commonRefCommit":{"commitId":"base"}}]),
                ),
            ),
            ("GET", connection_url(), None, connection()),
            (
                "GET",
                url(&format!("{pr}/iterations/3/changes"), "%24top=1000&"),
                None,
                json!({"changeEntries":[{"item":{"path":"/src/a.rs"},"changeTrackingId":42}],"nextSkip":4}),
            ),
            (
                "GET",
                url(
                    &format!("{pr}/iterations/3/changes"),
                    "%24skip=4&%24top=1000&",
                ),
                None,
                json!({"changeEntries":[{"item":{"path":"/src/b.rs"},"changeTrackingId":43}],"nextSkip":0}),
            ),
        ];
        let fixture = Fixture::new(steps);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let state = state(&mut transport, &target).unwrap();
        assert_eq!(state["iterationId"], 3);
        assert_eq!(state["sourceCommit"], "head");
        assert_eq!(state["mergeBaseCommit"], "base");
        assert_eq!(state["identity"], "user@example.test");
        let ids = file_identity(
            &mut transport,
            &target,
            &state,
            &["src/a.rs".into(), "src/b.rs".into()],
        )
        .unwrap();
        assert_eq!(ids, json!({"src/a.rs":42,"src/b.rs":43}));
        let handle = start(&mut transport, &target, &state).unwrap();
        assert_eq!(handle["reviewId"], "iteration/3");
        fixture.done();
    }

    #[test]
    fn publication_and_readback_keep_separate_receipts() {
        let pr = "proj/_apis/git/repositories/repo/pullRequests/7";
        let threads = format!("{pr}/threads");
        let line_body = json!({"comments":[{"parentCommentId":0,"content":"finding λ","commentType":"text"}],"status":"active",
            "threadContext":{"filePath":"/src/a.rs","leftFileStart":{"line":2,"offset":1},"leftFileEnd":{"line":4,"offset":2}},
            "pullRequestThreadContext":{"changeTrackingId":42,"iterationContext":{"firstComparingIteration":3,"secondComparingIteration":3}}});
        let reply_body = json!({"parentCommentId":5,"content":"answer","commentType":"text"});
        let summary_body = json!({"comments":[{"parentCommentId":0,"content":"summary","commentType":"text"}],"status":"closed"});
        let readback = list(json!([
            {"id":8,"status":"active","threadContext":{"filePath":"/src/a.rs","leftFileEnd":{"line":4}},"comments":[{"id":1,"content":"finding λ"}]},
            {"id":9,"status":"active","comments":[{"id":6,"content":"answer"}]},
            {"id":10,"status":"closed","comments":[{"id":1,"content":"summary"}]}
        ]));
        let fixture = Fixture::new(vec![
            (
                "POST",
                url(&threads, ""),
                Some(line_body),
                json!({"id":8,"comments":[{"id":1}]}),
            ),
            (
                "POST",
                url(&format!("{threads}/9/comments"), ""),
                Some(reply_body),
                json!({"id":6}),
            ),
            (
                "PATCH",
                url(&format!("{threads}/9"), ""),
                Some(json!({"status":"active"})),
                json!({"id":9,"status":"active"}),
            ),
            ("GET", url(&threads, ""), None, readback.clone()),
            (
                "POST",
                url(&threads, ""),
                Some(summary_body),
                json!({"id":10,"comments":[{"id":1}]}),
            ),
            ("GET", url(&threads, ""), None, readback),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let mut review = review();
        let operation = json!({"operation":"newLineThread","body":"finding λ","anchor":{"kind":"line","path":"src/a.rs","side":"left","startLine":2,"endLine":4},"changeTrackingId":42});
        assert_eq!(
            add(&mut transport, &target, &mut review, &operation).unwrap(),
            json!({"threadId":"8","commentId":"1"})
        );
        add(
            &mut transport,
            &target,
            &mut review,
            &json!({"operation":"reply","body":"answer","threadId":"9","commentId":"5"}),
        )
        .unwrap();
        reopen(&mut transport, &target, "9").unwrap();
        let findings = verify_findings(&mut transport, &target, &review).unwrap();
        assert!(!findings["isPublished"].as_bool().unwrap());
        assert_eq!(findings["comments"][0]["threadId"], "8");
        assert_eq!(findings["comments"][0]["line"], 4);
        assert_eq!(findings["comments"][0]["side"], "LEFT");
        assert!(findings["comments"][0]["startLine"].is_null());
        assert_eq!(findings["comments"][1]["subjectType"], "PULL_REQUEST");
        // The approval argument never initiates a vote: it is a separate effect.
        complete(&mut transport, &target, &mut review, "summary", true).unwrap();
        let checked = verify(&mut transport, &target, &review).unwrap();
        assert!(checked["isPublished"].as_bool().unwrap());
        assert_eq!(checked["body"], "summary");
        assert_eq!(checked["comments"].as_array().unwrap().len(), 2);
        assert!(discard(&mut transport, &target, &review).is_err());
        fixture.done();
    }

    #[test]
    fn approval_preserves_required_by_shared_identity() {
        let pr = "proj/_apis/git/repositories/repo/pullRequests/7";
        let reviewers = format!("{pr}/reviewers");
        let body = json!({"id":"account","vote":10,"isRequired":true});
        let fixture = Fixture::new(vec![
            ("GET", connection_url(), None, connection()),
            (
                "GET",
                url(&reviewers, ""),
                None,
                list(json!([{"id":"other","descriptor":"aad.account","isRequired":true}])),
            ),
            (
                "PUT",
                url(&format!("{reviewers}/account"), ""),
                Some(body.clone()),
                body,
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert_eq!(
            approve(&mut transport, &target).unwrap()["isRequired"],
            true
        );
        fixture.done();
    }

    #[test]
    fn missing_receipts_are_ambiguous_and_not_retried() {
        let pr = "proj/_apis/git/repositories/repo/pullRequests/7";
        let fixture = Fixture::new(vec![(
            "POST",
            url(&format!("{pr}/threads"), ""),
            Some(json!({
                "comments":[{"parentCommentId":0,"content":"summary","commentType":"text"}],"status":"closed"
            })),
            json!({"id":8,"comments":[{}]}),
        )]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let mut review = review();
        let error = complete(&mut transport, &target, &mut review, "summary", false).unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert!(review["summaryThreadId"].is_null());
        fixture.done();
    }

    #[test]
    fn missing_files_and_nonadvancing_cursor_refuse_before_writes() {
        let changes = "proj/_apis/git/repositories/repo/pullRequests/7/iterations/3/changes";
        let fixture = Fixture::new(vec![
            (
                "GET",
                url(changes, "%24top=1000&"),
                None,
                json!({"changeEntries":[],"nextSkip":4}),
            ),
            (
                "GET",
                url(changes, "%24skip=4&%24top=1000&"),
                None,
                json!({"changeEntries":[],"nextSkip":4}),
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert!(
            file_identity(
                &mut transport,
                &target,
                &json!({"iterationId":3}),
                &["missing".into()]
            )
            .is_err()
        );
        fixture.done();
    }

    #[test]
    fn file_thread_and_deleted_findings_are_read_back_as_absent() {
        let threads = "proj/_apis/git/repositories/repo/pullRequests/7/threads";
        let body = json!({"comments":[{"parentCommentId":0,"content":"file","commentType":"text"}],"status":"active",
            "threadContext":{"filePath":"/src/a.rs"},
            "pullRequestThreadContext":{"changeTrackingId":42,"iterationContext":{"firstComparingIteration":3,"secondComparingIteration":3}}});
        let fixture = Fixture::new(vec![
            (
                "POST",
                url(threads, ""),
                Some(body),
                json!({"id":8,"comments":[{"id":1}]}),
            ),
            (
                "GET",
                url(threads, ""),
                None,
                list(json!([
                    {"id":8,"status":"closed","threadContext":{"filePath":"/src/a.rs"},
                     "comments":[{"id":1,"isDeleted":true,"content":"file"}]}
                ])),
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let mut review = review();
        add(&mut transport, &target, &mut review, &json!({
            "operation":"newFileThread","body":"file","anchor":{"kind":"file","path":"src/a.rs"},"changeTrackingId":42
        })).unwrap();
        let result = verify_findings(&mut transport, &target, &review).unwrap();
        assert_eq!(result["comments"][0]["subjectType"], "FILE");
        assert!(result["comments"][0]["threadId"].is_null());
        assert_eq!(result["comments"][0]["state"], "unpublished");
        assert_eq!(result["comments"][0]["body"], "");
        fixture.done();
    }

    #[test]
    fn verification_reads_actual_line_ranges_and_missing_threads() {
        let threads = "proj/_apis/git/repositories/repo/pullRequests/7/threads";
        let fixture = Fixture::new(vec![(
            "GET",
            url(threads, ""),
            None,
            list(json!([
                {"id":9,"threadContext":{"filePath":"/right.rs","rightFileStart":{"line":7},"rightFileEnd":{"line":11}},
                 "comments":[{"id":3,"content":"right"}]},
                {"id":8,"threadContext":{"filePath":"/left.rs","leftFileStart":{"line":2},"leftFileEnd":{"line":4}},
                 "comments":[{"id":1,"content":"left"}]}
            ])),
        )]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let mut review = review();
        review["published"] = json!([
            {"threadId":"8","commentId":"1"}, {"threadId":"9","commentId":"3"},
            {"threadId":"10","commentId":"1"}
        ]);
        let result = verify_findings(&mut transport, &target, &review).unwrap();
        assert_eq!(result["commentsOrder"], "unavailable");
        assert_eq!(result["comments"][0]["threadId"], "8");
        assert_eq!(result["comments"][0]["side"], "LEFT");
        assert_eq!(result["comments"][0]["startLine"], 2);
        assert_eq!(result["comments"][0]["endLine"], 4);
        assert_eq!(result["comments"][0]["line"], 4);
        assert_eq!(result["comments"][1]["threadId"], "9");
        assert_eq!(result["comments"][1]["side"], "RIGHT");
        assert_eq!(result["comments"][1]["startLine"], 7);
        assert_eq!(result["comments"][1]["endLine"], 11);
        assert_eq!(result["comments"][1]["line"], 11);
        assert!(result["comments"][2]["threadId"].is_null());
        assert!(result["comments"][2]["threadCommentPosition"].is_null());
        assert!(!result["comments"][2]["isPublished"].as_bool().unwrap());
        fixture.done();
    }

    #[test]
    fn verification_exposes_provider_positions_for_interleaved_threads() {
        let threads = "proj/_apis/git/repositories/repo/pullRequests/7/threads";
        for (same_thread_comments, first_position, last_position) in [
            (
                json!([{"id":1,"content":"first"},{"id":4,"content":"unrelated"},{"id":5,"content":"last"}]),
                0,
                2,
            ),
            (
                json!([{"id":5,"content":"last"},{"id":4,"content":"unrelated"},{"id":1,"content":"first"}]),
                2,
                0,
            ),
        ] {
            let fixture = Fixture::new(vec![(
                "GET",
                url(threads, ""),
                None,
                list(json!([
                    {"id":9,"comments":[{"id":1,"content":"middle"}]},
                    {"id":8,"comments":same_thread_comments}
                ])),
            )]);
            let target = target();
            let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
            let mut review = review();
            review["published"] = json!([
                {"threadId":"8","commentId":"1"}, {"threadId":"9","commentId":"1"},
                {"threadId":"8","commentId":"5"}
            ]);
            let result = verify_findings(&mut transport, &target, &review).unwrap();
            assert_eq!(result["commentsOrder"], "unavailable");
            assert_eq!(result["comments"][0]["threadId"], "8");
            assert_eq!(
                result["comments"][0]["threadCommentPosition"],
                first_position
            );
            assert_eq!(result["comments"][1]["threadId"], "9");
            assert_eq!(result["comments"][1]["threadCommentPosition"], 0);
            assert_eq!(result["comments"][2]["threadId"], "8");
            assert_eq!(
                result["comments"][2]["threadCommentPosition"],
                last_position
            );
            fixture.done();
        }
    }

    #[test]
    fn approval_readback_requires_same_account_vote_and_required_status() {
        let reviewers = "proj/_apis/git/repositories/repo/pullRequests/7/reviewers";
        let receipt = json!({"id":"account","vote":10,"isRequired":true});
        let readback = json!({"id":"account","vote":10,"isRequired":true,"displayName":"Reviewer"});
        let fixture = Fixture::new(vec![
            ("GET", connection_url(), None, connection()),
            (
                "GET",
                url(&format!("{reviewers}/account"), ""),
                None,
                readback.clone(),
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert_eq!(
            verify_approval(&mut transport, &target, &receipt).unwrap(),
            readback
        );
        fixture.done();

        for readback in [
            json!({"id":"other","vote":10,"isRequired":true}),
            json!({"id":"account","vote":0,"isRequired":true}),
            json!({"id":"account","vote":10,"isRequired":false}),
            json!({"id":"account","vote":10}),
        ] {
            let fixture = Fixture::new(vec![
                ("GET", connection_url(), None, connection()),
                (
                    "GET",
                    url(&format!("{reviewers}/account"), ""),
                    None,
                    readback,
                ),
            ]);
            let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
            assert!(verify_approval(&mut transport, &target, &receipt).is_err());
            fixture.done();
        }

        let fixture = Fixture::new(vec![(
            "GET",
            connection_url(),
            None,
            json!({"authenticatedUser":{"id":"different","subjectDescriptor":"aad.different"}}),
        )]);
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert_eq!(
            classification(&verify_approval(&mut transport, &target, &receipt).unwrap_err()),
            "authentication"
        );
        fixture.done();
    }

    #[test]
    fn absent_reviewer_is_optional_and_missing_vote_receipt_is_ambiguous() {
        let reviewers = "proj/_apis/git/repositories/repo/pullRequests/7/reviewers";
        let fixture = Fixture::new(vec![
            ("GET", connection_url(), None, connection()),
            ("GET", url(reviewers, ""), None, list(json!([]))),
            (
                "PUT",
                url(&format!("{reviewers}/account"), ""),
                Some(json!({
                    "id":"account","vote":10,"isRequired":false
                })),
                json!({}),
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let error = approve(&mut transport, &target).unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        fixture.done();
    }

    #[test]
    fn missing_anchor_identity_and_unknown_file_refuse_before_publication() {
        let changes = "proj/_apis/git/repositories/repo/pullRequests/7/iterations/3/changes";
        let fixture = Fixture::new(vec![(
            "GET",
            url(changes, "%24top=1000&"),
            None,
            json!({"changeEntries":[]}),
        )]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert!(
            file_identity(
                &mut transport,
                &target,
                &json!({"iterationId":3}),
                &["missing".into()]
            )
            .is_err()
        );
        let mut review = review();
        assert!(add(&mut transport, &target, &mut review, &json!({
            "operation":"newFileThread","body":"file","anchor":{"kind":"file","path":"src/a.rs"}
        })).is_err());
        assert!(review["published"].as_array().unwrap().is_empty());
        fixture.done();
    }

    #[test]
    fn explicit_rate_refusal_is_retried_but_transport_failure_is_not() {
        struct RateProvider {
            fixture: Fixture,
            calls: Cell<usize>,
            waits: Cell<usize>,
            uncertain: bool,
        }

        impl Process for RateProvider {
            fn execute(
                &self,
                program: &str,
                arguments: &[String],
                input: &[u8],
            ) -> io::Result<ProcessOutput> {
                self.fixture.execute(program, arguments, input)
            }
        }

        impl Clock for RateProvider {
            fn now(&self) -> Timestamp {
                Timestamp::UNIX_EPOCH
            }
            fn wait(&self, _: Duration) -> Result<(), AppError> {
                assert_eq!(self.waits.get(), 0, "unexpected retry");
                self.waits.set(1);
                Ok(())
            }
        }

        impl Http for RateProvider {
            fn send(
                &self,
                method: &str,
                url: &str,
                token: &str,
                body: Option<&[u8]>,
            ) -> Result<HttpResponse, AppError> {
                let calls = self.calls.get();
                self.calls.set(calls + 1);
                if self.uncertain {
                    return Err(crate::error::fail("service", "injected transport failure"));
                }
                if calls == 0 {
                    return Ok(HttpResponse::new(429, BTreeMap::new(), Vec::new()));
                }
                self.fixture.send(method, url, token, body)
            }
        }

        let threads = "proj/_apis/git/repositories/repo/pullRequests/7/threads";
        let body = json!({"comments":[{"parentCommentId":0,"content":"summary","commentType":"text"}],"status":"closed"});
        let provider = RateProvider {
            fixture: Fixture::new(vec![(
                "POST",
                url(threads, ""),
                Some(body),
                json!({"id":8,"comments":[{"id":1}]}),
            )]),
            calls: Cell::new(0),
            waits: Cell::new(0),
            uncertain: false,
        };
        let target = target();
        let mut transport = Transport::new(&target, None, &provider, &provider, &provider);
        complete(&mut transport, &target, &mut review(), "summary", false).unwrap();
        assert_eq!(provider.calls.get(), 2);
        assert_eq!(provider.waits.get(), 1);
        provider.fixture.done();

        let provider = RateProvider {
            fixture: Fixture::new(vec![]),
            calls: Cell::new(0),
            waits: Cell::new(0),
            uncertain: true,
        };
        let mut transport = Transport::new(&target, None, &provider, &provider, &provider);
        let error = complete(&mut transport, &target, &mut review(), "summary", false).unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert_eq!(provider.calls.get(), 1);
        assert_eq!(provider.waits.get(), 0);
    }
}
