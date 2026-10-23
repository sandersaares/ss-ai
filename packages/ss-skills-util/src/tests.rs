#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Tests use bounded fixtures with explicit expected shapes"
)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write;
use std::io;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};

use crate::api::Api;
use crate::clock::timestamp;
use crate::error::fail;
use crate::identity::Identity;
use crate::selection::{Candidate, Comment, Journal, Provider, RemoteClaim, State, Status, run};
use crate::target::{Platform, Target};
use crate::transport::Transport;
use crate::{
    Clock, Http, HttpResponse, NativeHttp, NativeProcess, Process, ProcessOutput, SystemClock,
};

// Pin concrete binary/test boundary contracts without promising a supported Rust SDK.
static_assertions::assert_impl_all!(SystemClock: UnwindSafe, RefUnwindSafe);
static_assertions::assert_impl_all!(NativeProcess: UnwindSafe, RefUnwindSafe);
static_assertions::assert_impl_all!(ProcessOutput: UnwindSafe, RefUnwindSafe);
static_assertions::assert_impl_all!(HttpResponse: UnwindSafe, RefUnwindSafe);
// reqwest's blocking client owns a worker join handle without these guarantees.
static_assertions::assert_not_impl_any!(NativeHttp: UnwindSafe, RefUnwindSafe);

/// Advances only when the operation explicitly waits; no unit test reads real time.
struct FakeClock {
    now: RefCell<Timestamp>,
    waits: RefCell<Vec<Duration>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: RefCell::new(timestamp("2026-09-28T10:00:00Z").unwrap()),
            waits: RefCell::new(Vec::new()),
        }
    }
    fn advance(&self, delta: Duration) {
        *self.now.borrow_mut() += delta;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        *self.now.borrow()
    }
    fn wait(&self, duration: Duration) -> Result<(), AppError> {
        assert!(
            self.waits.borrow().len() < 20,
            "Unexpected nonprogressing retry loop"
        );
        self.waits.borrow_mut().push(duration);
        self.advance(duration);
        Ok(())
    }
}

/// Represents persisted complete records without accessing the filesystem.
#[derive(Default)]
struct MemoryJournal {
    state: Option<State>,
    transitions: Vec<Status>,
}

impl Journal for MemoryJournal {
    fn load(&mut self) -> Result<Option<State>, AppError> {
        Ok(self.state.clone())
    }
    fn save(&mut self, state: &State) -> Result<(), AppError> {
        state.validate()?;
        self.transitions.push(state.status);
        self.state = Some(state.clone());
        Ok(())
    }
}

/// Supplies controlled remote changes around a real selection state machine.
struct FakeProvider {
    comments: BTreeMap<u32, Vec<Comment>>,
    reasons: BTreeMap<u32, String>,
    creates: Vec<u32>,
    deletes: Vec<String>,
    fail_create: bool,
    fail_delete: bool,
    fail_read: bool,
}

impl FakeProvider {
    fn new() -> Self {
        Self {
            comments: BTreeMap::new(),
            reasons: BTreeMap::new(),
            creates: Vec::new(),
            deletes: Vec::new(),
            fail_create: false,
            fail_delete: false,
            fail_read: false,
        }
    }
}

impl Provider for FakeProvider {
    fn targets(&mut self, repository: &Target) -> Result<Vec<Target>, AppError> {
        Ok([11, 10]
            .map(|id| repository.candidate(id.try_into().unwrap()))
            .to_vec())
    }
    fn candidate(
        &mut self,
        target: &Target,
        _: &Identity,
        _: Timestamp,
    ) -> Result<Candidate, AppError> {
        if self.fail_read {
            return Err(fail("service", "Injected read failure"));
        }
        Ok(Candidate {
            target: target.clone(),
            reason: self.reasons.get(&target.pull_request_id).cloned(),
            latest: Some(timestamp("2026-09-27T00:00:00Z").unwrap()),
        })
    }
    fn comments(&mut self, target: &Target) -> Result<Vec<Comment>, AppError> {
        Ok(self
            .comments
            .get(&target.pull_request_id)
            .cloned()
            .unwrap_or_default())
    }
    fn create_claim(&mut self, target: &Target, body: &str) -> Result<RemoteClaim, AppError> {
        self.creates.push(target.pull_request_id);
        if self.fail_create {
            return Err(fail("ambiguousWrite", "Lost response"));
        }
        let id = format!("own-{}", target.pull_request_id);
        let thread_id = (target.platform == Platform::AzureDevOps).then(|| "thread".to_owned());
        self.comments
            .entry(target.pull_request_id)
            .or_default()
            .push(Comment {
                id: id.clone(),
                thread_id: thread_id.clone(),
                body: body.to_owned(),
                created_at: "2026-09-28T10:00:00Z".to_owned(),
                author: identity(target.platform),
            });
        Ok(RemoteClaim { id, thread_id })
    }
    fn remove_claim(&mut self, target: &Target, comment: &Comment) -> Result<(), AppError> {
        if self.fail_delete {
            return Err(fail("permissionDenied", "Deletion denied"));
        }
        self.deletes.push(comment.id.clone());
        self.comments
            .entry(target.pull_request_id)
            .or_default()
            .retain(|entry| entry.id != comment.id);
        Ok(())
    }
}

fn repository(platform: Platform) -> Target {
    Target::repository(match platform {
        Platform::GitHub => "https://github.com/example/widgets",
        Platform::AzureDevOps => "https://dev.azure.com/org/A%20Project/_git/A%20Repo",
    })
    .unwrap()
}

fn identity(platform: Platform) -> Identity {
    Identity::from_account(platform, &match platform {
        Platform::GitHub => json!({"id":42,"login":"reviewer","type":"User"}),
        Platform::AzureDevOps => json!({"id":"reviewer-id","uniqueName":"reviewer@example.com","descriptor":"aad.reviewer"}),
    }).unwrap()
}

fn initial(platform: Platform) -> State {
    let path = if cfg!(windows) {
        r"C:\session\state.json"
    } else {
        "/session/state.json"
    };
    State::new(
        &repository(platform),
        &identity(platform),
        "session-a",
        path,
    )
}

fn select(
    provider: &mut FakeProvider,
    journal: &mut MemoryJournal,
    clock: &FakeClock,
    confirm: bool,
) -> State {
    run(
        provider,
        journal,
        clock,
        initial(Platform::GitHub),
        "Reserving for review.",
        confirm,
    )
    .unwrap()
}

fn competing() -> Comment {
    Comment { id: "competitor".to_owned(), thread_id: None, author: identity(Platform::GitHub),
        body: "Comment\n\n<!-- ss-review-proactor:v1 session=session-b claim=aaaaaaaa-1234-1234-1234-123456789abc -->".to_owned(),
        created_at: "2026-09-28T09:59:00Z".to_owned() }
}

#[test]
fn successive_review_cycles_keep_independent_journals_and_claims() {
    for platform in [Platform::GitHub, Platform::AzureDevOps] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journals = [
            MemoryJournal::default(),
            MemoryJournal::default(),
            MemoryJournal::default(),
        ];
        let mut snapshots = Vec::new();
        let mut tokens = Vec::new();
        for (cycle, journal) in journals.iter_mut().enumerate() {
            let mut initial = initial(platform);
            initial.state_path = format!("{}.cycle-{cycle}", initial.state_path);
            let selected = run(
                &mut provider,
                journal,
                &clock,
                initial.clone(),
                "Reserving",
                false,
            )
            .unwrap();
            if cycle == 2 {
                assert_eq!(selected.status, Status::NoCandidate);
                continue;
            }
            let confirmed =
                run(&mut provider, journal, &clock, initial, "Reserving", true).unwrap();
            assert_eq!(confirmed.status, Status::Confirmed);
            assert_eq!(confirmed.session_id, "session-a");
            let id = confirmed.target.as_ref().unwrap().pull_request_id;
            assert_eq!(id, 10 + u32::try_from(cycle).unwrap());
            assert_eq!(confirmed.attempted, [id.try_into().unwrap()]);
            tokens.push(confirmed.claim.as_ref().unwrap().token.clone());
            snapshots.push(serde_json::to_value(journal.state.as_ref().unwrap()).unwrap());
            // Reviewer registration after a completed review excludes it from the next cycle.
            provider.reasons.insert(id, "alreadyReviewer".to_owned());
        }
        for (journal, snapshot) in journals.iter().zip(snapshots) {
            assert_eq!(
                serde_json::to_value(journal.state.as_ref().unwrap()).unwrap(),
                snapshot
            );
        }
        assert_ne!(tokens[0], tokens[1]);
        assert_eq!(provider.creates, [10, 11]);
        assert!(provider.deletes.is_empty());
        assert_eq!(provider.comments[&10].len(), 1);
        assert_eq!(provider.comments[&11].len(), 1);
        assert_eq!(*clock.waits.borrow(), [Duration::from_secs(60); 2]);
    }
}

#[test]
fn selection_and_confirmation_preserve_both_provider_journals() {
    for platform in [Platform::GitHub, Platform::AzureDevOps] {
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        let clock = FakeClock::new();
        let state = run(
            &mut provider,
            &mut journal,
            &clock,
            initial(platform),
            "Reserving for review.",
            false,
        )
        .unwrap();
        assert_eq!(state.status, Status::Claimed);
        assert_eq!(state.target.unwrap().pull_request_id, 10);
        assert!(clock.waits.borrow().is_empty());
        assert_eq!(
            journal.transitions,
            [Status::Selecting, Status::Claiming, Status::Claimed]
        );
        let state = run(
            &mut provider,
            &mut journal,
            &clock,
            initial(platform),
            "Reserving for review.",
            true,
        )
        .unwrap();
        assert_eq!(state.status, Status::Confirmed);
        assert_eq!(
            state.diagnostics.last().unwrap().reason,
            "soleLiveClaimAfterPreparation"
        );
        assert_eq!(*clock.waits.borrow(), [Duration::from_secs(60)]);
        assert_eq!(provider.creates, [10]);
        assert!(provider.deletes.is_empty());
    }
}

#[test]
fn preparation_counts_toward_the_single_confirmation_interval() {
    for (elapsed_ms, wait) in [
        (0, 60),
        (25_000, 35),
        (59_500, 1),
        (60_000, 0),
        (75_000, 0),
        (900_000, 0),
    ] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        select(&mut provider, &mut journal, &clock, false);
        clock.advance(Duration::from_millis(elapsed_ms));
        assert_eq!(
            select(&mut provider, &mut journal, &clock, true).status,
            Status::Confirmed
        );
        assert_eq!(
            *clock.waits.borrow(),
            if wait == 0 {
                vec![]
            } else {
                vec![Duration::from_secs(wait)]
            }
        );
    }
}

#[test]
fn contention_discards_only_owned_comment_and_never_chooses_an_earliest_claim() {
    let clock = FakeClock::new();
    let mut provider = FakeProvider::new();
    let mut journal = MemoryJournal::default();
    select(&mut provider, &mut journal, &clock, false);
    provider.comments.entry(10).or_default().push(competing());
    let replacement = select(&mut provider, &mut journal, &clock, true);
    assert_eq!(replacement.status, Status::Claimed);
    assert_eq!(replacement.target.unwrap().pull_request_id, 11);
    assert_eq!(
        replacement.attempted,
        [10, 11].map(|id| id.try_into().unwrap())
    );
    assert_eq!(replacement.diagnostics[0].reason, "competingClaims");
    assert_eq!(provider.comments[&10][0].id, "competitor");
    assert_eq!(provider.deletes, ["own-10"]);
    assert_eq!(
        journal.transitions,
        [
            Status::Selecting,
            Status::Claiming,
            Status::Claimed,
            Status::Discarding,
            Status::Selecting,
            Status::Claiming,
            Status::Claimed,
        ]
    );
    assert_eq!(
        select(&mut provider, &mut journal, &clock, true).status,
        Status::Confirmed
    );
    assert_eq!(
        *clock.waits.borrow(),
        [Duration::from_secs(60), Duration::from_secs(60)]
    );
}

#[test]
fn deleted_expired_or_ineligible_claims_get_replaced() {
    for reason in ["closed", "alreadyReviewer", "missing", "expired"] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        select(&mut provider, &mut journal, &clock, false);
        match reason {
            "missing" => {
                provider.comments.remove(&10);
            }
            "expired" => clock.advance(Duration::from_hours(1)),
            reason => {
                provider.reasons.insert(10, reason.to_owned());
            }
        }
        let state = select(&mut provider, &mut journal, &clock, true);
        assert_eq!(state.target.unwrap().pull_request_id, 11);
        assert_eq!(provider.deletes.len(), usize::from(reason != "missing"));
    }
}

#[test]
fn no_candidates_and_existing_claims_do_not_write_remote_state() {
    for blocked in [false, true] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        for id in [10, 11] {
            if blocked {
                provider.comments.insert(id, vec![competing()]);
            } else {
                provider.reasons.insert(id, "draft".to_owned());
            }
        }
        assert_eq!(
            select(&mut provider, &mut journal, &clock, false).status,
            Status::NoCandidate
        );
        assert!(provider.creates.is_empty());
        assert!(clock.waits.borrow().is_empty());
    }
}

#[test]
fn old_claims_and_ordinary_discussion_survive_confirmation() {
    let clock = FakeClock::new();
    let mut provider = FakeProvider::new();
    let mut journal = MemoryJournal::default();
    let mut expired = competing();
    expired.created_at = "2026-09-28T09:00:00Z".to_owned();
    let mut ordinary = competing();
    ordinary.id = "ordinary".to_owned();
    ordinary.body = "Ordinary discussion".to_owned();
    provider.comments.insert(10, vec![expired, ordinary]);
    select(&mut provider, &mut journal, &clock, false);
    provider
        .comments
        .get_mut(&10)
        .unwrap()
        .last_mut()
        .unwrap()
        .body = journal
        .state
        .as_ref()
        .unwrap()
        .claim
        .as_ref()
        .unwrap()
        .body
        .replace('\n', "\r\n");
    assert_eq!(
        select(&mut provider, &mut journal, &clock, true).status,
        Status::Confirmed
    );
    assert_eq!(provider.comments[&10].len(), 3);
    assert!(provider.deletes.is_empty());
}

#[test]
fn live_claim_checks_still_validate_later_timestamp_evidence() {
    for confirm in [false, true] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        if confirm {
            select(&mut provider, &mut journal, &clock, false);
        } else {
            provider.comments.insert(10, vec![competing()]);
        }
        let mut malformed = competing();
        malformed.id = "malformed-date".to_owned();
        malformed.created_at = "not a timestamp".to_owned();
        provider.comments.entry(10).or_default().push(malformed);
        let error = run(
            &mut provider,
            &mut journal,
            &clock,
            initial(Platform::GitHub),
            "Reserving",
            confirm,
        )
        .unwrap_err();
        assert_eq!(crate::classification(&error), "service");
        assert!(provider.deletes.is_empty());
        assert_eq!(provider.creates.len(), usize::from(confirm));
    }
}

#[test]
fn tampered_owner_body_or_duplicate_identifiers_cannot_delete_claims() {
    for mutation in ["author", "body", "duplicate"] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        select(&mut provider, &mut journal, &clock, false);
        let comments = provider.comments.get_mut(&10).unwrap();
        match mutation {
            "author" => {
                comments[0].author.id = Some("other".to_owned());
                comments[0].author.unique_name = None;
            }
            "body" => comments[0].body = "Replaced body".to_owned(),
            _ => comments.push(comments[0].clone()),
        }
        run(
            &mut provider,
            &mut journal,
            &clock,
            initial(Platform::GitHub),
            "Reserving",
            true,
        )
        .unwrap_err();
        assert!(provider.deletes.is_empty());
        assert_eq!(provider.creates, [10]);
    }
}

#[test]
fn ambiguous_writes_and_completed_checks_cannot_be_replayed() {
    for stage in ["create", "delete", "complete"] {
        let clock = FakeClock::new();
        let mut provider = FakeProvider::new();
        let mut journal = MemoryJournal::default();
        if stage == "create" {
            provider.fail_create = true;
            run(
                &mut provider,
                &mut journal,
                &clock,
                initial(Platform::GitHub),
                "Reserving",
                false,
            )
            .unwrap_err();
            assert_eq!(journal.state.as_ref().unwrap().status, Status::Claiming);
        } else {
            select(&mut provider, &mut journal, &clock, false);
            if stage == "delete" {
                provider.fail_delete = true;
                provider.comments.entry(10).or_default().push(competing());
                run(
                    &mut provider,
                    &mut journal,
                    &clock,
                    initial(Platform::GitHub),
                    "Reserving",
                    true,
                )
                .unwrap_err();
                assert_eq!(journal.state.as_ref().unwrap().status, Status::Discarding);
            } else {
                select(&mut provider, &mut journal, &clock, true);
            }
        }
        run(
            &mut provider,
            &mut journal,
            &clock,
            initial(Platform::GitHub),
            "Reserving",
            true,
        )
        .unwrap_err();
        run(
            &mut provider,
            &mut journal,
            &clock,
            initial(Platform::GitHub),
            "Reserving",
            false,
        )
        .unwrap_err();
        assert_eq!(provider.creates, [10]);
    }
}

#[test]
fn journal_ownership_and_required_fields_are_validated() {
    let clock = FakeClock::new();
    let mut provider = FakeProvider::new();
    let mut journal = MemoryJournal::default();
    select(&mut provider, &mut journal, &clock, false);
    for property in [
        "schemaVersion",
        "sessionId",
        "repositoryUrl",
        "identity",
        "status",
        "target",
        "claim",
        "attempted",
        "diagnostics",
        "statePath",
    ] {
        let mut value = serde_json::to_value(journal.state.as_ref().unwrap()).unwrap();
        value.as_object_mut().unwrap().remove(property);
        serde_json::from_value::<State>(value).unwrap_err();
    }
    for change in [
        "session",
        "repository",
        "identity",
        "schema",
        "target",
        "deadline",
    ] {
        let mut state = journal.state.clone().unwrap();
        match change {
            "session" => state.session_id = "other".to_owned(),
            "repository" => state.repository_url = "https://github.com/example/other".to_owned(),
            "identity" => {
                state.identity.id = Some("99".to_owned());
                state.identity.unique_name = None;
            }
            "schema" => state.schema_version = 99,
            "target" => state.target.as_mut().unwrap().repository = "other".to_owned(),
            _ => state.claim.as_mut().unwrap().check_after = None,
        }
        let mut damaged = MemoryJournal {
            state: Some(state),
            transitions: Vec::new(),
        };
        run(
            &mut provider,
            &mut damaged,
            &clock,
            initial(Platform::GitHub),
            "Reserving",
            true,
        )
        .unwrap_err();
    }
    assert_eq!(provider.creates, [10]);
    assert!(provider.deletes.is_empty());
}

#[test]
fn read_failures_do_not_become_no_candidate() {
    let clock = FakeClock::new();
    let mut provider = FakeProvider::new();
    provider.fail_read = true;
    let mut journal = MemoryJournal::default();
    run(
        &mut provider,
        &mut journal,
        &clock,
        initial(Platform::GitHub),
        "Reserving",
        false,
    )
    .unwrap_err();
    assert_ne!(journal.state.unwrap().status, Status::NoCandidate);
    assert!(provider.creates.is_empty());
}

/// A strict scripted primitive catches unexpected or omitted provider calls.
struct Script {
    steps: RefCell<VecDeque<Step>>,
    tokens: Cell<usize>,
}

impl Script {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: RefCell::new(steps.into()),
            tokens: Cell::new(0),
        }
    }
    fn done(&self) {
        assert!(self.steps.borrow().is_empty(), "Unconsumed provider calls");
    }
    fn next(&self, path: &str, method: &str, input: Option<&[u8]>) -> Step {
        let step = self
            .steps
            .borrow_mut()
            .pop_front()
            .expect("Unexpected provider call");
        assert_eq!(path, step.path);
        assert_eq!(method, step.method);
        if let Some(expected) = &step.body {
            assert_eq!(
                &serde_json::from_slice::<Value>(input.unwrap()).unwrap(),
                expected
            );
        } else {
            assert!(input.is_none_or(<[u8]>::is_empty));
        }
        step
    }
}

impl Process for Script {
    fn execute(
        &self,
        program: &str,
        arguments: &[String],
        input: &[u8],
    ) -> io::Result<ProcessOutput> {
        if program == "az" {
            self.tokens.set(self.tokens.get() + 1);
            assert_eq!(
                arguments,
                [
                    "account",
                    "get-access-token",
                    "--resource",
                    "499b84ac-1321-427f-aa17-267ca6975798",
                    "--query",
                    "accessToken",
                    "--output",
                    "tsv",
                    "--tenant",
                    "fixture-tenant"
                ]
            );
            return Ok(ProcessOutput::new(
                true,
                b"fixture-oauth".to_vec(),
                Vec::new(),
            ));
        }
        assert_eq!(program, "gh");
        let path = &arguments[1];
        let method = &arguments[arguments
            .iter()
            .position(|value| value == "--method")
            .unwrap()
            + 1];
        let step = self.next(path, method, Some(input));
        if path.contains("per_page=") {
            assert!(arguments.contains(&"--paginate".to_owned()));
            assert!(arguments.contains(&"--slurp".to_owned()));
        }
        let payload = step.response.to_string();
        let output = if step.status != 200 || !step.headers.is_empty() {
            let mut headers = String::new();
            for (name, value) in &step.headers {
                write!(headers, "{name}: {value}\r\n").unwrap();
            }
            format!("HTTP/2 {}\r\n{headers}\r\n{payload}", step.status)
        } else {
            payload
        };
        Ok(ProcessOutput::new(
            step.status < 400,
            output.into_bytes(),
            Vec::new(),
        ))
    }
}

impl Http for Script {
    fn send(
        &self,
        method: &str,
        url: &str,
        token: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(token, "fixture-oauth");
        let step = self.next(url, method, body);
        Ok(HttpResponse::new(
            step.status,
            step.headers,
            serde_json::to_vec(&step.response).unwrap(),
        ))
    }
}

/// One expected provider exchange, with response headers retained for retry evidence.
struct Step {
    path: String,
    method: String,
    body: Option<Value>,
    response: Value,
    status: u16,
    headers: BTreeMap<String, String>,
}

impl Step {
    fn read(path: &str, response: Value) -> Self {
        Self {
            path: path.to_owned(),
            method: "GET".to_owned(),
            body: None,
            response,
            status: 200,
            headers: BTreeMap::new(),
        }
    }
    fn write(path: &str, method: &str, body: Value, response: Value) -> Self {
        Self {
            method: method.to_owned(),
            body: Some(body),
            ..Self::read(path, response)
        }
    }
}

fn api<'a>(platform: Platform, script: &'a Script, clock: &'a FakeClock) -> Api<'a> {
    Api {
        transport: Transport::new(
            &repository(platform),
            Some("fixture-tenant"),
            script,
            script,
            clock,
        ),
    }
}

fn ado(path: &str, query: &str) -> String {
    format!("https://dev.azure.com/org/{path}?{query}api-version=7.1")
}

#[test]
fn github_owned_pages_preserve_order_objects_and_shape_errors() {
    let path = "repos/example/widgets/pulls?per_page=100";
    let script = Script::new(vec![Step::read(
        path,
        json!([
            [{"number":10}], [], [{"number":11}], {"users":[],"teams":[]}
        ]),
    )]);
    let clock = FakeClock::new();
    let items = api(Platform::GitHub, &script, &clock)
        .transport
        .list("repos/example/widgets/pulls")
        .unwrap();
    assert_eq!(
        items,
        vec![
            json!({"number":10}),
            json!({"number":11}),
            json!({"users":[],"teams":[]})
        ]
    );
    script.done();

    for response in [
        Value::Null,
        json!({}),
        json!([null]),
        json!([true]),
        json!([42]),
        json!(["page"]),
    ] {
        let script = Script::new(vec![Step::read(path, response)]);
        let error = api(Platform::GitHub, &script, &clock)
            .transport
            .list("repos/example/widgets/pulls")
            .unwrap_err();
        assert_eq!(crate::classification(&error), "service");
        script.done();
    }
}

#[test]
fn github_complete_commit_pages_and_malformed_paging() {
    for malformed in [false, true] {
        let mut steps = Vec::new();
        for (cursor, next, recent) in [
            (None, Some("next"), false),
            (Some("next"), Some("last"), false),
            (Some("last"), None, true),
        ] {
            let nodes = vec![
                json!({"commit":{"committedDate": if recent { "2026-09-27T10:00:00+02:00" } else { "2020-01-01T00:00:00Z" }}});
                if recent { 51 } else { 100 }
            ];
            steps.push(Step::write("graphql", "POST", json!({"query": crate::api::COMMIT_QUERY, "variables":{"owner":"example","repo":"widgets","num":10,"cur":cursor,"size":100}}),
                json!({"data":{"repository":{"pullRequest":{"commits":{"nodes":nodes,"pageInfo":{"hasNextPage":next.is_some(),"endCursor": if malformed && cursor.is_some() {Some("next")} else {next}}}}}}})));
            if malformed && cursor.is_some() {
                break;
            }
        }
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let mut api = api(Platform::GitHub, &script, &clock);
        let result = api.commit_recency(
            &repository(Platform::GitHub).candidate(10.try_into().unwrap()),
            clock.now(),
        );
        if malformed {
            result.unwrap_err();
        } else {
            assert_eq!(
                result.unwrap(),
                (Some(timestamp("2026-09-27T08:00:00Z").unwrap()), true)
            );
        }
        script.done();
    }
}

#[test]
fn ado_commit_tokens_use_committer_dates_and_fail_on_repetition() {
    for repeated in [false, true] {
        let path = "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/commits";
        let mut first = Step::read(
            &ado(path, "%24top=100&"),
            json!({"count":1,"value":[{"committer":{"date":"2020-01-01T00:00:00Z"},"author":{"date":"2026-09-28T09:00:00Z"}}]}),
        );
        first
            .headers
            .insert("x-ms-continuationtoken".to_owned(), "opaque".to_owned());
        let mut second = Step::read(
            &ado(path, "%24top=100&continuationToken=opaque&"),
            json!({"count":1,"value":[{"committer":{"date":"2026-09-27T00:00:00Z"}}]}),
        );
        if repeated {
            second
                .headers
                .insert("x-ms-continuationtoken".to_owned(), "opaque".to_owned());
        }
        let script = Script::new(vec![first, second]);
        let clock = FakeClock::new();
        let result = api(Platform::AzureDevOps, &script, &clock).commit_recency(
            &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
            clock.now(),
        );
        if repeated {
            result.unwrap_err();
        } else {
            assert!(result.unwrap().1);
        }
        script.done();
    }
}

#[test]
fn ado_short_pr_pages_continue_until_empty_and_repeated_prs_fail() {
    for repeated in [false, true] {
        let path = "A%20Project/_apis/git/repositories/A%20Repo/pullRequests";
        let mut steps = vec![
            Step::read(
                &ado(path, "%24skip=0&%24top=100&searchCriteria.status=active&"),
                json!({"count":1,"value":[{"pullRequestId":10}]}),
            ),
            Step::read(
                &ado(path, "%24skip=1&%24top=100&searchCriteria.status=active&"),
                json!({"count":1,"value":[{"pullRequestId":if repeated {10} else {11}}]}),
            ),
        ];
        if !repeated {
            steps.push(Step::read(
                &ado(path, "%24skip=2&%24top=100&searchCriteria.status=active&"),
                json!({"count":0,"value":[]}),
            ));
        }
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let result =
            api(Platform::AzureDevOps, &script, &clock).targets(&repository(Platform::AzureDevOps));
        if repeated {
            result.err().unwrap();
        } else {
            assert_eq!(result.unwrap().len(), 2);
        }
        script.done();
    }
}

#[test]
fn registration_preserves_existing_ado_votes_metadata_and_identity_aliases() {
    for matching in ["id", "descriptor", "uniqueName"] {
        let mut reviewer = json!({"id":"different","descriptor":"aad.different","uniqueName":"different@example.com","vote":-10,"hasDeclined":true,"isRequired":true,"isFlagged":true,"votedFor":[{"id":"team"}]});
        let own = serde_json::to_value(identity(Platform::AzureDevOps)).unwrap();
        reviewer[matching] = own[matching].clone();
        let script = Script::new(vec![Step::read(
            &ado(
                "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers",
                "",
            ),
            json!({"count":1,"value":[reviewer]}),
        )]);
        let clock = FakeClock::new();
        let result = api(Platform::AzureDevOps, &script, &clock)
            .register(
                &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
                &identity(Platform::AzureDevOps),
            )
            .unwrap();
        assert_eq!(result["status"], "alreadyRegistered");
        script.done();
    }
}

#[test]
fn registration_adds_only_the_new_ado_identity_with_neutral_vote() {
    let path = "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers";
    let script = Script::new(vec![
        Step::read(
            &ado(path, ""),
            json!({"count":1,"value":[{"id":"other","vote":10}]}),
        ),
        Step::write(
            &ado(&format!("{path}/reviewer-id"), ""),
            "PUT",
            json!({"id":"reviewer-id","vote":0}),
            json!({"id":"reviewer-id","displayName":"Reviewer"}),
        ),
    ]);
    let clock = FakeClock::new();
    let result = api(Platform::AzureDevOps, &script, &clock)
        .register(
            &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
            &identity(Platform::AzureDevOps),
        )
        .unwrap();
    assert_eq!(result["status"], "added");
    script.done();
}

#[test]
fn incomplete_registration_responses_require_reconciliation_without_replay() {
    let path = "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers";
    for (response, continuation) in [
        (Value::Null, None),
        (json!({"id":[]}), None),
        (json!({"id":"other"}), None),
        (json!({"id":"reviewer-id"}), Some("unexpected")),
    ] {
        let mut write = Step::write(
            &ado(&format!("{path}/reviewer-id"), ""),
            "PUT",
            json!({"id":"reviewer-id","vote":0}),
            response,
        );
        if let Some(token) = continuation {
            write
                .headers
                .insert("x-ms-continuationtoken".to_owned(), token.to_owned());
        }
        let script = Script::new(vec![
            Step::read(&ado(path, ""), json!({"count":0,"value":[]})),
            write,
        ]);
        let clock = FakeClock::new();
        let error = api(Platform::AzureDevOps, &script, &clock)
            .register(
                &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
                &identity(Platform::AzureDevOps),
            )
            .unwrap_err();
        assert_eq!(crate::classification(&error), "ambiguousWrite");
        assert!(clock.waits.borrow().is_empty());
        script.done();
    }
}

#[test]
fn github_registration_skips_author_and_preserves_existing_requests() {
    for scenario in ["author", "existing", "new"] {
        let path = "repos/example/widgets/pulls/10";
        let mut steps = vec![Step::read(
            path,
            json!({"user":{"id":if scenario == "author" {42} else {99},"login":"author"}}),
        )];
        if scenario != "author" {
            steps.push(Step::read(&format!("{path}/requested_reviewers?per_page=100"), json!([{"users":[{"id":if scenario == "existing" {42} else {99},"login":"account"}],"teams":[{"id":81,"slug":"team"}]}])));
        }
        if scenario == "new" {
            steps.push(Step::write(
                &format!("{path}/requested_reviewers"),
                "POST",
                json!({"reviewers":["reviewer"]}),
                json!({"users":[]}),
            ));
        }
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let result = api(Platform::GitHub, &script, &clock)
            .register(
                &repository(Platform::GitHub).candidate(10.try_into().unwrap()),
                &identity(Platform::GitHub),
            )
            .unwrap();
        assert_eq!(
            result["status"],
            match scenario {
                "author" => "selfIsAuthor",
                "existing" => "alreadyRegistered",
                _ => "added",
            }
        );
        script.done();
    }
}

#[test]
fn provider_identity_and_tenant_forwarding_ignore_authorized_user() {
    let script = Script::new(vec![Step::read(
        "https://dev.azure.com/org/_apis/connectionData?api-version=7.1-preview",
        json!({"authenticatedUser":{"id":"reviewer-id","subjectDescriptor":"aad.reviewer","providerDisplayName":"Reviewer","properties":{"Account":{"$type":"System.String","$value":"reviewer@example.com"}}},"authorizedUser":{"id":"other"}}),
    )]);
    let clock = FakeClock::new();
    let result = api(Platform::AzureDevOps, &script, &clock)
        .identity()
        .unwrap();
    assert!(result.same_account(&identity(Platform::AzureDevOps)));
    script.done();
}

#[test]
fn malformed_service_evidence_never_becomes_empty_success() {
    for reply in [
        Value::Null,
        json!({}),
        json!({"value":{}}),
        json!({"count":2,"value":[]}),
    ] {
        let script = Script::new(vec![Step::read(
            &ado(
                "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers",
                "",
            ),
            reply,
        )]);
        let clock = FakeClock::new();
        api(Platform::AzureDevOps, &script, &clock)
            .register(
                &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
                &identity(Platform::AzureDevOps),
            )
            .unwrap_err();
        script.done();
    }
}

#[test]
fn permission_refusals_and_ambiguous_claim_writes_are_not_retried() {
    for status in [403, 500] {
        let mut step = Step::write(
            "repos/example/widgets/issues/10/comments",
            "POST",
            json!({"body":"claim"}),
            json!({"message":"refused"}),
        );
        step.status = status;
        let script = Script::new(vec![step]);
        let clock = FakeClock::new();
        let error = api(Platform::GitHub, &script, &clock)
            .create_claim(
                &repository(Platform::GitHub).candidate(10.try_into().unwrap()),
                "claim",
            )
            .err()
            .unwrap();
        assert_eq!(
            crate::classification(&error),
            if status == 403 {
                "permissionDenied"
            } else {
                "ambiguousWrite"
            }
        );
        assert!(clock.waits.borrow().is_empty());
        script.done();
    }
}

#[test]
fn target_urls_and_explicit_segments_preserve_escaping() {
    for (url, platform, project, repo) in [
        (
            "https://github.com/example/widgets/",
            Platform::GitHub,
            None,
            "widgets",
        ),
        (
            "https://dev.azure.com/org/A%20Project/_git/A%20Repo",
            Platform::AzureDevOps,
            Some("A Project"),
            "A Repo",
        ),
        (
            "https://org.visualstudio.com/DefaultCollection/Project/_git/Repo",
            Platform::AzureDevOps,
            Some("Project"),
            "Repo",
        ),
    ] {
        let target = Target::repository(url).unwrap();
        assert_eq!(target.platform, platform);
        assert_eq!(target.project.as_deref(), project);
        assert_eq!(target.repository, repo);
        assert_eq!(target.pull_request_id, 0);
    }

    for invalid in [
        "https://github.com/example/widgets/pull/10",
        "http://github.com/example/widgets",
        "https://user@github.com/example/widgets",
        "https://github.com/example/widgets?query=1",
    ] {
        Target::repository(invalid).unwrap_err();
    }
}

#[test]
fn independent_rate_limits_and_transient_budgets_preserve_service_delays() {
    for platform in [Platform::GitHub, Platform::AzureDevOps] {
        let path = if platform == Platform::GitHub {
            "user".to_owned()
        } else {
            ado("_apis/resource", "")
        };
        let mut steps = Vec::new();
        // More throttles than the transient budget prove it is an independent counter.
        for _ in 0..6 {
            let mut step = Step::read(&path, json!({"message":"rate limit"}));
            step.status = 429;
            step.headers
                .insert("retry-after".to_owned(), "240".to_owned());
            steps.push(step);
        }
        for _ in 0..4 {
            let mut step = Step::read(&path, json!({"message":"server error"}));
            step.status = 500;
            steps.push(step);
        }
        steps.push(Step::read(&path, json!({"id":42})));
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let value = api(platform, &script, &clock)
            .transport
            .request(
                if platform == Platform::GitHub {
                    "user"
                } else {
                    "_apis/resource"
                },
                "GET",
                None,
            )
            .unwrap();
        assert_eq!(value["id"], 42);
        let waits = clock.waits.borrow();
        assert_eq!(&waits[..6], &[Duration::from_secs(240); 6]);
        assert_eq!(waits.len(), 10);
        assert_eq!(
            waits[6],
            Duration::from_millis(if platform == Platform::GitHub {
                1000
            } else {
                500
            })
        );
        script.done();
    }
}

#[test]
fn quota_headers_and_partial_graphql_data_are_not_retry_authorization() {
    let mut step = Step::read("user", json!({"id":42}));
    step.headers
        .insert("x-ratelimit-remaining".to_owned(), "429".to_owned());
    let script = Script::new(vec![step]);
    let clock = FakeClock::new();
    assert_eq!(
        api(Platform::GitHub, &script, &clock)
            .transport
            .request("user", "GET", None)
            .unwrap()["id"],
        42
    );
    assert!(clock.waits.borrow().is_empty());
    script.done();

    for response in [
        json!({"data":{"repository":{}},"errors":[{"type":"RATE_LIMITED"}]}),
        json!({"data":null,"errors":[{"type":"RATE_LIMITED"},{"message":"Invalid query"}]}),
    ] {
        let script = Script::new(vec![Step::write(
            "graphql",
            "POST",
            json!({"query":"query","variables":{}}),
            response,
        )]);
        api(Platform::GitHub, &script, &clock)
            .transport
            .graphql("query", &json!({}))
            .unwrap_err();
        assert!(clock.waits.borrow().is_empty());
        script.done();
    }
}

#[test]
fn ado_refreshes_authentication_once_for_reads_but_never_replays_writes() {
    for method in ["GET", "PUT"] {
        let mut refused = Step::read(&ado("_apis/resource", ""), json!({"message":"expired"}));
        method.clone_into(&mut refused.method);
        refused.status = 401;
        let mut steps = vec![refused];
        if method == "GET" {
            steps.push(Step::read(&ado("_apis/resource", ""), json!({"id":42})));
        }
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let result = api(Platform::AzureDevOps, &script, &clock)
            .transport
            .request("_apis/resource", method, None);
        if method == "GET" {
            assert_eq!(result.unwrap()["id"], 42);
            assert_eq!(script.tokens.get(), 2);
        } else {
            assert_eq!(
                crate::classification(&result.unwrap_err()),
                "authentication"
            );
            assert_eq!(script.tokens.get(), 1);
        }
        script.done();
    }
}

#[test]
fn successful_but_incomplete_claim_creation_requires_reconciliation() {
    for platform in [Platform::GitHub, Platform::AzureDevOps] {
        let step = match platform {
            Platform::GitHub => Step::write(
                "repos/example/widgets/issues/10/comments",
                "POST",
                json!({"body":"claim"}),
                json!({}),
            ),
            Platform::AzureDevOps => Step::write(
                &ado(
                    "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/threads",
                    "",
                ),
                "POST",
                json!({"status":2,"comments":[{"parentCommentId":0,"content":"claim","commentType":1}]}),
                json!({"status":2,"comments":[{}]}),
            ),
        };
        let script = Script::new(vec![step]);
        let clock = FakeClock::new();
        let error = api(platform, &script, &clock)
            .create_claim(
                &repository(platform).candidate(10.try_into().unwrap()),
                "claim",
            )
            .err()
            .unwrap();
        assert_eq!(crate::classification(&error), "ambiguousWrite");
        assert!(clock.waits.borrow().is_empty());
        script.done();
    }
}

#[test]
fn ado_selection_excludes_only_declined_reviewer_assignments() {
    for declined in [Some(true), Some(false), None] {
        let path = "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10";
        let mut reviewer =
            json!({"id":"service-alias","uniqueName":"reviewer@example.com","vote":0});
        if let Some(declined) = declined {
            reviewer["hasDeclined"] = json!(declined);
        }
        let mut steps = vec![
            Step::read(
                &ado(path, ""),
                json!({"status":"active","isDraft":false,"createdBy":{"id":"author"}}),
            ),
            Step::read(
                &ado(&format!("{path}/reviewers"), ""),
                json!({"value":[reviewer]}),
            ),
        ];
        if declined == Some(true) {
            steps.push(Step::read(
                &ado(&format!("{path}/commits"), "%24top=100&"),
                json!({"value":[{"committer":{"date":"2026-09-27T00:00:00Z"}}]}),
            ));
        }
        let script = Script::new(steps);
        let clock = FakeClock::new();
        let candidate = api(Platform::AzureDevOps, &script, &clock)
            .candidate(
                &repository(Platform::AzureDevOps).candidate(10.try_into().unwrap()),
                &identity(Platform::AzureDevOps),
                clock.now(),
            )
            .unwrap();
        assert_eq!(
            candidate.reason.as_deref(),
            if declined == Some(true) {
                None
            } else {
                Some("alreadyReviewer")
            }
        );
        script.done();
    }
}

#[test]
fn eligibility_exclusions_avoid_commit_reads_and_ignore_pending_reviews_or_teams() {
    for platform in [Platform::GitHub, Platform::AzureDevOps] {
        for exclusion in ["closed", "draft", "selfAuthored", "alreadyReviewer"] {
            let github = platform == Platform::GitHub;
            let path = if github {
                "repos/example/widgets/pulls/10".to_owned()
            } else {
                ado(
                    "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10",
                    "",
                )
            };
            let author = if github {
                json!({"id":if exclusion == "selfAuthored" {42} else {99},"login":"author"})
            } else {
                json!({"id":if exclusion == "selfAuthored" {"reviewer-id"} else {"other"}})
            };
            let pr = if github {
                json!({"state":if exclusion == "closed" {"closed"} else {"open"},"draft":exclusion=="draft","user":author})
            } else {
                json!({"status":if exclusion == "closed" {"completed"} else {"active"},"isDraft":exclusion=="draft","createdBy":author})
            };
            let mut steps = vec![Step::read(&path, pr)];
            if exclusion == "alreadyReviewer" {
                if github {
                    steps.push(Step::read(
                        &format!("{path}/requested_reviewers?per_page=100"),
                        json!([{"users":[{"id":42}],"teams":[]}]),
                    ));
                    steps.push(Step::read(
                        &format!("{path}/reviews?per_page=100"),
                        json!([[]]),
                    ));
                } else {
                    steps.push(Step::read(
                        &ado(
                            "A%20Project/_apis/git/repositories/A%20Repo/pullRequests/10/reviewers",
                            "",
                        ),
                        json!({"value":[{"id":"reviewer-id","vote":-10}]}),
                    ));
                }
            }
            let script = Script::new(steps);
            let clock = FakeClock::new();
            let candidate = api(platform, &script, &clock)
                .candidate(
                    &repository(platform).candidate(10.try_into().unwrap()),
                    &identity(platform),
                    clock.now(),
                )
                .unwrap();
            assert_eq!(candidate.reason.as_deref(), Some(exclusion));
            script.done();
        }
    }
    let script = Script::new(vec![
        Step::read(
            "repos/example/widgets/pulls/10",
            json!({"state":"open","draft":false,"user":{"id":99}}),
        ),
        Step::read(
            "repos/example/widgets/pulls/10/requested_reviewers?per_page=100",
            json!([{"users":[],"teams":[{"id":42,"slug":"reviewer"}]}]),
        ),
        Step::read(
            "repos/example/widgets/pulls/10/reviews?per_page=100",
            json!([[{"state":"PENDING","user":{"id":42}}]]),
        ),
        Step::write(
            "graphql",
            "POST",
            json!({"query":crate::api::COMMIT_QUERY,"variables":{"owner":"example","repo":"widgets","num":10,"cur":null,"size":100}}),
            json!({"data":{"repository":{"pullRequest":{"commits":{"nodes":[{"commit":{"committedDate":"2026-09-27T00:00:00Z"}}],"pageInfo":{"hasNextPage":false}}}}}}),
        ),
    ]);
    let clock = FakeClock::new();
    assert!(
        api(Platform::GitHub, &script, &clock)
            .candidate(
                &repository(Platform::GitHub).candidate(10.try_into().unwrap()),
                &identity(Platform::GitHub),
                clock.now()
            )
            .unwrap()
            .reason
            .is_none()
    );
    script.done();
}
