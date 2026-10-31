# Provider capture fuzzing

The `provider_capture` target uses scripted process and HTTP responses. It
checks retention across GitHub thread and comment connections and Azure DevOps
iteration, reviewer, and thread pages. It also checks that malformed later
pages and repeated or missing continuation cursors fail without returning a
partial capture.

From `packages/ss-ai-review`, run the target with an installed nightly toolchain
and `cargo-fuzz`:

```text
cargo +nightly fuzz run provider_capture
```

The scripts cap page and response counts and fail immediately if the collector
requests an unscripted response. No provider credentials or network access are
needed. The seed corpus includes complete and rejected sequences for both
providers.
