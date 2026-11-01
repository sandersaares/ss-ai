# Provider capture fuzzing

The `provider_capture` target uses scripted process and HTTP responses. It
checks retention across GitHub thread and comment connections and Azure DevOps
iteration, reviewer, and thread pages. It also checks that malformed later
pages and repeated or missing continuation cursors fail without returning a
partial capture.

From `packages/ss-ai-review-fuzz`, run the target with an installed nightly toolchain
and `cargo-fuzz`:

```text
cargo +nightly fuzz run --fuzz-dir . provider_capture
```

The scripts cap page and response counts and fail immediately if the collector
requests an unscripted response. No provider credentials or network access are
needed. The seed corpus includes complete and rejected sequences for both
providers.

The `final_review` target exercises GitHub publication with bounded finding
plans and accepted writes whose responses are lost, as well as failed draft and
submitted-review readbacks. It checks that a second invocation cannot contact
the provider after a potentially published transaction. Process, HTTP, and
clock effects are fake; the durable journal uses the checkout's `target`
directory and a process-specific file name.

```text
cargo +nightly fuzz run --fuzz-dir . final_review
```

The target covers GitHub file findings without approval or follow-up actions.
It does not inject journal failures or model Azure DevOps publication.

The `tracker_transaction` target drives tracker publication with an in-memory
journal and scripted provider effects. Bounded plans cover both providers,
write interruptions, lost responses, read failures, and explicit acceptance
continuations. It checks durable intent and receipt ordering, prevents repeated
creation of accepted items, and rejects changed continuation evidence.

```text
cargo +nightly fuzz run --fuzz-dir . tracker_transaction
```
