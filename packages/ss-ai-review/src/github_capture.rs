//! Collects GitHub discussion evidence into the shared pre-document shape.

use std::collections::{HashMap, HashSet};

use ohno::AppError;
use serde_json::{Value, json};

use crate::clock::timestamp;
use crate::error::fail;
use crate::identity::Identity;
use crate::json::{array, boolean, field, identifier, nullable_field, text};
use crate::target::{Platform, Target, escape};
use crate::transport::Transport;

// Nested connections multiply their requested sizes, so stay below GitHub's node limit.
const PAGE_SIZE: u32 = 50;
// This bounds an indefinitely advancing malformed connection, not normal discussion size.
const MAX_PAGES: usize = 1000;

const THREADS: &str = "query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 reviewThreads(first:$size,after:$cur){pageInfo{hasNextPage endCursor}
 nodes{id isResolved isOutdated isCollapsed path line originalLine startLine originalStartLine
 diffSide startDiffSide subjectType resolvedBy{login}
 comments(first:$size){pageInfo{hasNextPage endCursor}
 nodes{id databaseId url body createdAt lastEditedAt replyTo{id} state
 author{login __typename ... on User{id databaseId url} ... on Bot{id databaseId url}
 ... on Organization{id databaseId url} ... on Mannequin{id databaseId url}}
 authorAssociation minimizedReason}}}}}}}";
const THREAD_COMMENTS: &str = "query($threadId:ID!,$cur:String,$size:Int!){
 node(id:$threadId){... on PullRequestReviewThread{
 comments(first:$size,after:$cur){pageInfo{hasNextPage endCursor}
 nodes{id databaseId url body createdAt lastEditedAt replyTo{id} state
 author{login __typename ... on User{id databaseId url} ... on Bot{id databaseId url}
 ... on Organization{id databaseId url} ... on Mannequin{id databaseId url}}
 authorAssociation minimizedReason}}}}}";
const ISSUE_METADATA: &str = "query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 comments(first:$size,after:$cur){pageInfo{hasNextPage endCursor} nodes{id lastEditedAt}}}}}";
const REVIEW_METADATA: &str = "query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 reviews(first:$size,after:$cur){pageInfo{hasNextPage endCursor} nodes{id lastEditedAt comments{totalCount}}}}}}";
const FORCE_PUSHES: &str = "query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 timelineItems(first:$size,after:$cur,itemTypes:[HEAD_REF_FORCE_PUSHED_EVENT]){
 pageInfo{hasNextPage endCursor}
 nodes{__typename ... on HeadRefForcePushedEvent{id createdAt
 actor{login __typename ... on User{id databaseId url} ... on Bot{id databaseId url}
 ... on Organization{id databaseId url} ... on Mannequin{id databaseId url}}
 beforeCommit{oid} afterCommit{oid}}}}}}}";

/// Reads the complete provider capture without performing remote writes.
pub(crate) fn collect(transport: &mut Transport<'_>, target: &Target) -> Result<Value, AppError> {
    if target.platform != Platform::GitHub
        || transport.platform != Platform::GitHub
        || target.pull_request_id == 0
        || target.organization.trim().is_empty()
        || target.repository.trim().is_empty()
    {
        return Err(fail("invalidInput", "A GitHub PR target is required"));
    }
    let pr = transport.request(&target.pr_api(), "GET", None)?;
    if identifier(field(&pr, "number")?)? != target.pull_request_id.to_string() {
        return Err(fail("service", "GitHub returned another pull request"));
    }
    let reviews = transport.list(&format!("{}/reviews", target.pr_api()))?;
    let issue_comments = transport.list(&format!(
        "{}/issues/{}/comments",
        target.repository_api(),
        target.pull_request_id
    ))?;
    let requested = transport.list(&format!("{}/requested_reviewers", target.pr_api()))?;
    let mut submitted = Vec::new();
    for review in reviews {
        match text(field(&review, "state")?)? {
            "PENDING" => {}
            "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED" | "DISMISSED" => {
                submitted.push(review);
            }
            _ => return Err(fail("service", "Unknown GitHub review state")),
        }
    }
    unique(&submitted, "node_id")?;
    unique(&issue_comments, "node_id")?;

    let thread_nodes = connection(
        transport,
        target,
        THREADS,
        "/data/repository/pullRequest/reviewThreads",
        None,
        None,
    )?;
    unique(&thread_nodes, "id")?;
    let mut threads = Vec::new();
    let mut comment_ids = HashSet::new();
    for node in thread_nodes {
        let id = nonempty(field(&node, "id")?)?;
        let first = field(&node, "comments")?.clone();
        let comments = connection(
            transport,
            target,
            THREAD_COMMENTS,
            "/data/node/comments",
            Some(id),
            Some(first),
        )?;
        unique(&comments, "id")?;
        let mut visible = Vec::new();
        for comment in comments {
            match text(field(&comment, "state")?)? {
                "PENDING" => {}
                "SUBMITTED" => {
                    if !comment_ids.insert(nonempty(field(&comment, "id")?)?.to_owned()) {
                        return Err(fail(
                            "service",
                            "GitHub repeated a review comment across threads",
                        ));
                    }
                    visible.push(comment);
                }
                _ => return Err(fail("service", "Unknown GitHub review comment state")),
            }
        }
        if !visible.is_empty() {
            threads.push(review_thread(&node, &visible)?);
        }
    }
    let issue_metadata = metadata(connection(
        transport,
        target,
        ISSUE_METADATA,
        "/data/repository/pullRequest/comments",
        None,
        None,
    )?)?;
    let review_metadata = metadata(connection(
        transport,
        target,
        REVIEW_METADATA,
        "/data/repository/pullRequest/reviews",
        None,
        None,
    )?)?;
    let pushes = connection(
        transport,
        target,
        FORCE_PUSHES,
        "/data/repository/pullRequest/timelineItems",
        None,
        None,
    )?;
    unique(&pushes, "id")?;

    for review in &submitted {
        let state = text(field(review, "state")?)?;
        let body = body(review, "body")?;
        let meta = review_metadata
            .get(nonempty(field(review, "node_id")?)?)
            .ok_or_else(|| fail("service", "Review metadata is missing"))?;
        if state == "COMMENTED" && body.trim().is_empty() {
            continue;
        }
        threads.push(summary_thread(review, meta)?);
    }
    for comment in &issue_comments {
        let meta = issue_metadata
            .get(nonempty(field(comment, "node_id")?)?)
            .ok_or_else(|| fail("service", "Issue comment metadata is missing"))?;
        threads.push(issue_thread(comment, meta)?);
    }
    for push in &pushes {
        threads.push(force_push(push)?);
    }
    let reviewers = reviewers(&submitted, &requested)?;
    let authenticated = rest_identity(&transport.request("user", "GET", None)?)?;
    if authenticated.id.is_none() {
        return Err(fail(
            "authentication",
            "GitHub did not identify the authenticated account",
        ));
    }
    let base = field(&pr, "base")?;
    let head = field(&pr, "head")?;
    let base_repo = field(base, "repo")?;
    let expected_repository = format!("{}/{}", target.organization, target.repository);
    if !text(field(base_repo, "full_name")?)?.eq_ignore_ascii_case(&expected_repository) {
        return Err(fail(
            "service",
            "GitHub returned a PR from another repository",
        ));
    }
    let base_sha = text(field(base, "sha")?)?;
    let head_sha = text(field(head, "sha")?)?;
    ss_ai_assessment::commit(base_sha)?;
    ss_ai_assessment::commit(head_sha)?;
    let head_repo = nullable_field(head, "repo")?;
    let fork = !head_repo.is_null()
        && !text(field(head_repo, "full_name")?)?
            .eq_ignore_ascii_case(text(field(base_repo, "full_name")?)?);
    let merge_base = merge_base(transport, target, base_sha, head_sha, head_repo, fork)?;
    let source_ref = source_ref(head, head_repo, fork)?;
    unique(&threads, "id")?;
    Ok(json!({
        "target": target,
        "authenticatedIdentity": authenticated,
        "pullRequest": {
            "id": field(&pr, "number")?, "title": text(field(&pr, "title")?)?,
            "description": body(&pr, "body")?, "isDraft": boolean(field(&pr, "draft")?)?,
            "author": rest_identity(nullable_field(&pr, "user")?)?,
            "createdUtc": utc(field(&pr, "created_at")?)?,
            "sourceRef": source_ref, "targetRef": nonempty(field(base, "ref")?)?,
            "reviewedSourceCommit": head_sha, "reviewedMergeBaseCommit": merge_base,
            "mergeState": {"status": nullable_field(&pr, "mergeable_state")?,
                "isMergeable": nullable_field(&pr, "mergeable")?,
                "mergeCommit": nullable_field(&pr, "merge_commit_sha")?}
        },
        "reviewers": reviewers, "revisions": [], "threads": threads
    }))
}

/// Keeps an owner-qualified branch name whenever its repository is not local.
fn source_ref<'a>(head: &'a Value, head_repo: &Value, fork: bool) -> Result<&'a str, AppError> {
    // A deleted head repository cannot prove that the branch was local. GitHub's
    // label retains the owner and avoids making an ambiguous branch name local.
    nonempty(field(
        head,
        if fork || head_repo.is_null() {
            "label"
        } else {
            "ref"
        },
    )?)
}

/// Resolves an immutable merge base without treating other refusals as absence.
fn merge_base(
    transport: &Transport<'_>,
    target: &Target,
    base: &str,
    head: &str,
    head_repo: &Value,
    fork: bool,
) -> Result<String, AppError> {
    let compare = format!("{}/compare/", target.repository_api());
    // Fork-qualified commit comparisons use GitHub's cross-repository syntax.
    // Only a proven 404 justifies trying the shared commit network instead.
    let qualified = if fork {
        let owner = nonempty(field(field(head_repo, "owner")?, "login")?)?;
        transport.github_get_if_found(&format!(
            "{compare}{}...{}:{}",
            escape(base),
            escape(owner),
            escape(head)
        ))?
    } else {
        None
    };
    let comparison = if let Some(comparison) = qualified {
        comparison
    } else {
        transport
            .github_get_if_found(&format!("{compare}{}...{}", escape(base), escape(head)))?
            .ok_or_else(|| fail("service", "GitHub could not resolve the merge base"))?
    };
    let sha = text(field(field(&comparison, "merge_base_commit")?, "sha")?)?;
    ss_ai_assessment::commit(sha)?;
    Ok(sha.to_owned())
}

/// Walks one connection independently, rejecting missing or repeated cursors.
fn connection(
    transport: &Transport<'_>,
    target: &Target,
    query: &str,
    path: &str,
    thread_id: Option<&str>,
    first: Option<Value>,
) -> Result<Vec<Value>, AppError> {
    let mut cursor: Option<String> = None;
    let mut first = first;
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    for _ in 0..MAX_PAGES {
        let page = if let Some(page) = first.take() {
            page
        } else {
            let variables = json!({"owner":target.organization,"repo":target.repository,
                "num":target.pull_request_id,"threadId":thread_id,"cur":cursor,"size":PAGE_SIZE});
            let response = transport.graphql(query, &variables)?;
            response
                .pointer(path)
                .cloned()
                .ok_or_else(|| fail("service", "Missing GitHub connection"))?
        };
        nodes.extend_from_slice(array(field(&page, "nodes")?)?);
        let page_info = field(&page, "pageInfo")?;
        let end_cursor = nullable_field(page_info, "endCursor")?;
        if !end_cursor.is_null() {
            text(end_cursor)?;
        }
        if !boolean(field(page_info, "hasNextPage")?)? {
            return Ok(nodes);
        }
        let next = nonempty(end_cursor)?.to_owned();
        if !seen.insert(next.clone()) {
            return Err(fail("service", "GitHub repeated a pagination cursor"));
        }
        cursor = Some(next);
    }
    Err(fail("service", "GitHub connection exceeded the page limit"))
}

/// Keeps case-sensitive node identifiers unique at each service boundary.
fn unique(nodes: &[Value], key: &str) -> Result<(), AppError> {
    let mut seen = HashSet::new();
    for node in nodes {
        if !seen.insert(nonempty(field(node, key)?)?) {
            return Err(fail("service", format!("Duplicate GitHub {key}")));
        }
    }
    Ok(())
}

/// Indexes metadata without silently replacing ambiguous node identifiers.
fn metadata(nodes: Vec<Value>) -> Result<HashMap<String, Value>, AppError> {
    unique(&nodes, "id")?;
    let mut result = HashMap::new();
    for node in nodes {
        let id = nonempty(field(&node, "id")?)?.to_owned();
        nullable_field(&node, "lastEditedAt")?;
        result.insert(id, node);
    }
    Ok(result)
}

/// Reads an opaque identifier without accepting empty or malformed evidence.
fn nonempty(value: &Value) -> Result<&str, AppError> {
    let value = text(value)?;
    if value.trim().is_empty() {
        Err(fail("service", "Empty GitHub identifier"))
    } else {
        Ok(value)
    }
}

/// GitHub uses explicit null for absent bodies, not an omitted field.
fn body<'a>(node: &'a Value, name: &str) -> Result<&'a str, AppError> {
    let value = nullable_field(node, name)?;
    if value.is_null() { Ok("") } else { text(value) }
}

/// Converts service times to the PowerShell model's UTC round-trip form.
fn utc(value: &Value) -> Result<Value, AppError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let parsed = timestamp(text(value)?)?;
    let whole = parsed.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    Ok(json!(format!(
        "{whole}.{:07}Z",
        parsed
            .subsec_nanosecond()
            .checked_div(100)
            .expect("A fixed nonzero divisor")
    )))
}

/// Preserves optional account evidence for deleted GitHub actors.
fn rest_identity(value: &Value) -> Result<Identity, AppError> {
    if value.is_null() {
        Identity::from_account(Platform::GitHub, &json!({}))
    } else {
        Identity::from_account(Platform::GitHub, value)
    }
}

/// GraphQL and REST use different field names for the same account evidence.
fn actor_identity(value: &Value) -> Result<Identity, AppError> {
    if value.is_null() {
        return rest_identity(value);
    }
    rest_identity(&json!({"id":nullable_field(value,"databaseId")?,
        "login":nullable_field(value,"login")?, "node_id":nullable_field(value,"id")?,
        "type":nullable_field(value,"__typename")?, "html_url":nullable_field(value,"url")?}))
}

/// Distinguishes a service account or explicit agent marker from unknown authorship.
fn authorship(identity: &Identity, content: &str) -> (String, Vec<String>) {
    let mut evidence: Vec<String> = identity
        .evidence
        .iter()
        .map(|item| format!("identity:{item}"))
        .collect();
    let mut kind = if identity.kind == "service" {
        "agent"
    } else {
        "indeterminate"
    };
    if content.trim_start().starts_with("[Copilot speaking]") {
        kind = "agent";
        evidence.push("content:agentMarker".to_owned());
    }
    (kind.to_owned(), evidence)
}

/// Converts a GraphQL comment while preserving its body and reply relationship.
fn inline_comment(node: &Value) -> Result<Value, AppError> {
    let identity = actor_identity(nullable_field(node, "author")?)?;
    let content = body(node, "body")?;
    let (authorship, evidence) = authorship(&identity, content);
    let edited = nullable_field(node, "lastEditedAt")?;
    let reply = nullable_field(node, "replyTo")?;
    let parent = if reply.is_null() {
        Value::Null
    } else {
        field(reply, "id")?.clone()
    };
    Ok(
        json!({"id":nonempty(field(node,"id")?)?, "url":nullable_field(node,"url")?,
        "parentId":parent, "author":identity, "content":content,
        "publishedUtc":utc(field(node,"createdAt")?)?, "lastEditedUtc":utc(edited)?,
        "isEdited":!edited.is_null(), "authorship":authorship, "authorshipEvidence":evidence}),
    )
}

/// Retains thread state, structural opener, and all submitted comment metadata.
fn review_thread(node: &Value, comments: &[Value]) -> Result<Value, AppError> {
    let mut canonical = Vec::new();
    let mut minimized = Vec::new();
    for comment in comments {
        canonical.push(inline_comment(comment)?);
        let reason = nullable_field(comment, "minimizedReason")?;
        if !reason.is_null() {
            minimized.push(json!({"id":field(comment,"id")?, "reason":reason}));
        }
    }
    let mut roots = canonical
        .iter()
        .filter(|comment| comment["parentId"].is_null());
    let root = roots
        .next()
        .ok_or_else(|| fail("service", "GitHub review thread has no opener"))?;
    if roots.next().is_some() {
        return Err(fail(
            "service",
            "GitHub review thread has an ambiguous opener",
        ));
    }
    let opener = field(root, "id")?.clone();
    // Every reply must reach the unique opener; missing parents and cycles do not
    // establish ownership of a thread even when all comment bodies are present.
    let parents: HashMap<_, _> = canonical
        .iter()
        .map(|comment| {
            Ok((
                nonempty(field(comment, "id")?)?,
                nullable_field(comment, "parentId")?,
            ))
        })
        .collect::<Result<_, AppError>>()?;
    for comment in &canonical {
        let mut current = nonempty(field(comment, "id")?)?;
        let mut seen = HashSet::new();
        let mut reached_root = false;
        for _ in 0..=canonical.len() {
            if !seen.insert(current) {
                return Err(fail("service", "GitHub review thread has a reply cycle"));
            }
            let parent = parents
                .get(current)
                .ok_or_else(|| fail("service", "GitHub review reply has a missing parent"))?;
            if parent.is_null() {
                reached_root = true;
                break;
            }
            current = nonempty(parent)?;
        }
        if !reached_root {
            return Err(fail(
                "service",
                "GitHub review reply cannot reach its opener",
            ));
        }
    }
    // The connection order is not ownership evidence; a reply can precede its
    // opener in a returned page, so the opener supplies the thread's event.
    let published = nullable_field(root, "publishedUtc")?.clone();
    let url = nullable_field(root, "url")?.clone();
    let mut updated = published.clone();
    for comment in &canonical {
        for key in ["publishedUtc", "lastEditedUtc"] {
            let candidate = &comment[key];
            if !candidate.is_null() && (updated.is_null() || candidate.as_str() > updated.as_str())
            {
                updated = candidate.clone();
            }
        }
    }
    let path = nullable_field(node, "path")?;
    let line = nullable_field(node, "line")?;
    let start = nullable_field(node, "startLine")?;
    let original = nullable_field(node, "originalLine")?;
    let original_start = nullable_field(node, "originalStartLine")?;
    let side = nullable_field(node, "diffSide")?;
    let context = if path.is_null() || path.as_str() == Some("") {
        Value::Null
    } else {
        let side_lower = if side.is_null() {
            None
        } else {
            Some(text(side)?.to_ascii_lowercase())
        };
        json!({"filePath":text(path)?, "side":side_lower,
            "startLine":if start.is_null(){line}else{start}, "endLine":line,
            "originalStartLine":if original_start.is_null(){original}else{original_start},
            "originalEndLine":original})
    };
    let resolved = boolean(field(node, "isResolved")?)?;
    let resolved_by = nullable_field(node, "resolvedBy")?;
    let resolver = if resolved_by.is_null() {
        Value::Null
    } else {
        field(resolved_by, "login")?.clone()
    };
    Ok(
        json!({"id":field(node,"id")?, "url":url, "publishedUtc":published,
        "lastUpdatedUtc":updated, "eventUtc":published,
        "rawStatus":if resolved {"resolved"} else {"unresolved"},
        "resolution":if resolved {"resolved"} else {"active"}, "kind":"text",
        "isOutdated":boolean(field(node,"isOutdated")?)?, "context":context,
        "anchorKey":field(node,"id")?, "properties":{
            "isCollapsed":boolean(field(node,"isCollapsed")?)?,
            "subjectType":nullable_field(node,"subjectType")?, "diffSide":side,
            "startDiffSide":nullable_field(node,"startDiffSide")?, "resolvedBy":resolver,
            "minimizedComments":minimized},
        "openingCommentId":opener, "decision":null, "comments":canonical}),
    )
}

/// Converts REST discussion text using its independently joined edit evidence.
fn text_thread(node: &Value, meta: &Value, review: bool) -> Result<Value, AppError> {
    let identity = rest_identity(nullable_field(node, "user")?)?;
    let content = body(node, "body")?;
    let (mut author_kind, mut evidence) = authorship(&identity, content);
    if review
        && author_kind == "indeterminate"
        && content.trim().is_empty()
        && ["APPROVED", "CHANGES_REQUESTED", "DISMISSED"].contains(&text(field(node, "state")?)?)
    {
        let count = field(field(meta, "comments")?, "totalCount")?
            .as_u64()
            .ok_or_else(|| fail("service", "GitHub review lacks a valid comment count"))?;
        if count == 0 {
            "human".clone_into(&mut author_kind);
            evidence.push("review:voteOnly".to_owned());
        }
    }
    let id = nonempty(field(node, "node_id")?)?;
    let url = nullable_field(node, "html_url")?;
    let published = utc(field(
        node,
        if review { "submitted_at" } else { "created_at" },
    )?)?;
    let edited_raw = nullable_field(meta, "lastEditedAt")?;
    let edited = utc(edited_raw)?;
    let updated = if edited.is_null() {
        &published
    } else {
        &edited
    };
    let comment = json!({"id":id,"url":url,"parentId":null,"author":identity,"content":content,
        "publishedUtc":published,"lastEditedUtc":edited,"isEdited":!edited_raw.is_null(),
        "authorship":author_kind,"authorshipEvidence":evidence});
    let properties = if review {
        json!({"reviewId":field(node,"id")?, "state":field(node,"state")?,
            "authorAssociation":nullable_field(node,"author_association")?,
            "commitId":nullable_field(node,"commit_id")?})
    } else {
        json!({"commentId":field(node,"id")?,
            "authorAssociation":nullable_field(node,"author_association")?,
            "isMinimized":node.get("minimized").map(boolean).transpose()?.unwrap_or(false)})
    };
    Ok(
        json!({"id":id,"url":url,"publishedUtc":published,"lastUpdatedUtc":updated,
        "eventUtc":published,"rawStatus":null,"resolution":"indeterminate",
        "kind":if review {"reviewSummary"} else {"text"}, "isOutdated":false,
        "context":null,"anchorKey":id,"properties":properties,"openingCommentId":id,
        "decision":null,"comments":[comment]}),
    )
}

/// Represents submitted review prose separately from inline comments.
fn summary_thread(review: &Value, metadata: &Value) -> Result<Value, AppError> {
    text_thread(review, metadata, true)
}

/// Represents a non-threaded issue comment as its own canonical thread.
fn issue_thread(comment: &Value, metadata: &Value) -> Result<Value, AppError> {
    text_thread(comment, metadata, false)
}

/// Preserves explicit before/after commit evidence for branch rewrites.
fn force_push(node: &Value) -> Result<Value, AppError> {
    if text(field(node, "__typename")?)? != "HeadRefForcePushedEvent" {
        return Err(fail("service", "Unexpected GitHub timeline event"));
    }
    let id = nonempty(field(node, "id")?)?;
    let identity = actor_identity(nullable_field(node, "actor")?)?;
    let (authorship, evidence) = authorship(&identity, "");
    let at = utc(field(node, "createdAt")?)?;
    let before = text(field(field(node, "beforeCommit")?, "oid")?)?;
    let after = text(field(field(node, "afterCommit")?, "oid")?)?;
    ss_ai_assessment::commit(before)?;
    ss_ai_assessment::commit(after)?;
    Ok(
        json!({"id":id,"url":null,"publishedUtc":at,"lastUpdatedUtc":at,"eventUtc":at,
        "rawStatus":"HeadRefForcePushedEvent","resolution":"indeterminate","kind":"refUpdate",
        "isOutdated":false,"context":null,"anchorKey":id,
        "properties":{"beforeCommit":before,"afterCommit":after},"openingCommentId":null,
        "decision":{"type":"refUpdate","value":after,"label":"force pushed","by":identity,
            "utc":at,"authorship":authorship,"authorshipEvidence":evidence},"comments":[]}),
    )
}

/// Combines submitted reviewers with all requested users and teams.
fn reviewers(reviews: &[Value], pages: &[Value]) -> Result<Vec<Value>, AppError> {
    let mut identities = Vec::new();
    for review in reviews {
        let account = nullable_field(review, "user")?;
        // A deleted author has no roster identity, but the review itself remains
        // in the discussion with its unknown author and unmodified body.
        if account.is_null() {
            continue;
        }
        let identity = rest_identity(account)?;
        add_reviewer(&mut identities, identity, false)?;
    }
    if pages.is_empty() {
        return Err(fail("service", "Missing GitHub requested reviewers page"));
    }
    for page in pages {
        for user in array(field(page, "users")?)? {
            add_reviewer(&mut identities, rest_identity(user)?, false)?;
        }
        for team in array(field(page, "teams")?)? {
            add_reviewer(&mut identities, Identity::from_team(team)?, true)?;
        }
    }
    Ok(identities
        .into_iter()
        .map(|(identity, team)| json!({"identity":identity,"isRequired":false,"isTeam":team}))
        .collect())
}

/// Deduplicates the roster using the strongest shared identity evidence.
fn add_reviewer(
    result: &mut Vec<(Identity, bool)>,
    identity: Identity,
    team: bool,
) -> Result<(), AppError> {
    if team && identity.unique_name.is_none() {
        return Err(fail("service", "Missing team slug"));
    }
    if identity.id.is_none() && identity.descriptor.is_none() && identity.unique_name.is_none() {
        return Err(fail("service", "Missing reviewer identity"));
    }
    if result
        .iter()
        .any(|(existing, is_team)| *is_team == team && same_reviewer(existing, &identity))
    {
        return Ok(());
    }
    result.push((identity, team));
    Ok(())
}

/// Avoids weaker login matches when shared immutable identifiers disagree.
fn same_reviewer(left: &Identity, right: &Identity) -> bool {
    if let Some((left, right)) = left.descriptor.as_deref().zip(right.descriptor.as_deref()) {
        return left == right;
    }
    if let Some((left, right)) = left.id.as_deref().zip(right.id.as_deref()) {
        return left == right;
    }
    left.unique_name
        .as_deref()
        .zip(right.unique_name.as_deref())
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    reason = "Tests use bounded JSON fixtures and consume values through the JSON macro"
)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};

    use super::{
        collect, connection, force_push, merge_base, metadata, review_thread, reviewers,
        source_ref, utc,
    };
    use crate::target::Target;
    use crate::transport::{Http, HttpResponse, Transport};
    use crate::{Clock, Process, ProcessOutput};

    /// Strictly ordered replies assert the adapter's read-only transport contract.
    struct Recording {
        replies: RefCell<VecDeque<(&'static str, Value)>>,
    }

    impl Process for Recording {
        fn execute(
            &self,
            program: &str,
            arguments: &[String],
            input: &[u8],
        ) -> io::Result<ProcessOutput> {
            assert_eq!(program, "gh");
            let (expected, mut reply) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(arguments[1], expected);
            let method =
                &arguments[arguments.iter().position(|arg| arg == "--method").unwrap() + 1];
            assert_eq!(method, if expected == "graphql" { "POST" } else { "GET" });
            if expected.contains("per_page=100") {
                assert!(arguments.iter().any(|arg| arg == "--paginate"));
                assert!(arguments.iter().any(|arg| arg == "--slurp"));
            }
            if expected == "graphql" {
                let query: Value = serde_json::from_slice(input).unwrap();
                assert!(query["query"].as_str().unwrap().starts_with("query"));
                if let Some(expected_cursor) =
                    reply.as_object_mut().unwrap().remove("__expectedCursor")
                {
                    assert_eq!(query["variables"]["cur"], expected_cursor);
                }
                if let Some(expected_thread) =
                    reply.as_object_mut().unwrap().remove("__expectedThread")
                {
                    assert_eq!(query["variables"]["threadId"], expected_thread);
                }
            } else {
                assert!(input.is_empty());
            }
            let status = reply
                .as_object_mut()
                .and_then(|object| object.remove("__status"))
                .and_then(|value| value.as_u64())
                .unwrap_or(200);
            let stderr = reply
                .as_object_mut()
                .and_then(|object| object.remove("__stderr"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let include_headers = reply
                .as_object_mut()
                .and_then(|object| object.remove("__includeHeaders"))
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let payload = if status != 200 && include_headers {
                format!("HTTP/2 {status}\r\n\r\n{reply}")
            } else {
                reply.to_string()
            };
            Ok(ProcessOutput::new(
                status < 400,
                payload.into_bytes(),
                stderr.into_bytes(),
            ))
        }
    }

    impl Http for Recording {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            panic!("GitHub capture must use the authenticated CLI")
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            "2026-01-01T00:00:00Z".parse().unwrap()
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            panic!("no retries expected")
        }
    }

    /// Builds a response connection without depending on external test resources.
    fn page(nodes: Value, next: bool, cursor: Value) -> Value {
        json!({"nodes":nodes,"pageInfo":{"hasNextPage":next,"endCursor":cursor}})
    }

    #[test]
    fn captures_complete_paginated_discussion_and_immutable_merge_base() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let c = "cccccccccccccccccccccccccccccccccccccccc";
        let account = json!({"id":7,"login":"alice","node_id":"U_7","type":"User","html_url":"https://github.com/alice"});
        let actor = json!({"databaseId":7,"login":"alice","id":"U_7","__typename":"User","url":"https://github.com/alice"});
        let comment = |id, parent: Value, body| {
            json!({"id":id,"databaseId":1,"url":"https://github.com/comment",
            "body":body,"createdAt":"2026-01-02T03:04:05+01:00","lastEditedAt":null,
            "replyTo":parent,"state":"SUBMITTED","author":actor,"authorAssociation":"MEMBER","minimizedReason":null})
        };
        let first = page(
            json!([comment("C1", Value::Null, "é\nexact")]),
            true,
            json!("more"),
        );
        let thread = json!({"id":"T1","isResolved":true,"isOutdated":false,"isCollapsed":false,
            "path":"src/x.rs","line":4,"originalLine":3,"startLine":null,"originalStartLine":null,
            "diffSide":"RIGHT","startDiffSide":null,"subjectType":"LINE","resolvedBy":{"login":"alice"},
            "comments":first});
        let review = json!({"id":9,"node_id":"R1","state":"APPROVED","body":"","user":account,
            "submitted_at":"2026-01-02T03:04:06Z","html_url":"https://github.com/review",
            "author_association":"MEMBER","commit_id":b});
        let issue = json!({"id":10,"node_id":"I1","body":"[Copilot speaking] hi","user":account,
            "created_at":"2026-01-03T03:04:06Z","html_url":"https://github.com/issue",
            "author_association":"MEMBER"});
        let issue_two = json!({"id":11,"node_id":"I2","body":"later\r\n🐙","user":account,
            "created_at":"2026-01-04T03:04:06Z","html_url":"https://github.com/issue2",
            "author_association":"MEMBER"});
        let pr = json!({"number":42,"title":"Exact","body":"line\r\né","draft":false,"user":account,
            "created_at":"2026-01-01T00:00:00Z","head":{"sha":b,"ref":"feature","repo":{"full_name":"acme/repo"}},
            "base":{"sha":a,"ref":"main","repo":{"full_name":"acme/repo"}},
            "mergeable_state":"clean","mergeable":true,"merge_commit_sha":null});
        let wrap = |name: &str, connection: Value| json!({"data":{"repository":{"pullRequest":{name:connection}}}});
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![
                ("repos/acme/repo/pulls/42", pr),
                (
                    "repos/acme/repo/pulls/42/reviews?per_page=100",
                    json!([[review]]),
                ),
                (
                    "repos/acme/repo/issues/42/comments?per_page=100",
                    json!([[issue], [issue_two]]),
                ),
                (
                    "repos/acme/repo/pulls/42/requested_reviewers?per_page=100",
                    json!([{"users":[account],"teams":[]}]),
                ),
                (
                    "graphql",
                    wrap(
                        "reviewThreads",
                        page(json!([thread]), true, json!("threads-next")),
                    ),
                ),
                (
                    "graphql",
                    json!({"__expectedCursor":"threads-next","data":{"repository":{"pullRequest":{
                        "reviewThreads":page(json!([]),false,Value::Null)}}}}),
                ),
                (
                    "graphql",
                    json!({"data":{"node":{"comments":page(json!([comment("C2",json!({"id":"C1"}),"reply")]),false,Value::Null)}}}),
                ),
                (
                    "graphql",
                    wrap(
                        "comments",
                        page(
                            json!([{"id":"I1","lastEditedAt":null}]),
                            true,
                            json!("issues-next"),
                        ),
                    ),
                ),
                (
                    "graphql",
                    json!({"__expectedCursor":"issues-next","data":{"repository":{"pullRequest":{
                        "comments":page(json!([{"id":"I2","lastEditedAt":"2026-01-05T00:00:00Z"}]),false,Value::Null)}}}}),
                ),
                (
                    "graphql",
                    wrap(
                        "reviews",
                        page(
                            json!([{"id":"R1","lastEditedAt":null,"comments":{"totalCount":0}}]),
                            false,
                            Value::Null,
                        ),
                    ),
                ),
                (
                    "graphql",
                    wrap("timelineItems", page(json!([]), false, Value::Null)),
                ),
                ("user", account),
                (
                    "repos/acme/repo/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    json!({"merge_base_commit":{"sha":c}}),
                ),
            ])),
        };
        let mut transport = Transport::new(&target, None, &script, &script, &FixedClock);
        let captured = collect(&mut transport, &target).unwrap();
        assert!(script.replies.borrow().is_empty());
        assert_eq!(captured["pullRequest"]["description"], "line\r\né");
        assert_eq!(captured["pullRequest"]["reviewedMergeBaseCommit"], c);
        assert_eq!(captured["reviewers"].as_array().unwrap().len(), 1);
        assert_eq!(captured["threads"].as_array().unwrap().len(), 4);
        assert_eq!(captured["threads"][0]["comments"][0]["content"], "é\nexact");
        assert_eq!(captured["threads"][0]["comments"][1]["parentId"], "C1");
        assert_eq!(captured["threads"][0]["openingCommentId"], "C1");
        assert_eq!(captured["threads"][1]["comments"][0]["authorship"], "human");
        assert_eq!(captured["threads"][2]["comments"][0]["authorship"], "agent");
        assert_eq!(
            captured["threads"][3]["comments"][0]["content"],
            "later\r\n🐙"
        );
        assert_eq!(
            captured["threads"][3]["comments"][0]["lastEditedUtc"],
            "2026-01-05T00:00:00.0000000Z"
        );
        assert_eq!(
            captured["threads"][0]["publishedUtc"],
            "2026-01-02T02:04:05.0000000Z"
        );
    }

    #[test]
    fn shared_fixture_matches_powershell_pre_document() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/github.json"
        ))
        .unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ss-review-native-capture/github-context.json"
        ))
        .unwrap();
        let target: Target = serde_json::from_value(fixture["target"].clone()).unwrap();
        let responses = &fixture["responses"];
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![
                (
                    "repos/octo/widget/pulls/7",
                    responses["pullRequest"].clone(),
                ),
                (
                    "repos/octo/widget/pulls/7/reviews?per_page=100",
                    json!([responses["reviews"]]),
                ),
                (
                    "repos/octo/widget/issues/7/comments?per_page=100",
                    json!([responses["issueComments"]]),
                ),
                (
                    "repos/octo/widget/pulls/7/requested_reviewers?per_page=100",
                    json!([responses["requestedReviewers"]]),
                ),
                ("graphql", responses["reviewThreads"].clone()),
                ("graphql", responses["issueEdits"].clone()),
                ("graphql", responses["reviewEdits"].clone()),
                ("graphql", responses["timeline"].clone()),
                ("user", responses["user"].clone()),
                (
                    "repos/octo/widget/compare/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb...aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    responses["compare"].clone(),
                ),
            ])),
        };
        let mut transport = Transport::new(&target, None, &script, &script, &FixedClock);
        let actual = collect(&mut transport, &target).unwrap();
        assert!(script.replies.borrow().is_empty());
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_repeated_cursor_instead_of_truncating() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let response = json!({"data":{"repository":{"pullRequest":{"comments":page(json!([]),true,json!("same"))}}}});
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![
                ("graphql", response.clone()),
                ("graphql", response),
            ])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        connection(
            &transport,
            &target,
            super::ISSUE_METADATA,
            "/data/repository/pullRequest/comments",
            None,
            None,
        )
        .unwrap_err();
        assert!(script.replies.borrow().is_empty());
    }

    #[test]
    fn follows_independent_connection_cursors_to_exhaustion() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let response = |nodes, next, cursor, expected| {
            json!({"__expectedCursor":expected,
            "data":{"repository":{"pullRequest":{"comments":page(nodes,next,cursor)}}}})
        };
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![
                (
                    "graphql",
                    response(json!([{"id":"one"}]), true, json!("cursor-1"), Value::Null),
                ),
                (
                    "graphql",
                    response(json!([{"id":"two"}]), false, Value::Null, json!("cursor-1")),
                ),
            ])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        let result = connection(
            &transport,
            &target,
            super::ISSUE_METADATA,
            "/data/repository/pullRequest/comments",
            None,
            None,
        )
        .unwrap();
        assert_eq!(result, vec![json!({"id":"one"}), json!({"id":"two"})]);
        assert!(script.replies.borrow().is_empty());
    }

    #[test]
    fn follows_nested_comment_cursor_and_propagates_graphql_errors() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let first = page(json!([{"id":"root"}]), true, json!("next"));
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![(
                "graphql",
                json!({"__expectedCursor":"next","__expectedThread":"thread",
                    "data":{"node":{"comments":page(json!([{"id":"reply"}]),false,Value::Null)}}}),
            )])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        assert_eq!(
            connection(
                &transport,
                &target,
                super::THREAD_COMMENTS,
                "/data/node/comments",
                Some("thread"),
                Some(first),
            )
            .unwrap(),
            vec![json!({"id":"root"}), json!({"id":"reply"})]
        );
        assert!(script.replies.borrow().is_empty());

        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![(
                "graphql",
                json!({"data":{"node":{"comments":page(json!([]),false,Value::Null)}},
                    "errors":[{"message":"partial result"}]}),
            )])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        connection(
            &transport,
            &target,
            super::THREAD_COMMENTS,
            "/data/node/comments",
            Some("thread"),
            None,
        )
        .unwrap_err();
        assert!(script.replies.borrow().is_empty());
    }

    #[test]
    fn thread_event_and_url_follow_structural_opener_not_page_order() {
        let comment = |id, reply, at, url| {
            json!({"id":id,"url":url,"body":"exact\r\né","createdAt":at,
                "lastEditedAt":null,"replyTo":reply,"author":null,"minimizedReason":null})
        };
        let node = json!({"id":"T","isResolved":false,"isOutdated":false,"isCollapsed":false,
            "path":null,"line":null,"startLine":null,"originalLine":null,
            "originalStartLine":null,"diffSide":null,"startDiffSide":null,
            "subjectType":"FILE","resolvedBy":null});
        let reply = comment(
            "reply",
            json!({"id":"root"}),
            "2026-01-03T00:00:00Z",
            "reply-url",
        );
        let root = comment("root", Value::Null, "2026-01-01T00:00:00Z", "root-url");
        let thread = review_thread(&node, &[reply, root]).unwrap();
        assert_eq!(thread["openingCommentId"], "root");
        assert_eq!(thread["eventUtc"], "2026-01-01T00:00:00.0000000Z");
        assert_eq!(thread["url"], "root-url");
        assert_eq!(thread["lastUpdatedUtc"], "2026-01-03T00:00:00.0000000Z");
        assert_eq!(thread["comments"][1]["content"], "exact\r\né");
    }

    #[test]
    fn deleted_review_author_does_not_hide_discussion_or_invent_a_reviewer() {
        let review = json!({"id":1,"node_id":"R","state":"COMMENTED","body":"é\r\nfeedback",
            "user":null,"submitted_at":"2026-01-01T00:00:00Z","html_url":null,
            "author_association":"NONE","commit_id":null});
        let thread = super::summary_thread(&review, &json!({"lastEditedAt":null})).unwrap();
        assert_eq!(thread["comments"][0]["content"], "é\r\nfeedback");
        assert_eq!(thread["comments"][0]["author"]["id"], Value::Null);
        assert_eq!(
            reviewers(&[review], &[json!({"users":[],"teams":[]})]).unwrap(),
            Vec::<Value>::new()
        );
        reviewers(&[], &[]).unwrap_err();
    }

    #[test]
    fn deleted_head_repository_keeps_owner_qualified_source_ref() {
        let head = json!({"ref":"topic","label":"fork-owner:topic"});
        assert_eq!(source_ref(&head, &json!({}), false).unwrap(), "topic");
        assert_eq!(
            source_ref(&head, &json!({}), true).unwrap(),
            "fork-owner:topic"
        );
        assert_eq!(
            source_ref(&head, &Value::Null, false).unwrap(),
            "fork-owner:topic"
        );
        source_ref(&json!({"ref":"topic","label":null}), &Value::Null, false).unwrap_err();
    }

    #[test]
    fn reviewer_keys_preserve_identifier_namespaces_and_stable_team_ids() {
        let reviews = [
            json!({"user":{"id":7,"login":"old","node_id":"U_7","type":"User"}}),
            json!({"user":{"login":"7","type":"User"}}),
            json!({"user":{"id":7,"login":"renamed","node_id":"U_7","type":"User"}}),
        ];
        let pages = [
            json!({"users":[],"teams":[{"id":7,"slug":"old-team","name":"Old"}]}),
            json!({"users":[],"teams":[{"id":7,"slug":"new-team","name":"New"}]}),
        ];
        let roster = reviewers(&reviews, &pages).unwrap();
        assert_eq!(roster.len(), 3);
        assert_eq!(roster[0]["identity"]["id"], "7");
        assert_eq!(roster[1]["identity"]["uniqueName"], "7");
        assert_eq!(roster[2]["isTeam"], true);
    }

    #[test]
    fn reviewer_join_uses_strongest_shared_evidence() {
        let reviews = [
            json!({"user":{"id":7,"login":"alice","node_id":"Node_A","type":"User"}}),
            json!({"user":{"login":"ALICE","type":"User"}}),
            json!({"user":{"id":8,"login":"alice","node_id":"node_a","type":"User"}}),
        ];
        let roster = reviewers(&reviews, &[json!({"users":[],"teams":[]})]).unwrap();
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0]["identity"]["id"], "7");
        assert_eq!(roster[1]["identity"]["id"], "8");
    }

    #[test]
    fn rejects_ambiguous_openers_and_normalizes_timestamp_offsets() {
        let comment = json!({"id":"C1","url":null,"body":"","createdAt":"2026-01-01T00:00:00Z",
            "lastEditedAt":null,"replyTo":{"id":"missing"},"author":null,"minimizedReason":null});
        review_thread(&json!({"id":"T1"}), &[comment]).unwrap_err();
        assert_eq!(
            utc(&json!("2026-01-01T01:00:00.123456789+01:00")).unwrap(),
            "2026-01-01T00:00:00.1234567Z"
        );
    }

    #[test]
    fn preserves_force_push_identity_and_requires_exact_metadata() {
        let before = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let after = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let node = json!({"id":"F1","__typename":"HeadRefForcePushedEvent",
            "createdAt":"2026-01-02T00:00:00Z",
            "actor":{"databaseId":2,"login":"bot","id":"BOT_2","__typename":"Bot","url":"https://github.com/apps/bot"},
            "beforeCommit":{"oid":before},"afterCommit":{"oid":after}});
        let thread = force_push(&node).unwrap();
        assert_eq!(
            thread["properties"],
            json!({"beforeCommit":before,"afterCommit":after})
        );
        assert_eq!(thread["decision"]["authorship"], "agent");
        assert_eq!(thread["decision"]["by"]["id"], "2");
        assert_eq!(thread["openingCommentId"], Value::Null);
        metadata(vec![
            json!({"id":"X","lastEditedAt":null}),
            json!({"id":"X","lastEditedAt":null}),
        ])
        .unwrap_err();
        metadata(vec![json!({"id":"X"})]).unwrap_err();
    }

    #[test]
    fn fork_merge_base_uses_qualified_commits_and_falls_back_only_on_http_404() {
        let target = Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap();
        let base = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let head = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let result = "cccccccccccccccccccccccccccccccccccccccc";
        let repo = json!({"owner":{"login":"fork owner"}});
        let qualified = "repos/acme/repo/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...fork%20owner:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let plain = "repos/acme/repo/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let comparison = json!({"merge_base_commit":{"sha":result}});

        for replies in [
            vec![(qualified, comparison.clone())],
            vec![
                (qualified, json!({"__status":404,"message":"Not Found"})),
                (plain, comparison),
            ],
        ] {
            let script = Recording {
                replies: RefCell::new(replies.into()),
            };
            let transport = Transport::new(&target, None, &script, &script, &FixedClock);
            assert_eq!(
                merge_base(&transport, &target, base, head, &repo, true).unwrap(),
                result
            );
            assert!(script.replies.borrow().is_empty());
        }

        // A 404-looking diagnostic is not status evidence, and no fallback is allowed.
        for (failure, expected_classification) in [
            (
                json!({"__status":403,"__stderr":"HTTP 404","message":"Forbidden"}),
                "permissionDenied",
            ),
            (
                json!({"__status":404,"__includeHeaders":false,"__stderr":"HTTP 404","message":"Not Found"}),
                "service",
            ),
        ] {
            let script = Recording {
                replies: RefCell::new(VecDeque::from(vec![(qualified, failure)])),
            };
            let transport = Transport::new(&target, None, &script, &script, &FixedClock);
            let error = merge_base(&transport, &target, base, head, &repo, true).unwrap_err();
            assert_eq!(crate::classification(&error), expected_classification);
            assert!(script.replies.borrow().is_empty());
        }

        // Even a proven absence on both routes cannot become a successful capture.
        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![
                (qualified, json!({"__status":404})),
                (plain, json!({"__status":404})),
            ])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        merge_base(&transport, &target, base, head, &repo, true).unwrap_err();
        assert!(script.replies.borrow().is_empty());

        let script = Recording {
            replies: RefCell::new(VecDeque::from(vec![(
                qualified,
                json!({"merge_base_commit":null}),
            )])),
        };
        let transport = Transport::new(&target, None, &script, &script, &FixedClock);
        merge_base(&transport, &target, base, head, &repo, true).unwrap_err();
        assert!(script.replies.borrow().is_empty());
    }
}
