# Offline assessment operational replay

`operational_replay` builds invented source endpoints, replacement patches, and
discussion in memory and drives the native assessment to completion. The
discussion includes a review, replies, shared excerpt references, and a
top-level comment. The generated source is data, not a program: the replay
does not execute it, access a provider, invoke a model, or publish anything.

Run the replay from the checkout root:

```powershell
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

cargo run -p ss-ai-assessment --example operational_replay
```

The JSON report includes the actual aggregate patch bytes, discussion bytes,
unit count, successful native API call count, evidence-read call count,
state-mutation call count, and compact JSON bytes returned by measured calls.
Native calls here mean `Assessment` construction and methods; fixture
generation and `change_id` helper calls are outside the count. The mutation
count covers operations on an existing assessment, not construction.
The byte count includes serialized digest strings, not the production command
wrapper, pretty-printing, or process transport. It is not a token count.
Evidence reads mutate delivery progress and count in both read and mutation
categories. Creation and finish count as calls, but return no measured JSON.
The report also states that the replay itself starts no commands and performs
no filesystem reads or writes; Cargo and the terminal running it are outside
that boundary. The elapsed measurement includes fixture generation and the
in-process native replay, not compilation or output serialization. It is a local
observation, not a service, production command, or model latency measurement.

The simulated critical path is an idealized count of sequential native calls:
it replaces the sum of the independent unit chains with their longest chain
and leaves all other measured calls serial. It assumes unlimited workers and
no scheduling, process, serialization, network, contention, or model cost.
It is not a prediction of elapsed time or a claim that parallel execution was
performed. The synthetic notes and insignificant dispositions are fixed
placeholders used to exercise dependencies. They do not establish significance,
model quality, or authorization to approve.

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

A real-model canary needs an explicitly coordinated model invocation, an
approved execution environment, and a separately defined evaluation protocol.
This driver intentionally makes no model or service call and does not infer
quality from the placeholder results.

## Deterministic check

`cargo test -p ss-ai-assessment --example operational_replay` verifies the
fixture dimensions, completion, counts, and repeatability without a clock or
sleep. Native calls still perform their ordinary in-memory work.
