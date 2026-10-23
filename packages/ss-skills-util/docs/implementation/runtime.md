# Runtime boundaries

## Responsibility

The runtime boundaries keep external effects explicit across all functional
areas. The executable supplies process, HTTP, and time implementations to the
command dispatcher. Tests supply fakes through the same interfaces; no
environment variable or compilation mode changes production decisions.
See the [architecture](../implementation.md) for the responsibility split.

## Time and waits

The clock boundary supplies wall-clock timestamps and waits. Selection uses
timestamps for commit recency, claim lifetime, and preparation deadlines.
Transport uses the same boundary for rate-limit recovery, and journal storage
uses it for contention waits. Tests control observations and advance time
without real sleeps, preserving the production decision paths.

Offset-bearing service timestamps represent instants. Normalize them before
comparison and persist preparation deadlines in a form that another invocation
can parse. Round remaining preparation time upward so confirmation cannot run
before its deadline.

Jiff `Timestamp` represents instants; fixed `std::time::Duration` values express
elapsed policy intervals rather than calendar arithmetic. The service parser
checks the RFC 3339 shape before Jiff parses the timestamp because the broader
ISO syntax accepted by Jiff is not the service evidence contract. Wire deadlines
use an explicit UTC offset for schema-2 interoperability.

The production clock obtains timestamps through Jiff and delays through `tick`.
A local current-thread Tokio runtime drives each delay, keeping the command and
its injectable clock interface synchronous. This confines the asynchronous
timer driver to the wait boundary rather than introducing a runtime requirement
into the selection state machine or its fakes.

## Errors and command output

Errors preserve contextual causes through `ohno`. A classified failure carries
the machine-readable recovery category independently of its human explanation.
The CLI exposes only classifications useful to callers; diagnostics and
backtraces are not structured handoff data.

The command dispatcher returns the entire result only after its operation
completes. The executable writes that result to stdout, while failures go to
stderr with a failing exit status. Help and version output bypass JSON
serialization. Credentials never belong in results, journals, or diagnostics.
