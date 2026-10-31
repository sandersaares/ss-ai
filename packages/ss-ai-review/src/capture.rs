//! Resolves capture targets and collects provider evidence without remote writes.

use ohno::AppError;
use serde_json::{Value, from_value, to_value};

use crate::ado_capture::collect as collect_ado;
use crate::error::fail;
use crate::github_capture::collect as collect_github;
use crate::target::{Platform, Target, escape};
use crate::transport::Transport;
use crate::{Clock, Http, Process};

/// Resolves named target inputs before authentication or provider access.
pub fn capture_target(input: &Value) -> Result<Value, AppError> {
    let text = |name| {
        input
            .get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let platform = match text("platform") {
        Some("gitHub") => Some(Platform::GitHub),
        Some("azureDevOps") => Some(Platform::AzureDevOps),
        Some(_) => return Err(fail("invalidInput", "Unknown capture platform")),
        None => None,
    };
    let mut target = if let Some(url) = text("url") {
        Target::pull_request(url, platform)?
    } else {
        let platform =
            platform.ok_or_else(|| fail("invalidInput", "Capture requires a URL or platform"))?;
        let organization = text("organization")
            .ok_or_else(|| fail("invalidInput", "Capture requires an organization"))?;
        let id = input
            .get("pullRequestId")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                fail(
                    "invalidInput",
                    "Capture requires a positive pull request identifier",
                )
            })?;
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
            return Err(fail("invalidInput", "GitHub capture requires a repository"));
        }
    }
    if let Some(id) = input.get("pullRequestId") {
        target.pull_request_id = id
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                fail(
                    "invalidInput",
                    "Capture requires a positive pull request identifier",
                )
            })?;
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
            .ok_or_else(|| fail("service", "Provider omitted capture target"))?,
    )?;
    if returned.platform != target.platform
        || returned.pull_request_id != target.pull_request_id
        || returned.collection_url != target.collection_url
    {
        return Err(fail(
            "service",
            "Provider returned a different capture target",
        ));
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::capture_target;

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
