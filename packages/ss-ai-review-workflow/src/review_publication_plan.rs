//! Prepares the local portion of final pull request review publication.

use std::collections::HashSet;
use std::path::Path;

use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_git::Comparison;
use ss_ai_runtime::{Clock, fail, read_bytes};

use crate::anchors::resolve_anchor_in_diff;
use crate::approved::{
    check_approved_schema, check_approved_value, check_follow_up, identifier_matches,
};
use crate::markdown;
use crate::workflow_plan::parse_json;

/// Reads and validates an approved artifact without rereading it for planning.
///
/// The returned snapshot must still be checked against its capture by [`prepare`].
///
/// # Errors
///
/// Rejects missing files, duplicate JSON keys, and malformed approved artifacts.
pub fn read_approved(path: &Path, clock: &dyn Clock) -> Result<Value, AppError> {
    let bytes = read_bytes(path, clock)?.ok_or_else(|| invalid("Approved set is missing"))?;
    parse_approved(&bytes)
}

/// Checks the stored shape before returning a snapshot to the caller.
fn parse_approved(bytes: &[u8]) -> Result<Value, AppError> {
    let document = parse_json(bytes)?;
    if let Some(version) = document.get("schemaVersion")
        && version.as_u64() != Some(2)
    {
        return Err(invalid(format!(
            "Approved finding artifact reports version {version}; expected version 2"
        )));
    }
    if let Some(findings) = document.get("findings").and_then(Value::as_array) {
        let mut ids = HashSet::new();
        for finding in findings {
            if let Some(id) = finding.get("id").and_then(Value::as_str)
                && !ids.insert(id)
            {
                return Err(invalid(format!(
                    "Approved finding artifact holds more than one finding '{id}'"
                )));
            }
        }
    }
    check_approved_schema(&document)?;
    let hash = document.get("reviewedContextHash");
    if !document.get("target").is_some_and(Value::is_null) && hash.is_none_or(Value::is_null) {
        return Err(invalid(
            "A pull request target requires a reviewed context hash",
        ));
    }
    if let Some(hash) = hash.filter(|hash| !hash.is_null()) {
        let valid = hash.as_str().is_some_and(|text| {
            text.len() == 64
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
        });
        if !valid {
            return Err(invalid("Invalid reviewed context hash"));
        }
    }
    Ok(document)
}

/// Checks publication inputs that do not require a Git comparison.
///
/// `allow_empty` must only be set when the caller has verified merge approval.
///
/// # Errors
///
/// Rejects malformed or mismatched inputs and unpublishable follow-ups.
pub fn preflight(
    approved: &Value,
    context: &Value,
    diagnostics: &Value,
    allow_empty: bool,
) -> Result<(), AppError> {
    check_approved_value(approved, Some(context))?;
    let platform = validate_target(context)?;
    if !diagnostics.is_object() {
        return Err(invalid("Publication diagnostics must be an object"));
    }
    let findings = approved
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing findings"))?;
    if findings.is_empty() && !allow_empty {
        return Err(invalid("An empty review requires merge approval"));
    }
    let mut follow_up_count = 0_usize;
    for finding in findings {
        let id = text(finding, "id")?;
        if finding
            .get("followUp")
            .is_some_and(|value| !value.is_null())
        {
            check_follow_up(context, finding).map_err(|error| {
                invalid(format!(
                    "Finding '{id}' cannot be published as a follow-up: {error}"
                ))
            })?;
            follow_up_count = follow_up_count.saturating_add(1);
        }
    }
    markdown::render(&json!({
        "operation": "summary", "findings": findings, "platform": platform,
        "followUpCount": follow_up_count, "diagnostics": diagnostics
    }))
    .map_err(invalid)?;
    Ok(())
}

/// Prepares ordered publication operations and the review summary.
///
/// `allow_empty` must only be set when the caller has verified merge approval.
/// The comparison must use the capture's reviewed merge base and source commit.
/// The caller checks the destination, live evidence and publication authority,
/// and supplies any provider-specific file identities before publishing.
///
/// # Errors
///
/// Rejects malformed or mismatched inputs and unresolvable findings.
pub fn prepare(
    approved: &Value,
    context: &Value,
    comparison: &Comparison,
    diagnostics: &Value,
    allow_empty: bool,
) -> Result<Value, AppError> {
    preflight(approved, context, diagnostics, allow_empty)?;
    let platform = text(context, "platform")?;

    // The comparison exposes its endpoints through serialization, not getters.
    // Anchoring against another revision would publish against unreviewed code.
    let comparison_value = serde_json::to_value(comparison)?;
    if comparison_value.get("oldCommit") != approved.get("reviewedMergeBaseCommit")
        || comparison_value.get("newCommit") != approved.get("reviewedSourceCommit")
    {
        return Err(invalid("Comparison does not match the reviewed commits"));
    }

    let findings = approved
        .get("findings")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing findings"))?;
    let mut threads = Vec::new();
    let mut replies = Vec::new();
    for finding in findings {
        let id = text(finding, "id")?;
        let body = markdown::render(&json!({"operation": "comment", "finding": finding}))
            .map_err(|error| invalid(format!("Finding '{id}' cannot be rendered: {error}")))?;
        if body.as_str().is_none_or(|value| value.trim().is_empty()) {
            return Err(invalid(format!("Finding '{id}' rendered to no text")));
        }
        if let Some(follow_up) = finding.get("followUp").filter(|value| !value.is_null()) {
            let thread_id = text(follow_up, "threadId")?;
            let comment_id = text(follow_up, "commentId")?;
            let thread = captured_thread(context, thread_id)
                .ok_or_else(|| invalid(format!("Missing follow-up thread '{thread_id}'")))?;
            replies.push(json!({
                "operation": "reply", "findingId": id, "body": body, "anchor": null,
                "wasDowngraded": false, "threadId": thread_id, "commentId": comment_id,
                "requiresReopen": thread.get("resolution").and_then(Value::as_str) != Some("active"),
                "changeTrackingId": null
            }));
        } else {
            let anchor = finding
                .get("anchor")
                .ok_or_else(|| invalid(format!("Finding '{id}' has no anchor")))?;
            let anchor = resolve_anchor_in_diff(anchor, comparison)
                .map_err(|error| invalid(format!("Finding '{id}' cannot be anchored: {error}")))?;
            let operation = if anchor.get("kind").and_then(Value::as_str) == Some("line") {
                "newLineThread"
            } else {
                "newFileThread"
            };
            let was_downgraded = anchor.get("wasDowngraded").and_then(Value::as_bool) == Some(true);
            threads.push(json!({
                "operation": operation, "findingId": id, "body": body, "anchor": anchor,
                "wasDowngraded": was_downgraded, "threadId": null, "commentId": null,
                "requiresReopen": false, "changeTrackingId": null
            }));
        }
    }
    let summary = markdown::render(&json!({
        "operation": "summary", "findings": findings, "platform": platform,
        "followUpCount": replies.len(), "diagnostics": diagnostics
    }))
    .map_err(invalid)?;
    threads.extend(replies);
    Ok(json!({"plan": threads, "summaryBody": summary}))
}

/// Requires a pull request target consistent with the captured provider.
fn validate_target(context: &Value) -> Result<&str, AppError> {
    let target = context
        .get("target")
        .filter(|target| target.is_object())
        .ok_or_else(|| invalid("Capture requires a pull request target"))?;
    let platform = text(context, "platform")?;
    if !matches!(platform, "gitHub" | "azureDevOps")
        || target.get("platform").and_then(Value::as_str) != Some(platform)
    {
        return Err(invalid("Capture and target platforms do not match"));
    }
    text(target, "organization")?;
    if platform == "gitHub" {
        text(target, "repository")?;
    } else {
        text(target, "collectionUrl")?;
    }
    if !target
        .get("pullRequestId")
        .and_then(Value::as_u64)
        .is_some_and(|id| id > 0 && id <= i32::MAX as u64)
    {
        return Err(invalid("Target requires a pull request identifier"));
    }
    Ok(platform)
}

/// Finds the same captured thread that was checked for reply eligibility.
fn captured_thread<'a>(context: &'a Value, id: &str) -> Option<&'a Value> {
    match context.get("threads")? {
        Value::Array(threads) => threads
            .iter()
            .find(|thread| identifier_matches(thread.get("id"), id)),
        thread @ Value::Object(_) if identifier_matches(thread.get("id"), id) => Some(thread),
        _ => None,
    }
}

/// Reads a nonblank field without converting malformed values.
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, AppError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid(format!("Missing or invalid {field}")))
}

/// Classifies invalid local publication inputs.
fn invalid(message: impl Into<String>) -> AppError {
    fail("invalidInput", message.into())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::assertions_on_result_states,
        reason = "The fixtures use fixed record shapes and assert failures"
    )]

    use std::io;
    use std::path::Path;

    use serde_json::{Value, json};
    use ss_ai_git::{Comparison, compare};
    use ss_ai_review_context::evidence_hash;
    use ss_ai_runtime::{Process, ProcessOutput};

    use super::{parse_approved, prepare};

    /// Simulates Git's exact comparison without using a repository or subprocess.
    struct Git;

    impl Process for Git {
        fn execute(&self, _: &str, arguments: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            let output = if arguments.iter().any(|arg| arg == "cat-file") {
                "commit\n"
            } else if arguments.iter().any(|arg| arg == "diff") {
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
            } else {
                ""
            };
            Ok(ProcessOutput::new(true, output.as_bytes().to_vec(), vec![]))
        }
    }

    fn comparison(old: &str, new: &str) -> Comparison {
        compare(&Git, Path::new("."), old, new).unwrap()
    }

    fn capture() -> Value {
        json!({
            "schemaVersion": 3, "platform": "gitHub",
            "repositoryUrl": "https://github.com/org/repo",
            "pullRequestUrl": "https://github.com/org/repo/pull/1",
            "target": {"platform": "gitHub", "organization": "org", "repository": "repo", "pullRequestId": 1},
            "pullRequest": {"title": "Title", "description": "Body",
                "reviewedSourceCommit": "b".repeat(40), "reviewedMergeBaseCommit": "a".repeat(40)},
            "reviewers": [], "revisions": [],
            "threads": [
                {"id":"T","openingCommentId":"C","kind":"text","resolution":"resolved",
                 "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},
                 "decision":null,"url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[]},
                {"id":12,"openingCommentId":13,"kind":"text","resolution":"active",
                 "isOutdated":false,"isAuthoredBySelf":false,"context":{"filePath":"src/lib.rs"},
                 "decision":null,"url":null,"eventUtc":null,"lastUpdatedUtc":null,"comments":[]}
            ]
        })
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "Fixtures construct and pass owned JSON values"
    )]
    fn finding(id: &str, anchor: Value) -> Value {
        json!({"schemaVersion": 1, "id": id, "sourceFindingIds": [id], "origin": "reviewer",
            "title": "Finding", "description": "Problem", "reasoning": "Reason",
            "consequence": "Impact", "recommendedAction": "Fix", "anchor": anchor})
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "Fixtures construct and pass owned JSON values"
    )]
    fn approved(context: &Value, findings: Value) -> Value {
        json!({"schemaVersion": 2, "target": context["target"],
            "reviewedSourceCommit": context["pullRequest"]["reviewedSourceCommit"],
            "reviewedMergeBaseCommit": context["pullRequest"]["reviewedMergeBaseCommit"],
            "reviewedContextHash": evidence_hash(context).unwrap(), "findings": findings})
    }

    #[test]
    fn plans_threads_before_replies_and_renders_summary() {
        let context = capture();
        let mut reply = finding("reply", json!({"kind":"file","path":"not-in-diff"}));
        reply["followUp"] = json!({"threadId":"T","commentId":"C"});
        let mut active = finding("active", json!({"kind":"file","path":"also-absent"}));
        active["followUp"] = json!({"threadId":"12","commentId":"13"});
        let findings = json!([
            reply,
            finding(
                "line",
                json!({"kind":"line","path":"src/lib.rs","startLine":1})
            ),
            active,
            finding(
                "downgraded",
                json!({"kind":"line","path":"src/lib.rs","startLine":50})
            ),
            finding("file", json!({"kind":"file","path":"src/lib.rs"}))
        ]);
        let result = prepare(
            &approved(&context, findings),
            &context,
            &comparison(&"a".repeat(40), &"b".repeat(40)),
            &json!({"cache":"hit","inputState":"degraded","userInstructionFileCount":2}),
            false,
        )
        .unwrap();
        let plan = result["plan"].as_array().unwrap();
        assert_eq!(
            plan.iter()
                .map(|item| item["findingId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["line", "downgraded", "file", "reply", "active"]
        );
        assert_eq!(plan[0]["operation"], "newLineThread");
        assert_eq!(plan[0]["anchor"]["startLine"], 1);
        assert_eq!(plan[1]["operation"], "newFileThread");
        assert_eq!(plan[1]["wasDowngraded"], true);
        assert_eq!(plan[2]["wasDowngraded"], false);
        assert_eq!(plan[3]["anchor"], Value::Null);
        assert_eq!(plan[3]["requiresReopen"], true);
        assert_eq!(plan[4]["requiresReopen"], false);
        assert_eq!(plan[4]["threadId"], "12");
        for item in plan {
            assert_eq!(item.as_object().unwrap().len(), 9);
            assert_eq!(item["changeTrackingId"], Value::Null);
            assert!(
                item["body"]
                    .as_str()
                    .unwrap()
                    .starts_with("[Copilot speaking]\n\n")
            );
        }
        let summary = result["summaryBody"].as_str().unwrap();
        assert!(summary.contains("Published 5 findings. 2 findings follow up"));
        assert!(summary.contains("| Cache | Hit |"));
        assert!(summary.contains("| Instruction expansion | Degraded |"));
        assert!(summary.contains("| User instruction files | 2 |"));
        assert!(summary.contains("<details>"));
    }

    #[test]
    fn refuses_unbound_invalid_or_unpublishable_inputs() {
        let context = capture();
        let diff = comparison(&"a".repeat(40), &"b".repeat(40));
        let valid = finding("F", json!({"kind":"file","path":"src/lib.rs"}));
        let document = approved(&context, json!([valid]));
        assert!(prepare(&document, &context, &diff, &json!({}), false).is_ok());
        let mut stale = context.clone();
        stale["pullRequest"]["title"] = json!("Changed");
        assert!(prepare(&document, &stale, &diff, &json!({}), false).is_err());
        let mut malformed = document.clone();
        malformed["extra"] = json!(true);
        assert!(prepare(&malformed, &context, &diff, &json!({}), false).is_err());
        let mut duplicate = document.clone();
        duplicate["findings"]
            .as_array_mut()
            .unwrap()
            .push(document["findings"][0].clone());
        assert!(prepare(&duplicate, &context, &diff, &json!({}), false).is_err());
        assert!(
            prepare(
                &document,
                &context,
                &comparison(&"c".repeat(40), &"b".repeat(40)),
                &json!({}),
                false
            )
            .is_err()
        );
        assert!(prepare(&document, &context, &diff, &Value::Null, false).is_err());
        let absent = approved(
            &context,
            json!([finding("F", json!({"kind":"file","path":"absent"}))]),
        );
        assert!(prepare(&absent, &context, &diff, &json!({}), false).is_err());
        let mut reply = finding("F", json!({"kind":"file","path":"absent"}));
        reply["followUp"] = json!({"threadId":"T","commentId":"other"});
        assert!(
            prepare(
                &approved(&context, json!([reply])),
                &context,
                &diff,
                &json!({}),
                false
            )
            .is_err()
        );
        let mut target = context;
        target["target"] = Value::Null;
        assert!(
            prepare(
                &approved(&target, json!([])),
                &target,
                &diff,
                &json!({}),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn empty_review_requires_caller_authority_and_supports_azure_summary() {
        let mut context = capture();
        context["platform"] = json!("azureDevOps");
        context["target"] = json!({"platform":"azureDevOps","organization":"org",
            "collectionUrl":"https://dev.azure.com/org","pullRequestId":1});
        let diff = comparison(&"a".repeat(40), &"b".repeat(40));
        let approved = approved(&context, json!([]));
        assert!(prepare(&approved, &context, &diff, &json!({}), false).is_err());
        let result = prepare(&approved, &context, &diff, &json!({}), true).unwrap();
        assert_eq!(result["plan"], json!([]));
        let summary = result["summaryBody"].as_str().unwrap();
        assert!(summary.contains("This review raises no findings."));
        assert!(!summary.contains("<details>"));
    }

    #[test]
    fn validates_follow_up_threads_for_each_provider() {
        let diff = comparison(&"a".repeat(40), &"b".repeat(40));
        let mut context = capture();
        let mut reply = finding("F", json!({"kind":"file","path":"not-in-diff"}));
        reply["followUp"] = json!({"threadId":"T","commentId":"C"});
        context["threads"][0]["context"] = Value::Null;
        let document = approved(&context, json!([reply.clone()]));
        assert!(prepare(&document, &context, &diff, &json!({}), false).is_err());

        context["platform"] = json!("azureDevOps");
        context["target"] = json!({"platform":"azureDevOps","organization":"org",
            "collectionUrl":"https://dev.azure.com/org","pullRequestId":1});
        context["threads"][0]["resolution"] = json!("indeterminate");
        // The canonical capture reader also permits a singleton object in place of a list.
        context["threads"] = context["threads"][0].clone();
        let document = approved(&context, json!([reply.clone()]));
        let result = prepare(&document, &context, &diff, &json!({}), false).unwrap();
        assert_eq!(result["plan"][0]["requiresReopen"], true);
        assert_eq!(result["plan"][0]["commentId"], "C");
        context["threads"]["kind"] = json!("system");
        let document = approved(&context, json!([reply]));
        assert!(prepare(&document, &context, &diff, &json!({}), false).is_err());
    }

    #[test]
    fn rejects_inconsistent_target_and_invalid_diagnostics() {
        let mut context = capture();
        let diff = comparison(&"a".repeat(40), &"b".repeat(40));
        let valid = json!([finding("F", json!({"kind":"file","path":"src/lib.rs"}))]);
        let document = approved(&context, valid.clone());
        assert!(
            prepare(
                &document,
                &context,
                &diff,
                &json!({"userInstructionFileCount":-1}),
                false
            )
            .is_err()
        );
        context["target"]["platform"] = json!("azureDevOps");
        let document = approved(&context, valid);
        assert!(prepare(&document, &context, &diff, &json!({}), false).is_err());
    }

    #[test]
    fn reads_approved_schema_without_losing_duplicate_keys() {
        let context = capture();
        let document = approved(
            &context,
            json!([finding("F", json!({"kind":"file","path":"src/lib.rs"}))]),
        );
        assert_eq!(
            parse_approved(&serde_json::to_vec(&document).unwrap()).unwrap(),
            document
        );
        let duplicate_root = br#"{"schemaVersion":2,"schemaVersion":2,"target":null,"reviewedSourceCommit":null,"reviewedMergeBaseCommit":null,"reviewedContextHash":null,"findings":[]}"#;
        assert!(parse_approved(duplicate_root).is_err());
        let duplicate_nested = br#"{"schemaVersion":2,"target":{"platform":"gitHub","platform":"gitHub"},"reviewedSourceCommit":null,"reviewedMergeBaseCommit":null,"reviewedContextHash":null,"findings":[]}"#;
        assert!(parse_approved(duplicate_nested).is_err());
        let mut malformed = document.clone();
        malformed["findings"][0]["title"] = Value::Null;
        assert!(parse_approved(&serde_json::to_vec(&malformed).unwrap()).is_err());
        malformed = document;
        malformed["reviewedContextHash"] = json!("invalid");
        assert!(parse_approved(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
}
