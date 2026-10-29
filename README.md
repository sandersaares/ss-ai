# AI tooling by Sander Saares

[Agent skills](https://agentskills.io/home), their
native helper, and companion evaluations. See [LICENSE](LICENSE) for terms.
This repository supplies material used by agents. Installation does not manage
general machine setup or user-facing Copilot preferences.

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [`ss-cargo-deps-tidy`](skills/ss-cargo-deps-tidy/SKILL.md) | Tidies Rust dependency declarations across a Cargo workspace: unifies declaration style, centralises versions on the workspace level, disables default features, and verifies minimum-version compatibility for published libraries. |
| [`ss-review`](skills/ss-review/SKILL.md) | Reviews requested content with evidence, bounded workers, and explicit human approval before publication. Explicit `self-review` mode delivers local Markdown findings non-interactively, including skipped audit findings, without remote writes. |
| [`ss-review-deep`](skills/ss-review-deep/SKILL.md) | Explicitly requested deep review orchestration. |
| [`ss-review-proactor`](skills/ss-review-proactor/SKILL.md) | Selects and claims a pull request before a review in the primary session; retains human decision gates. |
| [`ss-autoreapprove`](skills/ss-autoreapprove/SKILL.md) | Renews invalidated approvals only under the installed significance policy; supports read-only dry runs. |

Each skill lives under `skills/<name>/`, with `SKILL.md` as its entry point
and runtime commands under `scripts/`. Instructions come from independently
configured directories rather than from this repository.

## Requirements

- Latest stable PowerShell (`pwsh`); scripts target the 7.6 release line.
- Git and an agent supporting skills.
- Rustup/Cargo and the pinned repository toolchain. Windows needs Visual Studio
  C++ Build Tools and Windows SDK; Ubuntu needs a compiler/linker such as
  `build-essential` for mimalloc. Cargo's configured installation binary
  directory must already be on `PATH`.

The native stack supports Windows and Ubuntu, including WSL. The Cargo
inventory helper and directory-linking tests also cover macOS; this is not
a macOS support promise for the complete installation.

## Quick start

Clone the repo first:

```sh
git clone https://github.com/sandersaares/ss-ai.git
```

### Install agent material

```powershell
pwsh -NoProfile -File path/to/ss-ai/Install.ps1
```

Run as the normal user without elevation or `sudo`. Installation resolves
sources from its own checkout, builds locked local `ss-ai`, and
compares Cargo's build artifact with the executable on `PATH` before
activating skills. Reinstall between skill invocations
to refresh local source. Cargo reports unrelated binary ownership conflicts;
the installer reports shadowing executables rather than guessing an install root.

Skills link into `~/.copilot/skills/`. The installer does not bundle instructions
or register instruction roots. Skills discover optional instructions through
`COPILOT_CUSTOM_INSTRUCTIONS_DIRS`, a comma-separated list of external roots.
Use your own instructions or run `Configure-Pc` in `dev-env` separately to
install and register its instructions. Other clients must support skill discovery
at the installed location or configure their own supported registration.

Windows uses junctions; Ubuntu uses symbolic links. Both report non-link
collisions explicitly. `-HomeDirectory` selects the home for links and legacy
cleanup; Windows environment cleanup remains user-scoped.
No installer changes Copilot mouse settings or other interaction preferences.
Start a new shell and agent session after setup. Configure Windows and each
WSL/Linux user environment separately.

### Updating an existing installation

Finish active skill invocations, then run `Install.ps1` here. The installer
removes the exact legacy `~/.config/ss-ai/agents` registration from the Windows
user environment, or recognized ss-ai exports and startup loaders on Ubuntu.
It preserves other instruction roots, user shell content, and non-link content.
An obsolete instruction link is removed only when it points at this checkout.
Modified or unrecognized legacy configuration remains available for manual review.
Configure external instructions separately through `dev-env` or your own setup.
Remove duplicate direct skill discovery roots through the client's supported
settings while preserving unrelated configuration. If Cargo reports
an actual source-ownership conflict, resolve that specific registration through
Cargo before retrying. Do not run old and new workflow state together or replay
uncertain remote writes; start fresh review sessions.

From outside this checkout, confirm each skill appears once, instruction
applicability is retained, and `ss-ai --version` resolves successfully.
Installation does not authenticate clients, enable schedules, or approve reviews.

### Review reports and scheduled work

Review HTML requires Pandoc, Node.js, and the skill's pinned Mermaid/browser
dependencies. Inspect readiness without installing anything:

```powershell
pwsh -NoProfile -File ./skills/ss-review/scripts/Test-ReviewReportTools.ps1
```

After explicitly approving dependency installation:

```powershell
pwsh -NoProfile -File ./skills/ss-review/scripts/Install-ReviewReportTools.ps1
```

The setup and readiness commands exercise a real embedded Mermaid diagram.
See the [platform setup guide](skills/ss-review/report-tool-setup.md) for
Node/Pandoc versions and Ubuntu browser system dependencies. Without HTML
readiness, the review reports that limitation and uses Markdown as specified
by its workflow.

Configure schedules explicitly for `ss-review-proactor` or `ss-autoreapprove`;
installation does not enable them. Proactor selects one review by default;
explicit loop mode repeats completed reviews without bypassing human questions.
Autoreapproval preserves external approval and uses its installed significance
policy and guarded writes. See each skill for supported providers and limitations.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the full development setup,
including how to run unit tests for skill helpers and end-to-end Waza
evaluations.

For agent-facing repository conventions (skill layout, Waza usage,
verified path-resolution rules), see [AGENTS.md](AGENTS.md).

Outstanding improvement ideas are tracked in [TODO.md](TODO.md).
