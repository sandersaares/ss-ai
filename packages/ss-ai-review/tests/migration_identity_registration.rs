//! Exercises conservative reviewer matching through the offline public command.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};
use ss_ai_review::{Clock, Http, HttpResponse, Process, ProcessOutput, run};

/// Replays reads and fails if registration attempts a provider write.
struct Provider(RefCell<VecDeque<Value>>);
impl Process for Provider {
    fn execute(&self, program: &str, _: &[String], _: &[u8]) -> io::Result<ProcessOutput> {
        assert_eq!(program, "az");
        Ok(ProcessOutput::new(true, b"fixture-token".to_vec(), vec![]))
    }
}
impl Http for Provider {
    fn send(
        &self,
        method: &str,
        _: &str,
        _: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        assert_eq!(method, "GET");
        assert!(body.is_none());
        Ok(HttpResponse::new(
            200,
            BTreeMap::new(),
            serde_json::to_vec(&self.0.borrow_mut().pop_front().unwrap()).unwrap(),
        ))
    }
}
impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }
    fn wait(&self, _: Duration) -> Result<(), AppError> {
        panic!("complete fixture must not wait")
    }
}

#[test]
fn shared_account_name_prevents_duplicate_registration_despite_other_conflicting_ids() {
    let responses = VecDeque::from([
        json!({"authenticatedUser":{"id":"first", "providerDisplayName":"Reviewer",
            "subjectDescriptor":"aad.first", "properties":{"Account":{"$type":"System.String","$value":"shared@example.com"}}}}),
        json!({"count":1,"value":[{"id":"second","descriptor":"aad.second",
            "uniqueName":"SHARED@example.com","vote":-10,"isRequired":true}]}),
    ]);
    let provider = Provider(RefCell::new(responses));
    let args: Vec<OsString> = [
        "ss-ai",
        "reviewer",
        "register",
        "--url",
        "https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let output: Value =
        serde_json::from_str(&run(&args, &provider, &provider, &provider).unwrap()).unwrap();
    assert_eq!(output.get("status").unwrap(), "alreadyRegistered");
    assert!(provider.0.borrow().is_empty());
}
