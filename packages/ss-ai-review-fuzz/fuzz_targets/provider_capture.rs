//! Exercises offline provider pagination with bounded scripted responses.
#![no_main]
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Inputs and generated fixtures are bounded; unexpected script shapes must fail"
)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use libfuzzer_sys::fuzz_target;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{
    Clock, Http, HttpResponse, Process, ProcessOutput, capture_target, collect_capture,
};

/// One expected request and its response; an empty queue is an immediate failure.
struct Reply {
    route: String,
    cursor: Option<String>,
    thread: Option<String>,
    body: Value,
    continuation: Option<String>,
}

/// Keeps each generated provider script isolated from live process and HTTP access.
struct Script {
    replies: RefCell<VecDeque<Reply>>,
    azure: bool,
    token_requested: Cell<bool>,
}

impl Process for Script {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if self.azure {
            assert_eq!(program, "az");
            assert!(
                !self.token_requested.replace(true),
                "unexpected token request"
            );
            return Ok(ProcessOutput::new(true, b"token".to_vec(), vec![]));
        }
        assert_eq!(program, "gh");
        let reply = self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("unexpected GitHub request");
        let graphql = reply.route.strip_prefix("graphql:");
        assert_eq!(
            arguments[1],
            if graphql.is_some() {
                "graphql"
            } else {
                &reply.route
            }
        );
        let method = &arguments[arguments.iter().position(|arg| arg == "--method").unwrap() + 1];
        assert_eq!(method, if graphql.is_some() { "POST" } else { "GET" });
        if let Some(expected_query) = graphql {
            let request: Value = serde_json::from_slice(input).unwrap();
            assert!(request["query"].as_str().unwrap().contains(expected_query));
            assert_eq!(request["variables"]["cur"], json!(reply.cursor));
            assert_eq!(request["variables"]["threadId"], json!(reply.thread));
        } else {
            assert!(input.is_empty());
            if reply.route.contains("per_page=100") {
                assert!(arguments.iter().any(|arg| arg == "--paginate"));
                assert!(arguments.iter().any(|arg| arg == "--slurp"));
            }
        }
        Ok(ProcessOutput::new(
            true,
            serde_json::to_vec(&reply.body).unwrap(),
            vec![],
        ))
    }
}

impl Http for Script {
    fn send(
        &self,
        method: &str,
        url: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert!(self.azure);
        assert_eq!(method, "GET");
        assert!(body.is_none());
        let reply = self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("unexpected Azure request");
        assert!(url.contains(&reply.route), "unexpected route: {url}");
        if let Some(cursor) = &reply.cursor {
            assert!(
                url.contains(&format!("continuationToken={cursor}")),
                "unexpected cursor: {url}"
            );
        } else {
            assert!(
                !url.contains("continuationToken="),
                "unexpected cursor: {url}"
            );
        }
        let mut headers = BTreeMap::new();
        if let Some(cursor) = reply.continuation {
            headers.insert("x-ms-continuationtoken".to_owned(), cursor);
        }
        Ok(HttpResponse::new(
            200,
            headers,
            serde_json::to_vec(&reply.body).unwrap(),
        ))
    }
}

/// Refuses any retry rather than sleeping or consulting wall-clock time.
struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("unexpected retry")
    }
}

/// Adds a single response to the finite request script.
fn reply(
    queue: &mut VecDeque<Reply>,
    route: &str,
    cursor: Option<String>,
    thread: Option<String>,
    body: Value,
    continuation: Option<String>,
) {
    queue.push_back(Reply {
        route: route.to_owned(),
        cursor,
        thread,
        body,
        continuation,
    });
}

/// Provides a connection page with explicit terminal state.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Owned JSON fixtures make generated page construction concise"
)]
fn page(nodes: Value, cursor: Option<String>) -> Value {
    json!({"nodes":nodes,"pageInfo":{"hasNextPage":cursor.is_some(),"endCursor":cursor}})
}

/// Produces distinct comments with one opener and replies to that opener.
fn comment(thread: usize, index: usize) -> Value {
    let id = format!("C{thread}-{index}");
    json!({"id":id,"databaseId":index+1,"url":null,"body":format!("body {thread}-{index} λ"),
        "createdAt":"2026-01-01T00:00:00Z","lastEditedAt":null,
        "replyTo":if index == 0 { Value::Null } else { json!({"id":format!("C{thread}-0")}) },
        "state":"SUBMITTED","author":null,"authorAssociation":"MEMBER","minimizedReason":null})
}

/// Generates independent outer and per-thread connections.
fn github(input: &[u8]) {
    let fault = input[1] % 4;
    let fault_nested = !input[2].is_multiple_of(2);
    let thread_count = usize::from(input[3] % 3) + 1;
    let comment_counts: Vec<_> = (0..thread_count)
        .map(|thread| (usize::from(input[4]) + thread * usize::from(input[5] % 3)) % 3 + 2)
        .collect();
    let mut replies = VecDeque::new();
    let account = json!({"id":7,"login":"alice","node_id":"U7","type":"User","html_url":"https://github.com/alice"});
    let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    reply(
        &mut replies,
        "repos/acme/repo/pulls/42",
        None,
        None,
        json!({"number":42,"title":"PR","body":"","draft":false,"user":account,"created_at":"2026-01-01T00:00:00Z","head":{"sha":b,"ref":"topic","repo":{"full_name":"acme/repo"}},"base":{"sha":a,"ref":"main","repo":{"full_name":"acme/repo"}},"mergeable_state":"clean","mergeable":true,"merge_commit_sha":null}),
        None,
    );
    for route in [
        "repos/acme/repo/pulls/42/reviews?per_page=100",
        "repos/acme/repo/issues/42/comments?per_page=100",
    ] {
        reply(&mut replies, route, None, None, json!([[]]), None);
    }
    reply(
        &mut replies,
        "repos/acme/repo/pulls/42/requested_reviewers?per_page=100",
        None,
        None,
        json!([{"users":[],"teams":[]}]),
        None,
    );

    // At least two outer pages let every fault occur after accepted evidence.
    let pages = thread_count.max(2);
    for index in 0..pages {
        let nodes = if index < thread_count {
            let id = format!("T{index}");
            json!([{"id":id,"isResolved":false,"isOutdated":false,"isCollapsed":false,
                "path":null,"line":null,"originalLine":null,"startLine":null,"originalStartLine":null,
                "diffSide":null,"startDiffSide":null,"subjectType":"LINE","resolvedBy":null,
                "comments":page(json!([comment(index,0)]),Some(format!("c{index}-0")))}])
        } else {
            json!([])
        };
        let mut connection = page(nodes, (index + 1 < pages).then(|| format!("t{index}")));
        if fault != 0 && !fault_nested && index == 1 {
            match fault {
                1 => connection["pageInfo"] = json!({"hasNextPage":true,"endCursor":"t0"}),
                2 => connection["pageInfo"] = json!({"hasNextPage":true,"endCursor":null}),
                _ => connection["nodes"] = Value::Null,
            }
        }
        reply(
            &mut replies,
            "graphql:reviewThreads",
            (index > 0).then(|| format!("t{}", index - 1)),
            None,
            json!({"data":{"repository":{"pullRequest":{"reviewThreads":connection}}}}),
            None,
        );
        if fault != 0 && !fault_nested && index == 1 {
            break;
        }
    }
    if fault == 0 || fault_nested {
        'threads: for (thread, &comment_count) in comment_counts.iter().enumerate() {
            for index in 1..comment_count {
                let mut connection = page(
                    json!([comment(thread, index)]),
                    (index + 1 < comment_count).then(|| format!("c{thread}-{index}")),
                );
                if fault != 0 && thread == 0 && index == 1 {
                    match fault {
                        1 => {
                            connection["pageInfo"] =
                                json!({"hasNextPage":true,"endCursor":format!("c{thread}-0")});
                        }
                        2 => connection["pageInfo"] = json!({"hasNextPage":true,"endCursor":null}),
                        _ => connection["nodes"] = Value::Null,
                    }
                }
                reply(
                    &mut replies,
                    "graphql:comments",
                    Some(format!("c{thread}-{}", index - 1)),
                    Some(format!("T{thread}")),
                    json!({"data":{"node":{"comments":connection}}}),
                    None,
                );
                if fault != 0 && thread == 0 && index == 1 {
                    break 'threads;
                }
            }
        }
    }
    if fault == 0 {
        for name in ["comments", "reviews", "timelineItems"] {
            reply(
                &mut replies,
                &format!("graphql:{name}"),
                None,
                None,
                json!({"data":{"repository":{"pullRequest":{name:page(json!([]),None)}}}}),
                None,
            );
        }
        reply(&mut replies, "user", None, None, account, None);
        reply(
            &mut replies,
            &format!("repos/acme/repo/compare/{a}...{b}"),
            None,
            None,
            json!({"merge_base_commit":{"sha":a}}),
            None,
        );
    }
    let script = Script {
        replies: RefCell::new(replies),
        azure: false,
        token_requested: Cell::new(false),
    };
    let target = capture_target(&json!({"url":"https://github.com/acme/repo/pull/42"})).unwrap();
    let result = collect_capture(&target, None, &script, &script, &FixedClock);
    assert!(
        script.replies.borrow().is_empty(),
        "script stopped before its last response"
    );
    if fault != 0 {
        result.unwrap_err();
        return;
    }
    let capture = result.unwrap();
    let threads = capture["threads"].as_array().unwrap();
    assert_eq!(threads.len(), thread_count);
    for (thread, actual) in threads.iter().enumerate() {
        assert_eq!(actual["id"], format!("T{thread}"));
        let comments = actual["comments"].as_array().unwrap();
        assert_eq!(comments.len(), comment_counts[thread]);
        for (index, actual) in comments.iter().enumerate() {
            assert_eq!(actual["id"], format!("C{thread}-{index}"));
            assert_eq!(actual["content"], format!("body {thread}-{index} λ"));
        }
    }
}

/// Generates independent Azure iteration, reviewer, and discussion pages.
fn azure(input: &[u8]) {
    let fault = input[1] % 3;
    let selected = usize::from(input[2] % 3);
    let mut replies = VecDeque::new();
    reply(
        &mut replies,
        "_apis/connectionData",
        None,
        None,
        json!({"authenticatedUser":{"id":"account","subjectDescriptor":"aad.account"}}),
        None,
    );
    reply(
        &mut replies,
        "pullRequests/7?",
        None,
        None,
        json!({"pullRequestId":7,"createdBy":{"id":"author"},"title":"PR","sourceRefName":"topic","targetRefName":"main"}),
        None,
    );
    let names = ["iterations", "reviewers", "threads"];
    let mut counts = [0; 3];
    for (kind, name) in names.iter().enumerate() {
        let count = usize::from(input[3 + kind] % 3) + 2;
        counts[kind] = count;
        for index in 0..count {
            let id = index + 1;
            let item = match kind {
                0 => {
                    json!({"id":id,"sourceRefCommit":{"commitId":format!("source-{id}")},"targetRefCommit":{"commitId":"target"},"commonRefCommit":{"commitId":format!("base-{id}")}})
                }
                1 => json!({"id":format!("reviewer-{id}")}),
                _ => {
                    json!({"id":id,"comments":[{"id":1,"author":{"id":"author"},"content":format!("thread {id} λ")}]})
                }
            };
            let mut payload = json!({"count":1,"value":[item]});
            let mut continuation = (index + 1 < count).then(|| format!("{name}{index}"));
            if fault != 0 && kind == selected && index == 1 {
                if fault == 1 {
                    continuation = Some(format!("{name}0"));
                } else {
                    payload["value"] = Value::Null;
                }
            }
            reply(
                &mut replies,
                &format!("/{name}?"),
                (index > 0).then(|| format!("{name}{}", index - 1)),
                None,
                payload,
                continuation,
            );
            if fault != 0 && kind == selected && index == 1 {
                break;
            }
        }
        if fault != 0 && kind == selected {
            break;
        }
    }
    let script = Script {
        replies: RefCell::new(replies),
        azure: true,
        token_requested: Cell::new(false),
    };
    let target =
        capture_target(&json!({"url":"https://dev.azure.com/org/proj/_git/repo/pullrequest/7"}))
            .unwrap();
    let result = collect_capture(&target, None, &script, &script, &FixedClock);
    assert!(script.token_requested.get());
    assert!(
        script.replies.borrow().is_empty(),
        "script stopped before its last response"
    );
    if fault != 0 {
        result.unwrap_err();
        return;
    }
    let capture = result.unwrap();
    assert_eq!(capture["revisions"].as_array().unwrap().len(), counts[0]);
    assert_eq!(capture["reviewers"].as_array().unwrap().len(), counts[1]);
    assert_eq!(capture["threads"].as_array().unwrap().len(), counts[2]);
    for index in 0..counts[0] {
        assert_eq!(capture["revisions"][index]["id"], index + 1);
        assert_eq!(
            capture["revisions"][index]["sourceCommit"],
            format!("source-{}", index + 1)
        );
    }
    for index in 0..counts[1] {
        assert_eq!(
            capture["reviewers"][index]["identity"]["id"],
            format!("reviewer-{}", index + 1)
        );
    }
    for index in 0..counts[2] {
        assert_eq!(capture["threads"][index]["id"], index + 1);
        assert_eq!(
            capture["threads"][index]["comments"][0]["content"],
            format!("thread {} λ", index + 1)
        );
    }
}

fuzz_target!(|input: &[u8]| {
    // The generator reads only these bytes and caps each connection to a few pages.
    if input.len() < 6 || input.len() > 64 {
        return;
    }
    if input[0].is_multiple_of(2) {
        github(input);
    } else {
        azure(input);
    }
});
