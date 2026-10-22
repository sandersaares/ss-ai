# Waza grader & task YAML conventions

See the upstream
[grader reference](https://microsoft.github.io/waza/guides/graders/) and
[YAML schema](https://microsoft.github.io/waza/reference/schema/).

## Paths are resolved against CWD, not the YAML file

Waza resolves several paths relative to the **current working
directory**. Invoke Waza from the active checkout root, including a worktree.

- The `trigger` grader's `skill_path` is resolved against the CWD.
- `--context-dir` defaults to `./fixtures` relative to the eval spec,
  but references inside the spec are easier to reason about with a
  fixed CWD.

## Grader and task YAML fields

- **Grader `skill_path`** - relative to the repo root, e.g.
  `skills/ss-cargo-deps-tidy/SKILL.md`. Do **not** try to traverse
  with `../..` from the task YAML; the path is not resolved against
  the task file's directory.
- **`inputs.files[].path`** - both source (under the active fixtures
  dir) AND destination (under the per-task workspace dir). To seed
  the workspace root, the fixture file must live directly under the
  active fixtures dir (not under a sub-folder like `before/`). When
  two tasks both need to seed the same workspace-relative path (e.g.
  each wants its own root `Cargo.toml`), give each task its own
  private fixture root via per-task `context_dir`.
- **Per-task `context_dir`** - task YAML files accept a top-level
  `context_dir:` that overrides the eval-level fixtures dir for that
  one task ([waza/internal/models/testcase.go][testcase-go], field
  `TestCase.ContextRoot`). Used directly with `filepath.Join` by the
  runner, so it is resolved relative to the **CWD of `waza run`**
  (not the task YAML or eval spec). Express it as a repo-root-relative
  path, e.g. `context_dir: evals/<skill>/fixtures/trigger/<task-id>`.
- **`inputs.files[].content`** - alternative to `path`, supplies the
  file body inline. Prefer `path` + per-task `context_dir` for real
  fixture files; reserve inline `content` for one-off cases where
  carving out a fixture directory adds more clutter than it removes.
- **`diff` grader `snapshot`** - path relative to the grader's
  `context_dir`. Store expected files under a clearly named subdir
  (e.g. `fixtures/<scenario>-expected/...`) so they don't collide
  with workspace-seeding fixtures.
- **`diff` grader `context_dir`** - IS a real field (per
  [docs/graders/diff.md][diff-docs]). Resolved relative to the CWD of
  `waza run`. The CLI default (`--context-dir ./fixtures` relative to
  spec) is unreliable in practice - it silently misresolves to CWD
  when filtering with `--tags` / `--task`. Always set it inline:
  `context_dir: evals/<skill>/fixtures`.
- **Grader config nesting** - `type:` and `name:` go at the top of
  the grader entry; type-specific keys go under `config:`. Example:

  ```yaml
  graders:
    - type: behavior
      name: skill-invoked
      config:
        required_tools:
          - skill
  ```

[testcase-go]: https://github.com/microsoft/waza/blob/main/internal/models/testcase.go
[diff-docs]: https://github.com/microsoft/waza/blob/main/docs/graders/diff.md

## Verifying skill invocation

Waza's bundled copilot-sdk emits `skill.invoked` events when the
agent invokes a skill. They surface in three places in the results
JSON:

- In each task's `runs[].transcript` as events with
  `type: "skill.invoked"` (the `content` field carries the full
  SKILL.md text).
- Aggregated into `runs[].skill_invocations` (omitted entirely if no
  skill was invoked).
- As a `skill` entry in `runs[].session_digest.tools_used` /
  `tool_calls`.

For **positive trigger tasks**, prefer the `skill_invocation`
grader - it inspects `runs[].skill_invocations` and matches against
a `required_skills` list, so it verifies *which* skill was invoked,
not just that the `skill` tool fired:

```yaml
graders:
  - type: skill_invocation
    name: ss-cargo-deps-tidy-invoked
    config:
      required_skills:
        - ss-cargo-deps-tidy
      mode: any_order
      allow_extra: true
```

The `behavior` grader with `required_tools: [skill]` is a looser
alternative - it only checks tool *names*, not arguments, so any
`skill` invocation (regardless of which skill) satisfies it. That's
fine when only one skill is discoverable in the eval's CWD (Waza
finds it at `skills/<name>/SKILL.md`), but it breaks silently as
soon as a second skill enters the picture. Use `skill_invocation`
for new positive tasks.

For **negative trigger tasks**, use the `behavior` grader with
`forbidden_tools: [skill]`:

```yaml
graders:
  - type: behavior
    name: no-skill-invoked
    config:
      forbidden_tools:
        - skill
```

This is **stricter than the question we actually want to ask.** The
accurate question for a negative-trigger task is "skill `S` was not
invoked" - invoking an unrelated skill would be acceptable. But:

- `behavior` filters on tool *name*, not arguments, so
  `forbidden_tools: [skill]` rejects *every* skill invocation
  regardless of which skill.
- `skill_invocation` would be the precise fit, but it requires a
  non-empty `required_skills` list and has no `forbidden_skills`
  field, so it cannot express "no specific skill is required, but X
  must not fire" (tracked upstream as a Waza feature request).

The trigger lane disables the repository's unrelated skills through
`config.disabled_skills`, so this assertion measures only the Cargo skill.
Keep that list aligned with the installed skill collection.

**Use this signal only for trigger-style evals.** In trigger evals
the whole point is "did the agent decide to invoke the skill", so
`skill` tool presence/absence is the metric. In workflow-style
evals, even when the prompt names the skill explicitly ("apply the
`ss-cargo-deps-tidy` skill"), the agent sometimes reads the skill
body from its system-prompt context and proceeds directly with
`view` / `str_replace` / `create` calls *without* invoking the
`skill` tool. Observed empirically: the `folo-3e1f241-cargo-only`
run at `eval-results/folo-debug/...20260520-083815.json` completed
79 tool calls with zero `skill.invoked` events. For workflow evals,
measure the *outcome* (e.g. `diff` graders against expected files)
rather than tool-call shape.

## Avoid the `trigger` grader

The `trigger` grader scores the *prompt* heuristically against the
skill's keyword set (no stop-word filtering, no session inspection),
so false positives on negative cases are easy and the agent's actual
behavior is invisible to it. Prefer `behavior` / `skill_invocation`
/ `diff` graders that observe what the agent actually did.

## Disable body injection in trigger eval specs

Current Waza requires a target `skill` and `metrics` for a legacy skill
suite. Set `skill: ss-cargo-deps-tidy` and
`config.inject_skill_body: false` in the trigger lane. This preserves
skill discovery without providing the full skill body before invocation.
Workflow evaluations may inject the body because their assertions measure
the resulting files rather than activation.
