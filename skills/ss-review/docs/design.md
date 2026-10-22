# ss-review design

## Purpose

`ss-review` turns a broad code-review request into trustworthy decision support.
Its value is not the volume of analysis it performs, but the confidence with
which a maintainer can understand, judge, and act on the resulting
recommendations.

The skill is designed to provide:

- omission-resistant coverage of the requested content;
- recommendations grounded in evidence rather than reviewer intuition;
- a coherent set of independently actionable findings;
- meaningful user control over what is approved and published; and
- delivery that preserves both the approved meaning and its relationship to
  the reviewed content.

This document defines those user-visible goals and the tenets behind them.
`SKILL.md` defines the workflow that realizes them. The implementation guide
describes the internal abstractions that keep the workflow dependable.

## Review vocabulary

The **reviewed content** is the exact set of files or changes the user asks the
skill to examine.

A **focus area** is a coherent review concern derived from applicable guidance.
Its guidelines establish finding eligibility; their rationale is explained to
the recipient rather than imposed as private authority.

A **review worker** assesses all reviewed content for applicability to one focus
area and examines content not explicitly excluded by its scope. It is the unit
of independent analysis. A generated focus area receives a worker unless its
complete scope provably excludes every reviewed file.

A **file group** is a bounded set of related reviewed files. It is a worker's
unit of iteration, completion journaling, and recovery, not a separate worker or findings
artifact.

A **finding** is the smallest recommendation that a recipient can understand,
judge, act on, and close independently. It carries its own evidence, reasoning,
consequence, remedy, and location.

An **approved review** is the set of findings whose exact claims have passed the
audit. Approval belongs to those claims, not to an intermediate document or a
particular publication service.

A **destination** is where the approved review is delivered: a pull request, a
work tracker, or a Markdown document.

## Interaction ownership

The central accountability tenet is that analysis may be delegated, but user
authority may not.

The primary user-facing session owns the review and every question posed to the
user. Subagents may inspect evidence and propose findings within bounded work.
The auto-approval worker may approve claims only under the defined policy;
workers cannot make user decisions or perform publication. When the workflow
asks a question, only the user's direct answer can resume work that depends on it.

A user-authorized `ss-review-proactor` invocation may select and claim a pull
request before handing it to this skill in the same primary session. This
scheduled entry retains the same human authority and uses the ordinary review
workflow. The proactor owns the claim check before handoff; this skill adds no
claim checks or special reviewer-registration behavior.

In proactor loop mode, this skill still completes one review at a time before
returning control to the proactor. A user instruction that the current review
is the last one disables future cycles without interrupting the review or
answering any pending question. Each handoff has separate review state.

Automation is permitted only where the review contract already defines the
decision. In particular, the narrow auto-approval categories are policy, not a
substitute for a user who has not answered. This distinction lets the workflow
remove predictable clerical decisions without manufacturing consent at an
unresolved boundary.

## Guidance scope

Workers receive explicit authorization for their bounded task, not for invoking
the orchestration workflow. Shared reading instructions include resolved paths,
document sizes, and a complete bounded reading route from the first call.
Original and replacement reviewers use the same assignment contract and retain
their durable findings and coverage state.

Worker input is selected by responsibility. A structured-output phase receives
the common data rules and its own contract, not the contracts of other phases.
Reviewers receive assessment and completion guidance; grouping and focus-area
preparation receive their corresponding contracts. References and overview
workers produce prose and need no machine-readable data guide. Canonical guides
are maintained once and included verbatim, so reducing context does not weaken
the records or their validation.

Shared guidance may be read in full through bounded slices. This permission
does not extend to canonical PR-context captures. Those captures remain subject
to selective evidence queries, even when a worker has a larger context tier.
Query continuation preserves the selected request and snapshot identity rather
than relying on a worker to reconstruct filters. Changed evidence requires an
explicit restart; errors never authorize a raw dump or silently skipped content.

Guidance retains the scope of its original source when reorganized into focus
areas. File-wide prose and frontmatter, including `applyTo` path globs, apply to
the source's sections together with any enclosing and local restrictions or
explicit exceptions. References inherit the referring instruction's scope as
well as restrictions in the referenced guidance. Scope describes the reviewed
artifact or activity, not merely the fact that an agent is performing a review.

Each generated section declares its effective scope and retains the original
declarations and source attribution. A focus-area file may declare a common scope once
when it applies to all sections; mixed-scope focus-area files retain section-specific
scopes. Similar scopes are useful grouping hints, not a requirement. Grouping
and deduplication preserve applicability rather than broadening it or losing
distinct contexts in which the same guidance applies.

Original source boundaries determine what "this file" means in scope prose.
Flattened user-scoped instruction context is not a reliable source of those
boundaries. Missing restrictions are not inferred from a category title or
filename, and unresolved applicability cannot justify excluding content.

Categorization has a versioned, machine-validated handoff. Agents supply category
and scope judgments; tools copy source text and render focus areas. Malformed
handoffs and uncovered raw guidance must be repaired before assembly. Semantic
scope verification remains necessary: structural validity cannot establish the
meaning of an instruction.

## Review coverage

The coverage tenet is that scope determines what is examined; predicted
relevance does not.

Every focus area accounts for the complete file inventory. Before dispatch,
explicit path-only scopes may exclude files only when every section and
referring alternative excludes them. Semantic, unsupported, missing, or uncertain
scopes remain assigned for review. An entirely excluded focus area has a
validated, completed-empty result without a reviewer. This differs from a
completed review with no findings, incomplete work, or a failed worker.

Workers examine remaining work one file group at a time. Groups follow directory
structure and contain no more than 20 files or
100 000 bytes of content; an oversized file forms a group on its own. These
bounds leave room for guidance and supporting context during analysis.
For each file, workers first assess all guideline sections' effective scopes.
They may skip analysis only when every section clearly excludes the file,
recording the path, controlling scope, and concrete reason. If any section
might apply, workers analyze the requested scope under the applicable sections.
Uncertainty requires investigation rather than exclusion. Excluded files remain
available as supporting context for included files.

Change reviews start with the diff. Workers choose supporting ranges, related
files, or whole-file reads according to the context needed for their analysis;
whole-file reads are optional. Reviews without a diff assess the requested
content through targeted inspection. Structured content with a known schema,
such as JSON, is preferably inspected through read-only queries selecting
relevant records and fields rather than whole-document reads. Coverage means
analyzing the requested scope with enough context, not reading every byte.

Workers run concurrently, with available slots filled as soon as work finishes.
Grouping bounds each analysis and recovery unit, not review coverage.
The same worker can relate observations across groups through concise evidence
pointers and its durable findings.

Independent shared-document reads within one worker assignment may run
concurrently. Their diagnostic counter updates tolerate temporary contention
and preserve every increment. Persistent contention and other I/O failures
remain explicit errors.

Shared workflow records accept overlapping commands within a session. Readers
observe complete records, and accepted concurrent updates preserve each other's
work. Initialization, immutable snapshots, and publication retain their conflict
checks; concurrency does not authorize replacement or duplicate delivery.

Review workers use the default context tier, relying on automatic compaction
and durable group progress to continue beyond a context window. Quality
refinement and synthesis use long context for review-wide analysis. Optional
subagent token budgets are not set. Notes guide automatic compaction toward
preserving instructions, progress, and exact reload paths while discarding
completed-work history and previously read file contents. File contents,
including focus-area guidance, are re-read rather than replaced with summaries.
Compaction timing remains the runtime's responsibility.

Each group is journaled as complete only after every member has analysis of the
requested scope or a recorded explicit scope exclusion and its findings are
durable.
Per-file assessments are schema-defined records maintained by commands, not
freeform checkpoint prose. Completion requires records for every group member.
Even a wholly excluded group has per-file evidence and journaled completion,
including when preparation supplies it without model work. All inventory
entries remain accounted for, including deleted paths and both sides of renames.
Group assessments and completion are persisted atomically as a batch bound to
the assigned inventory snapshots. Identical retries do not change state.
A replacement worker preserves earlier exclusion records, skips completed
groups, and repeats any unfinished group before taking the remaining work. All findings
survive replacement, including those from a partially processed group;
overlapping findings pass through ordinary deduplication. Queue claims alone
are not evidence of completed analysis. Missing or unsupported exclusion records
require targeted reassessment before coverage is accepted.

Machine-readable handoffs have explicit formats and prepared authoring tools.
Missing records and invalid shapes are errors, not successful empty work.
User feedback remains verbatim, and publication reads a validated PR- and
commit-bound merge decision rather than an agent-transcribed approval flag.

The content under review does not, by default, define the standard by which it
is reviewed. Repository-provided agent instructions are excluded from the
review criteria unless the user explicitly authorizes them. This prevents a
change from quietly weakening its own review standard. Repository configuration
may still govern operational facts for which the repository is authoritative,
such as where its work items belong.

The default review concerns the requested change rather than unrelated
pre-existing issues. The user can deliberately widen either the governing
guidance or the issue scope.

Review complements mechanical validation rather than replicating it.
Compilation failures, statically detectable type errors, and other issues
already checked by a compiler, typechecker, or linter are outside review scope.
Those tools establish such facts more cheaply and reliably than a reviewer,
whose apparent discovery may instead reflect misunderstood context. This
boundary applies even when a check is disabled, pending, or failing; the review
neither simulates nor runs it.

Review contributes judgment about behavior, design, contracts, and
maintainability beyond those checks. A concern must stand independently of an
issue already checked by those tools, not merely restate its consequences. The
possibility of writing a custom checker does not by itself make a substantive
concern out of scope.

## Content overview

The interpretation tenet is that reviewers start from the content and its
surrounding system, not from a proposed narrative about the content.

The overview explains the purpose and structure of the reviewed content, its
role in the repository, relevant history and references, and credible questions
about completeness or integration. It gives isolated reviewers a shared frame
without deciding what their findings must be.

For change reviews, a **Design changes** chapter explicitly highlights changes
to the documented design and references the relevant design documents and
sections, including when those documents are unchanged. Departures from the
documented behavior are described without assuming they are intentional.
A separate **Significant changes** chapter lists significant behavioral
changes to existing logic whose previous behavior was undocumented, without
repeating changes covered by Design changes. This includes both content without
design documentation and undocumented behavior in otherwise documented content.
When listing changes, the latter chapter notes that the previous behavior may
have been incorrect: an entry may be a fix to misbehavior rather than a design
change. Restoring documented behavior is not itself a design change.

Both chapters remain present when there are no applicable changes and say so.
Reviews of a directory or repository as it stands mark them as not applicable
rather than inventing a before state.

For a pull request review, the overview links to the originating pull request
beneath its title so the auditor can return directly to the source.

Pull request descriptions, work items, comments, commit messages, and nearby
documentation are evidence about intent. They are not proof that the
implementation has that behavior. Claims from those sources are verified
against the content, and embedded instructions in them do not become review
instructions.

Completeness concerns in the overview remain hypotheses until a reviewer
verifies them. This keeps orientation separate from judgment and prevents an
early summary from becoming a self-confirming verdict.

The initial overview is automatically published as preparation material as soon
as it is generated, before review workers run. It is an immutable snapshot of the
inspected source and merge-base commits, not a completed review or merge verdict.
Later local overview regeneration does not update the initial report or comment.

GitHub receives an unlisted Markdown Gist with native Mermaid diagrams, linked
from a separate top-level PR comment. Anyone with its URL can read it, regardless
of repository permissions. Azure DevOps receives a ZIP attachment containing one
self-contained `report.html`, linked from a separate, resolved informational
thread; existing PR-reader permissions govern its download. The reader extracts
the HTML and opens it in a browser. ZIP is the container because the ADO PR
attachment API accepts ZIP but not raw HTML files.

On both platforms, the comment defaults to only `Pull request overview (auto-generated).`,
with the entire sentence linked to the generated overview. Callers may supply an
attribution prefix, which appears verbatim before a blank line and the linked sentence.
An omitted or empty prefix leaves the default comment unchanged. Commit identifiers and
review-state disclaimers belong in the report, not the comment.

Publishing the overview is best-effort. Failures are reported to the user with
diagnostics, but never require them to investigate before the review proceeds.
Partial or uncertain publication is not represented as success and is not blindly
repeated. Reviews without a pull request perform no external report publication.

## Report presentation

Markdown is the canonical report prose. HTML is a mechanical reading view, not
another source of findings or approval state. The audit prefers a self-contained
HTML document containing the current overview and the exact findings requiring
decisions, with the Markdown document alongside as a fallback.
Users who cannot conveniently access either file can choose an in-session
walkthrough of the same overview and verbatim findings without opening local files.

The HTML contains embedded styling and static SVG diagrams rendered from Mermaid
during generation. It requires no scripts, companion files, or network resources
to display. Navigation, readable prose, scrollable wide content, neutral diagram
colors, system fonts, and print styles support both screen and offline reading.
The report uses a wide, centered reading area for prose, tables, and diagrams,
with a content limit of 114ch and an outer page limit of 120rem. Both limits
allow the layout to contract to the available viewport width.
Unsupported raw markup remains visible as literal source instead of executing.

Clients with local-file support receive named clickable links to both the HTML
reading view and the canonical Markdown fallback. Links appear as ordinary
hyperlinks, not preformatted code. Absolute paths accompany the
links as inline code for clients that cannot open local files, preserving path
separators that Markdown could otherwise interpret as escape syntax. The
question UI refers to the links already presented in session text rather than
repeating local paths. A viewing limitation does not
authorize uploading private audit material. Missing rendering tools require a
user installation prompt. Installation is never implicit; after an explicit
decline or a failed installation, the review uses Markdown and reports the
limitation. Other rendering failures also preserve the canonical Markdown.

Preparation verifies readiness by rendering a small HTML report with a Mermaid
diagram in the same environment used for the review. Installed files alone do
not establish readiness. Setup covers the browser's operating-system libraries,
fonts, and sandbox as well as the command-line tools. System package installation
requires explicit consent; setup does not disable browser or host security.
Incomplete browser-cache recovery requires separate cleanup approval and preserves
unrelated revisions and intentional Puppeteer configuration. A failed approved
installation produces recovery guidance rather than another generic installation prompt.

Report tools recognize Node.js installations managed by nvm even when the
noninteractive PowerShell session cannot find Node.js on `PATH`. They preserve
an active runtime choice and otherwise use the configured nvm default on
POSIX systems, without installing or switching Node.js versions.

## Findings

The finding-quality tenet is that each recommendation must stand on its own.

A finding explains what should improve, how the reviewer reached that
conclusion, why the improvement matters, and what action would address it. It
uses references available to the recipient and identifies the relevant content.
A verified, unjustified departure from an applicable guideline is sufficient
grounds for a finding within the review boundaries. The guideline's value is
assumed rather than independently reconsidered. The recipient need not share
that guidance: the consequence explains the undesirable outcome the guideline
helps avoid, without relying on private authority. Consequences include
clarity, consistency, correct usage, documentation, and maintenance concerns;
quantified harm or a demonstrated runtime defect is not required. Explanations
must not invent consequences or exaggerate their likelihood.

Finding descriptions, reasoning, consequences, and recommended actions support
multiline Markdown, including paragraphs, lists, and fenced code blocks. Reviewers
use this structure to make detailed explanations readable rather than compressing
each field into one paragraph. Section labels stand above their Markdown content.

Multiline finding prose permits tabs and line feeds but no other C0 control
characters. Finding titles permit no C0 control characters. This gives the
structured text and its Markdown rendering one visible interpretation.

Findings are advisory. The same eligibility rules apply to correctness,
security, preventive maintainability, documentation, organization, convention,
and style. Implementation effort, revalidation effort, and recurring
maintenance burden must be proportionate to the consequences avoided. A
disproportionate remedy is simplified; the finding is omitted when no
proportionate, actionable recommendation remains. High cost alone is not a veto
when the consequences warrant it. The finding states material trade-offs; the
maintainer retains responsibility for prioritization. Refinement and synthesis
preserve this policy rather than imposing an additional benefit threshold.

Duplication is judged by root cause and remedy rather than by shared vocabulary
or location. Consolidation preserves every independently affected location.
Synthesis may elevate a pattern into a broader finding only when one coherent
explanation and action account for the underlying observations. This produces a
smaller, more useful review without making minority or unrecognized findings
disappear.

The finding's identity survives refinement, audit, freshness checks, and
publication, so a decision always names the same logical recommendation.

## Audit and approval

The audit recognizes separate sources of approval. User approval binds the exact
claim the user inspected.

Findings needing judgment are presented together by default so the user can
assess the review as a coherent whole. The audit also offers an in-session
walkthrough alongside the applicable whole-document approval choices.
Selecting the walkthrough approves nothing. It shows the overview once, then
each undecided finding's complete rendered text before asking `Approve/Other`.
Approval applies only to that finding; rejection settles it, while clarification
or amendment keeps it open for another decision. Stable identifiers and recorded
verdicts preserve progress across interruptions without repeating settled findings.
Feedback remains verbatim and stays with the finding it concerns.

Before opening the audit question UI, the session text presents the audit
request and links to the audit documents with their absolute paths. For a pull
request review, every audit question repeats the PR number and full title, with
a clickable PR link labelled by that number and title in the preceding session
text. The reminder applies to both presentation modes, resumed or amended
presentations, and the final merge question, regardless of the publication
destination. The user can identify the review without scrolling back, and links
remain accessible independently of hyperlink support in the question UI.
In-session review places the actual finding text before each question instead
of requiring document access. Amended findings use the selected presentation
mode before requesting another decision.

Per-finding approval never grants merge approval. After the walkthrough settles
all findings, an eligible pull request review asks separately for the reviewer's
merge approval, including when every finding was rejected. Self-authored pull
requests and other destinations do not offer that choice. An interruption
preserves a pending merge question even when every finding already has a verdict.
Publication still waits for the complete audit and the freshness check.

Policy approval is a separate phase before the human audit, governed by
`../auto-approval-rules.md`. A dedicated worker classifies each finding under
those rules, recording either policy approval or the need for a user decision.
Uncertain classifications require the user's judgment. Classification is
resumable and never treats an unprocessed finding as approved or rejected.
Review diagnostics distinguish policy-approved findings from user-approved
findings, so automation is visible and is never represented as user consent.

The auto-approval worker does not rewrite findings, manufacture user feedback,
or approve pull request merges. Findings needing renewed approval after a
substantive revision return directly to the human audit.

Rendering does not summarize or reinterpret a finding between audit and
publication. A destination may add location or provenance around the finding,
but the substantive prose remains the prose that was approved.

## Freshness check

The freshness tenet is that approval applies to a claim supported by a specific
body of evidence. It applies when the review is associated with a pull request,
where the content and discussion can move underneath an in-progress review.

After refinement and synthesis, before policy classification or human audit,
the review refreshes source and discussion and retires stale findings. The
presented text and resulting decisions refer to that checked snapshot, which
remains unchanged while awaiting the user.

Immediately before publication, the review again checks both dimensions that can invalidate its
conclusions: the reviewed content and the surrounding discussion. Changes to
the source commit or merge base can change the diff; new, edited, removed, or
resolved discussion can change whether a finding is novel or still actionable.
Each pass compares against the last checked snapshot. Unchanged evidence does
not require repeated semantic work. A publication preflight checks live evidence
again before writing; changed evidence requires revalidation, not a silent
snapshot substitution. Force-pushes follow the same rules as other movement.

Publishing PR-backed findings to a tracker requires the same checked evidence
binding as publishing them to the PR. Choosing another destination does not
permit replacing or omitting the reviewed snapshot.

Diagnostics distinguish reused authorship decisions, newly classified comments,
findings retired before presentation, and findings changed after audit. Human
response intervals are separate from agent execution time.

A finding invalidated by fresh evidence is retired. A strict narrowing that
only removes unsupported scope remains within the approved claim. A new
finding, an expanded claim, or a materially different recommendation returns to
the user. Anchors and impacted locations are attribution metadata. Moving an
otherwise unchanged finding to a valid anchor retains its approval automatically,
even when its previous publication location became invalid. Pure anchor and
impacted-location adjustments do not require another user decision.

Approval of a pull request merge is bound to the reviewed source commit and to
the audit boundary. Source-commit or merge-base movement alone does not require
another merge decision. When no finding needs another audit, the prior merge
decision remains in effect and an existing approval carries to the refreshed
source commit without asking again. This includes a refresh that retires every
finding. Any change that returns findings to the user also requires a fresh
merge decision, whether it comes from code or discussion. Missing or negative
merge decisions never become approval through revalidation.

## Focus-area reuse

Caching is an optimization and must be behaviorally invisible.

Equivalent guidance inputs may reuse a complete generated focus-area set.
Different inputs do not share one, and incomplete or degraded inputs are not
promoted as reusable truth. A cache miss, damaged entry, or unavailable cache
changes performance only; it never reduces coverage.

Preparation quality remains visible in the review diagnostics. This lets the
user distinguish ordinary reuse from degraded instruction expansion without
making cache behavior part of the substantive conclusions.

## Local data

Generated guidance and audit artifacts may contain repository-specific or
user-specific information. They remain in local session or user state. The
publication-ready initial overview is the narrow exception described above;
private guidance, captured discussions, audit findings, feedback, and decision
records are not included in that external report.

## Deterministic discussion capture

The discussion-evidence tenet is that prior feedback must be reproduced, not
retold.

Existing pull request discussion is captured mechanically and verbatim.
Comments are not summarized, truncated, reflowed, or normalized into different
prose. A failed or incomplete retrieval is reported as a failure rather than as
an empty discussion, because silently missing feedback would cause duplicate or
contradictory findings.

The capture includes discussion that a reviewer can act on. Withdrawn comments,
unsubmitted drafts, and service activity with no review content do not become
author feedback. Approval standing and pull request lifecycle decisions also
remain outside the review evidence; they belong to merge administration rather
than to understanding what reviewers said.

## Captured pull request context

Pull request context is the authoritative review record for the content snapshot
and discussion the review observed. Reviewers receive the same evidence
regardless of the hosting platform, and service limitations remain visible
rather than being filled with invented certainty.

One record supports deduplication, contextual follow-ups, freshness checks, and
safe delivery. This keeps every conclusion tied to the discussion and code state
that informed it.

## On-demand evidence reads

Complete capture and bounded agent reading are separate requirements. The
canonical JSON retains all captured evidence for deterministic validation,
comparison, and publication. It is a data store, not a document loaded into a
reviewer's context.

Agents access it through a local query interface: selected metadata,
conversation descriptors, targeted comments, literal text search, and
initial-to-refreshed changes. Every response has a fixed serialized output
ceiling and explicit continuation. Long bodies remain verbatim through
contiguous slices rather than being summarized or silently truncated.
Descriptors and search excerpts locate evidence; they do not establish what a
complete conversation concluded.

Reviewers discover assigned-file discussions, PR-wide top-level comments and
review summaries, and cross-file topic or symbol matches before raising a
finding. File selection is not evidence of absence: prior names, other-file
anchors, resolved or outdated threads, and global decisions may remain relevant.
Relevant conversations include later replies and edits, not just opening
comments. The meaning of that discussion and the author's intent remain matters
of judgment, verified against the reviewed content.

The same reading contract applies to the primary session and every worker,
regardless of context size or phase. Readers select bounded work and retain
identifiers, evidence references, and decisions rather than accumulating page
dumps. Authorship classification and freshness change discovery cover their
complete selected sets in bounded batches. Metadata-only tasks, such as linking
an overview or binding merge approval, request only the needed fields.

The query interface bounds its own output; it is not filesystem access control.
Prompt policy prohibits raw capture reads, alternate-fetch bypasses, and
indiscriminate page accumulation. Programmatic consumers continue to receive
the complete canonical capture, so bounded agent reading does not reduce
publication or validation evidence.

## Authorship classification

Communication policy depends on who composed a comment, not merely on the
account used to post it. The model therefore separates **identity kind** from
**comment authorship**.

Identity kind describes an account as a person, a service, or indeterminate.
Comment authorship describes content as human, agent, or indeterminate. Service
identity and explicit agent markers can prove agent authorship. A person account
cannot prove human authorship because an agent may act through that account.

GitHub reviews that contain only a vote, with no body text or inline comments,
default to human authorship unless the account establishes agent authorship.
Whitespace-only bodies count as empty. This rule also covers dismissed votes.
The workflow does not request human guidance merely because such a review has
no text to evaluate.

The review states only classifications it can support. Unresolved authorship is
examined rather than guessed, and every conclusion retains its basis so later
consumers can understand why communication policy treated the comment as it
did.

Semantic decisions are separate from immutable captured evidence. Current
provider-account and marker checks take precedence; a prior semantic decision
is reusable only for the same provider, repository, pull request, comment,
exact body and attribution inputs, and classification policy/schema.
Removed comments remain baseline evidence, not members of the current capture.

The classifier reads the verbatim text and attribution of every remaining indeterminate
comment through the bounded query interface. A metadata-only guess is not a
classification. Only that comment's body and attribution inform authorship.
A reading batch observes an unchanged effective capture; decisions are
applied only after its reads finish. Subsequent batches select the remaining
indeterminate comments afresh, without continuing an obsolete cursor.

## Reviewer registration

Registration is a visible signal that review is in progress, not a claim about
the pull request's quality.

It is additive and idempotent: existing reviewers and their state are
preserved, and repeated registration changes nothing. A platform that does not
allow an author to request themselves as reviewer is treated as a successful
no-op rather than as a failed review.

## Publication

The publication tenet is that a destination receives the approved review
without semantic drift or accidental duplication.

An explicit request for tracked work selects the tracker even when a pull
request exists. Otherwise, a pull request is the natural destination when one
exists; without one, the user chooses between tracked work and a Markdown
document. Filing tracker items is never implicit. The destination is settled
before audit rendering so the user approves the substantive form that
destination will receive.

Publication is mechanical rather than interpretive. It renders each approved
finding in full, places it using validated destination metadata, and reads back
what the service can attest. A summary reports the review outcome and execution
diagnostics without restating the findings.

Remote writes are not assumed to be transactional. An ambiguous or partial run
requires reconciliation instead of an automatic retry, because duplicate
review content is more harmful than a paused delivery.

Explicit rate-limit rejections are definite refusals, not uncertain writes.
Reads and writes wait and retry until accepted, using exponential backoff when
the service provides no recovery time. Other failures retain their existing
recovery rules.

## Review publication

A pull request review keeps findings close to the changed content. New concerns
are placed on an appropriate changed file; follow-ups remain in the discussion
they answer. If an exact line anchor is unavailable but the claim still stands,
the finding becomes a file-level comment rather than being moved to an
unrelated line or discarded.

The review records no approval or request-for-changes verdict by default. When
the user explicitly approves the merge, that verdict is published with the
review. A reviewer does not approve their own pull request. On Azure DevOps,
approval preserves the account's existing required or optional reviewer status;
an account not yet registered as a reviewer is added as optional.

A pull request that closed while the review was running creates a new user
decision: deliberately publish to the closed discussion or deliver the findings
as tracked work. The workflow does not silently choose a less visible
destination.

## Tracker publication

A tracker turns recommendations into independently managed work. Each finding
becomes its own item so it can be scheduled, assigned, and closed without
coupling unrelated actions.

Each item is self-contained and carries the location and provenance a reader
needs without an open diff. The tracker is selected independently of the review
source, allowing a review from one platform to be filed on another. Repository
publication settings are proposed to the user, who confirms the destination and
required tracker metadata before anything is created.

Tracker items are visible as soon as they are created and cannot be staged as a
single review. Delivery reports only items whose stored content has been checked
against the approved review. If a service rewrite is neither clearly equivalent
nor clearly different, the user decides whether the stored form is acceptable.
A partial run can leave real work items behind and is reconciled rather than
replayed as though nothing happened.

Azure DevOps receives work-item bodies as Markdown. Literal angle brackets in
review prose remain visible as text rather than being interpreted as raw HTML;
the adapter applies the destination-specific representation needed to preserve
them.

## Document delivery

A Markdown destination preserves the approved review as a combined local
document for manual delivery. It performs no remote write and creates no work
items. The user receives the persisted document rather than a conversational
summary, so the same approved finding text remains available outside the
session.

## Learning

Audit feedback is evidence about the review system as well as about an
individual finding.

Repeated rejection, removal, confusion, or missing guidance can motivate a
focused improvement to the relevant instructions. Such a change is proposed in
the repository that owns the guidance and remains subject to the user's approval
and the ordinary contribution process. Learning improves future reviews without
quietly rewriting the standard during the review that exposed the issue.
