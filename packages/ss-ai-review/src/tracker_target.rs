//! Resolves tracker destinations without depending on the reviewed pull request.

use std::num::NonZeroU32;

use ohno::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_value, to_value};
use url::Url;

use crate::error::{caused_by, fail};
use crate::json::required_optional;
use crate::target::{Platform, decode, escape};

/// Inputs for choosing a destination; explicit coordinates override URL values.
#[derive(Clone, Copy, Default)]
pub(crate) struct TrackerTargetInput<'a> {
    pub(crate) url: Option<&'a str>,
    pub(crate) platform: Option<Platform>,
    pub(crate) organization: Option<&'a str>,
    pub(crate) project: Option<&'a str>,
    pub(crate) repository: Option<&'a str>,
    pub(crate) collection_url: Option<&'a str>,
}

/// A tracker identity and its browser link, independent of review evidence.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TrackerTarget {
    pub(crate) platform: Platform,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) collection_url: Option<String>,
    pub(crate) organization: String,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) project: Option<String>,
    #[serde(deserialize_with = "required_optional")]
    pub(crate) repository: Option<String>,
    pub(crate) web_url: String,
}

impl TrackerTarget {
    /// Resolves a browser URL or explicit coordinates without provider access.
    pub(crate) fn resolve(input: TrackerTargetInput<'_>) -> Result<Self, AppError> {
        let parsed = input
            .url
            .filter(|value| !value.is_empty())
            .map(parse_url)
            .transpose()?;
        let platform = match (input.platform, parsed.as_ref()) {
            (Some(platform), _) => platform,
            (None, Some(url)) => infer_platform(url)?,
            (None, None) => return Err(invalid("Supply a tracker URL or platform")),
        };
        let mut target = if let Some(url) = parsed.as_ref() {
            from_url(url, platform, &input)?
        } else {
            let organization = nonblank(input.organization)
                .ok_or_else(|| invalid("A tracker requires an organization"))?;
            Self {
                platform,
                collection_url: (platform == Platform::AzureDevOps)
                    .then(|| format!("https://dev.azure.com/{}", escape(organization))),
                organization: organization.to_owned(),
                project: None,
                repository: None,
                web_url: String::new(),
            }
        };
        if let Some(organization) = nonblank(input.organization) {
            coordinate(organization)?;
            if platform == Platform::AzureDevOps
                && nonblank(input.collection_url).is_none()
                && !target.organization.eq_ignore_ascii_case(organization)
            {
                target.collection_url = Some(retarget_azure_collection(
                    target.collection_url.as_deref().ok_or_else(|| {
                        invalid("An Azure DevOps tracker requires a collection URL")
                    })?,
                    organization,
                )?);
            }
            organization.clone_into(&mut target.organization);
        }
        match platform {
            Platform::GitHub => {
                if let Some(repository) = nonblank(input.repository) {
                    target.repository = Some(repository.to_owned());
                }
                if let Some(repository) = &mut target.repository
                    && repository.to_ascii_lowercase().ends_with(".git")
                {
                    // The suffix is ASCII, so removing its byte length keeps UTF-8 intact.
                    repository.truncate(repository.len().saturating_sub(".git".len()));
                }
            }
            Platform::AzureDevOps => {
                if let Some(project) = nonblank(input.project) {
                    target.project = Some(project.to_owned());
                }
                if let Some(collection) = nonblank(input.collection_url) {
                    target.collection_url = Some(collection.trim_end_matches('/').to_owned());
                }
            }
        }
        target.validate_identity()?;
        target.web_url = target.browser_url();
        Ok(target)
    }

    /// Rejects malformed persisted identities before they can route publication.
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        self.validate_identity()?;
        if self.web_url != self.browser_url() {
            return Err(invalid("Tracker browser URL does not match its identity"));
        }
        Ok(())
    }

    /// Compares the complete routing identity rather than the reviewed PR.
    pub(crate) fn matches(&self, other: &Self) -> Result<bool, AppError> {
        self.validate()?;
        other.validate()?;
        let equal = |left: Option<&str>, right: Option<&str>| match (left, right) {
            (Some(left), Some(right)) => left.to_lowercase() == right.to_lowercase(),
            (None, None) => true,
            _ => false,
        };
        Ok(self.platform == other.platform
            && equal(Some(&self.organization), Some(&other.organization))
            && equal(self.project.as_deref(), other.project.as_deref())
            && equal(self.repository.as_deref(), other.repository.as_deref())
            && equal(
                self.collection_url.as_deref(),
                other.collection_url.as_deref(),
            ))
    }

    /// Checks destination-specific metadata before a caller opens a transport.
    pub(crate) fn validate_arguments(&self, args: &TrackerArguments<'_>) -> Result<(), AppError> {
        self.validate()?;
        match self.platform {
            Platform::GitHub => {
                if nonblank(args.work_item_type).is_some()
                    || nonblank(args.area_path).is_some()
                    || nonblank(args.description_field).is_some()
                    || args.parent_work_item_id.is_some()
                {
                    return Err(invalid(
                        "Azure DevOps metadata cannot be used with a GitHub tracker",
                    ));
                }
            }
            Platform::AzureDevOps => {
                nonblank(args.work_item_type)
                    .ok_or_else(|| invalid("An Azure DevOps tracker requires a work item type"))?;
                let area = nonblank(args.area_path)
                    .ok_or_else(|| invalid("An Azure DevOps tracker requires an area path"))?;
                let root = area
                    .split('\\')
                    .find(|segment| !segment.is_empty())
                    .filter(|root| !root.trim().is_empty())
                    .ok_or_else(|| invalid("Area path must name the project"))?;
                if root.to_lowercase() != self.project.as_deref().unwrap_or_default().to_lowercase()
                {
                    return Err(invalid("Area path must be rooted at the tracker project"));
                }
            }
        }
        Ok(())
    }

    /// Ensures identity fields can name one tracker without path ambiguity.
    fn validate_identity(&self) -> Result<(), AppError> {
        coordinate(&self.organization)?;
        match self.platform {
            Platform::GitHub => {
                if self.project.is_some() || self.collection_url.is_some() {
                    return Err(invalid("GitHub tracker has Azure DevOps coordinates"));
                }
                coordinate(
                    self.repository
                        .as_deref()
                        .ok_or_else(|| invalid("A GitHub tracker requires a repository"))?,
                )?;
            }
            Platform::AzureDevOps => {
                if self.repository.is_some() {
                    return Err(invalid("Azure DevOps tracker has a repository coordinate"));
                }
                coordinate(
                    self.project
                        .as_deref()
                        .ok_or_else(|| invalid("An Azure DevOps tracker requires a project"))?,
                )?;
                let collection = self
                    .collection_url
                    .as_deref()
                    .ok_or_else(|| invalid("An Azure DevOps tracker requires a collection URL"))?;
                let url = parse_url(collection)?;
                if url.query().is_some() || url.fragment().is_some() || collection.ends_with('/') {
                    return Err(invalid("Invalid Azure DevOps collection URL"));
                }
            }
        }
        Ok(())
    }

    /// Constructs links only from already checked coordinates.
    fn browser_url(&self) -> String {
        match self.platform {
            Platform::GitHub => format!(
                "https://github.com/{}/{}/issues",
                escape(&self.organization),
                escape(self.repository.as_deref().unwrap_or_default())
            ),
            Platform::AzureDevOps => format!(
                "{}/{}/_workitems",
                self.collection_url.as_deref().unwrap_or_default(),
                escape(self.project.as_deref().unwrap_or_default())
            ),
        }
    }
}

/// Platform-specific publication metadata checked before any provider access.
#[derive(Default)]
pub(crate) struct TrackerArguments<'a> {
    pub(crate) work_item_type: Option<&'a str>,
    pub(crate) area_path: Option<&'a str>,
    pub(crate) description_field: Option<&'a str>,
    pub(crate) parent_work_item_id: Option<NonZeroU32>,
}

/// Resolves JSON destination coordinates into a tracker journal target.
///
/// The object accepts `url`, `platform`, `organization`, `project`,
/// `repository`, and `collectionUrl`. Platform names are `gitHub` and
/// `azureDevOps`, matched case-insensitively. Optional fields may be omitted
/// or null.
pub fn resolve_tracker_target(input: &Value) -> Result<Value, AppError> {
    let input: TrackerTargetRecord = from_value(input.clone())
        .map_err(|error| caused_by("invalidInput", "Invalid tracker destination", error))?;
    let platform = input.platform.as_deref().map(parse_platform).transpose()?;
    let target = TrackerTarget::resolve(TrackerTargetInput {
        url: input.url.as_deref(),
        platform,
        organization: input.organization.as_deref(),
        project: input.project.as_deref(),
        repository: input.repository.as_deref(),
        collection_url: input.collection_url.as_deref(),
    })?;
    to_value(target).map_err(|error| {
        caused_by(
            "invalidInput",
            "Cannot serialize tracker destination",
            error,
        )
        .into()
    })
}

/// Validates a tracker journal target and its platform-specific JSON metadata.
///
/// Metadata accepts `workItemType`, `areaPath`, `descriptionField`, and
/// `parentWorkItemId`. A supplied parent identifier must be a positive integer.
pub fn validate_tracker_arguments(target: &Value, metadata: &Value) -> Result<(), AppError> {
    let target: TrackerTarget = from_value(target.clone())
        .map_err(|error| caused_by("invalidInput", "Invalid tracker target", error))?;
    let metadata: TrackerMetadataRecord = from_value(metadata.clone())
        .map_err(|error| caused_by("invalidInput", "Invalid tracker metadata", error))?;
    target.validate_arguments(&TrackerArguments {
        work_item_type: metadata.work_item_type.as_deref(),
        area_path: metadata.area_path.as_deref(),
        description_field: metadata.description_field.as_deref(),
        parent_work_item_id: metadata.parent_work_item_id,
    })
}

/// Checks JSON shape before the resolver receives borrowed coordinates.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrackerTargetRecord {
    url: Option<String>,
    platform: Option<String>,
    organization: Option<String>,
    project: Option<String>,
    repository: Option<String>,
    collection_url: Option<String>,
}

/// Rejects malformed metadata rather than coercing it into absent values.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrackerMetadataRecord {
    work_item_type: Option<String>,
    area_path: Option<String>,
    description_field: Option<String>,
    parent_work_item_id: Option<NonZeroU32>,
}

/// Retargets known Azure host formats; custom hosts need an explicit collection.
fn retarget_azure_collection(collection: &str, organization: &str) -> Result<String, AppError> {
    let mut url = parse_url(collection)?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("Collection URL requires a host"))?;
    if host.eq_ignore_ascii_case("dev.azure.com") || host.eq_ignore_ascii_case("www.dev.azure.com")
    {
        let segments = url
            .path_segments()
            .ok_or_else(|| invalid("Collection URL requires an organization"))?;
        let mut collection = format!(
            "{}/{}",
            url.origin().ascii_serialization(),
            escape(organization)
        );
        // The first path segment is the organization; remaining segments identify the collection.
        for segment in segments.filter(|segment| !segment.is_empty()).skip(1) {
            collection.push('/');
            collection.push_str(segment);
        }
        Ok(collection)
    } else if host.ends_with(".visualstudio.com") {
        let host = format!("{organization}.visualstudio.com");
        url.set_host(Some(&host))
            .map_err(|error| caused_by("invalidInput", "Invalid Azure organization", error))?;
        Ok(url.as_str().trim_end_matches('/').to_owned())
    } else {
        // A custom host does not reveal how its organization maps to its URL.
        Err(invalid(
            "Supply a collection URL when overriding the organization on a custom Azure host",
        ))
    }
}

/// Parses the tracker independently from any PR target.
fn from_url(
    url: &Url,
    platform: Platform,
    input: &TrackerTargetInput<'_>,
) -> Result<TrackerTarget, AppError> {
    let segments: Vec<&str> = url
        .path_segments()
        .ok_or_else(|| invalid("URL requires a path"))?
        .filter(|segment| !segment.is_empty())
        .collect();
    match platform {
        Platform::GitHub => {
            let organization = segments
                .first()
                .ok_or_else(|| invalid("URL names no GitHub owner"))?;
            let repository = segments
                .get(1)
                .map(|value| decode(value))
                .transpose()?
                .or_else(|| nonblank(input.repository).map(str::to_owned));
            Ok(TrackerTarget {
                platform,
                collection_url: None,
                organization: decode(organization)?,
                project: None,
                repository,
                web_url: String::new(),
            })
        }
        Platform::AzureDevOps => {
            let host = url
                .host_str()
                .ok_or_else(|| invalid("URL requires a host"))?;
            let dev_azure = host.eq_ignore_ascii_case("dev.azure.com")
                || host.eq_ignore_ascii_case("www.dev.azure.com");
            // Azure service areas start with an underscore; the project precedes
            // that area or ends the URL. Modern hosts also put the organization
            // in the path, while per-organization hosts put it in the hostname.
            // Ref: skills/ss-review/scripts/lib/TrackerTarget.ps1.
            let boundary = segments
                .iter()
                .position(|segment| segment.starts_with('_'))
                .unwrap_or(segments.len());
            let minimum = usize::from(dev_azure);
            if boundary < minimum {
                return Err(invalid("URL names no Azure DevOps organization"));
            }
            let organization = if dev_azure {
                decode(
                    segments
                        .first()
                        .ok_or_else(|| invalid("URL names no Azure DevOps organization"))?,
                )?
            } else {
                host.split('.').next().unwrap_or_default().to_owned()
            };
            let names_project = boundary > minimum;
            let project = if names_project {
                let name = boundary
                    .checked_sub(1)
                    .and_then(|index| segments.get(index))
                    .ok_or_else(|| invalid("URL names no Azure DevOps project"))?;
                Some(decode(name)?)
            } else {
                nonblank(input.project).map(str::to_owned)
            };
            let depth = boundary.saturating_sub(usize::from(names_project));
            let mut collection = url.origin().ascii_serialization();
            for segment in segments.iter().take(depth) {
                collection.push('/');
                collection.push_str(segment);
            }
            Ok(TrackerTarget {
                platform,
                collection_url: Some(collection),
                organization,
                project,
                repository: None,
                web_url: String::new(),
            })
        }
    }
}

/// Accepts only network browser URLs without embedded credentials.
fn parse_url(value: &str) -> Result<Url, AppError> {
    let url = Url::parse(value).map_err(|error| {
        caused_by(
            "invalidInput",
            "Supply an absolute tracker URL".to_owned(),
            error,
        )
    })?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid("Supply an HTTP(S) URL without credentials"));
    }
    Ok(url)
}

/// Recognizes public service hosts; custom hosts require an explicit platform.
fn infer_platform(url: &Url) -> Result<Platform, AppError> {
    match url.host_str() {
        Some("github.com" | "www.github.com") => Ok(Platform::GitHub),
        Some(host)
            if host == "dev.azure.com"
                || host == "www.dev.azure.com"
                || host.ends_with(".visualstudio.com") =>
        {
            Ok(Platform::AzureDevOps)
        }
        _ => Err(invalid("Cannot determine tracker platform from URL host")),
    }
}

/// Matches the platform argument independently of its spelling's letter case.
fn parse_platform(value: &str) -> Result<Platform, AppError> {
    if value.eq_ignore_ascii_case("github") {
        Ok(Platform::GitHub)
    } else if value.eq_ignore_ascii_case("azuredevops") {
        Ok(Platform::AzureDevOps)
    } else {
        Err(invalid("Unknown tracker platform"))
    }
}

/// Rejects empty and path-shaped identities that cannot name a single segment.
fn coordinate(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty()
        || value == "."
        || value == ".."
        || value
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\')
    {
        return Err(invalid("Invalid tracker coordinate"));
    }
    Ok(())
}

/// Treats unbound and whitespace-only optional inputs as absent.
fn nonblank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// Uses the application's ordinary local input error classification.
fn invalid(message: &str) -> AppError {
    fail("invalidInput", message)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::num::NonZeroU32;

    use serde_json::json;

    use crate::target::Platform;
    use crate::tracker_target::{TrackerArguments, TrackerTarget, TrackerTargetInput};
    use crate::{resolve_tracker_target, validate_tracker_arguments};

    fn resolve_url(url: &str) -> TrackerTarget {
        TrackerTarget::resolve(TrackerTargetInput {
            url: Some(url),
            ..Default::default()
        })
        .unwrap()
    }

    fn github() -> TrackerTarget {
        resolve_url("https://github.com/octo/widget")
    }

    fn azure() -> TrackerTarget {
        resolve_url("https://dev.azure.com/contoso/Example%20Project")
    }

    #[test]
    fn github_browser_urls_and_clone_names_select_repository() {
        for url in [
            "https://github.com/octo/widget",
            "https://github.com/octo/widget/pull/7?x=1#discussion",
            "https://github.com/octo/widget/issues",
            "https://github.com/octo/widget/blob/main/file",
            "https://github.com/octo/widget.GIT",
        ] {
            let target = resolve_url(url);
            assert_eq!(target.organization, "octo");
            assert_eq!(target.repository.as_deref(), Some("widget"));
            assert_eq!(target.web_url, "https://github.com/octo/widget/issues");
            assert!(target.matches(&github()).unwrap());
        }
        let escaped = resolve_url("https://github.com/octo/widget%2Ecore");
        assert_eq!(escaped.repository.as_deref(), Some("widget.core"));
    }

    #[test]
    fn azure_service_areas_and_collection_select_project() {
        for suffix in [
            "",
            "/_git/repo/pullrequest/42",
            "/_workitems/edit/12",
            "/_boards/board/team",
            "/_other/stuff",
        ] {
            let target = resolve_url(&format!(
                "https://dev.azure.com/contoso/Example%20Project{suffix}"
            ));
            assert!(target.matches(&azure()).unwrap());
            assert_eq!(
                target.collection_url.as_deref(),
                Some("https://dev.azure.com/contoso")
            );
            assert_eq!(target.project.as_deref(), Some("Example Project"));
            assert_eq!(
                target.web_url,
                "https://dev.azure.com/contoso/Example%20Project/_workitems"
            );
        }
        let legacy = resolve_url(
            "https://example.visualstudio.com/DefaultCollection/Example%20Project/_git/repo",
        );
        assert_eq!(legacy.organization, "example");
        assert_eq!(
            legacy.collection_url.as_deref(),
            Some("https://example.visualstudio.com/DefaultCollection")
        );
        assert_eq!(legacy.project.as_deref(), Some("Example Project"));
        assert_eq!(
            resolve_url("https://example.visualstudio.com/Project")
                .collection_url
                .as_deref(),
            Some("https://example.visualstudio.com")
        );
    }

    #[test]
    fn explicit_coordinates_override_and_complete_urls() {
        let github = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://github.com/old"),
            organization: Some("new"),
            repository: Some("repo.git"),
            project: Some("ignored"),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(github.web_url, "https://github.com/new/repo/issues");
        let azure = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://dev.azure.com/old/_git/repo"),
            project: Some("New Project"),
            collection_url: Some("https://example.visualstudio.com/Collection/"),
            repository: Some("ignored"),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(azure.organization, "old");
        assert_eq!(
            azure.web_url,
            "https://example.visualstudio.com/Collection/New%20Project/_workitems"
        );
        assert_eq!(azure.repository, None);
        let custom = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://custom.example/collection/project/_workitems"),
            platform: Some(Platform::AzureDevOps),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            custom.collection_url.as_deref(),
            Some("https://custom.example/collection")
        );
    }

    #[test]
    fn azure_organization_override_routes_to_the_requested_organization() {
        for (url, expected) in [
            (
                "https://dev.azure.com/old/project/_workitems",
                "https://dev.azure.com/new/project/_workitems",
            ),
            (
                "https://www.dev.azure.com/old/collection/project/_workitems",
                "https://www.dev.azure.com/new/collection/project/_workitems",
            ),
            (
                "https://old.visualstudio.com/Collection/project/_workitems",
                "https://new.visualstudio.com/Collection/project/_workitems",
            ),
        ] {
            let target = TrackerTarget::resolve(TrackerTargetInput {
                url: Some(url),
                organization: Some("new"),
                ..Default::default()
            })
            .unwrap();
            assert_eq!(target.organization, "new");
            assert_eq!(target.web_url, expected);
        }
        let target = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://dev.azure.com/old/project"),
            organization: Some("new org"),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            target.collection_url.as_deref(),
            Some("https://dev.azure.com/new%20org")
        );
        let target = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://dev.azure.com/old/project"),
            organization: Some("new"),
            collection_url: Some("https://example.visualstudio.com/Collection/"),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            target.collection_url.as_deref(),
            Some("https://example.visualstudio.com/Collection")
        );
        TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://custom.example/collection/project"),
            platform: Some(Platform::AzureDevOps),
            organization: Some("new"),
            ..Default::default()
        })
        .unwrap_err();
        TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://custom.example/collection/project"),
            platform: Some(Platform::AzureDevOps),
            organization: Some("new"),
            collection_url: Some("https://new.example/collection"),
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn platform_specific_coordinates_do_not_change_other_tracker_types() {
        let github = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://github.com/octo/widget"),
            project: Some("ignored"),
            collection_url: Some("https://ignored.example/collection"),
            ..Default::default()
        })
        .unwrap();
        assert!(github.matches(&self::github()).unwrap());
        let azure = TrackerTarget::resolve(TrackerTargetInput {
            url: Some("https://dev.azure.com/contoso/Example%20Project"),
            repository: Some("ignored"),
            ..Default::default()
        })
        .unwrap();
        assert!(azure.matches(&self::azure()).unwrap());
    }

    #[test]
    fn composition_api_preserves_tracker_journal_shape() {
        let target = resolve_tracker_target(&json!({
            "url": "https://github.com/octo/widget/pull/42"
        }))
        .unwrap();
        assert_eq!(
            target,
            json!({
                "platform": "gitHub",
                "collectionUrl": null,
                "organization": "octo",
                "project": null,
                "repository": "widget",
                "webUrl": "https://github.com/octo/widget/issues"
            })
        );
        validate_tracker_arguments(&target, &json!({})).unwrap();
        let target: TrackerTarget = serde_json::from_value(target).unwrap();
        assert!(target.matches(&github()).unwrap());
    }

    #[test]
    fn composition_api_rejects_malformed_values_and_invalid_parent_ids() {
        for input in [
            json!(null),
            json!({"platform": "gitlab", "organization": "octo", "repository": "widget"}),
            json!({"platform": 1, "organization": "octo", "repository": "widget"}),
            json!({"platform": "gitHub", "organization": 9, "repository": "widget"}),
            json!({"platform": "gitHub", "organization": "octo", "repository": "widget", "extra": true}),
        ] {
            resolve_tracker_target(&input).unwrap_err();
        }
        let target = resolve_tracker_target(&json!({
            "platform": "azureDevOps", "organization": "contoso", "project": "Example Project"
        }))
        .unwrap();
        for parent in [json!(0), json!(-1), json!("1"), json!(1.5), json!(u64::MAX)] {
            validate_tracker_arguments(
                &target,
                &json!({"workItemType": "Bug", "areaPath": "Example Project", "parentWorkItemId": parent})
            )
            .unwrap_err();
        }
        validate_tracker_arguments(
            &target,
            &json!({"workItemType": "Bug", "areaPath": "Example Project", "parentWorkItemId": 1}),
        )
        .unwrap();
        for metadata in [
            json!(null),
            json!({"workItemType": 1, "areaPath": "Example Project"}),
            json!({"workItemType": "Bug", "areaPath": "Example Project", "extra": 1}),
        ] {
            validate_tracker_arguments(&target, &metadata).unwrap_err();
        }
        validate_tracker_arguments(
            &json!({"platform": "gitHub", "organization": "octo", "repository": "widget"}),
            &json!({}),
        )
        .unwrap_err();
    }

    #[test]
    fn composition_api_accepts_platform_names_case_insensitively() {
        for platform in ["gitHub", "GitHub", "github", "GITHUB"] {
            let target = resolve_tracker_target(&json!({
                "platform": platform, "organization": "octo", "repository": "widget"
            }))
            .unwrap();
            assert_eq!(target.get("platform").unwrap(), "gitHub");
        }
        for platform in ["azureDevOps", "AzureDevOps", "azuredevops", "AZUREDEVOPS"] {
            let target = resolve_tracker_target(&json!({
                "platform": platform, "organization": "contoso", "project": "Example Project"
            }))
            .unwrap();
            assert_eq!(target.get("platform").unwrap(), "azureDevOps");
        }
    }

    #[test]
    fn explicit_destinations_do_not_require_reviewed_pr() {
        let github = TrackerTarget::resolve(TrackerTargetInput {
            platform: Some(Platform::GitHub),
            organization: Some("octo"),
            repository: Some("widget.git"),
            ..Default::default()
        })
        .unwrap();
        assert!(github.matches(&self::github()).unwrap());
        let azure = TrackerTarget::resolve(TrackerTargetInput {
            platform: Some(Platform::AzureDevOps),
            organization: Some("contoso"),
            project: Some("Example Project"),
            ..Default::default()
        })
        .unwrap();
        assert!(azure.matches(&self::azure()).unwrap());
    }

    #[test]
    fn malformed_and_incomplete_inputs_fail_locally() {
        for url in [
            "octo/widget",
            "ftp://github.com/octo/widget",
            "https://user@github.com/octo/widget",
            "https://unknown.example/org/project",
            "https://github.com/octo",
            "https://dev.azure.com/contoso",
            "https://dev.azure.com/_git/repo",
            "https://dev.azure.com/contoso/_git/repo",
            "https://github.com/octo/%ZZ",
            "https://github.com/octo/%2F",
        ] {
            TrackerTarget::resolve(TrackerTargetInput {
                url: Some(url),
                ..Default::default()
            })
            .expect_err(url);
        }
        for input in [
            TrackerTargetInput::default(),
            TrackerTargetInput {
                platform: Some(Platform::GitHub),
                repository: Some("repo"),
                ..Default::default()
            },
            TrackerTargetInput {
                platform: Some(Platform::GitHub),
                organization: Some("octo"),
                ..Default::default()
            },
            TrackerTargetInput {
                platform: Some(Platform::GitHub),
                organization: Some("octo"),
                repository: Some(".git"),
                ..Default::default()
            },
            TrackerTargetInput {
                platform: Some(Platform::AzureDevOps),
                organization: Some("org"),
                ..Default::default()
            },
            TrackerTargetInput {
                platform: Some(Platform::AzureDevOps),
                organization: Some("org"),
                project: Some("project"),
                collection_url: Some("https://example.com/collection?x=1"),
                ..Default::default()
            },
        ] {
            TrackerTarget::resolve(input).unwrap_err();
        }
    }

    #[test]
    fn publication_arguments_are_checked_for_the_destination() {
        github()
            .validate_arguments(&TrackerArguments::default())
            .unwrap();
        for args in [
            TrackerArguments {
                work_item_type: Some("Bug"),
                ..Default::default()
            },
            TrackerArguments {
                area_path: Some("Project"),
                ..Default::default()
            },
            TrackerArguments {
                description_field: Some("System.Description"),
                ..Default::default()
            },
            TrackerArguments {
                parent_work_item_id: NonZeroU32::new(1),
                ..Default::default()
            },
        ] {
            github().validate_arguments(&args).unwrap_err();
        }
        for (kind, area, expected) in [
            (None, Some("Example Project"), false),
            (Some("Bug"), None, false),
            (Some("Bug"), Some("\\\\"), false),
            (Some("Bug"), Some("Other\\Area"), false),
            (Some("Bug"), Some("example project"), true),
            (Some("Bug"), Some("EXAMPLE PROJECT\\Area"), true),
        ] {
            let args = TrackerArguments {
                work_item_type: kind,
                area_path: area,
                description_field: Some("System.Description"),
                parent_work_item_id: NonZeroU32::new(7),
            };
            if expected {
                azure().validate_arguments(&args).unwrap();
            } else {
                azure().validate_arguments(&args).unwrap_err();
            }
        }
    }

    #[test]
    fn persisted_identity_is_checked_before_comparison_or_url_generation() {
        let original = azure();
        let encoded = serde_json::to_value(&original).unwrap();
        let restored: TrackerTarget = serde_json::from_value(encoded.clone()).unwrap();
        assert!(original.matches(&restored).unwrap());
        let mut malformed = encoded;
        *malformed.get_mut("webUrl").unwrap() = "https://example.com/wrong".into();
        let malformed: TrackerTarget = serde_json::from_value(malformed).unwrap();
        malformed.validate().unwrap_err();
        original.matches(&malformed).unwrap_err();
        assert!(!github().matches(&original).unwrap());
        let case = resolve_url("https://github.com/OCTO/WIDGET");
        assert!(case.matches(&github()).unwrap());
        let different = resolve_url("https://github.com/octo/other");
        assert!(!different.matches(&github()).unwrap());
    }
}
