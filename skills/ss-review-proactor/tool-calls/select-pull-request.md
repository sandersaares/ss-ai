```text
ss-ai review select --repository-url "{{REPOSITORY_URL}}" --session-id "{{SESSION_ID}}" --state-path "{{STATE_PATH}}" --claim-message "{{CLAIM_MESSAGE}}" --selection-mode "{{SELECTION_MODE}}"
```

Run this command once for the active repository in the ordered repository list.
The native command remains single-repository; the skill owns list advancement.
After a completed review, keep using the same repository. On `noCandidate`,
advance to the next repository and issue a fresh command with its URL and a
fresh state path.

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `REPOSITORY_URL` | Active GitHub or Azure DevOps repository browser URL resolved in stage 1 of `SKILL.md`: an entry from the explicit invocation/schedule list, or the invocation's Git repository by default. Not a clone or pull request URL. |
| `SESSION_ID` | Identifier of the primary user-facing session executing this invocation. |
| `STATE_PATH` | Fresh absolute path to a JSON journal in the current cycle's session-state subdirectory. Reuse this exact path for confirmation, never for a new cycle or selection run. |
| `CLAIM_MESSAGE` | Standard announcement from stage 2 of `SKILL.md`, adapted to the current user's communication policy, without protocol markers. Pass it as one literal string argument, escaping for the invoking shell if one is used. Reuse it for confirmation. |
| `SELECTION_MODE` | Resolved mode for the complete invocation: `available` by default, or `assigned` to require an assignment without an active submitted review. |
| `REVIEWED_SOURCE_COMMIT` | Exact final source revision the review evaluated, retained from its maintained artifacts. Required only for completion. |
| `TENANT` | Optional user-configured Azure tenant for the active repository. |

For Azure DevOps, optionally append `--tenant "{{TENANT}}"`. Use the same tenant
during `ss-review`.

For stage 3, replace `review select` with `review confirm` and preserve all
argument values from stage 2.
The helper reads the journal
after checkout preparation and any user interaction, waits only the remaining
portion of the claim's 60-second interval, and checks the claim once. A
`confirmed` result proceeds directly to `ss-review`; a replacement `claimed`
result needs its own preparation before confirmation.

Immediately before `ss-review` publishes, replace `review select` with
`review check` and preserve all argument values. The command verifies the
active reservation and returns `confirmed` without extending a deadline. If the
pull request closed and the user explicitly authorized closed-PR publication in
`ss-review`, append `--publish-to-closed-pull-request` only for this check.

After `ss-review` completes phase 9, replace `review select` with
`review complete`, preserve all argument values except the closed-PR switch,
and append `--reviewed-source-commit "{{REVIEWED_SOURCE_COMMIT}}"`. Resolve
that placeholder from the final checked review capture or publication result;
never use the live provider head at completion. The command changes the active
marker to `completed` and returns `completed`. Do not select another pull
request until this command succeeds.

The command writes the journal and emits one JSON object. Read `status`,
`target`, and `diagnostics` as described in `SKILL.md`. On a nonzero exit or error,
apply the shared [error-recovery policy](../../ss-review/error-recovery.md).
Correct invocation mistakes and retry safe transient failures only when no
remote write was attempted and the journal lifecycle permits re-entry.
Never reinterpret an error as `noCandidate` or automatically repeat a possibly
successful write. A journal in `claiming` or another uncertain write state
still requires human reconciliation.

Invoke the installed executable directly on `PATH`. If it is unavailable,
ask the user to run the `ss-ai` checkout's `Install.ps1`; do not install
or upgrade tools during a skill run.
