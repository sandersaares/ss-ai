# Skills utility implementation

## Architecture

One nonpublished Cargo package implements the utility-wide [design](design.md).
A thin binary installs mimalloc, discovers the execution environment, and
writes one result or diagnostic. A testable library owns command dispatch and
the complete operations. Internal Rust visibility does not define a supported SDK.

The command layer translates named inputs into explicit targets. Provider
adapters turn service evidence into identities, pull requests, and comments.
The selection state machine owns decisions and journal transitions; transport
and storage own external effects. Registration uses the same provider and
identity rules without requiring a selection journal.

## Implementation topics

Read the topics relevant to the responsibility being changed:

- [Automatic reapproval](reapproval.md) defines approval recognition, exact source
  evidence, assessment, publication recovery, and supported provider boundaries.
- [Provider evidence and identities](implementation/providers.md) explains
  complete enumeration, coordinate resolution, and non-destructive registration.
- [Transport and authentication](implementation/transport.md) explains process
  and HTTP boundaries, Unicode handling, and request replay rules.
- [Journal transactions and selection](implementation/journals.md) explains
  stage transitions, ownership, locking, and atomic publication.
- [Runtime boundaries](implementation/runtime.md) explains cross-cutting time,
  dependency injection, errors, and command output.
- [Development and installation](implementation/development.md) explains
  validation coverage, platform setup, and executable discovery.

The topic structure separates shared mechanisms from functional areas. For
example, provider evidence supports both registration and selection, and the
time boundary supports both claim decisions and transport recovery waits.
