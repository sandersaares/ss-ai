//! Covers target resolution before provider authentication or access.
#![allow(
    clippy::indexing_slicing,
    reason = "The tests inspect fixed JSON fixture shapes"
)]

use serde_json::json;
use ss_ai_review::capture_target;

#[test]
fn resolves_provider_urls_and_explicit_coordinates() {
    let azure = capture_target(&json!({
        "url":"https://dev.azure.com/example/Example%20Project/_git/example-repo/pullrequest/42"
    }))
    .unwrap();
    assert_eq!(azure["platform"], "azureDevOps");
    assert_eq!(azure["organization"], "example");
    assert_eq!(azure["project"], "Example Project");
    assert_eq!(azure["repository"], "example-repo");
    assert_eq!(azure["pullRequestId"], 42);
    assert_eq!(azure["collectionUrl"], "https://dev.azure.com/example");
    for (url, collection) in [
        (
            "https://contoso.visualstudio.com/Core/_git/widgets/pullrequest/7",
            "https://contoso.visualstudio.com",
        ),
        (
            "https://contoso.visualstudio.com/DefaultCollection/Core/_git/widgets/pullrequest/7",
            "https://contoso.visualstudio.com/DefaultCollection",
        ),
    ] {
        let target = capture_target(&json!({"url":url})).unwrap();
        assert_eq!(target["platform"], "azureDevOps");
        assert_eq!(target["organization"], "contoso");
        assert_eq!(target["project"], "Core");
        assert_eq!(target["collectionUrl"], collection);
    }
    let with_query = capture_target(&json!({
        "url":"https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7?_a=files&path=/src"
    }))
    .unwrap();
    assert_eq!(with_query["pullRequestId"], 7);
    assert_eq!(with_query["repository"], "widgets");
    let github =
        capture_target(&json!({"url":"https://github.com/example/widgets/pull/42"})).unwrap();
    assert_eq!(github["platform"], "gitHub");
    assert_eq!(github["organization"], "example");
    assert_eq!(github["repository"], "widgets");
    assert_eq!(github["pullRequestId"], 42);
    assert_eq!(
        github["webUrl"],
        "https://github.com/example/widgets/pull/42"
    );
    let explicit = capture_target(&json!({"platform":"azureDevOps","organization":"contoso",
        "project":"Core","repository":"widgets","pullRequestId":7}))
    .unwrap();
    assert_eq!(explicit["collectionUrl"], "https://dev.azure.com/contoso");
    assert_eq!(
        explicit["webUrl"],
        "https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7"
    );
}

#[test]
fn explicit_overrides_and_collection_normalization_rebuild_the_web_url() {
    let url = "https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7";
    let changed = capture_target(&json!({"url":url,"repository":"gadgets"})).unwrap();
    assert_eq!(changed["repository"], "gadgets");
    for (url, expected) in [
        (
            url,
            "https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/8",
        ),
        (
            "https://github.com/octo/widget/pull/7",
            "https://github.com/octo/widget/pull/8",
        ),
    ] {
        let target = capture_target(&json!({"url":url,"pullRequestId":8})).unwrap();
        assert_eq!(target["pullRequestId"], 8);
        assert_eq!(target["webUrl"], expected);
    }
    let arguments = json!({"platform":"azureDevOps","organization":"contoso",
        "project":"Core","repository":"widgets","pullRequestId":7,
        "collectionUrl":"https://dev.azure.com/contoso/"});
    let trailing = capture_target(&arguments).unwrap();
    let mut plain_arguments = arguments;
    plain_arguments["collectionUrl"] = json!("https://dev.azure.com/contoso");
    assert_eq!(trailing, capture_target(&plain_arguments).unwrap());
    assert_eq!(trailing["collectionUrl"], "https://dev.azure.com/contoso");
}

#[test]
fn rejects_unresolvable_targets_and_explicit_nonpositive_identifiers() {
    for id in [0, -3] {
        capture_target(&json!({"url":"https://github.com/octo/widget/pull/7","pullRequestId":id}))
            .unwrap_err();
    }
    assert_eq!(
        capture_target(&json!({"url":"https://github.com/octo/widget/pull/7"})).unwrap()["pullRequestId"],
        7
    );
    assert_eq!(
        capture_target(&json!({"url":"https://github.com/example/widgets/pull/1","verbose":true}))
            .unwrap(),
        capture_target(&json!({"url":"https://github.com/example/widgets/pull/1"})).unwrap()
    );
    for input in [
        json!({"url":"not a url"}),
        json!({"url":"https://example.invalid/a/b/pullrequest/1"}),
        json!({"url":"https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/abc"}),
        json!({"url":"https://github.com/example/widgets/issues/1"}),
        json!({"platform":"gitHub","organization":"example"}),
    ] {
        capture_target(&input).unwrap_err();
    }
}
