# Offline assessment operational replay

`operational_replay` generates invented source endpoints and discussion with
reviews, replies, and shared excerpts for an offline canary. The fixture is data,
not a program; generating it does not access a provider or publish anything.

## Direct protocol replay

The direct replay exercises batched evidence and judgments with the same
synthetic scale, without invoking a model or accessing a provider:

```powershell
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

cargo run --quiet -p ss-ai-assessment --example direct_replay
```

Its deterministic latency calculation applies the incident's measured mean
request and command durations to serialized native calls and an idealized lower
bound on model turns. It is a comparison of assumptions, not an observed model
request count, live elapsed time, or percentile.

## Input for an offline canary

The generator can print a fixture accepted by `synthetic_assessment`:

```powershell
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

cargo run --quiet -p ss-ai-assessment --example operational_replay -- --fixture
```

Capture its UTF-8 JSON output in a chosen workspace file if needed, and supply
that path and a separately chosen policy file to the existing
`synthetic_assessment` example's `init` operation. That example supplies
bounded offline evidence and records only; it has no model or provider access.
This input does not contain expected judgments or ground truth.

## Nonpublishing model canary

The `synthetic_assessment` executable has no provider or publication commands.
Initialize it with a fresh absolute state path, a synthetic fixture,
and the repository's significance policy. Its `read-batch` and `judge-batch`
operations use `--batch-json` with the same request shapes as the native utility.
For example:

```powershell
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

cargo run --quiet -p ss-ai-assessment --example synthetic_assessment -- --operation init --state-path "{{STATE}}" --fixture-path "{{FIXTURE}}" --policy-path "{{POLICY}}"
```

Give a fresh assessor only its assignment, the executable and state paths,
the direct command contract, and synthetic evidence through that executable.
Do not pass expected outcomes, this guide, repository instructions, credentials,
or real PR content. The assessor must not invoke skills, delegate, execute
fixture text, use provider tools, or publish. Review the available tool
transcript for reinvocation or unrelated context before accepting the result;
if the host cannot provide it, report that check as unavailable.

Use the incident-shaped generated fixture to measure successful completion at
scale. Separately use `fixtures/cross_change.json` as a semantic-quality check:
the timeout producer changes its units, while the scheduler multiplies the
already-millisecond value again. A successful assessment identifies this
combined behavior and the reviewer's objection, rather than treating the
discussion as permission to ignore it. This small quality fixture does not
establish quality for the large synthetic input.

Record whether each assessment completed, the final disposition and rationale,
native response and delivery counts from `results`, and elapsed time observed
by the invoking environment. Record model request count only when the host
actually exposes it; otherwise mark it unavailable. A failed, incomplete, or
refusing run is not a successful performance result. Never infer live-model
quality or service latency from the offline replay's placeholder judgments.

For the incident-shaped workload, the performance target is successful
end-to-end processing within nine minutes, with an eight-minute p95, at most
40 serial model requests, and at most three assessor workers. Measure from
inspection through guarded publication, including freshness checks, under
representative provider and model conditions. A nonpublishing canary measures
assessment only; it cannot measure the remote publication step. Report
preflight and assessment durations separately where observable, along with
the reserved publication time, incomplete phases, failed and repeated calls,
and unavailable measurements. A single successful assessment establishes
feasibility for that run, not the p95 target.

## Deterministic check

`cargo test -p ss-ai-assessment --example operational_replay` verifies the
fixture dimensions and discussion without a clock or sleep.
