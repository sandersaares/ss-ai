//! Exercises canonical document assembly independently of provider transport.
#![allow(
    clippy::indexing_slicing,
    reason = "The fixture and assertions use fixed JSON shapes"
)]

use serde_json::{Value, json};
use ss_ai_review_context::canonical_capture_from_evidence;

/// Reuses a complete provider capture as input evidence.
fn evidence() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/ss-review-native-capture/github-expected.json"
    ))
    .unwrap()
}

#[test]
fn assembly_orders_by_timestamp_and_numeric_identifier_and_keeps_structural_opener() {
    let mut context = evidence();
    let mut thread = context["threads"][0].clone();
    let mut opener = thread["comments"][0].clone();
    let mut reply = opener.clone();
    opener["id"] = json!("10");
    reply["id"] = json!("9");
    reply["author"] = context["authenticatedIdentity"].clone();
    thread["openingCommentId"] = json!("10");
    thread["comments"] = json!([opener, reply]);
    let mut later = thread.clone();
    later["id"] = json!("2");
    later["eventUtc"] = json!("2026-08-04T12:00:00.0000000Z");
    let mut undated = thread.clone();
    undated["id"] = json!("3");
    undated["eventUtc"] = Value::Null;
    thread["id"] = json!("1");
    context["threads"] = json!([undated, later, thread]);
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["pullRequest"]["isAuthoredBySelf"], false);
    assert_eq!(result["threads"][0]["id"], "1");
    assert_eq!(result["threads"][1]["id"], "2");
    assert_eq!(result["threads"][2]["id"], "3");
    assert_eq!(result["threads"][0]["comments"][0]["id"], "9");
    assert_eq!(result["threads"][0]["comments"][1]["id"], "10");
    assert_eq!(result["threads"][0]["isAuthoredBySelf"], false);
    assert_eq!(
        result["threads"][0]["comments"][0]["isAuthoredBySelf"],
        true
    );
    assert_eq!(
        result["threads"][0]["comments"][1]["isAuthoredBySelf"],
        false
    );

    context["threads"][2]["comments"][0]["publishedUtc"] = Value::Null;
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["threads"][0]["comments"][0]["id"], "9");
    assert_eq!(result["threads"][0]["comments"][1]["id"], "10");
    assert_eq!(result["threads"][0]["isAuthoredBySelf"], false);

    context["threads"][2]["comments"][0]["publishedUtc"] = json!("2026-08-03T11:00:00.0000000Z");
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["threads"][0]["comments"][0]["id"], "10");
    assert_eq!(result["threads"][0]["comments"][1]["id"], "9");
}

#[test]
fn assembly_attribution_uses_the_strongest_shared_account_identifier() {
    let mut context = evidence();
    context["pullRequest"]["author"] = context["authenticatedIdentity"].clone();
    context["threads"][0]["comments"][0]["author"] = context["authenticatedIdentity"].clone();
    context["threads"][0]["comments"][0]["author"]["descriptor"] = json!("u_RUNNER");
    context["threads"][0]["comments"][0]["author"]["id"] = json!("different");
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["pullRequest"]["isAuthoredBySelf"], true);
    assert_eq!(
        result["threads"][0]["comments"][0]["isAuthoredBySelf"],
        true
    );
    assert_eq!(result["threads"][0]["isAuthoredBySelf"], true);

    context["threads"][0]["comments"][0]["author"]["descriptor"] = json!("U_other");
    context["threads"][0]["comments"][0]["author"]["id"] = json!("701");
    context["pullRequest"]["author"] = Value::Null;
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["pullRequest"]["isAuthoredBySelf"], false);
    assert_eq!(
        result["threads"][0]["comments"][0]["isAuthoredBySelf"],
        false
    );
    assert_eq!(result["threads"][0]["isAuthoredBySelf"], false);

    context["threads"][0]["comments"][0]["author"]["descriptor"] = Value::Null;
    context["threads"][0]["comments"][0]["author"]["uniqueName"] = json!("renamed-login");
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(
        result["threads"][0]["comments"][0]["isAuthoredBySelf"],
        true
    );

    context["threads"][0]["comments"][0]["author"] = Value::Null;
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(
        result["threads"][0]["comments"][0]["isAuthoredBySelf"],
        false
    );
}

#[test]
fn bodyless_decision_threads_are_attributed_to_the_deciding_account() {
    let mut context = evidence();
    let decision = json!({
        "type":"refUpdate", "value":"commit001", "label":"force pushed",
        "by": context["authenticatedIdentity"], "utc":"2025-11-20T10:00:00Z",
        "authorship":"human"
    });
    context["threads"][0]["comments"] = json!([]);
    context["threads"][0]["openingCommentId"] = Value::Null;
    context["threads"][0]["kind"] = json!("refUpdate");
    context["threads"][0]["resolution"] = json!("indeterminate");
    context["threads"][0]["decision"] = decision;
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["threads"][0]["isAuthoredBySelf"], true);
    assert_eq!(result["threads"][0]["decision"]["authorship"], "human");
}

#[test]
fn assembly_derives_platform_repository_urls_with_escaped_segments() {
    let mut context = evidence();
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(result["repositoryUrl"], "https://github.com/octo/widget");
    context["target"]["platform"] = json!("azureDevOps");
    context["target"]["collectionUrl"] = json!("https://dev.azure.com/contoso");
    context["target"]["organization"] = json!("contoso");
    context["target"]["project"] = json!("Example Project");
    context["target"]["repository"] = json!("widgets");
    context["target"]["webUrl"] =
        json!("https://dev.azure.com/contoso/Example%20Project/_git/widgets/pullrequest/7");
    let result = canonical_capture_from_evidence(&context).unwrap();
    assert_eq!(
        result["repositoryUrl"],
        "https://dev.azure.com/contoso/Example%20Project/_git/widgets"
    );
}
