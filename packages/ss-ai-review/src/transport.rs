//! Owns authenticated request execution and evidence-based replay decisions.
//!
//! Provider adapters choose routes and interpret successful data; this module
//! preserves raw responses until it can classify refusal, failure, or success.
//! See `docs/implementation/transport.md` for recovery and credential boundaries.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use ohno::AppError;
use regex::Regex;
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde_json::{Value, json};

use crate::error::fail;
use crate::json::{array, field};
use crate::target::{Platform, Target, escape};
use crate::{Clock, Process};

/// Provides an injectable HTTP boundary for Azure DevOps.
///
/// Transport supplies the OAuth token and decides retry policy. Implementations
/// execute a single request and retain status, headers, and bytes for that
/// decision. Tests can supply replies without contacting a service.
pub trait Http {
    /// Sends one request without automatic application-level retries.
    ///
    /// `body`, when present, contains UTF-8 JSON. Keep `token` out of diagnostics
    /// and report HTTP refusals as responses so transport can classify them.
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError>;

    /// Sends attachment bytes without interpreting them as JSON.
    ///
    /// Implementations that do not support binary requests refuse them rather
    /// than accidentally labeling the payload as JSON.
    fn send_binary(
        &self,
        _method: &str,
        _url: &str,
        _token: &str,
        _body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        Err(fail("service", "Binary HTTP requests are not supported"))
    }
}

/// Retains HTTP status and continuation evidence separately from response bytes.
///
/// Use this at the HTTP boundary before parsing provider JSON. Status and
/// headers determine authentication recovery, throttling, and pagination even
/// when the body is empty or malformed.
#[derive(Debug)]
#[non_exhaustive]
pub struct HttpResponse {
    /// Status code used to distinguish success, refusal, and uncertain writes.
    pub status: u16,
    /// Lowercase header names preserving retry, content-type, and paging evidence.
    pub headers: BTreeMap<String, String>,
    /// Raw provider bytes, decoded only after transport classifies the reply.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Supplies captured response evidence from native HTTP or a test implementation.
    #[must_use]
    pub fn new(status: u16, headers: BTreeMap<String, String>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }
}

/// Reuses one HTTP client and keeps OAuth headers out of diagnostics.
///
/// The executable creates this boundary for Azure requests. Authentication
/// discovery and request replay remain with transport rather than the client.
#[derive(Debug)]
pub struct NativeHttp {
    /// Shared connection client configured not to redirect OAuth requests.
    client: Client,
}

impl NativeHttp {
    /// Creates the invocation's client with bounded requests and no redirects.
    pub fn new() -> Result<Self, AppError> {
        // Match the existing review transport's ordinary request timeout.
        // Redirects must not move an OAuth request to an unexpected destination.
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(100))
                .redirect(Policy::none())
                .build()?,
        })
    }

    /// Builds both request forms through the same credential and client boundary.
    fn request(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
        binary: bool,
    ) -> Result<reqwest::blocking::Request, AppError> {
        let mut request = self
            .client
            .request(Method::from_bytes(method.as_bytes())?, url)
            .bearer_auth(token)
            .header(
                "Accept",
                if binary && method == "GET" {
                    "application/octet-stream"
                } else {
                    "application/json"
                },
            );
        if let Some(body) = body {
            request = request
                .header(
                    "Content-Type",
                    if binary {
                        "application/octet-stream"
                    } else {
                        "application/json; charset=utf-8"
                    },
                )
                .body(body.to_owned());
        }
        Ok(request.build()?)
    }

    /// Preserves status and headers before the transport classifies a reply.
    fn execute(&self, request: reqwest::blocking::Request) -> Result<HttpResponse, AppError> {
        let response = self.client.execute(request)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| Ok((name.as_str().to_owned(), value.to_str()?.to_owned())))
            .collect::<Result<_, AppError>>()?;
        Ok(HttpResponse::new(
            status,
            headers,
            response.bytes()?.to_vec(),
        ))
    }
}

impl Http for NativeHttp {
    /// Sends JSON bytes directly so the Azure CLI cannot alter service Unicode.
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.execute(self.request(method, url, token, body, false)?)
    }

    fn send_binary(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.execute(self.request(method, url, token, body, true)?)
    }
}

/// Shares authentication, retry, and paging behavior across native operations.
///
/// An invocation retains one transport for its provider and injected effects.
/// Successful JSON is returned only after replay and authentication decisions;
/// uncertain writes are not converted into retryable read failures.
pub(crate) struct Transport<'a> {
    /// Routes requests through GitHub CLI or Azure native HTTP.
    pub(crate) platform: Platform,
    /// Azure collection root needed to turn relative routes into complete URLs.
    collection: Option<String>,
    /// Optional OAuth tenant passed to Azure CLI during token acquisition.
    tenant: Option<String>,
    /// Invocation-local OAuth credential; never serialized or quoted in errors.
    token: Option<String>,
    /// Launch boundary for GitHub requests and Azure authentication discovery.
    process: &'a dyn Process,
    /// Byte-preserving Azure request boundary independent of CLI output encoding.
    http: &'a dyn Http,
    /// Common time source for recovery guidance and waits.
    pub(crate) clock: &'a dyn Clock,
}

impl<'a> Transport<'a> {
    /// Binds provider coordinates and dependencies without acquiring credentials.
    ///
    /// Azure tokens are acquired lazily when the first request needs one.
    pub(crate) fn new(
        target: &Target,
        tenant: Option<&str>,
        process: &'a dyn Process,
        http: &'a dyn Http,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            platform: target.platform,
            collection: target.collection_url.clone(),
            tenant: tenant.map(str::to_owned),
            token: None,
            process,
            http,
            clock,
        }
    }

    /// Shares the invocation's injected process boundary with provider operations.
    pub(crate) fn process(&self) -> &dyn Process {
        self.process
    }

    /// Exposes the configured collection for provider route validation.
    pub(crate) fn ado_collection(&self) -> Option<&str> {
        self.collection.as_deref()
    }

    /// Makes an ordinary resource request, refusing unexpected ADO truncation.
    pub(crate) fn request(
        &mut self,
        path: &str,
        method: &str,
        body: Option<&Value>,
    ) -> Result<Value, AppError> {
        match self.platform {
            Platform::GitHub => self.github(path, method, body, false, true),
            Platform::AzureDevOps => {
                let page = self.ado(path, method, body, &[], "7.1")?;
                if page.continuation.is_some() {
                    return Err(fail(
                        if method == "GET" {
                            "service"
                        } else {
                            "ambiguousWrite"
                        },
                        format!("Unexpected continuation for {path}"),
                    ));
                }
                Ok(page.payload)
            }
        }
    }

    /// Reads a GitHub resource, accepting an explicit HTTP 404 as absence.
    ///
    /// No diagnostic text or response body can turn another failure into absence.
    pub(crate) fn github_get_if_found(&self, path: &str) -> Result<Option<Value>, AppError> {
        if self.platform != Platform::GitHub {
            return Err(fail("invalidInput", "A GitHub transport is required"));
        }
        self.github_optional(
            path,
            "GET",
            None,
            false,
            true,
            GithubResponseMode::OptionalNotFound,
        )
    }

    /// Renewal writes cannot retry even an explicit rate refusal without a new freshness check.
    pub(crate) fn github_write_once(&self, path: &str, body: &Value) -> Result<Value, AppError> {
        self.github(path, "POST", Some(body), false, false)
    }

    /// Distinguishes an explicit HTTP 429 from other single-attempt failures.
    ///
    /// Only a failed command with an included HTTP status line establishes the
    /// `explicitRateLimited` classification; diagnostic prose is not proof.
    pub(crate) fn github_write_once_with_refusal_evidence(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<Value, AppError> {
        self.github_optional(
            path,
            "POST",
            Some(body),
            false,
            false,
            GithubResponseMode::ExplicitRefusal,
        )?
        .ok_or_else(|| fail("service", "Unexpected absent GitHub response"))
    }

    /// A renewal must recheck freshness rather than replay a refused ADO write.
    pub(crate) fn ado_write_once(
        &mut self,
        path: &str,
        method: &str,
        body: &Value,
    ) -> Result<Value, AppError> {
        let page = self.ado_request(path, method, Some(body), &[], "7.1", false)?;
        if page.continuation.is_some() {
            return Err(fail("ambiguousWrite", "Unexpected ADO write continuation"));
        }
        Ok(page.payload)
    }

    /// Reads GitHub slurped REST pages or a checked ADO list envelope.
    pub(crate) fn list(&mut self, path: &str) -> Result<Vec<Value>, AppError> {
        match self.platform {
            Platform::GitHub => {
                let separator = if path.contains('?') { '&' } else { '?' };
                let pages = self.github(
                    &format!("{path}{separator}per_page=100"),
                    "GET",
                    None,
                    true,
                    true,
                )?;
                let Value::Array(pages) = pages else {
                    return Err(fail("service", "Expected an array in service evidence"));
                };
                // A successful slurped response contains at least one page, even
                // when the provider returned an empty collection.
                if pages.is_empty() {
                    return Err(fail("service", "GitHub list has no response pages"));
                }
                let mut values = Vec::new();
                for page in pages {
                    match page {
                        Value::Array(page) => values.extend(page),
                        Value::Object(_) => values.push(page),
                        _ => return Err(fail("service", "Invalid GitHub list page")),
                    }
                }
                Ok(values)
            }
            Platform::AzureDevOps => ado_items(&self.request(path, "GET", None)?),
        }
    }

    /// Executes a read-only GraphQL query while preserving complete evidence.
    ///
    /// Variables travel as JSON on stdin. Callers use this for queries, not
    /// mutations, because transport treats the POST as a replayable read.
    pub(crate) fn graphql(&self, query: &str, variables: &Value) -> Result<Value, AppError> {
        let value = self.github(
            "graphql",
            "POST",
            Some(&json!({ "query": query, "variables": variables })),
            false,
            true,
        )?;
        if value
            .get("errors")
            .is_some_and(|errors| errors.as_array().is_none_or(|errors| !errors.is_empty()))
        {
            return Err(fail(
                "service",
                format!(
                    "GraphQL returned incomplete evidence: {}",
                    diagnostic(&value.to_string())
                ),
            ));
        }
        Ok(value)
    }

    /// Runs GitHub CLI requests with separate refusal and transient-read recovery.
    ///
    /// Included headers support classification for individual requests; slurped
    /// pages preserve complete list results. Uncertain writes are never replayed.
    fn github(
        &self,
        path: &str,
        method: &str,
        body: Option<&Value>,
        paginated: bool,
        retry_refusals: bool,
    ) -> Result<Value, AppError> {
        self.github_optional(
            path,
            method,
            body,
            paginated,
            retry_refusals,
            GithubResponseMode::Standard,
        )?
        .ok_or_else(|| fail("service", "Unexpected absent GitHub response"))
    }

    /// Retains explicit HTTP absence separately from other request failures.
    fn github_optional(
        &self,
        path: &str,
        method: &str,
        body: Option<&Value>,
        paginated: bool,
        retry_refusals: bool,
        response_mode: GithubResponseMode,
    ) -> Result<Option<Value>, AppError> {
        let mut arguments = vec![
            "api".to_owned(),
            path.to_owned(),
            "--method".to_owned(),
            method.to_owned(),
        ];
        if paginated {
            arguments.extend(["--paginate".to_owned(), "--slurp".to_owned()]);
        } else {
            arguments.push("--include".to_owned());
        }
        let input = body.map(serde_json::to_vec).transpose()?;
        if input.is_some() {
            arguments.extend(["--input".to_owned(), "-".to_owned()]);
        }
        let read = method == "GET" || path == "graphql";
        let mut transient_attempt = 0_u32;
        let mut rate_attempt = 0_u32;
        loop {
            let output = self
                .process
                .execute("gh", &arguments, input.as_deref().unwrap_or_default())
                .map_err(|error| {
                    crate::error::caused_by(
                        if read { "service" } else { "ambiguousWrite" },
                        "Unable to run gh; install and authenticate GitHub CLI".to_owned(),
                        error,
                    )
                })?;
            let stdout = String::from_utf8(output.stdout).map_err(|error| {
                crate::error::caused_by(
                    if read { "service" } else { "ambiguousWrite" },
                    "GitHub output is not UTF-8".to_owned(),
                    error,
                )
            })?;
            let (headers, payload) = split_headers(&stdout);
            // Only a failed request with an actual status line can establish
            // absence. Error prose and JSON bodies are not status evidence.
            if matches!(response_mode, GithubResponseMode::OptionalNotFound)
                && !output.success
                && response_status(headers) == Some(404)
            {
                return Ok(None);
            }
            if matches!(response_mode, GithubResponseMode::ExplicitRefusal)
                && !output.success
                && response_status(headers) == Some(429)
            {
                return Err(fail(
                    "explicitRateLimited",
                    "GitHub refused the write for rate limiting; no write was retried",
                ));
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let error_text = format!("{stderr}\n{payload}");
            let status_text = format!("{headers}\n{stderr}");
            let json = serde_json::from_str::<Value>(payload);
            let rate_limited = github_rate_limit(&status_text, &error_text, json.as_ref().ok());
            if rate_limited {
                if !retry_refusals {
                    return Err(fail(
                        "rateLimited",
                        "GitHub refused the renewal for rate limiting; no write was retried",
                    ));
                }
                let delay = github_delay(
                    &format!("{status_text}\n{payload}"),
                    rate_attempt,
                    true,
                    self.clock,
                );
                rate_attempt = rate_attempt.saturating_add(1);
                eprintln!(
                    "GitHub rate-limit refusal; waiting {} ms before retrying",
                    delay.as_millis()
                );
                self.clock.wait(delay)?;
                continue;
            }
            if output.success {
                if payload.trim().is_empty() && method == "DELETE" {
                    return Ok(Some(Value::Null));
                }
                return json.map(Some).map_err(|error| {
                    crate::error::caused_by(
                        if read { "service" } else { "ambiguousWrite" },
                        "GitHub returned invalid JSON".to_owned(),
                        error,
                    )
                    .into()
                });
            }
            // Read-only GraphQL uses POST but can safely repeat transient failures.
            if read
                && transient_attempt < 4
                && github_transient(&format!("{status_text}\n{error_text}"))
            {
                self.clock.wait(github_delay(
                    &error_text,
                    transient_attempt,
                    false,
                    self.clock,
                ))?;
                transient_attempt = transient_attempt.saturating_add(1);
                continue;
            }
            let kind = if http_status(&status_text, 401) || error_text.contains("gh auth login") {
                "authentication"
            } else if http_status(&status_text, 403) || http_status(&status_text, 422) {
                "permissionDenied"
            } else if read {
                "service"
            } else {
                "ambiguousWrite"
            };
            return Err(fail(
                kind,
                format!("GitHub request failed: {}", diagnostic(&error_text)),
            ));
        }
    }

    /// Acquires only the ASCII OAuth token; service content never travels via az.
    fn access_token(&mut self) -> Result<&str, AppError> {
        if self.token.is_none() {
            self.token = Some(Self::azure_access_token(
                self.process,
                self.tenant.as_deref(),
            )?);
        }
        self.token
            .as_deref()
            .ok_or_else(|| fail("authentication", "Azure OAuth token unavailable"))
    }

    /// Shares OAuth discovery between REST reads and isolated Azure Git fetches.
    pub(crate) fn azure_access_token(
        process: &dyn Process,
        tenant: Option<&str>,
    ) -> Result<String, AppError> {
        let mut arguments = [
            "account",
            "get-access-token",
            "--resource",
            "499b84ac-1321-427f-aa17-267ca6975798",
            "--query",
            "accessToken",
            "--output",
            "tsv",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(tenant) = tenant {
            arguments.extend(["--tenant".to_owned(), tenant.to_owned()]);
        }
        let output = process.execute("az", &arguments, &[]).map_err(|error| {
            crate::error::caused_by(
                "authentication",
                "Cannot acquire Azure DevOps OAuth token; run az login".to_owned(),
                error,
            )
        })?;
        // Authentication output is secret, including unsuccessful command output.
        if !output.success {
            return Err(fail(
                "authentication",
                "Azure DevOps token acquisition failed; run az login and check --tenant",
            ));
        }
        let token = String::from_utf8(output.stdout).map_err(|error| {
            crate::error::caused_by(
                "authentication",
                "Azure token is not UTF-8".to_owned(),
                error,
            )
        })?;
        let token = token.trim();
        if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(fail(
                "authentication",
                "Azure DevOps token acquisition produced an invalid token",
            ));
        }
        Ok(token.to_owned())
    }

    /// Runs an ADO request and exposes continuation only to explicit paging callers.
    pub(crate) fn ado(
        &mut self,
        path: &str,
        method: &str,
        body: Option<&Value>,
        query: &[(&str, String)],
        version: &str,
    ) -> Result<Page, AppError> {
        self.ado_request(path, method, body, query, version, true)
    }

    /// Keeps ordinary transport recovery separate from freshness-bound publication.
    fn ado_request(
        &mut self,
        path: &str,
        method: &str,
        body: Option<&Value>,
        query: &[(&str, String)],
        version: &str,
        retry_refusals: bool,
    ) -> Result<Page, AppError> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let response = self.ado_response(
            path,
            method,
            body.as_deref(),
            query,
            version,
            AdoRequestKind::Json { retry_refusals },
        )?;
        let read = method == "GET";
        let text = String::from_utf8(response.body).map_err(|error| {
            crate::error::caused_by(
                if read { "service" } else { "ambiguousWrite" },
                "Azure DevOps output is not UTF-8".to_owned(),
                error,
            )
        })?;
        if response
            .headers
            .get("content-type")
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
            || text.trim_start().starts_with('<')
        {
            return Err(fail(
                if read {
                    "authentication"
                } else {
                    "ambiguousWrite"
                },
                "Azure DevOps returned a sign-in document; authenticate with the collection's tenant",
            ));
        }
        let payload = if text.trim().is_empty() && method == "DELETE" {
            Value::Null
        } else {
            serde_json::from_str(&text).map_err(|error| {
                crate::error::caused_by(
                    if read { "service" } else { "ambiguousWrite" },
                    "Azure DevOps returned invalid JSON".to_owned(),
                    error,
                )
            })?
        };
        let continuation = response
            .headers
            .get("x-ms-continuationtoken")
            .filter(|value| !value.trim().is_empty())
            .cloned();
        Ok(Page {
            payload,
            continuation,
        })
    }

    /// Uploads or reads attachment bytes while retaining response evidence.
    ///
    /// Paths are relative to the configured collection. The caller interprets
    /// successful upload metadata and downloaded bytes as appropriate.
    pub(crate) fn ado_binary(
        &mut self,
        path: &str,
        method: &str,
        body: Option<&[u8]>,
        query: &[(&str, String)],
        version: &str,
    ) -> Result<HttpResponse, AppError> {
        if self.platform != Platform::AzureDevOps
            || !matches!((method, body), ("GET", None) | ("POST", Some(_)))
        {
            return Err(fail("invalidInput", "Invalid Azure attachment request"));
        }
        // Publication records intent before upload and never replays a write.
        let response =
            self.ado_response(path, method, body, query, version, AdoRequestKind::Binary)?;
        // Attachment reads are verified by the caller's frozen content hash.
        if method == "POST"
            && response
                .headers
                .get("content-type")
                .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            return Err(fail(
                "ambiguousWrite",
                "Azure DevOps returned a sign-in document; authenticate with the collection's tenant",
            ));
        }
        Ok(response)
    }

    /// Applies the same authentication and replay decisions to all ADO bytes.
    fn ado_response(
        &mut self,
        path: &str,
        method: &str,
        body: Option<&[u8]>,
        query: &[(&str, String)],
        version: &str,
        kind: AdoRequestKind,
    ) -> Result<HttpResponse, AppError> {
        let collection = self
            .collection
            .as_deref()
            .ok_or_else(|| fail("invalidInput", "Azure DevOps requires a collection URL"))?;
        let mut query = query.to_vec();
        query.sort_by(|left, right| left.0.cmp(right.0));
        let mut parts: Vec<_> = query
            .iter()
            .map(|(name, value)| format!("{}={}", escape(name), escape(value)))
            .collect();
        parts.push(format!("api-version={}", escape(version)));
        let url = format!(
            "{}/{}?{}",
            collection.trim_end_matches('/'),
            path.trim_start_matches('/'),
            parts.join("&")
        );
        let read = method == "GET";
        let binary = matches!(kind, AdoRequestKind::Binary);
        let retry_refusals = match kind {
            AdoRequestKind::Json { retry_refusals } => retry_refusals,
            AdoRequestKind::Binary => read,
        };
        let mut transient_attempt = 0_u32;
        let mut rate_attempt = 0_u32;
        let mut refreshed = false;
        loop {
            let token = self.access_token()?.to_owned();
            let response = if binary {
                self.http.send_binary(method, &url, &token, body)
            } else {
                self.http.send(method, &url, &token, body)
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    if read
                        && transient_attempt < 4
                        && error.find_source::<reqwest::Error>().is_some()
                    {
                        self.clock.wait(ado_delay(None, transient_attempt, false))?;
                        transient_attempt = transient_attempt.saturating_add(1);
                        continue;
                    }
                    return Err(crate::error::caused_by(
                        if read { "service" } else { "ambiguousWrite" },
                        "Azure DevOps transport failed".to_owned(),
                        error.into_std_error(),
                    )
                    .into());
                }
            };
            if response.status == 429 {
                if !retry_refusals {
                    return Err(fail(
                        "rateLimited",
                        "ADO publication was rate limited; write not replayed",
                    ));
                }
                let delay = ado_delay(response.headers.get("retry-after"), rate_attempt, true);
                rate_attempt = rate_attempt.saturating_add(1);
                eprintln!(
                    "Azure DevOps rate-limit refusal; waiting {} ms before retrying",
                    delay.as_millis()
                );
                self.clock.wait(delay)?;
                continue;
            }
            if response.status == 401 && read && !refreshed {
                self.token = None;
                refreshed = true;
                continue;
            }
            if read && response.status >= 500 && transient_attempt < 4 {
                self.clock.wait(ado_delay(
                    response.headers.get("retry-after"),
                    transient_attempt,
                    false,
                ))?;
                transient_attempt = transient_attempt.saturating_add(1);
                continue;
            }
            if !(200..300).contains(&response.status) {
                let text = if binary {
                    String::from_utf8_lossy(&response.body).into_owned()
                } else {
                    String::from_utf8(response.body).map_err(|error| {
                        crate::error::caused_by(
                            if read { "service" } else { "ambiguousWrite" },
                            "Azure DevOps output is not UTF-8".to_owned(),
                            error,
                        )
                    })?
                };
                let kind = match response.status {
                    401 => "authentication",
                    400..500 => "permissionDenied",
                    _ if read => "service",
                    _ => "ambiguousWrite",
                };
                return Err(fail(
                    kind,
                    format!(
                        "Azure DevOps HTTP {}: {}",
                        response.status,
                        diagnostic(&text.replace(&token, "<redacted>"))
                    ),
                ));
            }
            return Ok(response);
        }
    }
}

/// Selects which additional response evidence a GitHub caller requires.
#[derive(Clone, Copy)]
enum GithubResponseMode {
    /// Preserve the ordinary request classification.
    Standard,
    /// Recognize an explicit not-found response for optional reads.
    OptionalNotFound,
    /// Recognize an explicit rate refusal for a single-attempt write.
    ExplicitRefusal,
}

/// Selects decoding and refusal replay without changing the existing JSON policy.
#[derive(Clone, Copy)]
enum AdoRequestKind {
    /// JSON requests select whether a refusal may be replayed.
    Json { retry_refusals: bool },
    /// Attachment reads may retry; uploads are single-attempt writes.
    Binary,
}

/// ADO paging callers explicitly consume continuation; others reject truncation.
pub(crate) struct Page {
    /// JSON evidence from this response, not an implicitly complete collection.
    pub(crate) payload: Value,
    /// Next-page token that the caller must consume or explicitly reject.
    pub(crate) continuation: Option<String>,
}

/// Recognizes status evidence without mistaking arbitrary body numbers for codes.
///
/// Protocol patterns are independent of responses and compile once for all requests.
static HTTP_STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)\bHTTP(?:/[0-9.]+)?\s+([0-9]{3})\b").expect("HTTP status pattern")
});
/// Extracts service-provided recovery intervals before fallback backoff is used.
static RETRY_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i-u)retry[-_]after"?\s*[:=]\s*"?([0-9]+)"#).expect("retry header pattern")
});
/// Extracts the quota-reset instant for clock-relative recovery waits.
static RATE_RESET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i-u)x-ratelimit-reset"?\s*[:=]\s*"?([0-9]+)"#).expect("rate reset pattern")
});
/// Distinguishes exhausted quota from incidental reset headers on other failures.
static RATE_REMAINING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i-u)x-ratelimit-remaining"?\s*[:=]\s*"?([0-9]+)"#)
        .expect("rate remaining pattern")
});
/// Redacts credential-shaped material before provider diagnostics reach the CLI.
static CREDENTIALS: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        r"(?i-u:authorization)[ \t]*:[^\r\n]*",
        r"(?i-u:\bbearer)[ \t]+[^ \t\r\n]+",
        r"(?i-u)\bgh[pousr]_[A-Za-z0-9_]{8,}",
    ]
    .map(|pattern| Regex::new(pattern).expect("fixed credential pattern"))
});

/// Validates complete list evidence instead of silently coercing malformed shapes.
pub(crate) fn ado_items(payload: &Value) -> Result<Vec<Value>, AppError> {
    let items = array(field(payload, "value")?)?;
    if let Some(count) = payload.get("count") {
        let count = count
            .as_u64()
            .or_else(|| {
                let value = count.as_str()?;
                value.parse().ok()
            })
            .ok_or_else(|| fail("service", "Invalid Azure DevOps list count"))?;
        if usize::try_from(count).ok() != Some(items.len()) {
            return Err(fail(
                "service",
                "Azure DevOps list count does not match its items",
            ));
        }
    }
    Ok(items.to_vec())
}

/// Separates `gh --include` evidence while retaining bare slurped JSON unchanged.
fn split_headers(output: &str) -> (&str, &str) {
    if output.starts_with("HTTP/") {
        if let Some((header, body)) = output.split_once("\r\n\r\n") {
            return (header, body);
        }
        if let Some((header, body)) = output.split_once("\n\n") {
            return (header, body);
        }
    }
    ("", output)
}

/// Reads only the HTTP status line supplied by `gh --include`.
fn response_status(headers: &str) -> Option<u16> {
    let mut parts = headers.lines().next()?.split_whitespace();
    let version = parts.next()?;
    if !matches!(
        version,
        "HTTP/1.0" | "HTTP/1.1" | "HTTP/2" | "HTTP/2.0" | "HTTP/3" | "HTTP/3.0"
    ) {
        return None;
    }
    let status = parts.next()?;
    if status.len() != 3 || !status.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    status.parse().ok()
}

/// Identifies transport and server failures eligible for bounded read retries.
fn github_transient(value: &str) -> bool {
    if [500, 502, 503, 504]
        .iter()
        .any(|status| http_status(value, *status))
    {
        return true;
    }
    let value = value.to_ascii_lowercase();
    [
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
        "connection reset",
        "connection refused",
        "i/o timeout",
        "tls handshake",
        "unexpected eof",
        "server error",
    ]
    .iter()
    .any(|pattern| value.contains(pattern))
}

/// Recognizes provider throttling language as evidence of an explicit refusal.
fn rate_message(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "rate limit",
        "ratelimit",
        "abuse detection",
        "http 429",
        "too many requests",
    ]
    .iter()
    .any(|pattern| value.contains(pattern))
}

/// Requires refusal evidence before allowing a request to use rate-limit recovery.
///
/// Partial GraphQL data is not a pure refusal; mixed errors must not authorize
/// replay through the rate-limit path.
fn github_rate_limit(status: &str, message: &str, response: Option<&Value>) -> bool {
    if response
        .and_then(|value| value.get("data"))
        .is_some_and(|data| !data.is_null())
    {
        return false;
    }
    if let Some(errors) = response
        .and_then(|value| value.get("errors"))
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        return errors.iter().all(|error| {
            error.get("type").and_then(Value::as_str) == Some("RATE_LIMITED")
                || error
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(rate_message)
        });
    }
    http_status(status, 429) || (http_status(status, 403) && rate_message(message))
}

/// Status evidence must not match an unrelated quota, request identifier, or body number.
fn http_status(value: &str, status: u16) -> bool {
    HTTP_STATUS.captures_iter(value).any(|captures| {
        captures
            .get(1)
            .is_some_and(|code| code.as_str().parse::<u16>() == Ok(status))
    })
}

/// Prefers GitHub recovery headers, then applies the appropriate bounded backoff.
///
/// Rate refusals and transient reads have separate budgets; the wall clock
/// converts quota-reset instants to waits without affecting claim timestamps.
fn github_delay(diagnostics: &str, attempt: u32, rate: bool, clock: &dyn Clock) -> Duration {
    // Preserve the review transport's backoff and service-provided recovery rules.
    let base = if rate { 60_000_u64 } else { 1_000 };
    let backoff = base
        .saturating_mul(2_u64.saturating_pow(attempt.min(7)))
        .min(120_000);
    let max = if rate { 2_147_483_647 } else { 120_000 };
    let number = |pattern: &Regex| -> Option<u64> {
        pattern.captures(diagnostics)?.get(1)?.as_str().parse().ok()
    };
    if let Some(seconds) = number(&RETRY_AFTER)
        && (seconds > 0 || !rate)
    {
        return Duration::from_millis(seconds.saturating_mul(1000).min(max));
    }
    if let Some(epoch) = number(&RATE_RESET) {
        let remaining = number(&RATE_REMAINING);
        if !rate || remaining.is_none_or(|remaining| remaining == 0) {
            let now = u64::try_from(clock.now().as_millisecond()).unwrap_or_default();
            let delay = epoch.saturating_mul(1000).saturating_sub(now);
            if delay > 0 || !rate {
                return Duration::from_millis(delay.min(max));
            }
        }
    }
    Duration::from_millis(backoff)
}

/// Selects Azure recovery waits without spending the transient budget on throttling.
fn ado_delay(retry_after: Option<&String>, attempt: u32, rate: bool) -> Duration {
    // Match the existing ADO bounded backoff; explicit throttling does not spend
    // the transient retry budget and can carry a longer service recovery time.
    let max = if rate { 2_147_483_647 } else { 30_000 };
    if let Some(seconds) = retry_after
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
    {
        return Duration::from_millis(seconds.saturating_mul(1000).min(max));
    }
    Duration::from_millis(
        500_u64
            .saturating_mul(2_u64.saturating_pow(attempt.min(6)))
            .min(30_000),
    )
}

/// Scrubs credential-shaped material before bounding any provider diagnostic.
fn diagnostic(value: &str) -> String {
    let mut value = value.to_owned();
    for pattern in CREDENTIALS.iter() {
        value = pattern.replace_all(&value, "<redacted>").into_owned();
    }
    // Keep useful error context without echoing an arbitrarily large response.
    value.chars().take(2000).collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        reason = "Tests use bounded fixtures with explicit expected shapes"
    )]
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::json;

    use super::{Http, HttpResponse, NativeHttp, Transport};
    use crate::error::fail;
    use crate::target::Target;
    use crate::{Clock, Process, ProcessOutput, classification};

    /// Supplies deterministic token acquisition and scripted HTTP responses.
    struct Fixture {
        replies: RefCell<VecDeque<Result<HttpResponse, AppError>>>,
        calls: RefCell<Vec<(String, String, Vec<u8>)>>,
        tokens: Cell<usize>,
        waits: Cell<usize>,
    }

    impl Fixture {
        fn new(replies: Vec<Result<HttpResponse, AppError>>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                calls: RefCell::new(Vec::new()),
                tokens: Cell::new(0),
                waits: Cell::new(0),
            }
        }
    }

    impl Process for Fixture {
        fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "az");
            self.tokens.set(self.tokens.get() + 1);
            Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
        }
    }

    impl Clock for Fixture {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn wait(&self, _: Duration) -> Result<(), AppError> {
            // An unexpected replay must fail immediately rather than loop forever.
            assert!(self.waits.get() < 5);
            self.waits.set(self.waits.get() + 1);
            Ok(())
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
            self.send_binary(method, url, token, body)
        }

        fn send_binary(
            &self,
            method: &str,
            url: &str,
            token: &str,
            body: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            assert_eq!(token, "fixture-token");
            self.calls.borrow_mut().push((
                method.to_owned(),
                url.to_owned(),
                body.unwrap_or_default().to_vec(),
            ));
            self.replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request")
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "Scripted replies contain both HTTP responses and transport errors"
    )]
    fn response(status: u16, body: &[u8]) -> Result<HttpResponse, AppError> {
        Ok(HttpResponse::new(status, BTreeMap::new(), body.to_vec()))
    }

    fn target() -> Target {
        Target::repository("https://dev.azure.com/org/project/_git/repo").unwrap()
    }

    #[test]
    fn native_requests_preserve_binary_body_and_content_type() {
        let http = NativeHttp::new().unwrap();
        let bytes = b"\0\xff<\x80";
        let request = http
            .request(
                "POST",
                "https://example.test/attachment",
                "token",
                Some(bytes),
                true,
            )
            .unwrap();
        assert_eq!(
            request.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(request.headers()["accept"], "application/json");
        assert_eq!(request.body().unwrap().as_bytes().unwrap(), bytes);
        assert_eq!(request.headers()["authorization"], "Bearer token");

        let json = http
            .request("POST", "https://example.test/", "token", Some(b"{}"), false)
            .unwrap();
        assert_eq!(
            json.headers()["content-type"],
            "application/json; charset=utf-8"
        );
        assert_eq!(json.headers()["accept"], "application/json");
        let download = http
            .request("GET", "https://example.test/", "token", None, true)
            .unwrap();
        assert_eq!(download.headers()["accept"], "application/octet-stream");
        assert!(!download.headers().contains_key("content-type"));
    }

    #[test]
    fn binary_upload_and_download_keep_bytes_and_http_evidence() {
        let bytes = b"<\0\xff\x80";
        let fixture = Fixture::new(vec![
            response(201, br#"{"id":"abc"}"#),
            response(200, bytes),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, Some("tenant"), &fixture, &fixture, &fixture);
        let upload = transport
            .ado_binary(
                "_apis/attachments",
                "POST",
                Some(bytes),
                &[("fileName", "a b".into())],
                "7.1",
            )
            .unwrap();
        assert_eq!(upload.status, 201);
        assert_eq!(upload.body, br#"{"id":"abc"}"#);
        let download = transport
            .ado_binary("_apis/attachments/abc", "GET", None, &[], "7.1")
            .unwrap();
        assert_eq!(download.body, bytes);
        assert_eq!(fixture.tokens.get(), 1);
        let calls = fixture.calls.borrow();
        assert_eq!(calls[0].0, "POST");
        assert_eq!(
            calls[0].1,
            "https://dev.azure.com/org/_apis/attachments?fileName=a%20b&api-version=7.1"
        );
        assert_eq!(calls[0].2, bytes);
        assert_eq!(calls[1].0, "GET");
        assert!(calls[1].2.is_empty());
    }

    #[test]
    fn binary_reads_refresh_and_retry_but_writes_are_not_replayed_when_ambiguous() {
        let fixture = Fixture::new(vec![
            response(401, b"expired"),
            response(429, b"limited"),
            response(503, b"unavailable"),
            response(200, b"\xff"),
            response(503, b"\xff"),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        assert_eq!(
            transport
                .ado_binary("attachment", "GET", None, &[], "7.1")
                .unwrap()
                .body,
            b"\xff"
        );
        assert_eq!(fixture.tokens.get(), 2);
        assert_eq!(fixture.waits.get(), 2);
        let error = transport
            .ado_binary("attachment", "POST", Some(b"data"), &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert_eq!(fixture.calls.borrow().len(), 5);
    }

    #[test]
    fn binary_errors_and_json_validation_remain_distinct() {
        let fixture = Fixture::new(vec![
            response(403, b"\xff fixture-token"),
            response(302, b"redirect"),
            Err(fail("service", "connection failed")),
            response(200, b"\xff"),
            response(200, b"not-json"),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let error = transport
            .ado_binary("attachment", "GET", None, &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "permissionDenied");
        assert!(!error.to_string().contains("fixture-token"));
        let error = transport
            .ado_binary("attachment", "GET", None, &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "service");
        let error = transport
            .ado_binary("attachment", "POST", Some(b"x"), &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        assert_eq!(
            classification(&transport.request("resource", "GET", None).unwrap_err()),
            "service"
        );
        assert_eq!(
            classification(&transport.request("resource", "POST", None).unwrap_err()),
            "ambiguousWrite"
        );
        assert_eq!(fixture.calls.borrow().len(), 5);
    }

    #[test]
    fn binary_upload_refusal_is_not_replayed_and_reads_do_not_sniff_html() {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_owned(), "text/html".to_owned());
        let fixture = Fixture::new(vec![
            response(429, b"limited"),
            Ok(HttpResponse::new(
                200,
                headers.clone(),
                b"<content".to_vec(),
            )),
            Ok(HttpResponse::new(200, headers, b"<sign-in".to_vec())),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &fixture, &fixture, &fixture);
        let error = transport
            .ado_binary("attachment", "POST", Some(b"data"), &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "rateLimited");
        assert_eq!(fixture.calls.borrow().len(), 1);
        assert_eq!(fixture.waits.get(), 0);
        assert_eq!(
            transport
                .ado_binary("attachment", "GET", None, &[], "7.1")
                .unwrap()
                .body,
            b"<content"
        );
        let error = transport
            .ado_binary("attachment", "POST", Some(b"data"), &[], "7.1")
            .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
    }

    #[test]
    fn existing_http_implementations_do_not_accidentally_send_binary_as_json() {
        struct JsonOnly;
        impl Http for JsonOnly {
            fn send(
                &self,
                _: &str,
                _: &str,
                _: &str,
                _: Option<&[u8]>,
            ) -> Result<HttpResponse, AppError> {
                panic!("binary requests must not use the JSON boundary")
            }
        }
        JsonOnly
            .send_binary("POST", "https://example.test/", "token", Some(b"\xff"))
            .unwrap_err();
    }

    #[test]
    fn github_explicit_refusal_requires_failed_command_and_http_status_line() {
        struct GithubProcess {
            replies: RefCell<VecDeque<ProcessOutput>>,
            calls: Cell<usize>,
        }

        impl Process for GithubProcess {
            fn execute(
                &self,
                program: &str,
                arguments: &[String],
                _: &[u8],
            ) -> io::Result<ProcessOutput> {
                assert_eq!(program, "gh");
                assert!(arguments.iter().any(|argument| argument == "--include"));
                self.calls.set(self.calls.get() + 1);
                Ok(self
                    .replies
                    .borrow_mut()
                    .pop_front()
                    .expect("unexpected request"))
            }
        }

        let process = GithubProcess {
            replies: RefCell::new(VecDeque::from([
                ProcessOutput::new(false, b"HTTP/2 429\n\n{}".to_vec(), vec![]),
                ProcessOutput::new(false, b"{}".to_vec(), b"HTTP 429".to_vec()),
                ProcessOutput::new(
                    false,
                    br#"{"errors":[{"message":"rate limit"}]}"#.to_vec(),
                    vec![],
                ),
                ProcessOutput::new(true, b"HTTP/2 429\n\n{}".to_vec(), vec![]),
                ProcessOutput::new(false, b"HTTP/2 429\n\n{}".to_vec(), vec![]),
            ])),
            calls: Cell::new(0),
        };
        let fixture = Fixture::new(vec![]);
        let target = Target::repository("https://github.com/org/repo").unwrap();
        let transport = Transport::new(&target, None, &process, &fixture, &fixture);
        for expected in [
            "explicitRateLimited",
            "rateLimited",
            "rateLimited",
            "rateLimited",
        ] {
            let error = transport
                .github_write_once_with_refusal_evidence("gists", &json!({}))
                .unwrap_err();
            assert_eq!(classification(&error), expected);
        }
        let error = transport
            .github_write_once("gists", &json!({}))
            .unwrap_err();
        assert_eq!(classification(&error), "rateLimited");
        assert_eq!(process.calls.get(), 5);
    }
}
