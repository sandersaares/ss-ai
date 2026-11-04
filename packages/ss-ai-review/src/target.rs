//! Keeps provider routing independent of checkout state and browser URL syntax.

use std::fmt::Write;
use std::num::NonZeroU32;

use ohno::AppError;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::fail;
use crate::json::required_optional;

/// Provider routing remains explicit in every persisted target and identity.
///
/// Wire names match the command and schema-2 contracts; they are not hostnames.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Platform {
    /// Uses GitHub account, repository, and pull-request APIs.
    #[serde(rename = "gitHub")]
    GitHub,
    /// Uses Azure collection/project routing and reviewer identities.
    #[serde(rename = "azureDevOps")]
    AzureDevOps,
}

/// Coordinates are independent of a skill's or caller's current directory.
///
/// Repository scope uses a zero PR identifier; candidates and persisted claim
/// targets use positive identifiers. Registration may temporarily omit Azure
/// project/repository coordinates until the adapter resolves them.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Target {
    /// Chooses the adapter and the meaning of the remaining coordinates.
    pub(crate) platform: Platform,
    /// Azure collection root, or explicit null for GitHub wire records.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) collection_url: Option<String>,
    /// GitHub owner or Azure organization, used for routing rather than identity.
    pub(crate) organization: String,
    /// Azure project name; null for GitHub or before identifier-only resolution.
    #[serde(deserialize_with = "required_optional")]
    pub(crate) project: Option<String>,
    /// Repository name or service identifier accepted by the provider route.
    pub(crate) repository: String,
    /// Positive for a PR; zero denotes repository-wide enumeration scope.
    pub(crate) pull_request_id: u32,
    /// Browser link returned to the skill, distinct from the API route.
    pub(crate) web_url: String,
}

impl Target {
    /// Restricts selection to a supported repository browser URL.
    ///
    /// The resulting target represents enumeration scope, not a concrete PR.
    /// Retain its browser URL unchanged for exact journal ownership checks.
    pub(crate) fn repository(url: &str) -> Result<Self, AppError> {
        let parsed = parse_url(url)?;
        if parsed.scheme() != "https" || parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(fail(
                "invalidInput",
                "Supply an HTTPS repository browser URL without query or fragment",
            ));
        }
        let parts = segments(&parsed)?;
        let platform = platform(&parsed)?;
        if (platform == Platform::GitHub && parts.len() != 2)
            || (platform == Platform::AzureDevOps
                && parts
                    .iter()
                    .rev()
                    .nth(1)
                    .is_none_or(|part| !part.eq_ignore_ascii_case("_git")))
        {
            return Err(fail(
                "invalidInput",
                "Supply a repository browser URL, not a pull request or branch URL",
            ));
        }
        let suffix = match platform {
            Platform::GitHub => "pull/1",
            Platform::AzureDevOps => "pullrequest/1",
        };
        let mut target =
            Self::pull_request(&format!("{}/{suffix}", url.trim_end_matches('/')), None)?;
        target.pull_request_id = 0;
        url.trim_end_matches('/').clone_into(&mut target.web_url);
        Ok(target)
    }

    /// Parses provider coordinates and allows an explicitly selected platform.
    ///
    /// Registration accepts links with browser query/fragment context; routing
    /// derives from path coordinates, not those decorations. An explicit platform
    /// allows custom Azure hosts that hostname inference cannot recognize.
    pub(crate) fn pull_request(url: &str, selected: Option<Platform>) -> Result<Self, AppError> {
        let parsed = parse_url(url)?;
        let platform = selected.map_or_else(|| platform(&parsed), Ok)?;
        let parts = segments(&parsed)?;
        let part = |index| -> Result<&str, AppError> {
            parts
                .get(index)
                .map(String::as_str)
                .ok_or_else(|| fail("invalidInput", "Incomplete pull request URL"))
        };
        let mut target = match platform {
            Platform::GitHub => {
                if !part(2)?.eq_ignore_ascii_case("pull") {
                    return Err(fail("invalidInput", "Not a GitHub pull request URL"));
                }
                Self {
                    platform,
                    collection_url: None,
                    organization: decode(part(0)?)?,
                    project: None,
                    repository: decode(part(1)?)?,
                    pull_request_id: positive_id(part(3)?)?.get(),
                    web_url: String::new(),
                }
            }
            Platform::AzureDevOps => {
                let index = parts
                    .iter()
                    .position(|part| part.eq_ignore_ascii_case("_git"))
                    .ok_or_else(|| fail("invalidInput", "Not an Azure DevOps pull request URL"))?;
                let project_index = index
                    .checked_sub(1)
                    .ok_or_else(|| fail("invalidInput", "Missing Azure DevOps project"))?;
                let repository_index = index
                    .checked_add(1)
                    .ok_or_else(|| fail("invalidInput", "Invalid URL"))?;
                let operation_index = index
                    .checked_add(2)
                    .ok_or_else(|| fail("invalidInput", "Invalid URL"))?;
                let id_index = index
                    .checked_add(3)
                    .ok_or_else(|| fail("invalidInput", "Invalid URL"))?;
                if !part(operation_index)?.eq_ignore_ascii_case("pullrequest") {
                    return Err(fail("invalidInput", "Not an Azure DevOps pull request URL"));
                }
                let host = parsed
                    .host_str()
                    .ok_or_else(|| fail("invalidInput", "URL requires a host"))?;
                let organization = if host.eq_ignore_ascii_case("dev.azure.com")
                    || host.eq_ignore_ascii_case("www.dev.azure.com")
                {
                    if index < 2 {
                        return Err(fail("invalidInput", "Missing Azure DevOps organization"));
                    }
                    decode(part(0)?)?
                } else {
                    host.split('.').next().unwrap_or_default().to_owned()
                };
                let mut collection_url = parsed.origin().ascii_serialization();
                for part in parts.iter().take(project_index) {
                    collection_url.push('/');
                    collection_url.push_str(part);
                }
                Self {
                    platform,
                    collection_url: Some(collection_url),
                    organization,
                    project: Some(decode(part(project_index)?)?),
                    repository: decode(part(repository_index)?)?,
                    pull_request_id: positive_id(part(id_index)?)?.get(),
                    web_url: String::new(),
                }
            }
        };
        target.refresh_url();
        Ok(target)
    }

    /// Each candidate gets its own coordinates; enumeration scope never changes.
    pub(crate) fn candidate(&self, id: NonZeroU32) -> Self {
        let mut target = self.clone();
        target.pull_request_id = id.get();
        target.refresh_url();
        target
    }

    /// Keeps returned URLs synchronized with the coordinates actually used.
    pub(crate) fn refresh_url(&mut self) {
        self.web_url = match self.platform {
            Platform::GitHub => format!(
                "https://github.com/{}/{}/pull/{}",
                escape(&self.organization),
                escape(&self.repository),
                self.pull_request_id
            ),
            Platform::AzureDevOps => {
                let collection = self
                    .collection_url
                    .as_deref()
                    .unwrap_or_default()
                    .trim_end_matches('/');
                self.project
                    .as_deref()
                    .filter(|project| !project.is_empty())
                    .filter(|_| !self.repository.is_empty())
                    .map_or_else(
                        || format!("{collection}/_git/pullrequest/{}", self.pull_request_id),
                        |project| {
                            format!(
                                "{collection}/{}/_git/{}/pullrequest/{}",
                                escape(project),
                                escape(&self.repository),
                                self.pull_request_id
                            )
                        },
                    )
            }
        };
    }

    /// Supplies escaped API routes rather than treating browser URLs as endpoints.
    pub(crate) fn repository_api(&self) -> String {
        match self.platform {
            Platform::GitHub => format!(
                "repos/{}/{}",
                escape(&self.organization),
                escape(&self.repository)
            ),
            Platform::AzureDevOps => format!(
                "{}/_apis/git/repositories/{}",
                escape(self.project.as_deref().unwrap_or_default()),
                escape(&self.repository)
            ),
        }
    }

    /// Extends repository routing for a concrete PR without using its browser URL.
    pub(crate) fn pr_api(&self) -> String {
        let segment = match self.platform {
            Platform::GitHub => "pulls",
            Platform::AzureDevOps => "pullRequests",
        };
        format!(
            "{}/{segment}/{}",
            self.repository_api(),
            self.pull_request_id
        )
    }
}

/// Registration accepts browser query/fragment links and explicit on-premises hosts.
fn parse_url(url: &str) -> Result<Url, AppError> {
    let parsed = Url::parse(url).map_err(|error| {
        crate::error::caused_by("invalidInput", "Invalid URL".to_owned(), error)
    })?;
    if !["http", "https"].contains(&parsed.scheme())
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(fail(
            "invalidInput",
            "Supply an HTTP(S) browser URL without credentials",
        ));
    }
    Ok(parsed)
}

/// Infers known public services; custom registration hosts need explicit routing.
fn platform(url: &Url) -> Result<Platform, AppError> {
    match url.host_str() {
        Some("github.com" | "www.github.com") => Ok(Platform::GitHub),
        Some(host)
            if host == "dev.azure.com"
                || host == "www.dev.azure.com"
                || host.ends_with(".visualstudio.com") =>
        {
            Ok(Platform::AzureDevOps)
        }
        _ => Err(fail(
            "invalidInput",
            "Cannot determine platform from URL host; supply --platform explicitly for registration",
        )),
    }
}

/// Separates encoded coordinates before decoding so escaped slashes stay data.
fn segments(url: &Url) -> Result<Vec<String>, AppError> {
    url.path_segments()
        .map(|parts| {
            parts
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| fail("invalidInput", "URL has no path segments"))
}

/// Encodes each UTF-8 path segment exactly once, including spaces and separators.
pub(crate) fn escape(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing into a String cannot fail");
        }
    }
    encoded
}

/// Percent-decodes path segments without treating plus as a space.
///
/// Browser coordinates use path syntax rather than form encoding; preserving
/// that distinction keeps repository and project names intact.
fn decode(value: &str) -> Result<String, AppError> {
    let mut bytes = Vec::new();
    let mut source = value.bytes();
    while let Some(byte) = source.next() {
        if byte == b'%' {
            let high = source
                .next()
                .and_then(|value| char::from(value).to_digit(16));
            let low = source
                .next()
                .and_then(|value| char::from(value).to_digit(16));
            let decoded = high
                .zip(low)
                .and_then(|(high, low)| high.checked_mul(16)?.checked_add(low))
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| fail("invalidInput", "Invalid URL escape"))?;
            bytes.push(decoded);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).map_err(|error| {
        crate::error::caused_by("invalidInput", "URL is not UTF-8".to_owned(), error).into()
    })
}

/// Establishes the positive-ID invariant before a PR can enter workflow state.
pub(crate) fn positive_id(value: &str) -> Result<NonZeroU32, AppError> {
    value.parse().map_err(|error| {
        crate::error::caused_by(
            "invalidInput",
            "Supply a positive pull request identifier".to_owned(),
            error,
        )
        .into()
    })
}
