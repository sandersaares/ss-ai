# Journal transactions and selection

## Responsibility

The journal records selection intent and outcome so an interrupted command
cannot silently repeat a remote write. The selection state machine owns
transitions; storage owns transaction exclusion and complete publication.
See the [selection design](../design/review-selection.md) for user-visible rules.

## Wire records and workflow stages

The journal uses schema 2 and its ownership fields. Typed validation checks
both structural and state-dependent requirements at read and write boundaries.
The workflow converts validated wire records to a private stage enum: claim
intents carry ownership and target data, while created reservations also carry
remote identifiers and a parsed deadline. Transitions change stages as complete
values; persistence converts them back to schema 2. Attempts and diagnostics
use nonzero PR identifiers while retaining numeric JSON fields.

Confirmation checks the supplied session, exact repository URL, and
authenticated identity before any claim effect. Ownership uses remote
comment/thread identifiers, author, session, and UUID, not surrounding prose.

`selecting` records precede enumeration; `claiming` precedes creation;
`claimed` contains identifiers and a check deadline; `confirmed` records the
sole-live-claim decision. `discarding` precedes deletion. `noCandidate` is a
successful completed search, not an error fallback. Only `claimed` can enter
confirmation. Interrupted states require human reconciliation.

## Transaction lifetime and publication

Acquire a retained sibling lock before checking whether the journal exists.
Keep it until the operation returns, including the preparation wait and all
remote side effects. Never unlink the lock. Persist through a unique
same-directory staging file and atomic replacement, permitting delete-sharing
readers on Windows. Retry only lock/sharing contention, never an entire
transaction. Each file operation allows 6,000 attempts at 50-millisecond
intervals, providing approximately five minutes for queued transactions.
Exhaustion remains an explicit contention failure. A failed staging write
leaves the old complete journal intact.

Holding the same lock across validation, waiting, and provider effects prevents
two local invocations from acting on the same journal concurrently. Remote
claims remain cooperative reservations rather than service-side locks.
