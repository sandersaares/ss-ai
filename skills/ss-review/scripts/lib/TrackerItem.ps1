#Requires -Version 7.0

# Turns approved findings into the items a tracker is asked to create (docs/design.md,
# "Publication"; docs/implementation.md, "Tracker publication").
#
# A tracker item is one finding, filed where work is tracked rather than posted where the code is.
# The rendering itself belongs to lib\FindingMarkdown.ps1, which is the only place a finding becomes
# prose; what belongs here is everything a tracker item needs that a pull request comment does not:
# the links back to the review it came from, and the order the items are created in.
#
# A tracker item is a standalone record. Nobody reading it is looking at a diff, and the pull request
# it came from may be closed by the time it is read, so the item states where the code is and where
# the discussion was. Those links come from the captured discussion, which is the only artifact that
# holds them, and a review of content that is not a pull request has none, so every link is optional
# and its absence costs a link rather than the item.
#
# Functions from lib\JsonValue.ps1 and lib\FindingMarkdown.ps1 are called directly; the executables
# dot-source all of them.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Labels the title in a rendered tracker item. A tracker holds the title in a field of its own, so
# the rendering states it as a labelled line rather than as a heading, and a user approving the text
# sees both fields as they will be filed.
$script:TrackerItemTitleLabel = 'Title:'

# Renders one finding as the text a user approves for a destination.
#
# The audit presents the form the findings will be published in, so a user who approves a body
# approves what a reader will receive rather than an approximation of it. A tracker rendering states
# the title on a labelled line of its own, because the tracker holds it in a separate field and a
# body alone would show the user only half of what is filed.
function ConvertTo-FindingDestinationText {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding,

        [Parameter(Mandatory)]
        [ValidateSet('pullRequest', 'tracker')]
        [string] $Destination,

        [System.Collections.IDictionary] $Context
    )

    if ($Destination -ceq 'pullRequest') {
        return ConvertTo-FindingMarkdownText -Finding $Finding
    }

    $item = New-TrackerItem -Finding $Finding -Context $Context

    @(
        "$($script:TrackerItemTitleLabel) $($item['title'])"
        $item['body']
    ) -join "`n`n"
}

# Renders every finding into one document for a destination.
#
# A document of tracker items repeats each item's authorship prefix, where a document of comment
# bodies states it once: each item is a post of its own, and a reader meeting one outside the
# document would otherwise meet a body claiming no author.
function ConvertTo-FindingDestinationDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [ValidateSet('pullRequest', 'tracker')]
        [string] $Destination,

        [System.Collections.IDictionary] $Context
    )

    if ($Destination -ceq 'pullRequest') {
        return ConvertTo-FindingMarkdownDocument -Finding $Finding
    }

    $ordered = Get-FindingPresentationOrder -Finding $Finding
    if ($ordered.Count -eq 0) { return '' }

    $texts = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $ordered) {
        $texts.Add((ConvertTo-FindingDestinationText -Finding $entry -Destination 'tracker' -Context $Context))
    }

    Join-FindingMarkdownDocument -Text $texts.ToArray()
}

# Renders the findings the audit must decide into one document (SKILL.md, "Phase 7: audit").
#
# Document-mode audits collect findings so the user can read them together and answer once.
# In-session audits use the individual destination texts. Each finding is labelled with its stable
# identifier, because the user's reply names a specific finding by that identifier. The
# label is a navigation handle around the finding text rather than part of it: the body a finding
# renders to here is the body it is published with, so approving the document approves what a reader
# receives.
#
# The destination is honoured, because the audit presents the form a finding is published in: a pull
# request comment or a tracker item. `-Context` supplies the links a tracker item carries back to
# the review, and is nothing for a pull request or for content that is not a pull request.
function ConvertTo-FindingAuditDocument {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [Parameter(Mandatory)]
        [ValidateSet('pullRequest', 'tracker')]
        [string] $Destination,

        [System.Collections.IDictionary] $Context
    )

    $ordered = Get-FindingPresentationOrder -Finding $Finding
    if ($ordered.Count -eq 0) { return '' }

    $sections = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $ordered) {
        $id = [string] (Get-JsonProperty -Node $entry -Name 'id')
        $text = ConvertTo-FindingDestinationText -Finding $entry -Destination $Destination -Context $Context
        $sections.Add((@("**Finding ``$id``**", $text) -join "`n`n"))
    }

    Join-FindingMarkdownDocument -Text $sections.ToArray()
}

# Builds the items for an approved set, in the order a reader meets them.
#
# Presentation order is the rendering order, so a tracker receiving a whole review holds its
# synthesis findings ahead of the rest, in the same sequence as the combined audit document.
# Creation follows this order, so tracker identifiers run the same way that document reads.
#
# `-Context` is the captured discussion, or nothing for a review of content that is not a pull
# request.
function New-TrackerItemSet {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Finding,

        [System.Collections.IDictionary] $Context
    )

    $items = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in (Get-FindingPresentationOrder -Finding $Finding)) {
        $items.Add((New-TrackerItem -Finding $entry -Context $Context))
    }

    , $items.ToArray()
}

# Builds one item: the finding it came from, its title, and its body.
function New-TrackerItem {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding,

        [System.Collections.IDictionary] $Context
    )

    $provenance = Get-TrackerItemProvenance -Finding $Finding -Context $Context
    $rendered = ConvertTo-FindingTrackerItem `
        -Finding $Finding `
        -FollowUpUrl ([string] $provenance['followUpUrl']) `
        -PullRequestUrl ([string] $provenance['pullRequestUrl'])

    [ordered]@{
        findingId = [string] (Get-JsonProperty -Node $Finding -Name 'id')
        title     = [string] $rendered['title']
        body      = [string] $rendered['body']
    }
}

# Resolves the links one item carries back to the review that produced it.
#
# The pull request link is the capture's own, so an item names the pull request that was reviewed
# rather than whatever the tracker happens to live beside; filing GitHub issues for an Azure DevOps
# review is a supported case and the two have no reason to agree.
#
# The follow-up link is the comment the finding answers. Where the capture records no link for that
# comment, the pull request link stands in: a reader following it reaches the discussion and finds
# the comment there, which is worth more than refusing to file the finding at all.
function Get-TrackerItemProvenance {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Finding,

        [System.Collections.IDictionary] $Context
    )

    $pullRequestUrl = ''
    if ($null -ne $Context) {
        $pullRequestUrl = [string] (Get-JsonProperty -Node $Context -Name 'pullRequestUrl')
    }

    $followUpUrl = ''
    $followUp = Get-JsonProperty -Node $Finding -Name 'followUp'
    if ($null -ne $followUp -and $null -ne $Context) {
        $comment = Find-TrackerFollowUpComment -Context $Context -FollowUp $followUp
        if ($null -ne $comment) {
            $followUpUrl = [string] (Get-JsonProperty -Node $comment -Name 'url')
        }
        if ([string]::IsNullOrWhiteSpace($followUpUrl)) { $followUpUrl = $pullRequestUrl }
    }

    [ordered]@{
        followUpUrl    = $followUpUrl
        pullRequestUrl = $pullRequestUrl
    }
}

# Confirms every follow-up an approved set carries can be linked, before anything is created.
#
# This is not the check publication to a pull request makes. That one asks whether a reply may be
# posted into the thread, and refuses a GitHub thread that accepts none - a review summary, a
# comment on the pull request rather than on the code. A tracker item posts no reply and only links,
# so those threads are perfectly linkable and refusing them here would drop findings for a
# restriction that does not apply.
#
# What is required is that the comment exists in the capture, so the finding names a real point in a
# real discussion, and that a capture was supplied at all when a finding claims to answer one.
function Assert-TrackerFollowUpTarget {
    param(
        [Parameter(Mandatory)]
        $Finding,

        [System.Collections.IDictionary] $Context
    )

    $followUp = Get-JsonProperty -Node $Finding -Name 'followUp'
    if ($null -eq $followUp) { return }

    $findingId = [string] (Get-JsonProperty -Node $Finding -Name 'id')
    $threadId = [string] (Get-JsonProperty -Node $followUp -Name 'threadId')
    $commentId = [string] (Get-JsonProperty -Node $followUp -Name 'commentId')

    if ($null -eq $Context) {
        throw "Finding '$findingId' follows up on comment '$commentId' in thread '$threadId', and no captured discussion was supplied to resolve it. Pass the capture as -ContextPath, or drop the follow-up from the finding."
    }

    if ($null -eq (Find-TrackerFollowUpComment -Context $Context -FollowUp $followUp)) {
        throw "Finding '$findingId' follows up on comment '$commentId' in thread '$threadId', which the captured discussion does not contain. Capture the pull request again and revalidate the finding, or drop the follow-up from it."
    }
}

# Finds the captured comment a follow-up names, or nothing.
#
# Identifier comparison is ordinal, because a GitHub node identifier is an opaque case-sensitive
# string and two identifiers differing only in case belong to two different comments.
function Find-TrackerFollowUpComment {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Context,

        [Parameter(Mandatory)]
        $FollowUp
    )

    $threadId = [string] (Get-JsonProperty -Node $FollowUp -Name 'threadId')
    $commentId = [string] (Get-JsonProperty -Node $FollowUp -Name 'commentId')

    foreach ($thread in (Get-JsonArrayProperty -Node $Context -Name 'threads')) {
        if (-not [string]::Equals([string] (Get-JsonProperty -Node $thread -Name 'id'), $threadId, [StringComparison]::Ordinal)) { continue }

        foreach ($comment in (Get-JsonArrayProperty -Node $thread -Name 'comments')) {
            if ([string]::Equals([string] (Get-JsonProperty -Node $comment -Name 'id'), $commentId, [StringComparison]::Ordinal)) {
                return $comment
            }
        }

        return $null
    }

    $null
}
