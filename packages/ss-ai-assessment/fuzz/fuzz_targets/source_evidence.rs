//! Bounded malformed-patch and operation-order coverage for the exact evidence contract.
#![no_main]

use libfuzzer_sys::fuzz_target;

mod source_driver;

fuzz_target!(|input: &[u8]| {
    let _completed = source_driver::exercise(input);
});
