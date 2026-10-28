//! Adapts complete service evidence into shared review operations.
//!
//! Provider-specific fields and pagination stay here; selection consumes
//! normalized snapshots and registration preserves existing participation.
//! See `docs/implementation/providers.md` for the evidence boundary.

use std::collections::HashSet;
use std::num::NonZeroU32;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};

use crate::clock::{is_recent, latest_commit};
use crate::error::fail;
use crate::identity::Identity;
use crate::json::{array, boolean, field, identifier, text};
use crate::selection::{Candidate, Comment, Provider, RemoteClaim};
use crate::target::{Platform, Target, escape};
use crate::transport::{Transport, ado_items};

/// Maps provider payloads into the operations consumed by both skills.
///
/// One adapter retains its invocation's authenticated transport. Its `Provider`
/// implementation serves selection; `identity` and `register` also serve the
/// independent registration command.
pub(crate) struct Api<'a> {
    /// Executes requests and recovery policy without leaking credentials into state.
    pub(crate) transport: Transport<'a>,
}

/// GitHub's commits connection avoids the REST pull-request commit result cap.
///
/// The adapter follows this query's cursor until all commit dates are available;
/// ranking must not depend on which page contains the newest commit.
pub(crate) const COMMIT_QUERY: &str = include_str!("commits.graphql");

impl Api<'_> {
    /// Finds the actual account before touching a journal or registering a reviewer.
    pub(crate) fn identity(&mut self) -> Result<Identity, AppError> {
        let identity = match self.transport.platform {
            Platform::GitHub => Identity::from_account(
                Platform::GitHub,
                &self.transport.request("user", "GET", None)?,
            )?,
            Platform::AzureDevOps => {
                let page =
                    self.transport
                        .ado("_apis/connectionData", "GET", None, &[], "7.1-preview")?;
                if page.continuation.is_some() {
                    return Err(fail("service", "Unexpected connection-data continuation"));
                }
                Identity::from_connection(&page.payload)?
            }
        };
        if identity.id.is_none() {
            return Err(fail(
                "authentication",
                "The service did not identify the authenticated user",
            ));
        }
        identity.validate()?;
        Ok(identity)
    }

    /// Supplies commit recency independently of reviewer eligibility.
    ///
    /// Returns the latest non-future commit instant and whether it meets the
    /// fixed cutoff. Keeping this separate lets callers reuse recency without
    /// adopting the proactor's author and reviewer exclusions.
    pub(crate) fn commit_recency(
        &mut self,
        target: &Target,
        now: Timestamp,
    ) -> Result<(Option<Timestamp>, bool), AppError> {
        let timestamps = self.commit_timestamps(target)?;
        let latest = latest_commit(&timestamps, now)?;
        Ok((latest, is_recent(latest, now)))
    }

    /// Collects complete committer evidence before recency policy runs.
    /// Ref: docs/implementation/providers.md, "Complete evidence".
    fn commit_timestamps(&mut self, target: &Target) -> Result<Vec<String>, AppError> {
        let mut timestamps = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor: Option<String> = None;
        match target.platform {
            Platform::GitHub => loop {
                let response = self.transport.graphql(
                    COMMIT_QUERY,
                    &json!({
                        "owner": target.organization, "repo": target.repository,
                        "num": target.pull_request_id, "cur": cursor, "size": 100
                    }),
                )?;
                let connection = response
                    .pointer("/data/repository/pullRequest/commits")
                    .ok_or_else(|| fail("service", "GitHub returned no PR commits connection"))?;
                for node in array(field(connection, "nodes")?)? {
                    timestamps
                        .push(text(field(field(node, "commit")?, "committedDate")?)?.to_owned());
                }
                let page = field(connection, "pageInfo")?;
                if !boolean(field(page, "hasNextPage")?)? {
                    break;
                }
                let next = text(field(page, "endCursor")?)?;
                if next.is_empty() || !seen.insert(next.to_owned()) {
                    return Err(fail(
                        "service",
                        "GitHub repeated or omitted a commit cursor",
                    ));
                }
                // Match the capture adapter's guard against endlessly advancing bad pages.
                if seen.len() >= 1000 {
                    return Err(fail(
                        "service",
                        "GitHub commits exceeded the page safety limit",
                    ));
                }
                cursor = Some(next.to_owned());
            },
            Platform::AzureDevOps => loop {
                let mut query = vec![("$top", "100".to_owned())];
                if let Some(cursor) = &cursor {
                    query.push(("continuationToken", cursor.clone()));
                }
                let page = self.transport.ado(
                    &format!("{}/commits", target.pr_api()),
                    "GET",
                    None,
                    &query,
                    "7.1",
                )?;
                for commit in ado_items(&page.payload)? {
                    timestamps.push(text(field(field(&commit, "committer")?, "date")?)?.to_owned());
                }
                let Some(next) = page.continuation else {
                    break;
                };
                if !seen.insert(next.clone()) {
                    return Err(fail(
                        "service",
                        "Azure DevOps repeated a commit continuation token",
                    ));
                }
                cursor = Some(next);
            },
        }
        Ok(timestamps)
    }

    /// Shares reviewer evidence between conservative registration and selection.
    /// Ref: docs/implementation/providers.md, "Account identity and registration".
    fn reviewers(
        &mut self,
        target: &Target,
        include_teams: bool,
        exclude_declined: bool,
    ) -> Result<Vec<Identity>, AppError> {
        let path = format!(
            "{}/{}",
            target.pr_api(),
            if target.platform == Platform::GitHub {
                "requested_reviewers"
            } else {
                "reviewers"
            }
        );
        let values = self.transport.list(&path)?;
        let mut identities = Vec::new();
        if target.platform == Platform::GitHub {
            if values.is_empty() {
                return Err(fail(
                    "service",
                    "GitHub did not return requested-reviewer pages",
                ));
            }
            for page in values {
                for account in array(field(&page, "users")?)? {
                    identities.push(Identity::from_account(target.platform, account)?);
                }
                // Registration preserves its conservative identity matching; proactor
                // eligibility must not infer membership from an assigned team.
                if include_teams {
                    for team in array(field(&page, "teams")?)? {
                        identities.push(Identity::from_team(team)?);
                    }
                }
            }
        } else {
            for value in values {
                // Selection ignores declined assignments; registration preserves every entry.
                // Ref: docs/implementation/providers.md, "Account identity and registration".
                if exclude_declined
                    && value
                        .get("hasDeclined")
                        .map(boolean)
                        .transpose()?
                        .unwrap_or(false)
                {
                    continue;
                }
                identities.push(Identity::from_account(target.platform, &value)?);
            }
        }
        Ok(identities)
    }

    /// Resolves bare ADO identifiers before writes and reports the resolved URL.
    fn resolve(&mut self, target: &Target) -> Result<Target, AppError> {
        let mut target = target.clone();
        if target.platform == Platform::AzureDevOps
            && (target.project.as_deref().is_none_or(str::is_empty) || target.repository.is_empty())
        {
            let response = self.transport.request(
                &format!("_apis/git/pullrequests/{}", target.pull_request_id),
                "GET",
                None,
            )?;
            let repository = field(&response, "repository")?;
            text(field(repository, "name")?)?.clone_into(&mut target.repository);
            target.project = Some(text(field(field(repository, "project")?, "name")?)?.to_owned());
            if target.repository.is_empty() || target.project.as_deref().is_none_or(str::is_empty) {
                return Err(fail(
                    "service",
                    "Azure DevOps did not resolve project and repository",
                ));
            }
        }
        target.refresh_url();
        Ok(target)
    }

    /// Registers only a new reviewer; existing votes and metadata remain untouched.
    ///
    /// Accepts the authenticated identity discovered for this invocation and
    /// returns the command result, not a review verdict. Identifier-only Azure
    /// targets are resolved before reporting the final browser URL.
    pub(crate) fn register(
        &mut self,
        target: &Target,
        identity: &Identity,
    ) -> Result<Value, AppError> {
        let target = self.resolve(target)?;
        let result = |status: &str, identity: &Identity| {
            json!({
                "platform": target.platform, "pullRequestUrl": target.web_url,
                "status": status, "identity": identity
            })
        };
        if target.platform == Platform::GitHub {
            let login = identity.unique_name.as_deref().ok_or_else(|| {
                fail(
                    "authentication",
                    "Reviewer registration requires a GitHub login",
                )
            })?;
            let pr = self.transport.request(&target.pr_api(), "GET", None)?;
            let author = Identity::from_account(target.platform, field(&pr, "user")?)?;
            if identity.may_match(&author) {
                return Ok(result("selfIsAuthor", identity));
            }
            for existing in self.reviewers(&target, true, false)? {
                if identity.may_match(&existing) {
                    return Ok(result("alreadyRegistered", &existing));
                }
            }
            self.transport.request(
                &format!("{}/requested_reviewers", target.pr_api()),
                "POST",
                Some(&json!({"reviewers": [login]})),
            )?;
            Ok(result("added", identity))
        } else {
            let id = identity.id.as_deref().ok_or_else(|| {
                fail(
                    "authentication",
                    "Reviewer registration requires an account identifier",
                )
            })?;
            for existing in self.reviewers(&target, false, false)? {
                if identity.may_match(&existing) {
                    return Ok(result("alreadyRegistered", &existing));
                }
            }
            // Only an absent reviewer is written; neutral vote cannot erase an existing vote.
            let response = self.transport.request(
                &format!("{}/reviewers/{}", target.pr_api(), escape(id)),
                "PUT",
                Some(&json!({"id": id, "vote": 0})),
            )?;
            let registered =
                Identity::from_account(target.platform, &response).map_err(|error| {
                    crate::error::caused_by(
                    "ambiguousWrite",
                    "Reviewer registration response is incomplete; human reconciliation is required"
                        .to_owned(),
                    error.into_std_error(),
                )
                })?;
            if !registered.may_match(identity) {
                return Err(fail(
                    "ambiguousWrite",
                    "Azure DevOps did not return the registered identity",
                ));
            }
            Ok(result("added", &registered))
        }
    }

    /// Enumerates raw metadata once, without applying any skill's reviewer filters.
    pub(crate) fn open_pull_requests(
        &mut self,
        repository: &Target,
    ) -> Result<Vec<(NonZeroU32, Value)>, AppError> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        match repository.platform {
            Platform::GitHub => {
                for pr in self
                    .transport
                    .list(&format!("{}/pulls?state=open", repository.repository_api()))?
                {
                    let id = pr_id(field(&pr, "number")?)?;
                    if !seen.insert(id) {
                        return Err(fail(
                            "service",
                            "GitHub repeated a pull request while paging",
                        ));
                    }
                    result.push((id, pr));
                }
            }
            Platform::AzureDevOps => {
                let mut skip = 0_usize;
                loop {
                    let page = self.transport.ado(
                        &format!("{}/pullRequests", repository.repository_api()),
                        "GET",
                        None,
                        &[
                            ("searchCriteria.status", "active".to_owned()),
                            ("$top", "100".to_owned()),
                            ("$skip", skip.to_string()),
                        ],
                        "7.1",
                    )?;
                    if page.continuation.is_some() {
                        return Err(fail(
                            "service",
                            "Unexpected continuation in offset PR listing",
                        ));
                    }
                    let values = ado_items(&page.payload)?;
                    if values.is_empty() {
                        break;
                    }
                    skip = skip
                        .checked_add(values.len())
                        .ok_or_else(|| fail("service", "PR offset overflow"))?;
                    for pr in values {
                        let id = pr_id(field(&pr, "pullRequestId")?)?;
                        if !seen.insert(id) {
                            return Err(fail(
                                "service",
                                "Azure DevOps repeated a pull request while paging",
                            ));
                        }
                        result.push((id, pr));
                    }
                }
            }
        }
        Ok(result)
    }
}

impl Provider for Api<'_> {
    /// Enumerates open PR coordinates completely before selection ranks evidence.
    fn targets(&mut self, repository: &Target) -> Result<Vec<Target>, AppError> {
        Ok(self
            .open_pull_requests(repository)?
            .into_iter()
            .map(|(id, _)| repository.candidate(id))
            .collect())
    }

    /// Refreshes eligibility independently of cooperative discussion reservations.
    ///
    /// Uses commit recency only after author, draft, and reviewer exclusions pass;
    /// claim existence is checked separately by the workflow immediately before use.
    fn candidate(
        &mut self,
        target: &Target,
        identity: &Identity,
        now: Timestamp,
    ) -> Result<Candidate, AppError> {
        let pr = self.transport.request(&target.pr_api(), "GET", None)?;
        let github = target.platform == Platform::GitHub;
        let status = text(field(&pr, if github { "state" } else { "status" })?)?;
        let author = Identity::from_account(
            target.platform,
            field(&pr, if github { "user" } else { "createdBy" })?,
        )?;
        let draft = if github {
            boolean(field(&pr, "draft")?)?
        } else {
            pr.get("isDraft").map(boolean).transpose()?.unwrap_or(false)
        };
        let mut reason = if status != if github { "open" } else { "active" } {
            Some("closed")
        } else if draft {
            Some("draft")
        } else if identity.may_match(&author) {
            Some("selfAuthored")
        } else {
            None
        };
        if reason.is_none() {
            if self
                .reviewers(target, false, true)?
                .iter()
                .any(|reviewer| identity.may_match(reviewer))
            {
                reason = Some("alreadyReviewer");
            }
            if github {
                for review in self
                    .transport
                    .list(&format!("{}/reviews", target.pr_api()))?
                {
                    if text(field(&review, "state")?)?.eq_ignore_ascii_case("PENDING") {
                        continue;
                    }
                    if identity.may_match(&Identity::from_account(
                        target.platform,
                        field(&review, "user")?,
                    )?) {
                        reason = Some("alreadyReviewer");
                    }
                }
            }
        }
        let mut latest = None;
        if reason.is_none() {
            let (time, recent) = self.commit_recency(target, now)?;
            latest = time;
            if !recent {
                reason = Some("noRecentCommit");
            }
        }
        Ok(Candidate {
            target: target.clone(),
            reason: reason.map(str::to_owned),
            latest,
        })
    }

    /// Normalizes undeleted discussion evidence, including resolved Azure threads.
    ///
    /// Creation timestamps remain distinct from edit times so editing a claim
    /// cannot renew its lifetime.
    fn comments(&mut self, target: &Target) -> Result<Vec<Comment>, AppError> {
        let mut comments = Vec::new();
        match target.platform {
            Platform::GitHub => {
                for comment in self.transport.list(&format!(
                    "{}/issues/{}/comments",
                    target.repository_api(),
                    target.pull_request_id
                ))? {
                    comments.push(Comment {
                        id: identifier(field(&comment, "id")?)?,
                        thread_id: None,
                        body: text(field(&comment, "body")?)?.to_owned(),
                        created_at: text(field(&comment, "created_at")?)?.to_owned(),
                        author: Identity::from_account(target.platform, field(&comment, "user")?)?,
                    });
                }
            }
            Platform::AzureDevOps => {
                for thread in self
                    .transport
                    .list(&format!("{}/threads", target.pr_api()))?
                {
                    if thread
                        .get("isDeleted")
                        .map(boolean)
                        .transpose()?
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    for comment in array(field(&thread, "comments")?)? {
                        if comment
                            .get("isDeleted")
                            .map(boolean)
                            .transpose()?
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        comments.push(Comment {
                            id: identifier(field(comment, "id")?)?,
                            thread_id: Some(identifier(field(&thread, "id")?)?),
                            body: comment
                                .get("content")
                                .filter(|value| !value.is_null())
                                .map(text)
                                .transpose()?
                                .unwrap_or_default()
                                .to_owned(),
                            created_at: text(field(comment, "publishedDate")?)?.to_owned(),
                            author: Identity::from_account(
                                target.platform,
                                field(comment, "author")?,
                            )?,
                        });
                    }
                }
            }
        }
        Ok(comments)
    }

    /// Creates the journaled announcement and requires usable ownership identifiers.
    ///
    /// An incomplete reply remains an ambiguous write rather than permission
    /// to create another comment.
    fn create_claim(&mut self, target: &Target, body: &str) -> Result<RemoteClaim, AppError> {
        let outcome = match target.platform {
            Platform::GitHub => {
                let response = self.transport.request(
                    &format!(
                        "{}/issues/{}/comments",
                        target.repository_api(),
                        target.pull_request_id
                    ),
                    "POST",
                    Some(&json!({"body": body})),
                )?;
                field(&response, "id")
                    .and_then(identifier)
                    .map(|id| RemoteClaim {
                        id,
                        thread_id: None,
                    })
            }
            Platform::AzureDevOps => {
                // A claim is coordination, not an outstanding review task; create it resolved.
                let response = self.transport.request(&format!("{}/threads", target.pr_api()), "POST", Some(&json!({"status": 2, "comments": [{"parentCommentId": 0, "content": body, "commentType": 1}]})))?;
                let parse = || -> Result<RemoteClaim, AppError> {
                    let status = field(&response, "status")?;
                    if status.as_str() != Some("fixed") && status.as_u64() != Some(2) {
                        return Err(fail(
                            "ambiguousWrite",
                            "Azure DevOps did not resolve the claim thread",
                        ));
                    }
                    let comment =
                        array(field(&response, "comments")?)?
                            .first()
                            .ok_or_else(|| {
                                fail("ambiguousWrite", "Claim creation returned no comment")
                            })?;
                    Ok(RemoteClaim {
                        id: identifier(field(comment, "id")?)?,
                        thread_id: Some(identifier(field(&response, "id")?)?),
                    })
                };
                parse()
            }
        };
        outcome.map_err(|error| {
            crate::error::caused_by(
                "ambiguousWrite",
                "Claim creation response is incomplete; human reconciliation is required"
                    .to_owned(),
                error.into_std_error(),
            )
            .into()
        })
    }

    /// Deletes only the comment whose ownership the workflow has already checked.
    ///
    /// Azure thread coordinates locate the comment; the containing thread is
    /// never deleted as part of reservation cleanup.
    fn remove_claim(&mut self, target: &Target, comment: &Comment) -> Result<(), AppError> {
        let path = match target.platform {
            Platform::GitHub => format!(
                "{}/issues/comments/{}",
                target.repository_api(),
                escape(&comment.id)
            ),
            Platform::AzureDevOps => format!(
                "{}/threads/{}/comments/{}",
                target.pr_api(),
                escape(
                    comment
                        .thread_id
                        .as_deref()
                        .ok_or_else(|| fail("invalidState", "Missing owned thread identifier"))?
                ),
                escape(&comment.id)
            ),
        };
        self.transport.request(&path, "DELETE", None)?;
        Ok(())
    }
}

/// Establishes concrete PR identity before enumeration creates workflow targets.
fn pr_id(value: &Value) -> Result<NonZeroU32, AppError> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .and_then(NonZeroU32::new)
        .ok_or_else(|| fail("service", "Missing or invalid pull request identifier"))
}
