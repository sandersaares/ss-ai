# In-session audit

Use this procedure only when the user selects "review findings one by one in this session" in phase 7 or resumes that mode. It changes presentation, not finding eligibility, approval authority, feedback handling, or publication. Follow phase 7's shared feedback and decision rules throughout.

Apply phase 7's PR reminder in both session text and the question UI for every per-finding and final merge question.

# Stage 1: prepare the presentation

Selecting this mode is not approval or feedback and writes no decision state. Keep the audited capture and destination unchanged during the presentation. In the existing prose checkpoint, retain the selected mode, the absolute audited capture and rendered-file paths, whether the overview has been shown, and whether this audit still needs its final merge decision. A new or returned audit starts a fresh presentation; an interruption preserves the existing presentation's progress instead of resetting it. Mark that question pending before asking about findings when starting an audit whose destination is the pull request and `isAuthoredBySelf` is false, including on a returned audit.

Show `content-overview.md` in ordinary session text once before the findings, retaining its pull request link when applicable. Read the full content using bounded reads if necessary. Do not require the user to open a local file.

# Stage 2: decide each finding

Execute `pwsh-calls/get-next-auto-approval-finding.md` before each iteration and on resumption. Use only its `auditFindingIds`, in their returned order, to select the next undecided finding. The `finding`, `remainingCount`, and `isComplete` fields describe policy classification, not human-audit completion. Do not rerun classification or create a separate queue. Recorded verdicts remain authoritative, so settled and policy-approved findings are not presented again.

For the next identifier:

1. Reload its accumulated feedback and locate its per-finding Markdown using the `findings` entries (`findingId` and `path`) emitted by `pwsh-calls/render-findings.md`. Render again for a new presentation or amendment, or if the mapping needs recovery after interruption, using that recipe with the pending identifiers and the same destination and audited capture arguments as phase 7. Read the entire current file; complete any truncated read before presentation.
2. Emit `**Finding <id>**` followed by the complete rendered Markdown verbatim in an ordinary assistant message in the session text output. Do not summarize, truncate, paraphrase, or substitute a file link. The user must see the exact destination-specific text before the question tool opens.
3. Invoke the user-question tool for that finding alone, asking "Approve finding <id>?" with `Approve` as the explicit choice and the tool's built-in Other/free-form response for rejection, clarification, or amendment. This is the `Approve/Other` interaction; do not add a duplicate `Other` choice when the tool already provides it. Wait for the user's direct answer before continuing.
4. `Approve` means `approved` for only the displayed finding, never other findings or the merge. Persist the verdict with `pwsh-calls/record-finding-decision.md` before selecting another finding. For Other/free-form input, preserve verbatim feedback with `pwsh-calls/record-audit-feedback.md` and apply phase 7's rules. Record an explicit rejection as `rejected` with the same decision command before advancing; a clarification or amendment leaves the finding undecided. Redisplay the complete current text and ask about the same finding again after clarification or amendment. Other without an actual instruction requires a follow-up question, not an inferred verdict or a feedback entry containing only the choice label.

An unanswered question pauses the workflow without a new verdict or advancement. Preserve earlier decisions and the pending final merge question in the checkpoint. After all pending identifiers have decisions, continue below; an empty audit list alone does not settle a pending merge question.

# Stage 3: finish the audit

When the destination is the pull request and `isAuthoredBySelf` is false, ask a separate final question offering "approve the pull request merge" and "do not approve the pull request merge", even if the user rejected every finding. This records a reviewer's approval, not an instruction to merge the pull request. A returned audit requires this fresh answer rather than inheriting an earlier merge decision. On resumption, ask it if still pending even when `auditFindingIds` is empty.

Apply phase 7's user-decision and merge-recording rules to the direct answer, binding it to the audited capture. Do not write merge state for an unanswered question. Clear the pending question in the checkpoint only after the answer is recorded. Do not offer merge approval for self-authored pull requests or other destinations; a completed self-authored pull request audit records `DoNotApprove` through the shared rules.

Return to phase 7's approved-set construction only after every finding and any applicable final merge question are settled. Keep the normal phase 8 freshness check before publication; the walkthrough does not publish individual findings as the user approves them.
