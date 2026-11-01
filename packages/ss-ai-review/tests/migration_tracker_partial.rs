//! Exercises interrupted tracker publication through the bounded offline harness.

use ss_ai_review::fuzz_tracker_transaction;

#[test]
fn accepted_first_item_is_not_recreated_after_second_item_loses_its_response() {
    for platform in [0, 1] {
        // Two items: accept and read the first, pace, then lose the second reply.
        // The harness checks durable intent and receipt before effects and reruns
        // the transaction to reject replay of already accepted creations.
        fuzz_tracker_transaction(&[platform, 1, 0, 0, 0, 0, 0, 2]);
    }
}

#[test]
fn accepted_item_is_not_recreated_when_readback_fails() {
    for platform in [0, 1] {
        // The first create succeeds and its readback fails before the next item.
        fuzz_tracker_transaction(&[platform, 1, 0, 0, 0, 1]);
    }
}

#[test]
fn refusal_after_partial_progress_does_not_duplicate_accepted_items() {
    for platform in [0, 1] {
        // The first item has a receipt before the second create is refused.
        fuzz_tracker_transaction(&[platform, 1, 0, 0, 0, 0, 0, 1]);
    }
}
