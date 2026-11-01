//! Composes tracker destination preflight and the journaled provider transaction.

use std::any::type_name;
use std::collections::HashSet;
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::path::Path;
use std::time::Duration;

use ohno::AppError;
use serde_json::{Value, from_value};
use ss_ai_runtime::FileTransaction;

use crate::error::{caused_by, fail};
use crate::target::Platform;
use crate::tracker_target::TrackerTarget;
use crate::tracker_transaction::{self, Acceptance, Effects};
use crate::transport::Transport;
use crate::{Clock, Http, Process, tracker_ado, tracker_github, validate_tracker_arguments};

/// A person's exact acceptance of a stored work item rewrite.
#[derive(Debug)]
#[expect(
    clippy::exhaustive_structs,
    reason = "This is an internal command composition interface"
)]
pub struct TrackerAcceptance<'a> {
    /// Identifier of the item whose rewrite was accepted.
    pub item_id: &'a str,
    /// Hash of the approved body.
    pub expected_body_hash: &'a str,
    /// Hash of the stored body.
    pub stored_body_hash: &'a str,
}

impl TrackerAcceptance<'_> {
    /// Converts the human decision to the journal's exact binding.
    fn checked(&self) -> Result<Acceptance<'_>, AppError> {
        let valid_hash = |hash: &str| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if self.item_id.trim().is_empty()
            || !valid_hash(self.expected_body_hash)
            || !valid_hash(self.stored_body_hash)
        {
            return Err(fail("invalidInput", "Invalid tracker rewrite acceptance"));
        }
        Ok(Acceptance {
            item_id: self.item_id,
            expected_body_hash: self.expected_body_hash,
            stored_body_hash: self.stored_body_hash,
        })
    }
}

/// Local inputs prepared for one tracker publication.
#[expect(
    clippy::exhaustive_structs,
    reason = "This is an internal command composition interface"
)]
pub struct TrackerPublicationInput<'a> {
    /// Resolved destination independent of the reviewed source.
    pub target: &'a Value,
    /// Destination-specific publication settings.
    pub metadata: &'a Value,
    /// Prepared items and summary from the checked local evidence.
    pub plan: &'a Value,
    /// Optional exact acceptance of an earlier paused rewrite.
    pub acceptance: Option<&'a TrackerAcceptance<'a>>,
    /// Optional Azure OAuth tenant.
    pub tenant: Option<&'a str>,
}

impl Debug for TrackerPublicationInput<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct(type_name::<Self>())
            .field(
                "item_count",
                &self
                    .plan
                    .get("items")
                    .and_then(Value::as_array)
                    .map(Vec::len),
            )
            .field("acceptance", &self.acceptance.is_some())
            .finish_non_exhaustive()
    }
}

/// Carries the local publication prerequisites across the provider boundary.
#[derive(Debug)]
struct ValidatedTrackerInput<'a> {
    target: TrackerTarget,
    metadata: &'a Value,
    tenant: Option<&'a str>,
    plan: &'a [Value],
    summary: &'a str,
}

/// Checks a prior result while retaining ownership of the publication lock.
///
/// # Errors
///
/// Rejects a prior result requiring reconciliation or a mismatched acceptance.
pub fn tracker_publication_check_prior(
    journal: &mut FileTransaction<'_>,
    target: &Value,
    acceptance: Option<&TrackerAcceptance<'_>>,
) -> Result<(), AppError> {
    let acceptance = acceptance.map(TrackerAcceptance::checked).transpose()?;
    tracker_transaction::check_prior(journal, target, acceptance.as_ref())
}

/// Preflights and publishes prepared tracker items under the retained lock.
///
/// The caller checks source evidence and prepares the plan before this call.
/// A failed or paused transaction returns an outcome for the caller to emit
/// before exiting nonzero. `result_path` identifies the locked journal.
///
/// # Errors
///
/// Rejects invalid local inputs, blocked prior runs, and preflight failures.
pub fn publish_tracker_items(
    journal: &mut FileTransaction<'_>,
    result_path: &Path,
    input: &TrackerPublicationInput<'_>,
    process: &dyn Process,
    http: &dyn Http,
    clock: &dyn Clock,
) -> Result<Value, AppError> {
    let validated = validate_input(input)?;
    if result_path.as_os_str().is_empty() {
        return Err(fail("invalidInput", "Missing tracker result path"));
    }
    let acceptance = input
        .acceptance
        .map(TrackerAcceptance::checked)
        .transpose()?;
    let (transport, state) = provider_preflight(&validated, process, http, clock, || {
        tracker_transaction::check_plan(journal, input.target, validated.plan, acceptance.as_ref())
    })?;
    let mut effects = ProviderEffects {
        transport,
        target: validated.target,
        state: &state,
        clock,
    };
    tracker_transaction::execute(
        journal,
        result_path,
        &mut effects,
        input.target,
        &state,
        validated.plan,
        validated.summary,
        &clock.now().to_string(),
        acceptance.as_ref(),
    )
}

/// Checks the journal before opening a transport for validated inputs.
fn provider_preflight<'a>(
    input: &ValidatedTrackerInput<'_>,
    process: &'a dyn Process,
    http: &'a dyn Http,
    clock: &'a dyn Clock,
    guard: impl FnOnce() -> Result<(), AppError>,
) -> Result<(Transport<'a>, Value), AppError> {
    guard()?;
    let mut transport = Transport::new_tracker(&input.target, input.tenant, process, http, clock);
    let state = match input.target.platform {
        Platform::GitHub => tracker_github::state(&transport, &input.target),
        Platform::AzureDevOps => tracker_ado::state(&mut transport, &input.target, input.metadata),
    }?;
    Ok((transport, state))
}

/// Rejects all locally detectable malformed inputs before provider access.
fn validate_input<'a>(
    input: &'a TrackerPublicationInput<'_>,
) -> Result<ValidatedTrackerInput<'a>, AppError> {
    validate_tracker_arguments(input.target, input.metadata)?;
    let target: TrackerTarget = from_value(input.target.clone())
        .map_err(|error| caused_by("invalidInput", "Invalid tracker target", error))?;
    if input.tenant.is_some_and(|tenant| tenant.trim().is_empty()) {
        return Err(fail("invalidInput", "Invalid Azure tenant"));
    }
    if target.platform == Platform::AzureDevOps {
        let area = input.metadata["areaPath"]
            .as_str()
            .ok_or_else(|| fail("invalidInput", "Missing area path"))?;
        if area
            .split('\\')
            .any(|part| part.trim().is_empty() || matches!(part, "." | ".."))
        {
            return Err(fail("invalidInput", "Invalid area path"));
        }
        if let Some(field) = input.metadata["descriptionField"]
            .as_str()
            .filter(|field| !field.trim().is_empty())
        {
            tracker_ado::validate_description_field(field)?;
        }
    }
    let plan = input
        .plan
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| fail("invalidInput", "Missing tracker publication items"))?;
    let summary = input
        .plan
        .get("summaryBody")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .ok_or_else(|| fail("invalidInput", "Missing tracker publication summary"))?;
    let mut ids = HashSet::new();
    for item in plan {
        let text = |field: &str| {
            item.get(field)
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
        };
        let id = text("findingId")
            .ok_or_else(|| fail("invalidInput", "Invalid tracker publication item"))?;
        if text("title").is_none() || text("body").is_none() || !ids.insert(id) {
            return Err(fail("invalidInput", "Invalid tracker publication item"));
        }
    }
    Ok(ValidatedTrackerInput {
        target,
        metadata: input.metadata,
        tenant: input.tenant,
        plan,
        summary,
    })
}

/// Routes journal effects through the provider selected by the tracker target.
struct ProviderEffects<'a> {
    transport: Transport<'a>,
    target: TrackerTarget,
    state: &'a Value,
    clock: &'a dyn Clock,
}

impl Effects for ProviderEffects<'_> {
    fn create(&mut self, item: &Value) -> Result<Value, AppError> {
        match self.target.platform {
            Platform::GitHub => tracker_github::create(&self.transport, self.state, item),
            Platform::AzureDevOps => {
                tracker_ado::create(&mut self.transport, &self.target, self.state, item)
            }
        }
    }

    fn read(&mut self, receipt: &Value) -> Result<Value, AppError> {
        match self.target.platform {
            Platform::GitHub => tracker_github::read(&self.transport, receipt),
            Platform::AzureDevOps => {
                tracker_ado::read(&mut self.transport, &self.target, self.state, receipt)
            }
        }
    }

    fn wait_between_creates(&mut self) -> Result<(), AppError> {
        // GitHub asks clients to pace mutations to avoid secondary rate limits.
        // Ref: skills/ss-review/scripts/lib/TrackerPublication.ps1.
        self.clock.wait(Duration::from_secs(1))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    use jiff::Timestamp;
    use ohno::AppError;
    use serde_json::{Value, json};

    use super::{
        ProviderEffects, TrackerAcceptance, TrackerPublicationInput, provider_preflight,
        validate_input,
    };
    use crate::error::fail;
    use crate::tracker_transaction::{Acceptance, Effects, check_plan_bytes, paused_fixture};
    use crate::{Clock, Http, HttpResponse, Process, ProcessOutput, resolve_tracker_target};

    /// Provides scripted GitHub replies and records the requests and pacing.
    struct Offline {
        replies: RefCell<VecDeque<Value>>,
        requests: RefCell<Vec<Value>>,
        http_requested: RefCell<bool>,
        waits: RefCell<Vec<Duration>>,
    }

    impl Process for Offline {
        fn execute(&self, _: &str, _: &[String], input: &[u8]) -> io::Result<ProcessOutput> {
            self.requests
                .borrow_mut()
                .push(serde_json::from_slice(input).unwrap());
            let reply = self.replies.borrow_mut().pop_front().unwrap();
            Ok(ProcessOutput::new(
                true,
                format!("HTTP/2 200\n\n{reply}").into_bytes(),
                vec![],
            ))
        }
    }

    impl Http for Offline {
        fn send(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[u8]>,
        ) -> Result<HttpResponse, AppError> {
            *self.http_requested.borrow_mut() = true;
            Err(fail("service", "Unexpected HTTP request"))
        }
    }

    impl Clock for Offline {
        fn now(&self) -> Timestamp {
            Timestamp::UNIX_EPOCH
        }

        fn wait(&self, duration: Duration) -> Result<(), AppError> {
            self.waits.borrow_mut().push(duration);
            Ok(())
        }
    }

    #[test]
    fn rejects_incomplete_and_duplicate_local_plans() {
        let target =
            resolve_tracker_target(&json!({"url":"https://github.com/acme/tracker"})).unwrap();
        let metadata = json!({});
        let valid_item = json!({"findingId":"f1","title":"title","body":"body"});
        for plan in [
            json!({"items":[],"summaryBody":"summary"}),
            json!({"items":[valid_item,valid_item],"summaryBody":"summary"}),
            json!({"items":[{"findingId":"f1","title":"title","body":null}],"summaryBody":"summary"}),
            json!({"items":[valid_item],"summaryBody":null}),
        ] {
            let input = TrackerPublicationInput {
                target: &target,
                metadata: &metadata,
                plan: &plan,
                acceptance: None,
                tenant: None,
            };
            validate_input(&input).unwrap_err();
        }
        let plan = json!({"items":[valid_item],"summaryBody":"summary"});
        let input = TrackerPublicationInput {
            target: &target,
            metadata: &metadata,
            plan: &plan,
            acceptance: None,
            tenant: None,
        };
        validate_input(&input).unwrap();
        _ = format!("{input:?}");
    }

    #[test]
    fn acceptance_requires_exact_hash_shapes() {
        let hash = "a".repeat(64);
        let acceptance = TrackerAcceptance {
            item_id: "42",
            expected_body_hash: &hash,
            stored_body_hash: &hash,
        };
        acceptance.checked().unwrap();
        let Err(_) = (TrackerAcceptance {
            stored_body_hash: "not-a-hash",
            ..acceptance
        })
        .checked() else {
            panic!("malformed acceptance was accepted");
        };
    }

    #[test]
    fn malformed_metadata_is_rejected_before_provider_access() {
        let target = resolve_tracker_target(&json!({
            "url":"https://dev.azure.com/acme/project"
        }))
        .unwrap();
        let plan = json!({"items":[{"findingId":"f1","title":"title","body":"body"}],
            "summaryBody":"summary"});
        let offline = Offline {
            replies: RefCell::new(VecDeque::new()),
            requests: RefCell::new(Vec::new()),
            http_requested: RefCell::new(false),
            waits: RefCell::new(Vec::new()),
        };
        for metadata in [
            json!({"workItemType":"Bug","areaPath":"project","descriptionField":"System.Title"}),
            json!({"workItemType":"Bug","areaPath":"project","descriptionField":"System.Description/extra"}),
            json!({"workItemType":"Bug","areaPath":"project","descriptionField":"System..Description"}),
            json!({"workItemType":"Bug","areaPath":"project\\.."}),
            json!({"workItemType":"Bug","areaPath":"project\\\\child"}),
            json!({"workItemType":"Bug","areaPath":"other"}),
            json!({"workItemType":"Bug","areaPath":"project","parentWorkItemId":0}),
            json!({"workItemType":"Bug","areaPath":"project","unexpected":true}),
        ] {
            let input = TrackerPublicationInput {
                target: &target,
                metadata: &metadata,
                plan: &plan,
                acceptance: None,
                tenant: None,
            };
            let Err(error) = validate_input(&input) else {
                panic!("invalid metadata was accepted: {metadata}");
            };
            assert_eq!(crate::classification(&error), "invalidInput", "{metadata}");
            assert!(offline.requests.borrow().is_empty());
            assert!(!*offline.http_requested.borrow());
        }
    }

    #[test]
    fn changed_paused_plan_is_rejected_before_provider_access() {
        let target = resolve_tracker_target(&json!({
            "url":"https://dev.azure.com/acme/project"
        }))
        .unwrap();
        let item = json!({"findingId":"f1","title":"title","body":"body"});
        let state = json!({"platform":"azureDevOps", "workItemType":"Bug",
            "areaPath":"project", "descriptionField":"System.Description", "parent":null});
        let journal = paused_fixture(&target, &state, std::slice::from_ref(&item));
        let bytes = serde_json::to_vec(&journal).unwrap();
        let adjudication = journal.get("adjudication").unwrap();
        let acceptance = Acceptance {
            item_id: adjudication.get("itemId").unwrap().as_str().unwrap(),
            expected_body_hash: adjudication
                .get("expectedBodyHash")
                .unwrap()
                .as_str()
                .unwrap(),
            stored_body_hash: adjudication
                .get("storedBodyHash")
                .unwrap()
                .as_str()
                .unwrap(),
        };
        check_plan_bytes(
            &bytes,
            &target,
            std::slice::from_ref(&item),
            Some(&acceptance),
        )
        .unwrap();
        let metadata = json!({"workItemType":"Bug","areaPath":"project"});
        let changed_plan = json!({"items":[{"findingId":"f1","title":"title","body":"changed"}],
            "summaryBody":"summary"});
        let input = TrackerPublicationInput {
            target: &target,
            metadata: &metadata,
            plan: &changed_plan,
            acceptance: None,
            tenant: None,
        };
        let offline = Offline {
            replies: RefCell::new(VecDeque::new()),
            requests: RefCell::new(Vec::new()),
            http_requested: RefCell::new(false),
            waits: RefCell::new(Vec::new()),
        };
        let validated = validate_input(&input).unwrap();
        let Err(error) = provider_preflight(&validated, &offline, &offline, &offline, || {
            check_plan_bytes(
                &bytes,
                &target,
                changed_plan.get("items").unwrap().as_array().unwrap(),
                Some(&acceptance),
            )
        }) else {
            panic!("a changed paused plan was accepted");
        };
        assert_eq!(crate::classification(&error), "invalidState");
        assert!(offline.requests.borrow().is_empty());
        assert!(!*offline.http_requested.borrow());
    }

    #[test]
    fn github_effects_route_create_read_and_pacing_offline() {
        let issue = json!({"id":"node","number":42,"url":"https://github.com/acme/tracker/issues/42",
                    "title":"title","body":"body"});
        let offline = Offline {
            replies: RefCell::new(VecDeque::from([
                json!({"data":{"repository":{"id":"repo","nameWithOwner":"acme/tracker",
                            "hasIssuesEnabled":true,"isArchived":false,"viewerPermission":"WRITE"}}}),
                json!({"data":{"createIssue":{"issue":issue}}}),
                json!({"data":{"node":issue}}),
            ])),
            requests: RefCell::new(Vec::new()),
            http_requested: RefCell::new(false),
            waits: RefCell::new(Vec::new()),
        };
        let target =
            resolve_tracker_target(&json!({"url":"https://github.com/acme/tracker"})).unwrap();
        let metadata = json!({});
        let plan = json!({"items":[{"findingId":"f1","title":"title","body":"body"}],
            "summaryBody":"summary"});
        let input = TrackerPublicationInput {
            target: &target,
            metadata: &metadata,
            plan: &plan,
            acceptance: None,
            tenant: None,
        };
        let validated = validate_input(&input).unwrap();
        let (transport, state) =
            provider_preflight(&validated, &offline, &offline, &offline, || Ok(())).unwrap();
        let mut effects = ProviderEffects {
            transport,
            target: validated.target,
            state: &state,
            clock: &offline,
        };
        let receipt = effects
            .create(&json!({"title":"title","body":"body"}))
            .unwrap();
        assert_eq!(effects.read(&receipt).unwrap(), receipt);
        effects.wait_between_creates().unwrap();
        assert_eq!(offline.requests.borrow().len(), 3);
        assert_eq!(*offline.waits.borrow(), [Duration::from_secs(1)]);
    }
}
