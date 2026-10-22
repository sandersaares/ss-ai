#Requires -Version 7.0

# Renders findings as the Markdown a reader sees
# (docs/implementation.md, "Finding rendering").
#
# This is the only place in the skill that turns a finding into Markdown, and nothing ever turns
# Markdown back into a finding. A finding travels as a structured record from the reviewer that
# found it to the moment it is published, and the record is what every phase reasons about; prose is
# produced once, at the edge. A second renderer would let two publication paths disagree about what
# a finding says, and a parser would make the prose authoritative again, which is exactly what the
# structured record exists to prevent.
#
# The section order is fixed and an empty section is dropped rather than rendered as a heading with
# nothing under it, so the same finding produces the same bytes on every run and a reader meets the
# same shape in every comment.
#
# Two published forms exist, a pull request comment and a tracker item, and both are assembled from
# one block builder. They differ in which blocks surround the prose and in where the title goes; the
# prose itself is emitted identically, so no destination can restate a finding differently.
#
# Functions from lib\JsonValue.ps1 are called directly, along with Resolve-PullRequestPath from
# lib\PullRequestModel.ps1; the executables dot-source all three.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Marks every body this skill publishes as written by an agent rather than by the operator whose
# account posts it (instructions/global.instructions.md, "Communication policy").
# It is applied by the renderer rather than by each publication path, so no path can omit it.
$script:FindingMarkdownAuthorshipPrefix = '[Copilot speaking]'

# The labels the prose sections carry, in the order they are rendered. The order and the wording
# match the finding structure reviewers are given in review.md, which is the shape the audience of
# these comments already reads.
$script:FindingMarkdownProseSection = @(
    @{ field = 'reasoning'; label = 'Reproducible reasoning' }
    @{ field = 'consequence'; label = 'Consequence' }
    @{ field = 'recommendedAction'; label = 'Recommended action' }
)

# Separates findings in the combined document. A rule is used rather than a heading level, because
# each finding already opens with its title as a top-level heading.
$script:FindingMarkdownSeparator = '---'

# Renders one finding as a complete comment body.
#
# The finding is the stored record, as an artifact holds it. Prose fields are emitted as the author
# wrote them, because a finding's prose is Markdown already and reformatting it would alter what the
# reviewer said.
function ConvertTo-FindingMarkdownText {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding
    )

    @(
        $script:FindingMarkdownAuthorshipPrefix
        ConvertTo-FindingBodyMarkdown -Finding $Finding
    ) -join "`n`n"
}

# Renders one finding as a tracker item: a title field and a body (docs/design.md, "Publication").
#
# A tracker item says what a pull request comment says, in the form a tracker takes it. The title
# becomes a field rather than a heading, because a tracker holds it separately and would otherwise
# show a reader a body opening with a heading that repeats the title above it. The anchor becomes a
# rendered location, because the item hangs off nothing and a reader who is not looking at a diff
# needs to be told where the code is. A follow-up links the comment it answers instead of replying
# into its thread, and the item names the pull request it came from, because a tracker item outlives
# the review that produced it and a reader meeting it later has no other way back.
#
# `-FollowUpUrl` is the link to the comment the finding answers, and `-PullRequestUrl` the link to
# the pull request that was reviewed. A caller that has neither passes neither, which is the case of
# a review of content that is not a pull request.
function ConvertTo-FindingTrackerItem {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding,

        [string] $FollowUpUrl,

        [string] $PullRequestUrl
    )

    $blocks = Get-FindingMarkdownBlock -Finding $Finding -Destination 'tracker' -FollowUpUrl $FollowUpUrl -PullRequestUrl $PullRequestUrl

    [ordered]@{
        title = [string] (Get-JsonProperty -Node $Finding -Name 'title')
        body  = (@($script:FindingMarkdownAuthorshipPrefix) + $blocks) -join "`n`n"
    }
}

# Renders one finding without the authorship prefix, for a caller that carries its own.
#
# The prefix identifies the author of a post, so it belongs once per post rather than once per
# finding: a document collecting many findings states its author at the top and would otherwise
# repeat the same line between every pair of findings.
function ConvertTo-FindingBodyMarkdown {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding
    )

    (Get-FindingMarkdownBlock -Finding $Finding -Destination 'pullRequest') -join "`n`n"
}

# Builds the blocks a finding renders to, in the order they are read.
#
# Both published forms are built here rather than each by its own renderer, so that a pull request
# comment and a tracker item cannot come to disagree about what a finding says. They differ only in
# which blocks surround the prose: the prose fields themselves are emitted identically, as the
# author wrote them, whichever destination asked for them.
#
# The blocks are returned rather than joined, because the two callers state their author in
# different places and only the caller knows whether a prefix precedes them.
function Get-FindingMarkdownBlock {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding,

        [Parameter(Mandatory)]
        [ValidateSet('pullRequest', 'tracker')]
        [string] $Destination,

        [string] $FollowUpUrl,

        [string] $PullRequestUrl
    )

    $blocks = [System.Collections.Generic.List[string]]::new()

    # A pull request comment carries its title as a heading, because the comment is the finding and
    # nothing above it says what the finding is. A tracker item carries the same text in its title
    # field instead, so repeating it here would open every item with a heading duplicating the line
    # the reader just read.
    if ($Destination -ceq 'pullRequest') {
        $title = [string] (Get-JsonProperty -Node $Finding -Name 'title')
        if (-not [string]::IsNullOrWhiteSpace($title)) {
            $blocks.Add("# $title")
        }
    }

    $description = [string] (Get-JsonProperty -Node $Finding -Name 'description')
    if (-not [string]::IsNullOrWhiteSpace($description)) {
        $blocks.Add($description)
    }

    # The location sits directly after the description, so a reader of a tracker item learns where
    # the code is before being told why it matters. A pull request comment needs no such block: it is
    # posted on the code the anchor names.
    if ($Destination -ceq 'tracker') {
        $anchor = Get-JsonProperty -Node $Finding -Name 'anchor'
        if ($null -ne $anchor) {
            $blocks.Add("**Location**: ``$(Get-FindingLocationText -Location $anchor)``")
        }
    }

    foreach ($section in $script:FindingMarkdownProseSection) {
        $value = [string] (Get-JsonProperty -Node $Finding -Name $section['field'])
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        # A field may begin with a Markdown block, so the label must not share its first line
        # (docs/implementation.md, "Finding rendering").
        $blocks.Add("**$($section['label'])**:`n`n$value")
    }

    $references = Get-JsonArrayProperty -Node $Finding -Name 'references'
    if ($references.Count -gt 0) {
        $items = [System.Collections.Generic.List[string]]::new()
        foreach ($reference in $references) {
            $items.Add("- $(Get-FindingReferenceMarkdown -Reference $reference)")
        }
        # The list is separated from its label by a blank line, because a list that immediately
        # follows a paragraph line is rendered as a list by some Markdown implementations and as
        # part of the paragraph by others, and these bodies are published on two platforms.
        $blocks.Add("**References**:`n`n$($items -join "`n")")
    }

    $locations = Get-JsonArrayProperty -Node $Finding -Name 'impactedLocations'
    if ($locations.Count -gt 0) {
        $items = [System.Collections.Generic.List[string]]::new()
        foreach ($location in $locations) {
            $items.Add("- ``$(Get-FindingLocationText -Location $location)``")
        }
        $blocks.Add("**Impacted locations**:`n`n$($items -join "`n")")
    }

    # Provenance closes the item rather than opening it, because it tells the reader where the
    # finding came from rather than what it says. A pull request comment states neither: it is
    # already in the discussion it would be pointing at.
    if ($Destination -ceq 'tracker') {
        if (-not [string]::IsNullOrWhiteSpace($FollowUpUrl)) {
            $blocks.Add("**Follows up on**: $FollowUpUrl")
        }

        if (-not [string]::IsNullOrWhiteSpace($PullRequestUrl)) {
            $blocks.Add("**Reviewed in**: $PullRequestUrl")
        }
    }

    , $blocks.ToArray()
}

# Renders the summary that accompanies a published review (SKILL.md, "Phase 9: publication").
#
# The summary states how much was published and how the review was prepared, and deliberately says
# nothing about what the findings contain: each finding is published in full where the code is, and
# a summary that restated them would give the reader two accounts to reconcile.
#
# `-Diagnostics` is the focus-area cache diagnostics record, as Get-FocusAreaCacheDiagnostics reads
# it. A caller that has none passes nothing, which reports a miss, because a session that recorded
# no restore reused nothing.
function New-ReviewSummaryMarkdown {
    param(
        [AllowEmptyCollection()]
        [object[]] $Finding = @(),

        [ValidateRange(0, [int]::MaxValue)]
        [int] $FollowUpCount = 0,

        [System.Collections.IDictionary] $Diagnostics = @{},

        [Parameter(Mandatory)]
        [ValidateSet('gitHub', 'azureDevOps')]
        [string] $Platform
    )

    $leadText = if ($Finding.Count -eq 0) {
        # A pull request review reaches publication with no findings only to record the reviewer's
        # approval of the merge (SKILL.md, "Phase 9: publication"). The verdict is carried by the
        # platform's own review signal, so the summary states the absence of findings rather than a
        # count of zero.
        'This review raises no findings.'
    } else {
        $findingText = if ($Finding.Count -eq 1) { '1 finding' } else { "$($Finding.Count) findings" }
        $followUpText = if ($FollowUpCount -eq 0) {
            'No finding follows up on an existing discussion thread.'
        } elseif ($FollowUpCount -eq 1) {
            'One finding follows up on an existing discussion thread.'
        } else {
            "$FollowUpCount findings follow up on existing discussion threads."
        }
        "Published $findingText. $followUpText"
    }

    $table = [System.Collections.Generic.List[string]]::new()
    $table.Add('| Diagnostic | Value |')
    $table.Add('|------------|-------|')

    # Reported on every summary, because the reader's first question about a review that took an
    # unusual amount of time to start is whether focus-area preparation ran (docs/design.md,
    # "Focus-area reuse").
    $cache = [string] (Get-JsonProperty -Node $Diagnostics -Name 'cache')
    $table.Add("| Cache | $(if ([string]::Equals($cache, 'Hit', [StringComparison]::OrdinalIgnoreCase)) { 'Hit' } else { 'Miss' }) |")

    # Reported only when expansion was degraded. Complete expansion is the ordinary case and stating
    # it on every review would train the reader to skip the row that matters.
    $inputState = [string] (Get-JsonProperty -Node $Diagnostics -Name 'inputState')
    if ([string]::Equals($inputState, 'Degraded', [StringComparison]::OrdinalIgnoreCase)) {
        $table.Add('| Instruction expansion | Degraded |')
    }

    $tableText = $table -join "`n"

    # GitHub collapses the diagnostics so they do not compete with the review itself; Azure DevOps
    # renders collapsible sections inconsistently, so the same table is inlined there rather than
    # hidden behind markup that may reach the reader as literal HTML.
    $diagnosticsText = if ($Platform -ceq 'gitHub') {
        "<details><summary>See diagnostics</summary>`n`n$tableText`n`n</details>"
    } else {
        $tableText
    }

    @(
        $script:FindingMarkdownAuthorshipPrefix
        $leadText
        $diagnosticsText
    ) -join "`n`n"
}

# Renders every finding into one document, for a review whose target takes no comments.
#
# Each finding carries the same text a pull request comment would, so a user delivering the document
# by hand delivers what publication would have posted. The document states its author once, at the
# top, in the place a reader of a document looks for it.
function ConvertTo-FindingMarkdownDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding
    )

    $ordered = Get-FindingPresentationOrder -Finding $Finding
    if ($ordered.Count -eq 0) { return '' }

    $bodies = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $ordered) {
        $bodies.Add((ConvertTo-FindingBodyMarkdown -Finding $entry))
    }

    @(
        $script:FindingMarkdownAuthorshipPrefix
        Join-FindingMarkdownDocument -Text $bodies.ToArray()
    ) -join "`n`n"
}

# Joins rendered findings into one document.
#
# The separator is applied in one place, so a document collecting comment bodies and a document
# collecting tracker items divide their findings the same way and a reader who has seen one knows
# where a finding ends in the other.
function Join-FindingMarkdownDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]] $Text
    )

    $Text -join "`n`n$($script:FindingMarkdownSeparator)`n`n"
}

# Composes the path one finding's rendered body is written to.
#
# The filename is the finding identifier, which is what the audit and the user quote a finding by, so
# a re-rendered finding replaces its own file rather than accumulating alongside it.
function Get-FindingMarkdownPath {
    param(
        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [Parameter(Mandatory)]
        [string] $FindingId
    )

    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory "$FindingId.md")
}

# Orders findings for a reader meeting them all at once.
#
# Findings produced by synthesis come first, because they describe the review's overall picture and
# a reader who meets them after the individual findings has already formed one. Order within each
# group is the order the artifact holds, which is the order the findings were consolidated in, so
# the document is stable across runs over the same artifact.
function Get-FindingPresentationOrder {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding
    )

    $synthesis = [System.Collections.Generic.List[object]]::new()
    $reviewer = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $Finding) {
        if ([string] (Get-JsonProperty -Node $entry -Name 'origin') -ceq 'synthesis') {
            $synthesis.Add($entry)
        } else {
            $reviewer.Add($entry)
        }
    }

    , @($synthesis.ToArray() + $reviewer.ToArray())
}

# Renders one reference, as a link where the author supplied a target.
#
# A reference without a URL is a pointer into the repository under review, which the reader opens by
# path rather than by following a link, so it is rendered as the author wrote it.
function Get-FindingReferenceMarkdown {
    param(
        [Parameter(Mandatory)]
        $Reference
    )

    $text = [string] (Get-JsonProperty -Node $Reference -Name 'text')
    $url = [string] (Get-JsonProperty -Node $Reference -Name 'url')
    if ([string]::IsNullOrWhiteSpace($url)) { return $text }

    "[$text]($url)"
}

# Renders one location as the path-and-line form a reader can paste into an editor.
#
# A location naming no line is about the file as a whole and is rendered as the bare path, rather
# than as a range covering the whole file, because the finding makes no claim about which lines
# matter.
function Get-FindingLocationText {
    param(
        [Parameter(Mandatory)]
        $Location
    )

    $path = [string] (Get-JsonProperty -Node $Location -Name 'path')
    $startLine = Get-JsonProperty -Node $Location -Name 'startLine'
    if ($null -eq $startLine) { return $path }

    $endLine = Get-JsonProperty -Node $Location -Name 'endLine' -Default $startLine
    if ([long] $endLine -le [long] $startLine) { return "${path}:$startLine" }

    "${path}:$startLine-$endLine"
}
