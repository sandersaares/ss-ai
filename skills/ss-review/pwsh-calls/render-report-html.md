```text
ss-ai workflow render-review-html --path "{{MARKDOWN_PATH}}" --output-path "{{HTML_PATH}}" --skill-root "{{SKILL_ROOT}}" --tools-directory "{{TOOLS_DIRECTORY}}" --title-path "{{TITLE_PATH}}"
```

| Placeholder | Description |
| --- | --- |
| `SKILL_ROOT` | Absolute directory containing the active `SKILL.md`. |
| `MARKDOWN_PATH` | Absolute path of the canonical Markdown document. For audit, use the emitted `auditDocumentPath`. |
| `HTML_PATH` | Absolute sibling HTML path, such as the session's `audit-findings.html`. |
| `TOOLS_DIRECTORY` | Absolute shared user-local tools directory from `check-report-tools.md`. |
| `TITLE_PATH` | Absolute path to a UTF-8 file containing the reader-facing title, such as `Review audit`. |

Write the title file in session state. The output directory must be writable. The native process
keeps the caller's working directory.

The command emits JSON containing `sourcePath`, `sourceHash`, `htmlPath`, `htmlUri`, `htmlHash`,
and `diagramCount`. Present `htmlUri` as a named Markdown hyperlink where the client supports
local-file links, enclosing the destination in angle brackets so paths containing parentheses
remain clickable:

[Review audit (HTML)](<file:///path/to/audit-findings.html>)

Replace the example destination with the emitted URI. Write links directly as Markdown,
not inside backticks or a fenced or indented code block, and do not quote the destination
URI or path inside a link. Retain the absolute HTML path separately as inline code. Inline code
preserves Windows backslashes that Markdown would otherwise interpret as escapes, including the
separator before `.copilot`. Also present a named link to the canonical Markdown document's URI
(for audit, the `auditDocumentUri` emitted by `render-findings.md`) and retain its absolute path
as inline code. In clients with a browser canvas, open the file URI there
when local-file viewing is supported. Do not start a server or upload the audit to preview it.

If tools are missing or fail to launch, follow `check-report-tools.md` and its conditional platform
setup guide. Keep the full diagnostic, including missing shared-library or sandbox errors.
Respect the installation decision already recorded for this run; request permission only for
setup not already decided. Retry only after approved setup passes the HTML/Mermaid probe.
If installation is declined or fails, use the Markdown fallback explicitly.

Other rendering failures also leave Markdown available: notify the user and present that document.
Never present a pre-existing HTML file as current after a failed render or changed Markdown source.
Rendering is presentation only; findings and approval state still come from the structured records.
