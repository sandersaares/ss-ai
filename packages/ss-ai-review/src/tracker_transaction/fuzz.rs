//! Offline transaction exploration with bounded, deterministic effects.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::result_large_err,
    reason = "The generated plans and effect counts are strictly bounded"
)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use ohno::AppError;
use serde_json::{Value, json};

use crate::error::fail;
use crate::tracker_transaction::{Acceptance, Effects, Journal, run};

/// Simulates atomic journal replacement and one selected failed write.
struct Memory {
    durable: Rc<RefCell<Option<Value>>>,
    writes: usize,
    fail_at: usize,
}

impl Journal for Memory {
    fn read(&mut self) -> Result<Option<Vec<u8>>, AppError> {
        Ok(self
            .durable
            .borrow()
            .as_ref()
            .map(|value| serde_json::to_vec(value).expect("generated journal serializes")))
    }

    fn write(&mut self, record: &Value) -> Result<(), AppError> {
        self.writes += 1;
        assert!(self.writes <= 128, "journal write budget exhausted");
        if self.writes == self.fail_at {
            return Err(fail("io", "injected journal failure"));
        }
        *self.durable.borrow_mut() = Some(record.clone());
        Ok(())
    }
}

/// Models accepted writes separately from responses delivered to the transaction.
struct Provider<'a> {
    durable: Rc<RefCell<Option<Value>>>,
    script: &'a [u8],
    calls: usize,
    created: HashSet<String>,
    items: Vec<Value>,
}

impl Provider<'_> {
    /// Bounds all provider activity and consumes a deterministic effect choice.
    fn next(&mut self) -> u8 {
        assert!(self.calls < 128, "provider effect budget exhausted");
        let value = self.script.get(self.calls).copied().unwrap_or_default();
        self.calls += 1;
        value
    }
}

impl Effects for Provider<'_> {
    fn create(&mut self, item: &Value) -> Result<Value, AppError> {
        let choice = self.next();
        let durable = self.durable.borrow();
        let op = durable
            .as_ref()
            .and_then(|record| record["operations"].as_array())
            .and_then(|ops| ops.iter().find(|op| op["findingId"] == item["findingId"]))
            .expect("creation requires a durable intent");
        assert_eq!(op["outcome"], "ambiguous");
        drop(durable);

        let finding = item["findingId"].as_str().expect("generated finding");
        assert!(
            !self.created.contains(finding),
            "recreated an accepted item"
        );
        if choice % 8 == 1 {
            return Err(fail("permissionDenied", "injected refusal"));
        }

        self.created.insert(finding.to_owned());
        let id = (self.items.len() + 1).to_string();
        let mut body = item["body"].as_str().expect("generated body").to_owned();
        if choice & 8 != 0 {
            body.push('!');
        }
        let receipt = json!({"itemId":id, "itemNodeId":id,
            "url":format!("https://example.test/{id}"), "title":item["title"],
            "body":body, "descriptionFormat":"Markdown"});
        self.items.push(receipt.clone());
        match choice % 8 {
            2 => Err(fail("ambiguousWrite", "injected lost response")),
            3 => Ok(json!({"itemId":id})),
            _ => Ok(receipt),
        }
    }

    fn read(&mut self, receipt: &Value) -> Result<Value, AppError> {
        let choice = self.next();
        let durable = self.durable.borrow();
        assert!(
            durable.as_ref().expect("durable journal")["operations"]
                .as_array()
                .expect("operations")
                .iter()
                .any(|op| op["outcome"] == "succeeded" && op["itemId"] == receipt["itemId"]),
            "readback requires a durable receipt"
        );
        drop(durable);
        if choice % 8 == 1 {
            return Err(fail("service", "injected read failure"));
        }
        Ok(self
            .items
            .iter()
            .find(|item| item["itemId"] == receipt["itemId"])
            .expect("receipt identifies an accepted item")
            .clone())
    }

    fn wait_between_creates(&mut self) -> Result<(), AppError> {
        _ = self.next();
        Ok(())
    }
}

/// Drives the production transaction with bounded offline journal and provider effects.
#[doc(hidden)]
pub fn fuzz_tracker_transaction(data: &[u8]) {
    let data = &data[..data.len().min(64)];
    let choice = |index: usize| data.get(index).copied().unwrap_or_default();
    let platform = if choice(0) & 1 == 0 {
        "gitHub"
    } else {
        "azureDevOps"
    };
    let target = if platform == "gitHub" {
        json!({"platform":"gitHub","collectionUrl":null,"organization":"org","project":null,
            "repository":"repo","webUrl":"https://github.com/org/repo/issues"})
    } else {
        json!({"platform":"azureDevOps","collectionUrl":"https://dev.azure.com/org",
            "organization":"org","project":"Project","repository":null,
            "webUrl":"https://dev.azure.com/org/Project/_workitems"})
    };
    let state = if platform == "gitHub" {
        json!({"platform":"gitHub","repositoryId":"repo-id"})
    } else {
        json!({"platform":"azureDevOps","workItemType":"Bug","areaPath":"Project",
            "descriptionField":"System.Description","parent":null})
    };
    // Small plans and invocations cover partial progress without unbounded fuzz cases.
    let count = usize::from(choice(1) % 4) + 1;
    let plan: Vec<Value> = (0..count)
        .map(|index| {
            let id = if choice(2) & 1 == 0 {
                index
            } else {
                count - 1 - index
            };
            json!({"findingId":format!("F{id}"),"title":format!("Title {id}"),
            "body":format!("Finding {id} contains enough approved text to compare accurately.")})
        })
        .collect();
    let durable = Rc::new(RefCell::new(None));
    let mut journal = Memory {
        durable: Rc::clone(&durable),
        writes: 0,
        // Zero means no failure; other values choose a single write boundary.
        fail_at: usize::from(choice(3) % 20),
    };
    let mut provider = Provider {
        durable: Rc::clone(&durable),
        script: data.get(4..).unwrap_or_default(),
        calls: 0,
        created: HashSet::new(),
        items: Vec::new(),
    };
    let mut decision: Option<(String, String, String)> = None;
    for _ in 0..6 {
        let saved = durable.borrow().clone();
        if let Some(record) = &saved
            && record["status"] == "needsAdjudication"
        {
            let adjudication = &record["adjudication"];
            decision = Some((
                adjudication["itemId"].as_str().expect("item ID").to_owned(),
                adjudication["expectedBodyHash"]
                    .as_str()
                    .expect("expected hash")
                    .to_owned(),
                adjudication["storedBodyHash"]
                    .as_str()
                    .expect("stored hash")
                    .to_owned(),
            ));
        }
        if let Some((id, expected, stored)) = &decision
            && saved.as_ref().is_some_and(|record| {
                matches!(
                    record["status"].as_str(),
                    Some("needsAdjudication" | "resumeReady")
                )
            })
        {
            let calls = provider.calls;
            let writes = journal.writes;
            for wrong in [
                Acceptance {
                    item_id: "wrong",
                    expected_body_hash: expected,
                    stored_body_hash: stored,
                },
                Acceptance {
                    item_id: id,
                    expected_body_hash: "wrong",
                    stored_body_hash: stored,
                },
                Acceptance {
                    item_id: id,
                    expected_body_hash: expected,
                    stored_body_hash: "wrong",
                },
            ] {
                _ = run(
                    &mut journal,
                    Path::new("result.json"),
                    &mut provider,
                    &target,
                    &state,
                    &plan,
                    "summary",
                    "now",
                    Some(&wrong),
                )
                .expect_err("changed acceptance must be rejected");
            }
            let mut changed_plan = plan.clone();
            changed_plan[0]["body"] = json!("changed");
            let acceptance = Acceptance {
                item_id: id,
                expected_body_hash: expected,
                stored_body_hash: stored,
            };
            _ = run(
                &mut journal,
                Path::new("result.json"),
                &mut provider,
                &target,
                &state,
                &changed_plan,
                "summary",
                "now",
                Some(&acceptance),
            )
            .expect_err("changed plan must be rejected");
            assert_eq!(provider.calls, calls);
            assert_eq!(journal.writes, writes);
            assert_eq!(*durable.borrow(), saved);
        }
        let acceptance = decision.as_ref().map(|(id, expected, stored)| Acceptance {
            item_id: id,
            expected_body_hash: expected,
            stored_body_hash: stored,
        });
        _ = run(
            &mut journal,
            Path::new("result.json"),
            &mut provider,
            &target,
            &state,
            &plan,
            "summary",
            "now",
            acceptance.as_ref(),
        );
    }
}
