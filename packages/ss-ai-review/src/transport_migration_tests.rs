//! Contract checks for predecessor transport behavior at the injected boundary.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Duration;

use jiff::Timestamp;
use ohno::AppError;
use serde_json::{Value, json};

use crate::{
    Clock, Http, HttpResponse, Process, ProcessOutput, classification, error::fail, target::Target,
    transport::Transport,
};

/// Serves a finite sequence of responses without a provider or real waits.
struct Provider {
    responses: RefCell<VecDeque<HttpResponse>>,
    requests: RefCell<Vec<String>>,
    waits: Cell<usize>,
}

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
        _: Option<&[u8]>,
    ) -> Result<HttpResponse, AppError> {
        self.requests.borrow_mut().push(method.to_owned());
        Ok(self
            .responses
            .borrow_mut()
            .pop_front()
            .expect("unexpected provider request"))
    }
}

impl Clock for Provider {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn wait(&self, _: Duration) -> Result<(), AppError> {
        self.waits.set(self.waits.get().checked_add(1).unwrap());
        Err(fail("service", "injected wait interruption"))
    }
}

/// Supplies only the responses each individual test permits.
fn provider(statuses: &[u16]) -> Provider {
    Provider {
        responses: RefCell::new(
            statuses
                .iter()
                .map(|status| HttpResponse::new(*status, BTreeMap::new(), vec![]))
                .collect(),
        ),
        requests: RefCell::new(Vec::new()),
        waits: Cell::new(0),
    }
}

#[test]
fn azure_empty_delete_responses_are_accepted_without_accepting_empty_json_elsewhere() {
    let target = Target::repository("https://dev.azure.com/org/project/_git/repo").unwrap();
    for status in [200, 204] {
        let provider = provider(&[status, status, status]);
        let mut transport = Transport::new(&target, None, &provider, &provider, &provider);
        assert_eq!(
            transport.request("resource", "DELETE", None).unwrap(),
            Value::Null
        );
        assert_eq!(
            classification(&transport.request("resource", "GET", None).unwrap_err()),
            "service"
        );
        assert_eq!(
            classification(
                &transport
                    .request("resource", "POST", Some(&json!({})))
                    .unwrap_err()
            ),
            "ambiguousWrite"
        );
        assert_eq!(*provider.requests.borrow(), ["DELETE", "GET", "POST"]);
        assert_eq!(provider.waits.get(), 0);
    }
}

#[test]
fn azure_interrupted_throttle_wait_preserves_refusal_and_stops_before_replay() {
    let target = Target::repository("https://dev.azure.com/org/project/_git/repo").unwrap();
    let provider = provider(&[429]);
    let mut transport = Transport::new(&target, None, &provider, &provider, &provider);
    let error = transport
        .request("resource", "POST", Some(&json!({"body":"finding"})))
        .unwrap_err();
    assert_eq!(classification(&error), "rateLimited");
    assert_eq!(*provider.requests.borrow(), ["POST"]);
    assert_eq!(provider.waits.get(), 1);
}
