//! GitHub effects for publishing one final review through an injected transport.

use std::collections::{HashMap, HashSet};

use ohno::AppError;
use serde_json::{Value, json};

use crate::error::fail;
use crate::identity::Identity;
use crate::json::{array, boolean, field, nullable_field, text};
use crate::target::{Platform, Target, escape};
use crate::transport::Transport;

// Use GitHub's maximum unnested connection page size.
const PAGE_SIZE: u32 = 100;
// Nested thread/comment pages multiply their node count; match capture's bound.
const THREAD_PAGE_SIZE: u32 = 50;
// This arbitrarily generous cap bounds work if a provider never terminates.
const MAX_PAGES: usize = 1000;
// GitHub permits one pending review per account; this arbitrary small page
// leaves room for unexpected extra records without fetching a full page.
const PENDING_PAGE_SIZE: u32 = 10;

const PENDING: &str = "query($owner:String!,$repo:String!,$num:Int!,$login:String!,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 reviews(first:$size,states:[PENDING],author:$login){nodes{id state}}}}}";
const START: &str = "mutation($pullRequestId:ID!,$commitOid:GitObjectID!){
 addPullRequestReview(input:{pullRequestId:$pullRequestId,commitOID:$commitOid}){
 pullRequestReview{id state}}}";
const LINE: &str = "mutation($reviewId:ID!,$path:String!,$line:Int!,$side:DiffSide!,$body:String!){
 addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,line:$line,side:$side,subjectType:LINE,body:$body}){
 thread{id comments(first:1){nodes{id databaseId}}}}}";
const RANGE: &str = "mutation($reviewId:ID!,$path:String!,$startLine:Int!,$line:Int!,$side:DiffSide!,$body:String!){
 addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,startLine:$startLine,line:$line,startSide:$side,side:$side,subjectType:LINE,body:$body}){
 thread{id comments(first:1){nodes{id databaseId}}}}}";
const FILE: &str = "mutation($reviewId:ID!,$path:String!,$body:String!){
 addPullRequestReviewThread(input:{pullRequestReviewId:$reviewId,path:$path,subjectType:FILE,body:$body}){
 thread{id comments(first:1){nodes{id databaseId}}}}}";
const REPLY: &str = "mutation($reviewId:ID!,$threadId:ID!,$body:String!){
 addPullRequestReviewThreadReply(input:{pullRequestReviewId:$reviewId,pullRequestReviewThreadId:$threadId,body:$body}){
 comment{id databaseId}}}";
const SUBMIT: &str = "mutation($reviewId:ID!,$body:String!,$event:PullRequestReviewEvent!){
 submitPullRequestReview(input:{pullRequestReviewId:$reviewId,event:$event,body:$body}){
 pullRequestReview{id state url body}}}";
const DELETE: &str = "mutation($reviewId:ID!){
 deletePullRequestReview(input:{pullRequestReviewId:$reviewId}){pullRequestReview{id}}}";
const UNRESOLVE: &str = "mutation($threadId:ID!){
 unresolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}";
const VERIFY: &str = "query($reviewId:ID!,$cur:String,$size:Int!){
 node(id:$reviewId){... on PullRequestReview{
 id state url body comments(first:$size,after:$cur){pageInfo{hasNextPage endCursor}
 nodes{id databaseId state path line startLine subjectType body}}}}}";
const THREADS: &str = "query($owner:String!,$repo:String!,$num:Int!,$cur:String,$size:Int!){
 repository(owner:$owner,name:$repo){pullRequest(number:$num){
 reviewThreads(first:$size,after:$cur){pageInfo{hasNextPage endCursor}
 nodes{id diffSide subjectType comments(first:$size){pageInfo{hasNextPage endCursor}
 nodes{id}}}}}}}";
const THREAD_COMMENTS: &str = "query($threadId:ID!,$cur:String,$size:Int!){
 node(id:$threadId){... on PullRequestReviewThread{
 comments(first:$size,after:$cur){pageInfo{hasNextPage endCursor} nodes{id}}}}}";

/// Associates a review comment with the provider's thread anchor.
struct ThreadEvidence {
    id: String,
    side: String,
    subject_type: String,
}

/// Rejects routing across providers or repository-wide targets.
fn check(transport: &Transport<'_>, target: &Target) -> Result<(), AppError> {
    if transport.platform != Platform::GitHub
        || target.platform != Platform::GitHub
        || target.pull_request_id == 0
        || target.organization.trim().is_empty()
        || target.repository.trim().is_empty()
    {
        return Err(fail(
            "invalidInput",
            "A GitHub pull request target is required",
        ));
    }
    Ok(())
}

/// Requires an identifier before a request or receipt can identify an effect.
fn required<'a>(value: &'a Value, name: &str, kind: &'static str) -> Result<&'a str, AppError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| fail(kind, format!("Missing or invalid {name}")))
}

/// Requires the queried GraphQL node rather than treating absence as no data.
fn node<'a>(value: &'a Value, path: &str) -> Result<&'a Value, AppError> {
    value
        .pointer(path)
        .filter(|v| v.is_object())
        .ok_or_else(|| fail("service", format!("Missing GitHub node {path}")))
}

/// Requires a returned write receipt, including for mutations with no result value.
fn receipt<'a>(response: &'a Value, path: &str, id: &str) -> Result<&'a Value, AppError> {
    let value = response
        .pointer(path)
        .filter(|v| v.is_object())
        .ok_or_else(|| fail("ambiguousWrite", "GitHub mutation returned no receipt"))?;
    required(value, id, "ambiguousWrite")?;
    Ok(value)
}

/// Reports live PR state, the authenticated login, and that account's draft.
pub(crate) fn state(transport: &mut Transport<'_>, target: &Target) -> Result<Value, AppError> {
    check(transport, target)?;
    let pr = transport.request(&target.pr_api(), "GET", None)?;
    let identity =
        Identity::from_account(Platform::GitHub, &transport.request("user", "GET", None)?)?;
    let login = identity.unique_name.ok_or_else(|| {
        fail(
            "authentication",
            "GitHub did not report the authenticated login",
        )
    })?;
    let response = transport.graphql(
        PENDING,
        &json!({"owner":target.organization,"repo":target.repository,
        "num":target.pull_request_id,"login":login,"size":PENDING_PAGE_SIZE}),
    )?;
    let reviews = node(&response, "/data/repository/pullRequest/reviews")?;
    let pending = array(field(reviews, "nodes")?)?
        .first()
        .map(|review| required(review, "id", "service"))
        .transpose()?;
    let status = text(field(&pr, "state")?)?;
    let head = field(&pr, "head")?;
    let source = text(field(head, "sha")?)?;
    let base = field(&pr, "base")?;
    let base_sha = text(field(base, "sha")?)?;
    let merge_base = merge_base(transport, target, base, head, base_sha, source)?;
    Ok(
        json!({"platform":"gitHub","pullRequestNodeId":required(&pr,"node_id","service")?,
        "url":text(field(&pr,"html_url")?)?,"status":status,
        "isActive":status == "open" && !boolean(field(&pr,"merged")?)?,
        "isDraft":boolean(field(&pr,"draft")?)?,"sourceCommit":source,
        "mergeBaseCommit":merge_base.unwrap_or_default(),"identity":login,"pendingReviewId":pending}),
    )
}

/// Compares fork-qualified commits first, falling back only on an explicit 404.
fn merge_base(
    transport: &Transport<'_>,
    target: &Target,
    base: &Value,
    head: &Value,
    base_sha: &str,
    head_sha: &str,
) -> Result<Option<String>, AppError> {
    if base_sha.is_empty() || head_sha.is_empty() {
        return Ok(None);
    }
    let prefix = format!("{}/compare/", target.repository_api());
    let base_repo = field(base, "repo")?;
    let head_repo = nullable_field(head, "repo")?;
    let mut specs = Vec::new();
    if !head_repo.is_null()
        && !text(field(head_repo, "full_name")?)?
            .eq_ignore_ascii_case(text(field(base_repo, "full_name")?)?)
    {
        let owner = text(field(field(head_repo, "owner")?, "login")?)?;
        if !owner.is_empty() {
            specs.push(format!(
                "{}...{}:{}",
                escape(base_sha),
                escape(owner),
                escape(head_sha)
            ));
        }
    }
    specs.push(format!("{}...{}", escape(base_sha), escape(head_sha)));
    for spec in specs {
        if let Some(value) = transport.github_get_if_found(&format!("{prefix}{spec}"))? {
            return Ok(Some(
                text(field(field(&value, "merge_base_commit")?, "sha")?)?.to_owned(),
            ));
        }
    }
    Ok(None)
}

/// GitHub addresses files directly by path, without separate identifiers.
pub(crate) fn file_identity(
    transport: &Transport<'_>,
    target: &Target,
    _state: &Value,
    _paths: &[String],
) -> Result<Value, AppError> {
    check(transport, target)?;
    Ok(json!({}))
}

/// Creates a draft pinned to the reviewed source commit.
pub(crate) fn start(
    transport: &Transport<'_>,
    target: &Target,
    state: &Value,
) -> Result<Value, AppError> {
    check(transport, target)?;
    let response = transport.graphql_mutation(
        START,
        &json!({"pullRequestId":required(state,"pullRequestNodeId","invalidInput")?,
        "commitOid":required(state,"sourceCommit","invalidInput")?}),
    )?;
    let review = receipt(
        &response,
        "/data/addPullRequestReview/pullRequestReview",
        "id",
    )?;
    let status = required(review, "state", "ambiguousWrite")?;
    Ok(json!({"reviewId":review["id"],"state":status}))
}

/// Adds a line, file, or reply finding to the same draft.
pub(crate) fn add(
    transport: &Transport<'_>,
    target: &Target,
    review: &Value,
    operation: &Value,
) -> Result<Value, AppError> {
    check(transport, target)?;
    let review_id = required(review, "reviewId", "invalidInput")?;
    let body = operation
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalidInput", "Finding requires a body"))?;
    let operation_name = required(operation, "operation", "invalidInput")?;
    let (query, variables, reply) = match operation_name {
        "reply" => {
            let thread = required(operation, "threadId", "invalidInput")?;
            (
                REPLY,
                json!({"reviewId":review_id,"threadId":thread,"body":body}),
                Some(thread),
            )
        }
        "newFileThread" => {
            let anchor = field(operation, "anchor")?;
            (
                FILE,
                json!({"reviewId":review_id,"path":required(anchor,"path","invalidInput")?,"body":body}),
                None,
            )
        }
        "newLineThread" => {
            let anchor = field(operation, "anchor")?;
            let line = anchor
                .get("endLine")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
                .ok_or_else(|| fail("invalidInput", "Line anchor requires a positive end line"))?;
            let start = anchor
                .get("startLine")
                .and_then(Value::as_i64)
                .unwrap_or(line);
            let side = anchor
                .get("side")
                .and_then(Value::as_str)
                .unwrap_or("right")
                .to_ascii_uppercase();
            if !["LEFT", "RIGHT"].contains(&side.as_str()) {
                return Err(fail("invalidInput", "Unknown GitHub diff side"));
            }
            let mut vars = json!({"reviewId":review_id,"path":required(anchor,"path","invalidInput")?,
                "line":line,"side":side,"body":body});
            let query = if start > 0 && start < line {
                vars.as_object_mut()
                    .ok_or_else(|| fail("invalidInput", "Invalid line variables"))?
                    .insert("startLine".to_owned(), json!(start));
                RANGE
            } else {
                LINE
            };
            (query, vars, None)
        }
        _ => {
            return Err(fail(
                "invalidInput",
                format!("Unknown GitHub operation {operation_name}"),
            ));
        }
    };
    let response = transport.graphql_mutation(query, &variables)?;
    if let Some(thread) = reply {
        let comment = receipt(
            &response,
            "/data/addPullRequestReviewThreadReply/comment",
            "id",
        )?;
        return Ok(json!({"threadId":thread,"commentId":comment["id"]}));
    }
    let thread = receipt(&response, "/data/addPullRequestReviewThread/thread", "id")?;
    let comment = thread
        .pointer("/comments/nodes/0")
        .ok_or_else(|| fail("ambiguousWrite", "GitHub returned no comment receipt"))?;
    let comment_id = required(comment, "id", "ambiguousWrite")?;
    Ok(json!({"threadId":thread["id"],"commentId":comment_id}))
}

/// Submits the draft as a comment or an explicitly authorized approval.
pub(crate) fn complete(
    transport: &Transport<'_>,
    target: &Target,
    review: &Value,
    summary: &str,
    approve: bool,
) -> Result<Value, AppError> {
    check(transport, target)?;
    let id = required(review, "reviewId", "invalidInput")?;
    let response = transport.graphql_mutation(
        SUBMIT,
        &json!({"reviewId":id,"body":summary,
        "event":if approve {"APPROVE"} else {"COMMENT"}}),
    )?;
    let submitted = receipt(
        &response,
        "/data/submitPullRequestReview/pullRequestReview",
        "id",
    )?;
    if required(submitted, "id", "ambiguousWrite")? != id {
        return Err(fail(
            "ambiguousWrite",
            "GitHub returned a different review receipt",
        ));
    }
    Ok(
        json!({"reviewId":id,"state":required(submitted,"state","ambiguousWrite")?,
        "url":required(submitted,"url","ambiguousWrite")?}),
    )
}

/// Reads the same review before submission to verify its pending findings.
pub(crate) fn verify_findings(
    transport: &Transport<'_>,
    target: &Target,
    review: &Value,
) -> Result<Value, AppError> {
    verify(transport, target, review)
}

/// Reads all review-scoped comments, following each advancing connection cursor.
pub(crate) fn verify(
    transport: &Transport<'_>,
    target: &Target,
    review: &Value,
) -> Result<Value, AppError> {
    check(transport, target)?;
    let id = required(review, "reviewId", "invalidInput")?;
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    let mut comment_ids = HashSet::new();
    let mut comments = Vec::new();
    let mut first: Option<Value> = None;
    for _ in 0..MAX_PAGES {
        let response = transport.graphql(
            VERIFY,
            &json!({"reviewId":id,"cur":cursor,"size":PAGE_SIZE}),
        )?;
        let review_node = node(&response, "/data/node")?;
        if required(review_node, "id", "service")? != id {
            return Err(fail("service", "GitHub returned a different review"));
        }
        if first.is_none() {
            first = Some(review_node.clone());
        }
        let connection = field(review_node, "comments")?;
        for comment in array(field(connection, "nodes")?)? {
            let status = text(field(comment, "state")?)?;
            let comment_id = required(comment, "id", "service")?;
            if !comment_ids.insert(comment_id.to_owned()) {
                return Err(fail("service", "GitHub repeated a review comment"));
            }
            comments.push(json!({"id":comment_id,"threadId":"",
                "path":text(field(comment,"path")?)?,"line":nullable_field(comment,"line")?,
                "startLine":nullable_field(comment,"startLine")?,"side":"",
                "subjectType":text(field(comment,"subjectType")?)?,"state":status,
                "isPublished":status == "SUBMITTED","body":text(field(comment,"body")?)?}));
        }
        let page = field(connection, "pageInfo")?;
        if !boolean(field(page, "hasNextPage")?)? {
            let first = first.ok_or_else(|| fail("service", "Missing review"))?;
            let status = text(field(&first, "state")?)?;
            let threads = review_threads(transport, target, &comment_ids)?;
            for comment in &mut comments {
                let id = required(comment, "id", "service")?;
                let thread = threads.get(id).ok_or_else(|| {
                    fail("service", "GitHub review comment has no visible thread")
                })?;
                if text(field(comment, "subjectType")?)? != thread.subject_type {
                    return Err(fail("service", "GitHub comment and thread subjects differ"));
                }
                let object = comment
                    .as_object_mut()
                    .ok_or_else(|| fail("service", "GitHub review comment is not an object"))?;
                object.insert("threadId".to_owned(), json!(thread.id));
                object.insert("side".to_owned(), json!(thread.side));
            }
            return Ok(
                json!({"reviewId":id,"state":status,"url":text(field(&first,"url")?)?,
                "body":text(field(&first,"body")?)?,"isPublished":status != "PENDING","comments":comments}),
            );
        }
        let next = required(page, "endCursor", "service")?.to_owned();
        if !seen.insert(next.clone()) {
            return Err(fail("service", "GitHub repeated a pagination cursor"));
        }
        cursor = Some(next);
    }
    Err(fail("service", "GitHub review exceeded the page limit"))
}

/// Walks PR threads and their comment connections to prove review membership.
fn review_threads(
    transport: &Transport<'_>,
    target: &Target,
    wanted: &HashSet<String>,
) -> Result<HashMap<String, ThreadEvidence>, AppError> {
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let variables = json!({"owner":target.organization,"repo":target.repository,
        "num":target.pull_request_id,"size":THREAD_PAGE_SIZE});
    let threads = connection(
        transport,
        THREADS,
        variables,
        "/data/repository/pullRequest/reviewThreads",
        None,
    )?;
    let mut result = HashMap::new();
    let mut thread_ids = HashSet::new();
    for thread in threads {
        let id = required(&thread, "id", "service")?;
        if !thread_ids.insert(id.to_owned()) {
            return Err(fail("service", "GitHub repeated a review thread"));
        }
        let comments = connection(
            transport,
            THREAD_COMMENTS,
            json!({"threadId":id,"size":THREAD_PAGE_SIZE}),
            "/data/node/comments",
            Some(field(&thread, "comments")?.clone()),
        )?;
        for comment in &comments {
            let comment_id = required(comment, "id", "service")?;
            if !wanted.contains(comment_id) {
                continue;
            }
            let subject_type = required(&thread, "subjectType", "service")?;
            let side = match subject_type {
                "LINE" => {
                    let side = required(&thread, "diffSide", "service")?;
                    if !["LEFT", "RIGHT"].contains(&side) {
                        return Err(fail("service", "Unknown GitHub thread diff side"));
                    }
                    side
                }
                "FILE" => "",
                _ => return Err(fail("service", "Unknown GitHub thread subject")),
            };
            if result
                .insert(
                    comment_id.to_owned(),
                    ThreadEvidence {
                        id: id.to_owned(),
                        side: side.to_owned(),
                        subject_type: subject_type.to_owned(),
                    },
                )
                .is_some()
            {
                return Err(fail(
                    "service",
                    "GitHub comment belongs to multiple thread entries",
                ));
            }
        }
    }
    Ok(result)
}

/// Reads an entire connection and rejects non-advancing pagination.
fn connection(
    transport: &Transport<'_>,
    query: &str,
    mut variables: Value,
    path: &str,
    mut first: Option<Value>,
) -> Result<Vec<Value>, AppError> {
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    for _ in 0..MAX_PAGES {
        let mut page = if let Some(page) = first.take() {
            page
        } else {
            variables
                .as_object_mut()
                .ok_or_else(|| fail("service", "Invalid query variables"))?
                .insert("cur".to_owned(), json!(cursor));
            let mut response = transport.graphql(query, &variables)?;
            response
                .pointer_mut(path)
                .filter(|value| value.is_object())
                .ok_or_else(|| fail("service", format!("Missing GitHub node {path}")))?
                .take()
        };
        array(field(&page, "nodes")?)?;
        let page_nodes = page
            .get_mut("nodes")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| fail("service", "Expected an array in service evidence"))?;
        nodes.append(page_nodes);
        let info = field(&page, "pageInfo")?;
        if !boolean(field(info, "hasNextPage")?)? {
            return Ok(nodes);
        }
        let next = required(info, "endCursor", "service")?.to_owned();
        if !seen.insert(next.clone()) {
            return Err(fail("service", "GitHub repeated a pagination cursor"));
        }
        cursor = Some(next);
    }
    Err(fail("service", "GitHub connection exceeded the page limit"))
}

/// Reopens a resolved thread after publication has made its reply visible.
pub(crate) fn reopen(
    transport: &Transport<'_>,
    target: &Target,
    thread_id: &str,
) -> Result<(), AppError> {
    check(transport, target)?;
    if thread_id.trim().is_empty() {
        return Err(fail("invalidInput", "Thread identifier is required"));
    }
    let response = transport.graphql_mutation(UNRESOLVE, &json!({"threadId":thread_id}))?;
    let thread = receipt(&response, "/data/unresolveReviewThread/thread", "id")?;
    if required(thread, "id", "ambiguousWrite")? != thread_id
        || thread.get("isResolved") != Some(&json!(false))
    {
        return Err(fail(
            "ambiguousWrite",
            "GitHub returned an incomplete reopen receipt",
        ));
    }
    Ok(())
}

/// Removes only the identified draft; callers decide whether removal is allowed.
pub(crate) fn discard(
    transport: &Transport<'_>,
    target: &Target,
    review: &Value,
) -> Result<(), AppError> {
    check(transport, target)?;
    let id = required(review, "reviewId", "invalidInput")?;
    let response = transport.graphql_mutation(DELETE, &json!({"reviewId":id}))?;
    let deleted = receipt(
        &response,
        "/data/deletePullRequestReview/pullRequestReview",
        "id",
    )?;
    if required(deleted, "id", "ambiguousWrite")? != id {
        return Err(fail(
            "ambiguousWrite",
            "GitHub returned a different deleted review",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::indexing_slicing,
    reason = "The tests index bounded fixtures with known fields and lengths"
)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, from_slice, json};

    use super::{
        add, complete, connection, discard, file_identity, reopen, start, state, verify,
        verify_findings,
    };
    use crate::error::fail;
    use crate::target::Target;
    use crate::transport::Transport;
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, classification};

    /// Strict scripted process proves routes, bodies and number of attempts.
    struct Script {
        replies: RefCell<VecDeque<(String, Value, bool, String)>>,
        queries: RefCell<Vec<String>>,
    }

    impl Script {
        fn new(replies: Vec<(&str, Value, bool, Value)>) -> Self {
            Self {
                replies: RefCell::new(
                    replies
                        .into_iter()
                        .map(|(route, vars, ok, response)| {
                            (route.to_owned(), vars, ok, response.to_string())
                        })
                        .collect(),
                ),
                queries: RefCell::new(Vec::new()),
            }
        }

        fn done(&self) {
            assert!(self.replies.borrow().is_empty());
        }
    }

    impl Process for Script {
        fn execute(
            &self,
            program: &str,
            arguments: &[String],
            input: &[u8],
        ) -> io::Result<ProcessOutput> {
            let (route, variables, ok, response) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(program, "gh");
            assert_eq!(arguments.get(1), Some(&route));
            if route == "graphql" {
                let payload: Value = from_slice(input).unwrap();
                assert_eq!(payload["variables"], variables);
                self.queries
                    .borrow_mut()
                    .push(payload["query"].as_str().unwrap().to_owned());
            } else {
                assert!(input.is_empty());
            }
            Ok(ProcessOutput::new(ok, response.into_bytes(), Vec::new()))
        }
    }

    /// The GitHub adapter never uses the Azure HTTP boundary.
    struct NoHttp;
    impl Http for NoHttp {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            Err(fail("service", "Unexpected HTTP call"))
        }
    }

    /// Tests reject unexpected waits rather than measuring wall-clock time.
    struct NoWait;
    impl Clock for NoWait {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }
        fn wait(&self, _: Duration) -> Result<(), AppError> {
            Err(fail("service", "Unexpected retry"))
        }
    }

    fn target() -> Target {
        Target::pull_request("https://github.com/acme/repo/pull/42", None).unwrap()
    }

    fn thread_response() -> Value {
        json!({"data":{"addPullRequestReviewThread":{"thread":{"id":"thread",
            "comments":{"nodes":[{"id":"comment"}]}}}}})
    }

    #[test]
    fn state_reads_identity_pending_review_and_fork_merge_base() {
        let script = Script::new(vec![
            (
                "repos/acme/repo/pulls/42",
                json!(null),
                true,
                json!({"node_id":"pr","html_url":"url",
                "state":"open","merged":false,"draft":true,"base":{"sha":"base","repo":{"full_name":"acme/repo"}},
                "head":{"sha":"head","repo":{"full_name":"other/repo","owner":{"login":"other"}}}}),
            ),
            (
                "user",
                json!(null),
                true,
                json!({"id":5,"login":"reviewer","type":"User"}),
            ),
            (
                "graphql",
                json!({"owner":"acme","repo":"repo","num":42,"login":"reviewer","size":10}),
                true,
                json!({"data":{"repository":{"pullRequest":{"reviews":{"nodes":[{"id":"draft","state":"PENDING"}]}}}}}),
            ),
            (
                "repos/acme/repo/compare/base...other:head",
                json!(null),
                true,
                json!({"merge_base_commit":{"sha":"merge"}}),
            ),
        ]);
        let target = target();
        let mut transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        assert_eq!(
            state(&mut transport, &target).unwrap(),
            json!({"platform":"gitHub","pullRequestNodeId":"pr",
            "url":"url","status":"open","isActive":true,"isDraft":true,"sourceCommit":"head",
            "mergeBaseCommit":"merge","identity":"reviewer","pendingReviewId":"draft"})
        );
        assert_eq!(
            file_identity(&transport, &target, &json!({}), &["x".into()]).unwrap(),
            json!({})
        );
        script.done();
    }

    #[test]
    fn review_mutations_use_expected_variables_and_receipts() {
        let script = Script::new(vec![
            (
                "graphql",
                json!({"pullRequestId":"pr","commitOid":"head"}),
                true,
                json!({"data":{"addPullRequestReview":{"pullRequestReview":{"id":"r","state":"PENDING"}}}}),
            ),
            (
                "graphql",
                json!({"reviewId":"r","path":"src/a","line":9,"startLine":4,"side":"LEFT","body":"line"}),
                true,
                thread_response(),
            ),
            (
                "graphql",
                json!({"reviewId":"r","path":"src/a","body":"file"}),
                true,
                thread_response(),
            ),
            (
                "graphql",
                json!({"reviewId":"r","threadId":"old","body":"reply"}),
                true,
                json!({"data":{"addPullRequestReviewThreadReply":{"comment":{"id":"reply"}}}}),
            ),
            (
                "graphql",
                json!({"reviewId":"r","body":"summary","event":"APPROVE"}),
                true,
                json!({"data":{"submitPullRequestReview":{"pullRequestReview":{"id":"r","state":"APPROVED","url":"url"}}}}),
            ),
            (
                "graphql",
                json!({"threadId":"old"}),
                true,
                json!({"data":{"unresolveReviewThread":{"thread":{"id":"old","isResolved":false}}}}),
            ),
            (
                "graphql",
                json!({"reviewId":"r"}),
                true,
                json!({"data":{"deletePullRequestReview":{"pullRequestReview":{"id":"r"}}}}),
            ),
        ]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let review = start(
            &transport,
            &target,
            &json!({"pullRequestNodeId":"pr","sourceCommit":"head"}),
        )
        .unwrap();
        assert_eq!(
            add(
                &transport,
                &target,
                &review,
                &json!({"operation":"newLineThread","body":"line",
            "anchor":{"path":"src/a","startLine":4,"endLine":9,"side":"left"}})
            )
            .unwrap(),
            json!({"threadId":"thread","commentId":"comment"})
        );
        add(
            &transport,
            &target,
            &review,
            &json!({"operation":"newFileThread","body":"file","anchor":{"path":"src/a"}}),
        )
        .unwrap();
        assert_eq!(
            add(
                &transport,
                &target,
                &review,
                &json!({"operation":"reply","body":"reply","threadId":"old"})
            )
            .unwrap(),
            json!({"threadId":"old","commentId":"reply"})
        );
        assert_eq!(
            complete(&transport, &target, &review, "summary", true).unwrap()["state"],
            "APPROVED"
        );
        reopen(&transport, &target, "old").unwrap();
        discard(&transport, &target, &review).unwrap();
        script.done();
        let queries = script.queries.borrow();
        assert!(queries[1].contains("startSide:$side"));
        assert!(queries[2].contains("subjectType:FILE"));
        assert!(queries[3].contains("addPullRequestReviewThreadReply"));
    }

    #[test]
    fn missing_receipts_are_ambiguous_without_replaying() {
        let script = Script::new(vec![(
            "graphql",
            json!({"pullRequestId":"pr","commitOid":"head"}),
            true,
            json!({"data":{"addPullRequestReview":{"pullRequestReview":{"state":"PENDING"}}}}),
        )]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let error = start(
            &transport,
            &target,
            &json!({"pullRequestNodeId":"pr","sourceCommit":"head"}),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "ambiguousWrite");
        script.done();
    }

    #[test]
    fn mutation_failures_and_graphql_errors_do_not_retry() {
        for (ok, response) in [
            (false, json!({"message":"transient gateway failure"})),
            (true, json!({"errors":[{"message":"uncertain"}]})),
        ] {
            let script = Script::new(vec![(
                "graphql",
                json!({"pullRequestId":"pr","commitOid":"head"}),
                ok,
                response,
            )]);
            let target = target();
            let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
            let error = start(
                &transport,
                &target,
                &json!({"pullRequestNodeId":"pr","sourceCommit":"head"}),
            )
            .unwrap_err();
            assert_eq!(classification(&error), "ambiguousWrite");
            script.done();
        }
    }

    #[test]
    fn verification_reads_every_page_and_preserves_pending_state() {
        let page = |cursor: Value, next: bool, id: &str| {
            json!({"data":{"node":{"id":"r","state":"PENDING",
            "url":"url","body":"", "comments":{"pageInfo":{"hasNextPage":next,"endCursor":cursor},
            "nodes":[{"id":id,"state":"PENDING","path":"x","line":9,"startLine":null,
                "subjectType":"LINE","body":"finding"}]}}}})
        };
        let script = Script::new(vec![
            (
                "graphql",
                json!({"reviewId":"r","cur":null,"size":100}),
                true,
                page(json!("next"), true, "c1"),
            ),
            (
                "graphql",
                json!({"reviewId":"r","cur":"next","size":100}),
                true,
                page(json!(null), false, "c2"),
            ),
            (
                "graphql",
                json!({"owner":"acme","repo":"repo","num":42,"cur":null,"size":50}),
                true,
                json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                    "pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"id":"thread","subjectType":"LINE","diffSide":"LEFT",
                        "comments":{"pageInfo":{"hasNextPage":true,"endCursor":"more"},
                        "nodes":[{"id":"c1"}]}}]}}}}}),
            ),
            (
                "graphql",
                json!({"threadId":"thread","cur":"more","size":50}),
                true,
                json!({"data":{"node":{"comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"id":"c2"}]}}}}),
            ),
        ]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let result = verify_findings(&transport, &target, &json!({"reviewId":"r"})).unwrap();
        assert_eq!(result["isPublished"], false);
        assert_eq!(result["comments"].as_array().unwrap().len(), 2);
        assert_eq!(result["comments"][1]["isPublished"], false);
        assert_eq!(result["comments"][0]["threadId"], "thread");
        assert_eq!(result["comments"][1]["side"], "LEFT");
        script.done();
    }

    #[test]
    fn connection_preserves_supplied_and_fetched_nodes() {
        let script = Script::new(vec![(
            "graphql",
            json!({"cur":"next"}),
            true,
            json!({"data":{"connection":{"nodes":[{"id":"second","body":"evidence"}],
                "pageInfo":{"hasNextPage":false,"endCursor":null}}}}),
        )]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let first = json!({"nodes":[{"id":"first","body":"evidence"}],
            "pageInfo":{"hasNextPage":true,"endCursor":"next"}});
        let result = connection(
            &transport,
            "query",
            json!({}),
            "/data/connection",
            Some(first),
        )
        .unwrap();
        assert_eq!(
            result,
            vec![
                json!({"id":"first","body":"evidence"}),
                json!({"id":"second","body":"evidence"}),
            ]
        );
        script.done();
    }

    #[test]
    fn connection_rejects_malformed_pages_and_cursor_cycles() {
        for page in [
            json!({"pageInfo":{"hasNextPage":false}}),
            json!({"nodes":null,"pageInfo":{"hasNextPage":false}}),
            json!({"nodes":{},"pageInfo":{"hasNextPage":false}}),
            json!({"nodes":[]}),
            json!({"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":null}}),
        ] {
            let script = Script::new(vec![]);
            let target = target();
            let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
            let error = connection(
                &transport,
                "query",
                json!({}),
                "/data/connection",
                Some(page),
            )
            .unwrap_err();
            assert_eq!(classification(&error), "service");
            script.done();
        }

        let script = Script::new(vec![(
            "graphql",
            json!({"cur":"same"}),
            true,
            json!({"data":{"connection":{"nodes":[],"pageInfo":{
                "hasNextPage":true,"endCursor":"same"}}}}),
        )]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let first = json!({"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"same"}});
        let error = connection(
            &transport,
            "query",
            json!({}),
            "/data/connection",
            Some(first),
        )
        .unwrap_err();
        assert_eq!(classification(&error), "service");
        script.done();

        let script = Script::new(vec![(
            "graphql",
            json!({"cur":null}),
            true,
            json!({"data":{}}),
        )]);
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        let error =
            connection(&transport, "query", json!({}), "/data/connection", None).unwrap_err();
        assert_eq!(classification(&error), "service");
        script.done();
    }

    #[test]
    fn verification_reports_actual_thread_and_diff_side() {
        for state in ["PENDING", "SUBMITTED"] {
            let comments = ["reply", "left", "right", "file"].map(|id| {
                json!({"id":id,"state":state,"path":"x","line":9,"startLine":null,
                    "subjectType":if id == "file" {"FILE"} else {"LINE"},"body":id})
            });
            let review_page = json!({"data":{"node":{"id":"r",
                "state":if state == "PENDING" {"PENDING"} else {"COMMENTED"},
                "url":"url","body":"summary","comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                "nodes":comments}}}});
            let thread = |id: &str, subject: &str, side: Value, comment: &str| {
                json!({"id":id,"subjectType":subject,"diffSide":side,
                    "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"id":comment}]}})
            };
            let first = json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                "pageInfo":{"hasNextPage":true,"endCursor":"next"},
                "nodes":[thread("actual-reply","LINE",json!("RIGHT"),"reply"),
                    thread("left-thread","LINE",json!("LEFT"),"left")]}}}}});
            let second = json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                "pageInfo":{"hasNextPage":false,"endCursor":null},
                "nodes":[thread("right-thread","LINE",json!("RIGHT"),"right"),
                    thread("file-thread","FILE",Value::Null,"file")]}}}}});
            let script = Script::new(vec![
                (
                    "graphql",
                    json!({"reviewId":"r","cur":null,"size":100}),
                    true,
                    review_page,
                ),
                (
                    "graphql",
                    json!({"owner":"acme","repo":"repo","num":42,"cur":null,"size":50}),
                    true,
                    first,
                ),
                (
                    "graphql",
                    json!({"owner":"acme","repo":"repo","num":42,"cur":"next","size":50}),
                    true,
                    second,
                ),
            ]);
            let target = target();
            let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
            let result = verify(&transport, &target, &json!({"reviewId":"r"})).unwrap();
            assert_eq!(result["comments"][0]["threadId"], "actual-reply");
            assert_ne!(result["comments"][0]["threadId"], "expected-reply");
            assert_eq!(result["comments"][1]["side"], "LEFT");
            assert_ne!(result["comments"][1]["side"], "RIGHT");
            assert_eq!(result["comments"][2]["side"], "RIGHT");
            assert_ne!(result["comments"][2]["side"], "LEFT");
            assert_eq!(result["comments"][3]["threadId"], "file-thread");
            assert_eq!(result["comments"][3]["side"], "");
            script.done();
        }
    }

    #[test]
    fn verification_fails_when_thread_or_line_side_is_missing() {
        for thread_nodes in [
            json!([]),
            json!([{"id":"thread","subjectType":"LINE","diffSide":null,
                "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"c"}]}}]),
        ] {
            let script = Script::new(vec![
                (
                    "graphql",
                    json!({"reviewId":"r","cur":null,"size":100}),
                    true,
                    json!({"data":{"node":{"id":"r","state":"PENDING","url":"url","body":"",
                        "comments":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
                        {"id":"c","state":"PENDING","path":"x","line":9,"startLine":null,"subjectType":"LINE","body":"x"}]}}}}),
                ),
                (
                    "graphql",
                    json!({"owner":"acme","repo":"repo","num":42,"cur":null,"size":50}),
                    true,
                    json!({"data":{"repository":{"pullRequest":{"reviewThreads":{
                        "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":thread_nodes}}}}}),
                ),
            ]);
            let target = target();
            let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
            let error = verify(&transport, &target, &json!({"reviewId":"r"})).unwrap_err();
            assert_eq!(classification(&error), "service");
            script.done();
        }
    }

    #[test]
    fn verification_rejects_repeated_cursor() {
        let page = json!({"data":{"node":{"id":"r","state":"PENDING","url":"url","body":"",
            "comments":{"pageInfo":{"hasNextPage":true,"endCursor":"same"},"nodes":[]}}}});
        let script = Script::new(vec![
            (
                "graphql",
                json!({"reviewId":"r","cur":null,"size":100}),
                true,
                page.clone(),
            ),
            (
                "graphql",
                json!({"reviewId":"r","cur":"same","size":100}),
                true,
                page,
            ),
        ]);
        let target = target();
        let transport = Transport::new(&target, None, &script, &NoHttp, &NoWait);
        verify(&transport, &target, &json!({"reviewId":"r"})).unwrap_err();
        script.done();
    }
}
