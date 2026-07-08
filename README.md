# AI skills by Sander Saares

Reusable [agent skills](https://agentskills.io/home) - an open format
for packaging procedural knowledge that AI agents load on demand -
plus companion evaluations that the author finds useful. Feel free to
do what you like with them; see [LICENSE](LICENSE) for terms.

## Skills in this repo

| Skill | Purpose |
| --- | --- |
| [`ss-cargo-deps-tidy`](skills/ss-cargo-deps-tidy/SKILL.md) | Tidies Rust dependency declarations across a Cargo workspace: unifies declaration style, centralises versions on the workspace level, disables default features, and verifies minimum-version compatibility for published libraries. |

Each skill lives under `skills/<name>/` with `SKILL.md` as the
entrypoint. Helper scripts (when present) sit alongside the SKILL file.

## Requirements

- An agent that supports the [agent skills](https://agentskills.io/home)
  format - tested with GitHub Copilot (CLI and VS Code) and Claude
  Opus 4.8. See [the client showcase](https://agentskills.io/clients)
  for the broader list of compatible agents.
- PowerShell 7+ on the machine running the agent. Skills that ship
  helper scripts use `pwsh`; no other modules need to be installed for
  end-user use.

## Quick start

Clone the repo first:

```sh
git clone https://github.com/sandersaares/ss-ai.git
```

### Copilot CLI

Add the repo's `skills/` directory once. Copilot CLI persists the
directory in its config and discovers every skill under it
automatically, in this and future sessions:

```sh
copilot
/skills add path/to/ss-ai/skills
```

### GitHub Copilot in VS Code (and other skills-compatible agents)

Use [`Install-CopilotSkills.ps1`](Install-CopilotSkills.ps1) to
symlink every skill into the location your agent reads from. With no
arguments, skills land in `$HOME/.copilot/skills/` and are available
to all repos. Pass `-Path <dir>` to install into
`<dir>/.github/copilot/skills/` instead - typically a repo root so the
skills get scoped to that repo.

```powershell
pwsh path/to/ss-ai/Install-CopilotSkills.ps1                # user-wide
pwsh path/to/ss-ai/Install-CopilotSkills.ps1 -Path .         # cwd
pwsh path/to/ss-ai/Install-CopilotSkills.ps1 -Path path/to/repo
```

Existing links with the same name are replaced; existing plain
directories with the same name are left alone (with a warning). On
Windows the script uses NTFS junctions for the directory links, so it
works in an ordinary non-elevated shell without Developer Mode.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the full development setup,
including how to run unit tests for skill helpers and end-to-end Waza
evaluations.

For agent-facing repository conventions (skill layout, Waza usage,
verified path-resolution rules), see [AGENTS.md](AGENTS.md).

Outstanding improvement ideas are tracked in [TODO.md](TODO.md).
