//! Model-visible discussion is separate from retained approval authority.

use std::collections::BTreeMap;

use ohno::AppError;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::ado_approval::Evidence as AdoEvidence;
use crate::approval::Evidence;
use crate::error::fail;
use crate::github_approval::Evidence as GitHubEvidence;
use crate::json::{array, field, identifier, nullable_field, text};

/// Binds the interpretation of discussion without exposing authority bookkeeping.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    schema_version: u32,
    #[serde(flatten)]
    discussion: Discussion,
}

/// Provider-shaped locations preserve absent/null coordinates and historical context.
#[derive(Serialize)]
#[serde(tag = "platform", rename_all = "camelCase")]
enum Discussion {
    GitHub {
        #[serde(rename = "pullRequest")]
        pull_request: Map<String, Value>,
        reviews: Vec<Map<String, Value>>,
        comments: Vec<Map<String, Value>>,
        #[serde(rename = "reviewComments")]
        review_comments: Vec<Map<String, Value>>,
        hunks: BTreeMap<String, String>,
    },
    AzureDevOps {
        #[serde(rename = "pullRequest")]
        pull_request: Map<String, Value>,
        #[serde(rename = "sourceCommit")]
        source_commit: String,
        threads: Vec<Map<String, Value>>,
    },
}

/// Selects complete semantic discussion from an immutable provider observation.
pub(crate) fn render(evidence: &Evidence) -> Result<String, AppError> {
    let discussion = match evidence {
        Evidence::GitHub(GitHubEvidence::History {
            pull_request,
            reviews,
            comments,
            review_comments,
            ..
        }) => github(pull_request, reviews, comments, review_comments)?,
        Evidence::AzureDevOps(AdoEvidence::History {
            pull_request,
            source_commit,
            threads,
            ..
        }) => ado(pull_request, source_commit, threads)?,
        Evidence::GitHub(GitHubEvidence::State { .. })
        | Evidence::AzureDevOps(AdoEvidence::State { .. }) => {
            return Err(fail(
                "invalidState",
                "Assessment discussion requires complete history evidence",
            ));
        }
    };
    // The version identifies the projection contract, independently of provider/run schemas.
    Ok(serde_json::to_string(&Document {
        schema_version: 1,
        discussion,
    })?)
}

/// Removes authority reconstruction while retaining every review and comment body.
fn github(
    pr: &Value,
    reviews: &[Value],
    comments: &[Value],
    inline: &[Value],
) -> Result<Discussion, AppError> {
    nullable_text(nullable_field(pr, "body")?)?;
    let mut pull_request = project(pr, &["number", "title", "body", "user", "head"])?;
    identity(&mut pull_request, "user", &["id", "type"])?;
    if let Some(head) = pull_request.get_mut("head") {
        *head = Value::Object(project(head, &["sha", "ref"])?);
    }
    let reviews = reviews
        .iter()
        .map(|review| {
            identifier(field(review, "id")?)?;
            text(field(review, "body")?)?;
            let mut review = project(
                review,
                &["id", "state", "submitted_at", "body", "commit_id", "user"],
            )?;
            identity(&mut review, "user", &["id", "type"])?;
            Ok(review)
        })
        .collect::<Result<_, AppError>>()?;
    let mut hunks = BTreeMap::new();
    let comments = github_comments(comments, &mut hunks)?;
    let review_comments = github_comments(inline, &mut hunks)?;
    Ok(Discussion::GitHub {
        pull_request,
        reviews,
        comments,
        review_comments,
        hunks,
    })
}

/// Shares only exact excerpt text; each comment keeps its own location and revision.
fn github_comments(
    comments: &[Value],
    hunks: &mut BTreeMap<String, String>,
) -> Result<Vec<Map<String, Value>>, AppError> {
    comments
        .iter()
        .map(|comment| {
            identifier(field(comment, "id")?)?;
            text(field(comment, "body")?)?;
            let mut comment = project(
                comment,
                &[
                    "id",
                    "body",
                    "created_at",
                    "updated_at",
                    "user",
                    "in_reply_to_id",
                    "path",
                    "diff_hunk",
                    "line",
                    "side",
                    "start_line",
                    "start_side",
                    "original_line",
                    "original_start_line",
                    "position",
                    "original_position",
                    "commit_id",
                    "original_commit_id",
                    "pull_request_review_id",
                    "subject_type",
                ],
            )?;
            identity(&mut comment, "user", &["id", "type"])?;
            if let Some(hunk) = comment.remove("diff_hunk") {
                let reference = match hunk {
                    Value::Null => Value::Null,
                    Value::String(hunk) => {
                        let digest = format!("{:x}", Sha256::digest(hunk.as_bytes()));
                        if let Some(existing) = hunks.get(&digest) {
                            if existing != &hunk {
                                return Err(fail(
                                    "invalidState",
                                    "Discussion hunk digest collision",
                                ));
                            }
                        } else {
                            hunks.insert(digest.clone(), hunk);
                        }
                        Value::String(digest)
                    }
                    _ => return Err(fail("service", "Discussion hunk must be text or null")),
                };
                comment.insert("hunkReference".to_owned(), reference);
            }
            Ok(comment)
        })
        .collect()
}

/// Thread context remains semantic evidence; vote/policy reconstruction does not.
fn ado(pr: &Value, source: &str, threads: &[Value]) -> Result<Discussion, AppError> {
    let mut pull_request = project(
        pr,
        &[
            "pullRequestId",
            "title",
            "description",
            "createdBy",
            "sourceRefName",
        ],
    )?;
    identity(
        &mut pull_request,
        "createdBy",
        &["id", "isContainer", "inactive", "isDeletedInOrigin"],
    )?;
    let threads = threads
        .iter()
        .map(|thread| {
            identifier(field(thread, "id")?)?;
            let mut projected = project(
                thread,
                &[
                    "id",
                    "status",
                    "isDeleted",
                    "publishedDate",
                    "threadContext",
                    "pullRequestThreadContext",
                ],
            )?;
            // Deleted tombstones may omit comments; surviving threads require the collection.
            if thread.get("comments").is_some()
                || thread.get("isDeleted") != Some(&Value::Bool(true))
            {
                let comments = array(field(thread, "comments")?)?
                    .iter()
                    .map(|comment| {
                        ado_comment(comment, thread.get("isDeleted") == Some(&Value::Bool(true)))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                projected.insert("comments".to_owned(), serde_json::to_value(comments)?);
            }
            if let Some(kind) = thread.pointer("/properties/CodeReviewThreadType") {
                projected.insert("threadType".to_owned(), kind.clone());
            }
            Ok(projected)
        })
        .collect::<Result<_, AppError>>()?;
    Ok(Discussion::AzureDevOps {
        pull_request,
        source_commit: source.to_owned(),
        threads,
    })
}

/// Preserves human replies even when they share a thread with a service event.
fn ado_comment(comment: &Value, deleted_thread: bool) -> Result<Map<String, Value>, AppError> {
    identifier(field(comment, "id")?)?;
    let requires_content = !deleted_thread
        && comment.get("isDeleted") != Some(&Value::Bool(true))
        && comment.get("commentType").and_then(Value::as_str) != Some("system");
    if requires_content {
        text(field(comment, "content")?)?;
    } else if let Some(content) = comment.get("content") {
        nullable_text(content)?;
    }
    let mut comment = project(
        comment,
        &[
            "id",
            "parentCommentId",
            "author",
            "content",
            "publishedDate",
            "lastContentUpdatedDate",
            "lastUpdatedDate",
            "isDeleted",
            "commentType",
        ],
    )?;
    identity(
        &mut comment,
        "author",
        &["id", "isContainer", "inactive", "isDeletedInOrigin"],
    )?;
    Ok(comment)
}

/// Keeps provider-shaped optional fields without inventing missing text or records.
fn project(value: &Value, fields: &[&str]) -> Result<Map<String, Value>, AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| fail("service", "Discussion record must be an object"))?;
    for name in ["title", "description"] {
        if let Some(value) = object.get(name) {
            nullable_text(value)?;
        }
    }
    Ok(object
        .iter()
        .filter(|(name, _)| fields.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect())
}

/// Stable identity and provider type retain attribution without profile decoration.
fn identity(record: &mut Map<String, Value>, name: &str, fields: &[&str]) -> Result<(), AppError> {
    if let Some(value) = record.get_mut(name)
        && !value.is_null()
    {
        *value = Value::Object(project(value, fields)?);
    }
    Ok(())
}

/// Explicit null text stays distinct from a missing provider field.
fn nullable_text(value: &Value) -> Result<(), AppError> {
    if !value.is_null() {
        text(value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "Tests use bounded synthetic records and volume counters"
    )]

    use serde_json::{Value, json};

    use super::render;
    use crate::approval::Evidence;

    /// Fixture history remains complete while each test varies only discussion.
    fn github() -> Value {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/github-approval.json")).unwrap();
        json!({"platform": "gitHub", "observation": fixture["evidence"]})
    }

    /// A mixed service/human thread exercises semantic content independently of vote authority.
    fn ado() -> Value {
        json!({
            "platform": "azureDevOps",
            "observation": {
                "scope": "history", "sourceCommit": "a".repeat(40),
                "pullRequest": {"pullRequestId": 1, "title": "Requirements", "description": "Full description",
                    "createdBy": {"id": "author", "displayName": "profile"},
                    "sourceRefName": "refs/heads/topic", "repository": {"id": "authority-only"}},
                "reviewers": [], "iterations": [], "policies": [], "evaluations": [],
                "threads": [{
                    "id": 1, "status": "pending", "isDeleted": false, "publishedDate": "captured-time",
                    "threadContext": {"filePath": "/src/file.rs", "rightFileStart": {"line": 8, "offset": 1}},
                    "pullRequestThreadContext": {"iterationContext": {"firstComparingIteration": 1, "secondComparingIteration": 2}},
                    "properties": {"CodeReviewThreadType": {"$type": "System.String", "$value": "VoteUpdate"},
                        "CodeReviewVoteResult": {"$type": "System.String", "$value": "10"}},
                    "identities": {"1": {"id": "authority-only"}},
                    "comments": [
                        {"id": 1, "commentType": "system"},
                        {"id": 2, "parentCommentId": 1, "commentType": "text", "content": "Human objection remains.",
                            "author": {"id": "human", "isContainer": false, "displayName": "profile"},
                            "publishedDate": "created", "lastContentUpdatedDate": "edited"},
                        {"id": 3, "parentCommentId": 2, "commentType": "text", "isDeleted": true, "content": null}
                    ]
                }, {"id": 2, "isDeleted": true}]
            }
        })
    }

    fn project(value: &Value) -> Value {
        let evidence: Evidence = serde_json::from_value(value.clone()).unwrap();
        serde_json::from_str(&render(&evidence).unwrap()).unwrap()
    }

    #[test]
    fn github_projection_keeps_bodies_authors_locations_and_exact_shared_hunks() {
        let mut original = github();
        original["observation"]["pullRequest"]["title"] = json!("Requirement in title");
        let hunk = "@@ -1 +1 @@\n-old\n+intermediate";
        original["observation"]["reviewComments"] = json!([
            {"id": 10, "body": "Human objection", "user": {"id": 99, "type": "User", "login": "profile"},
                "path": "old.rs", "diff_hunk": hunk, "commit_id": "b".repeat(40), "original_commit_id": "a".repeat(40),
                "line": null, "original_line": 1, "side": "RIGHT", "pull_request_review_id": 1},
            {"id": 11, "body": "[Copilot speaking] Reply", "in_reply_to_id": 10, "user": {"id": 99, "type": "User"},
                "path": "other.rs", "diff_hunk": hunk, "commit_id": "c".repeat(40), "line": 2, "side": "LEFT"},
            {"id": 12, "body": "Another concern", "diff_hunk": hunk.replace("old", "Old")}
        ]);
        let before = original.clone();
        let result = project(&original);
        assert_eq!(original, before);
        assert_eq!(result["schemaVersion"], 1);
        assert_eq!(result["platform"], "gitHub");
        assert_eq!(
            result["pullRequest"]["body"],
            original["observation"]["pullRequest"]["body"]
        );
        assert_eq!(result["pullRequest"]["title"], "Requirement in title");
        assert_eq!(result["reviews"].as_array().unwrap().len(), 3);
        for (review, original) in result["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .zip(original["observation"]["reviews"].as_array().unwrap())
        {
            for field in ["id", "state", "body", "submitted_at", "commit_id"] {
                assert_eq!(review[field], original[field]);
            }
            assert_eq!(review["user"]["id"], original["user"]["id"]);
            assert!(review.get("node_id").is_none());
        }
        assert_eq!(result["comments"], original["observation"]["comments"]);
        assert_eq!(result["hunks"].as_object().unwrap().len(), 2);
        assert_eq!(
            result["reviewComments"][0]["hunkReference"],
            result["reviewComments"][1]["hunkReference"]
        );
        assert_ne!(
            result["reviewComments"][0]["hunkReference"],
            result["reviewComments"][2]["hunkReference"]
        );
        for (comment, original) in result["reviewComments"].as_array().unwrap().iter().zip(
            original["observation"]["reviewComments"]
                .as_array()
                .unwrap(),
        ) {
            for field in [
                "id",
                "body",
                "path",
                "line",
                "original_line",
                "side",
                "in_reply_to_id",
                "commit_id",
                "original_commit_id",
                "pull_request_review_id",
            ] {
                assert_eq!(comment.get(field), original.get(field));
            }
            assert_eq!(
                result["hunks"][comment["hunkReference"].as_str().unwrap()],
                original["diff_hunk"]
            );
            assert_eq!(comment["user"]["id"], original["user"]["id"]);
            assert_eq!(comment["user"]["type"], original["user"]["type"]);
        }
        assert!(result.get("timeline").is_none());
        assert!(result.get("policy").is_none());
        assert!(!result.to_string().contains("profile"));
    }

    #[test]
    fn ado_projection_keeps_human_replies_pending_context_and_tombstones() {
        let original = ado();
        let result = project(&original);
        assert_eq!(result["platform"], "azureDevOps");
        assert_eq!(result["sourceCommit"], "a".repeat(40));
        assert_eq!(result["pullRequest"]["description"], "Full description");
        for (thread, original) in result["threads"]
            .as_array()
            .unwrap()
            .iter()
            .zip(original["observation"]["threads"].as_array().unwrap())
        {
            for field in [
                "id",
                "status",
                "isDeleted",
                "publishedDate",
                "threadContext",
                "pullRequestThreadContext",
            ] {
                assert_eq!(thread.get(field), original.get(field));
            }
        }
        let comments = &result["threads"][0]["comments"];
        assert_eq!(comments[0], json!({"id": 1, "commentType": "system"}));
        assert_eq!(comments[1]["content"], "Human objection remains.");
        assert_eq!(comments[1]["parentCommentId"], 1);
        assert_eq!(
            comments[1]["author"],
            json!({"id": "human", "isContainer": false})
        );
        assert_eq!(comments[1]["lastContentUpdatedDate"], "edited");
        assert_eq!(comments[2]["isDeleted"], true);
        assert_eq!(comments[2]["content"], Value::Null);
        assert!(result["threads"][1].get("comments").is_none());
        assert_eq!(result["threads"][0]["threadType"]["$value"], "VoteUpdate");
        assert!(!result.to_string().contains("authority-only"));
        assert!(!result.to_string().contains("profile"));
        assert!(result.get("policies").is_none());
        assert!(result.get("reviewers").is_none());
    }

    #[test]
    fn malformed_or_incomplete_discussion_never_becomes_empty_evidence() {
        let mut missing = github();
        missing["observation"]
            .as_object_mut()
            .unwrap()
            .remove("reviewComments");
        serde_json::from_value::<Evidence>(missing).unwrap_err();
        for (mut value, path, replacement) in [
            (github(), "/observation/comments/0/body", json!(null)),
            (github(), "/observation/reviews/0/body", json!(42)),
            (ado(), "/observation/threads/0/comments", json!(null)),
            (
                ado(),
                "/observation/threads/0/comments/1/content",
                json!(null),
            ),
        ] {
            *value.pointer_mut(path).unwrap() = replacement;
            let evidence: Evidence = serde_json::from_value(value).unwrap();
            render(&evidence).unwrap_err();
        }
        let mut invalid_hunk = github();
        invalid_hunk["observation"]["reviewComments"] =
            json!([{"id": 10, "body": "Objection", "diff_hunk": false}]);
        render(&serde_json::from_value(invalid_hunk).unwrap()).unwrap_err();
        for platform in ["gitHub", "azureDevOps"] {
            let evidence: Evidence = serde_json::from_value(json!({
                "platform": platform, "observation": {"scope": "state", "pullRequest": {}}
            }))
            .unwrap();
            render(&evidence).unwrap_err();
        }
    }

    #[test]
    fn repeated_hunks_reduce_total_document_volume_without_filtering_comments() {
        // Repeated excerpts dominate short comments so the assertion detects accidental expansion.
        let hunk = "@@ -1 +1 @@\n-old\n+new\n".repeat(512);
        let mut original = github();
        original["observation"]["reviewComments"] =
            json!((1..=64).map(|id| json!({
            "id": id, "body": format!("Objection {id}"), "path": format!("file-{id}.rs"),
            "diff_hunk": hunk, "line": id
        })).collect::<Vec<_>>());
        let evidence: Evidence = serde_json::from_value(original).unwrap();
        let compact = render(&evidence).unwrap();
        let result: Value = serde_json::from_str(&compact).unwrap();
        assert_eq!(result["reviewComments"].as_array().unwrap().len(), 64);
        assert_eq!(result["hunks"].as_object().unwrap().len(), 1);
        assert_eq!(
            result["hunks"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap(),
            &hunk
        );
        assert!(compact.len() * 10 < serde_json::to_string_pretty(&evidence).unwrap().len());
        assert_eq!(render(&evidence).unwrap(), compact);
    }
}
