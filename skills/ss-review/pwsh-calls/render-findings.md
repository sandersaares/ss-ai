```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/ConvertTo-FindingMarkdown.ps1" `
  -Path "{{SESSION_STATE}}/findings.json" `
  -OutputDirectory "{{SESSION_STATE}}/rendered" `
  -FindingId "{{IMPORTANT_FINDING_IDS}}" `
  -AuditDocumentPath "{{SESSION_STATE}}/audit-findings.md" `
  -OverviewPath "{{SESSION_STATE}}/content-overview.md"
```

Renders the findings the audit must decide: one file per finding under `rendered/`, and one
`audit-findings.md` collecting them for document review. In-session review displays each
per-finding file verbatim, using the emitted `findings` entries' `findingId` and absolute `path`
rather than reconstructing prose or enumerating old files in the output directory. `-AuditDocumentPath`
honours `-FindingId`, so both cover only the named findings - policy-approved findings
are not rendered and not presented. `-OverviewPath` prepends the content overview
to that document so the auditor has it before the findings. The emitted JSON reports
`auditDocumentPath` and `auditDocumentUri`. Present `auditDocumentUri` as a named Markdown link
whose destination is enclosed in angle brackets:

[Review audit (Markdown)](<file:///path/to/audit-findings.md>)

Replace the example destination with the emitted URI. Write the link directly as Markdown,
not inside backticks or a fenced or indented code block, and do not quote the destination
URI or path inside the link. Retain `auditDocumentPath` separately as an inline-code fallback.
Inline code preserves Windows backslashes that Markdown would otherwise interpret as escapes,
including the separator before `.copilot`.
Render that exact document with `render-report-html.md` and present HTML first, retaining this
Markdown path alongside it as the canonical fallback. Skip this selection command when there are
no important findings; an empty identifier list selects every finding.

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `SESSION_STATE` | Session state directory for this review. |
| `IMPORTANT_FINDING_IDS` | `auditFindingIds` from `get-next-auto-approval-finding.md`, joined as one comma separated value. |
