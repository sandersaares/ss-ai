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
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls Get-JsonProperty, Get-JsonArrayProperty and
# Set-JsonProperty from JsonValue.ps1, ConvertTo-CanonicalFindingPath from FindingModel.ps1, and
# Get-GitDiffFile and Test-GitDiffLineInHunk from GitDiff.ps1, which the executable composition root
# loads alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

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

    $kind = [string] (Get-JsonProperty -Node $Anchor -Name 'kind' -Default 'file')
    if ($script:PullRequestAnchorKinds -notcontains $kind) {
        throw "An anchor of kind '$kind' cannot be resolved. An anchor is either a line anchor or a file anchor."
    }

    $anchorPath = ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $Anchor -Name 'path'))
    $filePath = [string] (Get-JsonProperty -Node $DiffFile -Name 'path')

    # The pairing is the caller's, and a mismatched pair would resolve one file's anchor against
    # another file's hunks, which is the one way this function could produce a confidently wrong
    # answer rather than a failure.
    if (-not [string]::Equals($anchorPath, $filePath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "An anchor on '$anchorPath' cannot be resolved against the diff entry for '$filePath'."
    }

    if ($kind -eq 'file') {
        return New-ResolvedPullRequestAnchor -Kind 'file' -Path $filePath
    }

    $startLine = [int] (Get-JsonProperty -Node $Anchor -Name 'startLine' -Default 0)
    $endLine = [int] (Get-JsonProperty -Node $Anchor -Name 'endLine' -Default 0)
    if ($endLine -le 0) { $endLine = $startLine }
    if ($startLine -le 0) {
        throw "A line anchor on '$filePath' names no line."
    }
    if ($endLine -lt $startLine) {
        throw "A line anchor on '$filePath' ends at line $endLine, before it starts at line $startLine."
    }

    $side = [string] (Get-JsonProperty -Node $Anchor -Name 'side' -Default $script:PullRequestAnchorDefaultSide)
    if ([string]::IsNullOrWhiteSpace($side)) { $side = $script:PullRequestAnchorDefaultSide }
    $side = $side.ToLowerInvariant()
    if ($script:PullRequestAnchorSides -notcontains $side) {
        throw "A line anchor on '$filePath' names side '$side', which is neither the post-change nor the pre-change side."
    }

    if (Test-GitDiffLineInHunk -File $DiffFile -StartLine $startLine -EndLine $endLine -Side $side) {
        return New-ResolvedPullRequestAnchor -Kind 'line' -Path $filePath -StartLine $startLine -EndLine $endLine -Side $side
    }

    $range = if ($startLine -eq $endLine) { "line $startLine" } else { "lines $startLine-$endLine" }
    New-ResolvedPullRequestAnchor -Kind 'file' -Path $filePath -DowngradeReason "No hunk of the reviewed diff covers $range on the $side side of '$filePath', so the finding is reported against the file."
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

    $anchorPath = ConvertTo-CanonicalFindingPath -Path ([string] (Get-JsonProperty -Node $Anchor -Name 'path'))
    $file = Get-GitDiffFile -Diff $Diff -Path $anchorPath
    if ($null -eq $file) {
        throw "The reviewed diff does not change '$anchorPath', so a finding cannot be anchored to it. Anchor the finding to a file the pull request changes."
    }

    Resolve-PullRequestAnchor -Anchor $Anchor -DiffFile $file
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
    foreach ($finding in $findings) {
        if ($null -ne (Get-JsonProperty -Node $finding -Name 'followUp')) {
            $kept.Add($finding)
            continue
        }

        $findingId = [string] (Get-JsonProperty -Node $finding -Name 'id')
        $anchor = $null
        $path = ''

        try {
            $anchor = Get-JsonProperty -Node $finding -Name 'anchor'
            $path = [string] (Get-JsonProperty -Node $anchor -Name 'path' -Default '')
            Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $Diff | Out-Null
            $kept.Add($finding)
            continue
        } catch {
            $reason = $_.Exception.Message
        }

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
