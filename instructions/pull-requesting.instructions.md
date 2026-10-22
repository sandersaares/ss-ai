# Pull request description

Applies to: agent workflows.

The pull request description should describe:

- What is the motivation for the pull request (a description of the task or problem).
- What are the behavioral or substantive changes this pull request delivers.

The above is expected from every pull request.

The following are desired from pull requests when you are authoring them but do not nitpick if they are missing from pull requests authored by others:
- A picture is worth a thousand words - if a diagram is helpful to illustrate structural design or implementation changes, draw an ASCII diagram in the PR description.
- If reviewers may require additional context from outside the repo to understand the content of the pull request, provide the necessary links or references.
- Do NOT emit any listing of changed files, validation steps or other trivia - we describe the submitted result, not the work we did to get there.

# Pull request title

Applies to: agent workflows.

When authoring a pull request in a repo with multiple libraries/apps/packages, the title should identify the affected library/app/package, unless it is a wide-reaching change that affects many/most parts of the repo.

# Addressing pull request comments

Applies to: agent workflows.

When you are instructed to address pull request comments, you are by default authorized to fix breakages and make straightforward mechanical
improvements/clarifications autonomously as long as they do not contradict any explicit or context-provided instructions. Assume that every
instance of a problem pointed out in a pull request may also exist in other instances not mentioned. Sweep the codebase for potential other
equivalent issues and address them all (either in the same pull request, if in scope, or by adding backlog items to address out-of-scope
findings separately).

For feedback that requires design changes to address or feedback that requests changes in user-visible behavior (e.g. in app functionality
or public API), you must always ask the user for approval. Do not accept any skipped questions as signs of approval - if the user is
not present to provide approval, do not proceed with the changes.

The Copilot reviewer on GitHub sometimes posts comments marked as low-confidence. Treat those the same as any other comments and address
them normally, making your own judgement on whether they are relevant or not.

Obey the communication policy defined in your instructions when addressing comments.

Do not assume that all comments from an agent-empowered user are agent comments - the same user may mix human and agent comments.
Look for an agent tag/identifier/name on each comment to understand whether it is an agent-authored comment or a human-authored comment.

Do not ignore comments because they appear to be from a "system" user - those can be legitimate AI reviewers.

Search pull request discussion history (both threads and top-level discussion) for duplicates before addressing a comment.
If a comment has already been addressed in a previous commit, do not duplicate the effort; simply reference the previous resolution.

After pushing a commit that addresses some pull request comment:

* Post a reply in the thread to summarize how you addressed it. Do not offer follow-up options or make requests/suggestions that try to elaborate and discuss further - if you need input, ask your user instead of the reviewer. Assume your reply is the end of the thread and no further discussion will occur.
* Mark the comment thread as resolved.

If you asked the user for input to decide on a course of action, ensure that any rationale provided by the user is included in the reply to help the reviewer also understand why the specific decision was made.

# GitHub review summary is also a comment

Applies to: agent workflows.

A review body is posted in GitHub as a separate item in the discussion, which may need a separate query to fetch. Make sure you read
the review bodies, as well, whenever you look for comments in a pull request. Treat the review body the same as regular discussion comments
posted in a pull request discussion.
