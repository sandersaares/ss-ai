//! Completion seeds must exercise the same native workflow on each checkout platform.

#[path = "../fuzz/fuzz_targets/source_driver.rs"]
mod source_driver;

#[test]
fn completion_seed_reaches_completion_with_lf_and_crlf() {
    let seed = include_str!("../fuzz/seeds/completion.txt").replace("\r\n", "\n");
    let crlf_seed = seed.replace('\n', "\r\n");
    assert!(source_driver::exercise(seed.as_bytes()));
    assert!(source_driver::exercise(crlf_seed.as_bytes()));
}
