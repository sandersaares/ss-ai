# AI skills by Sander Saares

Reusable [Copilot skills](https://docs.github.com/en/copilot/) and
companion evaluations that the author finds useful. Feel free to do
what you like with them; see [LICENSE](LICENSE) for terms.

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [`ss-cargo-deps-tidy`](skills/ss-cargo-deps-tidy/SKILL.md) | Tidies Rust dependency declarations across a Cargo workspace: unifies declaration style, centralises versions on the workspace level, disables default features, and verifies minimum-version compatibility for published libraries. |

Each skill lives under `skills/<name>/` with `SKILL.md` as the
entrypoint. Helper scripts (when present) sit alongside the SKILL file.

## Requirements

- A coding agent that supports the [Copilot skill format](https://docs.github.com/en/copilot/) — tested with GitHub Copilot and Claude Opus 4.7.
- PowerShell 7+ on the machine running the agent. Skills that ship
  helper scripts use `pwsh`; no other modules need to be installed for
  end-user use.

## Quick start

Clone the repo first:

```sh
git clone https://github.com/sandersaares/ss-ai.git
```

### Copilot CLI

```sh
copilot
/skills add path/to/ss-ai/skills
```

### GitHub Copilot in VS Code

Symlink (or copy) the skill folder into your target repo:

```sh
mkdir -p .github/copilot/skills
ln -s path/to/ss-ai/skills/ss-cargo-deps-tidy .github/copilot/skills/ss-cargo-deps-tidy
```

Then start a Copilot Chat agent session in that workspace.

### Claude

Point Claude at `skills/<name>/SKILL.md` directly (system-prompt
attachment or your harness's skill-loading mechanism). Helper scripts
must be reachable from the agent's working directory.

### Standalone

`SKILL.md` is plain Markdown — read it and follow the workflow
manually. Helper scripts (e.g. `Generate-Inventory.ps1`) run directly
from a shell.

> Each skill's `SKILL.md` lists prerequisites specific to that skill.
> `ss-cargo-deps-tidy`, for example, requires a Rust/Cargo workspace
> and is a no-op otherwise.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the full development setup,
including how to run unit tests for skill helpers and end-to-end Waza
evaluations.

For agent-facing repository conventions (skill layout, Waza usage,
verified path-resolution rules), see [AGENTS.md](AGENTS.md).

Outstanding improvement ideas are tracked in [TODO.md](TODO.md).
