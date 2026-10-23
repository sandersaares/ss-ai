# Transport and authentication

## Responsibility

Transport executes provider requests without exposing credentials to workflow
records. The provider adapter supplies routes and payloads; transport controls
authentication, byte handling, and whether a request may be repeated.
See the [architecture](../implementation.md) for the surrounding boundaries.

## Native process and HTTP boundaries

GitHub runs `gh api` with explicit argument lists and UTF-8 byte capture.
Credentials remain inside `gh`. Query variables and write payloads travel
through stdin.

Azure CLI supplies only an OAuth access token for the Azure DevOps resource,
optionally forwarding the configured tenant. Native HTTP reads and writes
UTF-8 JSON so Azure CLI cannot alter Unicode content. Collection and repository
coordinates use escaped URL segments.

Isolated Azure Git fetches use the same OAuth acquisition and tenant binding.
The native process supplies a repository-URL-scoped bearer header through
invocation-only Git configuration in the child environment, not argv or files.
Fetches disable redirects and credential helpers; failure diagnostics redact
the token. GitHub fetches retain the GitHub CLI credential helper. Source
inventories never inherit the user's Git configuration or execute reviewed code.

Native processes have explicit working directories, environments, and
arguments. Windows command shims require the platform command processor;
untrusted payloads stay on stdin rather than in shell syntax. Output and error
pipes drain while stdin is written so large documents cannot deadlock.

## Recovery and replay

Read retries are bounded for transient faults. Explicit service rate-limit
refusals honor recovery guidance separately. Claim writes never retry an
ambiguous response. OAuth refresh applies only where repeating a request is
permitted.

HTTP replies retain status, normalized headers, and raw bytes until transport
classifies them. GitHub failures use captured CLI diagnostics and explicit
rate-limit evidence. Neither path treats an uncertain write as permission to
try again; journal reconciliation belongs to the calling workflow.

Approval publication uses explicit single-attempt methods for both providers,
including ADO's reviewer PUT and audit-thread POST. These methods do not wait
and replay a rate-limit refusal: publication must refresh authority before any
new effect. ADO sign-in documents or malformed successful write replies are
ambiguous effects, not proof that the service made no change.
