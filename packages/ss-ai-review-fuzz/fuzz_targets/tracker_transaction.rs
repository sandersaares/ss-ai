//! Exercises durable tracker publication and continuation without live effects.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ss_ai_review::fuzz_tracker_transaction;

fuzz_target!(|data: &[u8]| {
    fuzz_tracker_transaction(data);
});
