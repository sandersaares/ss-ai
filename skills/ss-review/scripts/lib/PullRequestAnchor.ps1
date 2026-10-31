#Requires -Version 7.0

# Anchor resolution for a finding about to be published on a pull request.
#
# Role in the overall design: a reviewer states where a finding applies, and the hosting service
# accepts a comment only where the pull request actually changed something. This library is the one
# place that reconciles the two, against the diff GitDiff.ps1 reads from the local repository, so
# neither platform adapter carries a rule of its own and a finding is judged identically whichever
# service it is destined for.
#
# An anchor is a dictionary of the form
#     { kind = 'line' | 'file'; path = <canonical path>; startLine = <int>; endLine = <int>; side = 'right' | 'left' }
# where the line fields apply to a line anchor only. The path is repository-relative with forward
# slashes and no leading separator, which is the form both services address a file by.
#
# The outcomes publication can represent are a publishable anchor, a file-level downgrade with a
# reason, and a resolution failure the freshness gate reports as unanchored.
#
# The resolution rules are:
#
#   - A file the diff does not change fails resolution. The finding names a file the pull request
#     does not touch, so there is nothing to say about it here, and publishing it against some other
#     file would attribute a reviewer's words to code they did not read.
#   - A line range that falls outside every hunk becomes a file anchor on the same file. The
#     observation still holds for the file even when the exact lines are not commentable, and losing
#     it entirely would be a worse outcome than reporting it a little less precisely. The downgrade
#     is recorded so the publication result can state it.
#   - A renamed file is matched by its post-change path, which is the path the service and the
#     reviewer both see.
#   - A deleted file has no post-change lines. A right-side line anchor on one follows the hunk rule
#     and becomes a file anchor, while a left-side line anchor can address the removed lines.
#
# Nothing else is substituted. A rule that guessed a nearby line would move a reviewer's words to
# code they did not write.
#
# The same rules decide, one phase earlier, which approved findings the reviewed diff can still
# carry, so that a finding whose anchor publication would reject is removed while the run can still
# report it rather than during publication.
#
# The native resolver owns the shared anchor rules; this library adapts the
# publication calls and keeps the existing document mutation interface.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The kinds of anchor a finding may carry. A line anchor addresses a range within a changed file; a
# file anchor addresses the file as a whole.
$script:PullRequestAnchorKinds = @('line', 'file')

# The side of the change a line anchor addresses, in the vocabulary the pull request capture already
# uses for a thread's context: 'right' is the post-change file, 'left' the pre-change one.
$script:PullRequestAnchorSides = @('right', 'left')

# The side assumed when a line anchor names none. A review comments on the state it reviewed, which
# is the post-change file, and that is also what both services default a comment to.
$script:PullRequestAnchorDefaultSide = 'right'

# Resolves one anchor against the diff entry for the file it names.
#
# The result restates the anchor in publishable form and reports whether it had to be weakened. A
# caller reads wasDowngraded to report the weakening; nothing about it is put to the reviewer,
# because there is no alternative to offer and no decision left to make.
function Resolve-PullRequestAnchor {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Anchor,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $DiffFile
    )

    $result = Invoke-NativeReviewWorkflowWithJson -Arguments @('resolve-anchor') -PathOption 'request-path' -Value @{
        anchor = $Anchor
        diffFile = $DiffFile
    }
    ConvertFrom-JsonVerbatim -Text $result
}

# Resolves one anchor against a whole diff, failing when the pull request does not change the file.
#
# This is the entry point a publication call site uses: it performs the lookup that decides whether
# the file is in the diff at all, which is the check that fails the run.
function Resolve-PullRequestAnchorInDiff {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Anchor,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Diff
    )

    $result = Invoke-NativeReviewWorkflowWithJson -Arguments @('resolve-anchor') -PathOption 'request-path' -Value @{
        anchor = $Anchor
        diff = $Diff
    }
    ConvertFrom-JsonVerbatim -Text $result
}

# Resolves a set of authored anchors with one validated copy of the diff.
function Resolve-PullRequestAnchorsInDiff {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]] $Anchor,
        [Parameter(Mandatory)][System.Collections.IDictionary] $Diff
    )

    $result = Invoke-NativeReviewWorkflowWithJson -Arguments @('resolve-anchor') -PathOption 'request-path' -Value @{
        anchors = $Anchor
        diff = $Diff
    }
    $response = ConvertFrom-JsonVerbatim -Text $result
    if ($response -isnot [System.Collections.IDictionary] -or
        $response.Count -ne 1 -or -not $response.Contains('results') -or
        $response['results'] -isnot [array] -or $response['results'].Count -ne $Anchor.Count) {
        throw 'Invalid native anchor resolution response.'
    }
    foreach ($entry in $response['results']) {
        if ($entry -isnot [System.Collections.IDictionary] -or $entry.Count -ne 1) {
            throw 'Invalid native anchor resolution result.'
        }
        if ($entry.Contains('anchor') -and $entry['anchor'] -is [System.Collections.IDictionary]) {
            $resolved = $entry['anchor']
            if ($resolved['kind'] -cin @('file', 'line') -and
                $resolved['path'] -is [string] -and $resolved['path'].Length -gt 0 -and
                $resolved['wasDowngraded'] -is [bool] -and
                ($resolved['kind'] -cne 'line' -or
                    ($resolved['startLine'] -is [long] -and $resolved['endLine'] -is [long] -and
                        $resolved['side'] -cin @('left', 'right'))) -and
                (-not $resolved['wasDowngraded'] -or $resolved['downgradeReason'] -is [string])) { continue }
        }
        if (($entry.Contains('error') -and $entry['error'] -is [string] -and $entry['error'].StartsWith('Unresolvable anchor:')) -or
            ($entry.Contains('argumentError') -and $entry['argumentError'] -is [string] -and $entry['argumentError'].Length -gt 0)) { continue }
        throw 'Invalid native anchor resolution result.'
    }
    , $response['results']
}

# Removes every approved finding the reviewed diff can no longer carry, reporting what it removed
# (SKILL.md, "Phase 8: freshness check").
#
# A finding is anchored while the review runs and the pull request keeps moving underneath it, so a
# file that was under review when the finding was written can be absent from the diff by the time
# the audit has approved it. Publication refuses such an anchor, and refusing it there means
# refusing the whole publication after the user has approved every finding; removing it here is the
# last point at which the run can still state which finding it lost and why.
#
# The publication resolver is the freshness rule. A finding is removed when that resolver throws; a
# downgrade stays in the approved set because it is a publishable file-level comment. The locations a
# finding cites beyond its anchor are not positions it is published at and are not consulted. A
# follow-up is left alone for the same reason Resolve-PullRequestFindingAnchor never resolves one: it
# is published into a thread that already exists, so the diff has nothing to say about where it goes,
# and removing it would discard a reply the review owes.
function Remove-ApprovedFindingOutsideDiff {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Document,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Diff
    )

    $kept = [System.Collections.Generic.List[object]]::new()
    $removed = [System.Collections.Generic.List[object]]::new()

    $findings = Get-JsonArrayProperty -Node $Document -Name 'findings'
    $anchors = [System.Collections.Generic.List[object]]::new()
    foreach ($finding in $findings) {
        if ($null -eq (Get-JsonProperty -Node $finding -Name 'followUp')) {
            $anchors.Add((Get-JsonProperty -Node $finding -Name 'anchor'))
        }
    }
    $results = if ($anchors.Count -gt 0) { Resolve-PullRequestAnchorsInDiff -Anchor $anchors.ToArray() -Diff $Diff } else { , @() }
    $index = 0
    foreach ($finding in $findings) {
        if ($null -ne (Get-JsonProperty -Node $finding -Name 'followUp')) {
            $kept.Add($finding)
            continue
        }

        $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')
        $anchor = Get-JsonProperty -Node $finding -Name 'anchor'
        $path = [string] (Get-JsonProperty -Node $anchor -Name 'path' -Default '')
        $result = $results[$index]
        $index++
        if ($result.Contains('argumentError')) {
            throw $result['argumentError']
        }
        if ($result.Contains('anchor')) {
            $kept.Add($finding)
            continue
        }
        $reason = $result['error']

        try {
            $path = ConvertTo-CanonicalFindingPath -Path $path -Context "finding '$findingId'"
        } catch {
            # If canonicalization itself failed, the reason already carries the validation failure;
            # the path field remains the provided anchor text so the user can identify it.
        }
        $removed.Add([ordered]@{
                findingId = $findingId
                path      = $path
                reason    = $reason
            })
    }

    Set-JsonProperty -Node $Document -Name 'findings' -Value $kept.ToArray()

    , $removed.ToArray()
}

# Builds the resolution result.
#
# A file anchor carries no position, exactly as a stored file anchor carries none, so the line
# fields are absent rather than null: a reader that finds them present would take them for a
# position the anchor does not have.
function New-ResolvedPullRequestAnchor {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('line', 'file')]
        [string] $Kind,

        [Parameter(Mandatory)]
        [string] $Path,

        [int] $StartLine = 0,

        [int] $EndLine = 0,

        [AllowNull()]
        [string] $Side,

        [AllowNull()]
        [string] $DowngradeReason
    )

    $resolved = [ordered]@{
        kind = $Kind
        path = $Path
    }

    if ($Kind -eq 'line') {
        $resolved['startLine'] = $StartLine
        $resolved['endLine'] = if ($EndLine -gt 0) { $EndLine } else { $StartLine }
        $resolved['side'] = if ([string]::IsNullOrWhiteSpace($Side)) { $script:PullRequestAnchorDefaultSide } else { $Side }
    }

    $resolved['wasDowngraded'] = -not [string]::IsNullOrWhiteSpace($DowngradeReason)
    if ($resolved['wasDowngraded']) {
        $resolved['downgradeReason'] = $DowngradeReason
    }

    $resolved
}
