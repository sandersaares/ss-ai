```text
ss-skills-util reviewer register --url "{{PR_URL}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `PR_URL` | Browser URL of the pull request under review. |
| `TENANT` | Optional Azure tenant from the invocation or proactor handoff. |

For Azure DevOps, append `--tenant "{{TENANT}}"` when a tenant was supplied.
Read the JSON `status` (`added`, `alreadyRegistered`, or `selfIsAuthor`) and
`identity`; report the result using `pullRequestUrl`. Preserve the registration
failure procedure in `SKILL.md`, including permission refusals.

Invoke the installed executable directly on `PATH`. If it is unavailable,
ask the user to run the repository's `scripts\Configure-Pc.ps1`; do not install
or upgrade tools during a skill run.
