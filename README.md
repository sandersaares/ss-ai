# AI tooling by Sander Saares

Reusable instructions, [agent skills](https://agentskills.io/home), their
native helper, and companion evaluations. See [LICENSE](LICENSE) for terms.
This repository owns material used by agents. General machine setup and
user-facing Copilot preferences belong to [dev-env](https://github.com/sandersaares/dev-env).

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [`ss-cargo-deps-tidy`](skills/ss-cargo-deps-tidy/SKILL.md) | Tidies Rust dependency declarations across a Cargo workspace: unifies declaration style, centralises versions on the workspace level, disables default features, and verifies minimum-version compatibility for published libraries. |
| [`ss-review`](skills/ss-review/SKILL.md) | Reviews requested content with evidence, bounded workers, and explicit human approval before publication. |
| [`ss-review-deep`](skills/ss-review-deep/SKILL.md) | Explicitly requested deep review orchestration. |
| [`ss-review-proactor`](skills/ss-review-proactor/SKILL.md) | Selects and claims a pull request before a review in the primary session; retains human decision gates. |
| [`ss-autoreapprove`](skills/ss-autoreapprove/SKILL.md) | Renews invalidated approvals only under the installed significance policy; supports read-only dry runs. |

Each skill lives under `skills/<name>/`, with `SKILL.md` as its entry point
and runtime commands under `scripts/`. The parallel `instructions/` root
contains reusable guidance with its original applicability metadata.

## Requirements

- Latest stable PowerShell (`pwsh`); scripts target the 7.6 release line.
- Git and an agent supporting skills. Copilot CLI uses the instruction registration below.
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
sources from its own checkout, builds locked local `ss-skills-util`, and
compares Cargo's build artifact with the executable on `PATH` before
activating instructions and skills. Reinstall between skill invocations
to refresh local source. Cargo reports unrelated binary ownership conflicts;
the installer reports shadowing executables rather than guessing an install root.

Skills link into `~/.copilot/skills/`. Instructions link from
`~/.config/ss-ai/agents/.github/instructions/` to this checkout's `instructions/`;
the installer registers the containing repo-shaped `agents` directory once in
`COPILOT_CUSTOM_INSTRUCTIONS_DIRS`. This does not load the checkout's development
`AGENTS.md` as global guidance. Other clients must support these installed
discovery locations or configure their own supported registration.

Windows uses junctions and user-registry instruction registration. Ubuntu uses
symbolic links and `~/.config/ss-ai/environment.sh`, loaded from Bash startup
files before noninteractive SSH guards. Both preserve unrelated entries and
report non-link collisions explicitly. `-HomeDirectory` selects the home for
links and Bash configuration; Windows environment registration remains user-scoped.
No installer changes Copilot mouse settings or other interaction preferences.
Start a new shell and agent session after setup. Configure Windows and each
WSL/Linux user environment separately.

### Manual cutover

Finish active skill invocations. Remove only identified obsolete instruction
registrations, skill links, and duplicate direct skill discovery roots through
the client's supported settings. Preserve unrelated configuration and non-link
installations. Refresh general machine setup so its generated Bash file contains
only its own PATH configuration, then run `Install.ps1` here. If Cargo reports
an actual source-ownership conflict, resolve that specific registration through
Cargo before retrying. Do not run old and new workflow state together or replay
uncertain remote writes; start fresh review sessions.

From outside both repositories, confirm each skill appears once, instruction
applicability is retained, and `ss-skills-util --version` resolves successfully.
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
