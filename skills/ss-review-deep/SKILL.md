---
name: ss-review-deep
description: Code review orchestrator for performing thorough high-quality deep code reviews. ONLY use this when the skill is explicitly invoked (by name) by the user. This skill is NOT meant for automatic invocation.
---

This skill is a wrapper around the `/ss-review` skill for long-running extra deep reviews on an existing codebase. If the user requested a review of a pull request, notify them that they probably meant to use `/ss-review` instead and ask for confirmation.

To execute this skill, simply forward the user's review request to the `/ss-review` skill, with the following addendums:

* Using `AGENTS.md` and other agent instructions files from the reviewed content is AUTHORIZED and EXPECTED as part of the instruction set preparation stage (as an exception to the default no-in-repo-instructions rule).
* It is expected that this review may take significant time due to the depth and thoroughness of the analysis (possibly several days across many hundreds of subagent executions).
