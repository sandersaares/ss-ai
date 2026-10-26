Your task is to create a listing of important document references required to understand content under review.

Explore the content to be reviewed to identify key topics it touches and to collect references to relevant documentation (searching for both locally present documentation as well as external references reachable via network).

Explore the content to be reviewed to identify important related files in the same project that may be relevant to the review (e.g. relevant type definitions, configuration files, security model descriptions, design documents).

The purpose of this is to collect important references and avoid duplicated searches for the same data later on in the process.

If pull request context is supplied, use `pwsh-calls/query-pr-context.md` for any relevant metadata or discussion lookup. The full capture is a data store, not a reading document; do not view or dump it, bypass the recipe with a provider fetch, or concatenate pages. Retain relevant references rather than discussion dumps. Apply [error recovery](error-recovery.md) to query and other command errors: correct orchestration mistakes and retry safe transient failures before reporting an unresolved blocker.

Emit your findings as `references.md` file in session state, containing a categorized list of references. For example:

```text
Documentation references:

- Windows HTTP client API (WinHTTP): https://learn.microsoft.com/en-us/windows/win32/winhttp/about-winhttp
- Frobnicator design: crates/frobnicator/docs/DESIGN.md
- Known gaps in frobnicator: crates/frobnicator/TODO.md
- cargo-mutants user guide: https://mutants.rs/

Important design elements:

- GleemAPI user guide: book/gleem/index.md
- GleemAPI related constants: crates/gleem_core/src/constants.rs
- Frobnicator data model: grpc/datamodel/frobnicator.proto

High importance types:
- FleemManager (top level type for operating on Fleem instances): src/dotnet/Fleems/FleemManager.cs
- FleemSelector (applies queries to find Fleem instances to operate on): src/dotnet/Query/FleemSelector.cs
- TelemetryCallback (universal approach to telemetry in project): crates/telemputer/src/types.rs
```