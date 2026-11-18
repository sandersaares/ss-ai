//! Resolves capture targets and collects provider evidence without remote writes.

use ohno::AppError;
use serde_json::{Value, from_value, to_value};

use crate::ado_capture::collect as collect_ado;
use crate::github_capture::collect as collect_github;
use crate::target::{Platform, Target, escape};
use crate::transport::Transport;
use crate::{Clock, Http, Process};

/// Groups capture validation failures without changing application recovery.
#[ohno::error]
pub(crate) struct CaptureError;

// Keep the validation invariant and the runtime recovery category in the same
// source chain, matching the assessment package's classified-error pattern.
macro_rules! classified_capture_error {
    ($name:ident, $kind:literal, $message:literal, field) => {
        #[ohno::error]
        pub(crate) struct $name;

        impl $name {
            fn classified(field: &str) -> ohno::AppError {
                crate::capture::CaptureError::caused_by(Self::caused_by(
                    ss_ai_runtime::fail($kind, format!($message, field)).into_std_error(),
                ))
                .into()
            }
        }
    };
    ($name:ident, $kind:literal, $message:literal) => {
        #[ohno::error]
        pub(crate) struct $name;

        impl $name {
            fn classified() -> ohno::AppError {
                crate::capture::CaptureError::caused_by(Self::caused_by(
                    ss_ai_runtime::fail($kind, $message).into_std_error(),
                ))
                .into()
            }
        }
    };
}
pub(crate) use classified_capture_error;

classified_capture_error!(UnknownPlatform, "invalidInput", "Unknown capture platform");
classified_capture_error!(
    MissingPlatform,
    "invalidInput",
    "Capture requires a URL or platform"
);
classified_capture_error!(
    MissingOrganization,
    "invalidInput",
    "Capture requires an organization"
);
classified_capture_error!(
    InvalidPullRequestId,
    "invalidInput",
    "Capture requires a positive pull request identifier"
);
classified_capture_error!(
    MissingGitHubRepository,
    "invalidInput",
    "GitHub capture requires a repository"
);
classified_capture_error!(
    MissingReturnedTarget,
    "service",
    "Provider omitted capture target"
);
classified_capture_error!(
    ChangedReturnedTarget,
    "service",
    "Provider returned a different capture target"
);

/// Resolves named target inputs before authentication or provider access.
///
/// The JSON object accepts `url` or `platform`, `organization`, and
/// `pullRequestId` coordinates. `platform` accepts GitHub or `AzureDevOps`
/// case-insensitively. Optional `repository`, `project`, `collectionUrl`, and
/// `pullRequestId` fields override URL coordinates where applicable. The result
/// is a target object with canonical `gitHub` or `azureDevOps` platform names.
pub fn capture_target(input: &Value) -> Result<Value, AppError> {
    let text = |name| {
        input
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let platform = match text("platform") {
        Some(value) if value.eq_ignore_ascii_case("github") => Some(Platform::GitHub),
        Some(value) if value.eq_ignore_ascii_case("azuredevops") => Some(Platform::AzureDevOps),
        Some(_) => return Err(UnknownPlatform::classified()),
        None => None,
    };
    let mut target = if let Some(url) = text("url") {
        Target::pull_request(url, platform)?
    } else {
        let platform = platform.ok_or_else(MissingPlatform::classified)?;
        let organization = text("organization").ok_or_else(MissingOrganization::classified)?;
        let id = input
            .get("pullRequestId")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or_else(InvalidPullRequestId::classified)?;
        Target {
            platform,
            collection_url: (platform == Platform::AzureDevOps).then(|| {
                text("collectionUrl").map_or_else(
                    || format!("https://dev.azure.com/{}", escape(organization)),
                    str::to_owned,
                )
            }),
            organization: organization.to_owned(),
            project: text("project").map(str::to_owned),
            repository: text("repository").unwrap_or_default().to_owned(),
            pull_request_id: id,
            web_url: String::new(),
        }
    };
    if let Some(repository) = text("repository") {
        repository.clone_into(&mut target.repository);
    }
    if target.platform == Platform::AzureDevOps {
        if let Some(project) = text("project") {
            target.project = Some(project.to_owned());
        }
        if let Some(collection) = text("collectionUrl") {
            target.collection_url = Some(collection.trim_end_matches('/').to_owned());
        }
    } else {
        target.project = None;
        if target.repository.is_empty() {
            return Err(MissingGitHubRepository::classified());
        }
    }
    if let Some(id) = input.get("pullRequestId") {
        target.pull_request_id = id
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or_else(InvalidPullRequestId::classified)?;
    }
    target.refresh_url();
    Ok(to_value(target)?)
}

/// Reads complete provider evidence using the invocation's injected boundaries.
pub fn collect_capture(
    target: &Value,
    tenant: Option<&str>,
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let target: Target = from_value(target.clone())?;
    let mut transport = Transport::new(&target, tenant, process, http, clock);
    let context = match target.platform {
        Platform::GitHub => collect_github(&mut transport, &target),
        Platform::AzureDevOps => collect_ado(&mut transport, &target),
    }?;
    let returned: Target = from_value(
        context
            .get("target")
            .cloned()
            .ok_or_else(MissingReturnedTarget::classified)?,
    )?;
    if returned.platform != target.platform
        || returned.pull_request_id != target.pull_request_id
        || returned.collection_url != target.collection_url
    {
        return Err(ChangedReturnedTarget::classified());
    }
    Ok(context)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use serde_json::{Value, json};

    use crate::capture_target;

    #[test]
    fn platform_names_are_case_insensitive_and_serialized_canonically() {
        for (name, canonical) in [
            ("GitHub", "gitHub"),
            ("GITHUB", "gitHub"),
            ("gitHub", "gitHub"),
            ("AzureDevOps", "azureDevOps"),
            ("AZUREDEVOPS", "azureDevOps"),
            ("azureDevOps", "azureDevOps"),
        ] {
            let target = capture_target(&json!({
                "platform": name, "organization": "org", "repository": "repo",
                "pullRequestId": 17
            }))
            .unwrap();
            assert_eq!(
                target.get("platform").and_then(Value::as_str),
                Some(canonical)
            );
        }
        capture_target(&json!({"platform": "other"})).unwrap_err();
        let target = capture_target(&json!({
            "platform":"GITHUB", "url":"https://github.com/org/repo/pull/17"
        }))
        .unwrap();
        assert_eq!(
            target.get("platform").and_then(Value::as_str),
            Some("gitHub")
        );
        let target = capture_target(&json!({
            "platform":"AZUREDEVOPS",
            "url":"https://dev.azure.com/org/project/_git/repo/pullrequest/17"
        }))
        .unwrap();
        assert_eq!(
            target.get("platform").and_then(Value::as_str),
            Some("azureDevOps")
        );
    }

    #[test]
    fn capture_validation_exposes_distinct_leaves_and_runtime_classifications() {
        let platform = capture_target(&json!({"platform":"other"})).unwrap_err();
        let id = capture_target(&json!({
            "platform":"GitHub", "organization":"org", "repository":"repo", "pullRequestId":0
        }))
        .unwrap_err();
        assert!(platform.find_source::<super::CaptureError>().is_some());
        assert!(platform.find_source::<super::UnknownPlatform>().is_some());
        assert!(
            platform
                .find_source::<super::InvalidPullRequestId>()
                .is_none()
        );
        assert!(id.find_source::<super::InvalidPullRequestId>().is_some());
        assert_eq!(crate::classification(&platform), "invalidInput");
        assert_eq!(crate::classification(&id), "invalidInput");
    }

    #[test]
    fn resolves_azure_www_url_without_changing_the_organization() {
        let target = capture_target(&json!({
            "url": "https://www.dev.azure.com/org/project/_git/repo/pullrequest/17"
        }))
        .unwrap();
        assert_eq!(
            target.get("platform").and_then(|value| value.as_str()),
            Some("azureDevOps")
        );
        assert_eq!(
            target.get("organization").and_then(|value| value.as_str()),
            Some("org")
        );
        assert_eq!(
            target.get("collectionUrl").and_then(|value| value.as_str()),
            Some("https://www.dev.azure.com/org")
        );
    }
}
