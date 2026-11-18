```text
ss-ai workflow save-focus-area-cache --session-state "{{SESSION_STATE}}" --input-directory "{{SOURCE_DIRECTORY}}" --preparation-identity "{{PREPARATION_IDENTITY}}" --skill-root "{{SKILL_ROOT}}" --recipe-path "{{SKILL_ROOT}}/prepare-focus-areas.md" --cache-directory "{{CACHE_DIRECTORY}}" --generator-profile "gpt-6-luna" --freshness-days 7
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `SESSION_STATE` | Absolute session-state directory with a finalized expansion manifest. |
| `SOURCE_DIRECTORY` | `sourceDirectory` returned by this invocation's instruction snapshot. |
| `PREPARATION_IDENTITY` | Exact `preparationIdentity` returned by this invocation's cache restore. |
| `CACHE_DIRECTORY` | Absolute path to `.copilot/cache/ss-review/focus-areas/v1` under the current user's home directory. |

The profile identifies the final scope verifier and freshness forces reference refresh; see
`docs/implementation.md`, "Cache inputs", and `prepare-focus-areas.md`, Stage 1.

Run only after publishing this generation and validating its artifacts. The
command checks the generation receipt and derives completeness from the manifest. Stored/Existing means a
complete set is cached; Bypassed reports why storage was skipped. On validation
failure, repair the manifest or source preparation before retrying.
