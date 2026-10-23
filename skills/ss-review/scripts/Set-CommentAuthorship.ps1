#Requires -Version 7.6

# Records who wrote the comments the capture could not settle
# (docs/design.md, "Authorship classification").
#
# The capture proves machine authorship from account types and the markers agents write, and leaves
# everything else indeterminate rather than guessing. Agents identify themselves by conventions no
# fixed rule captures — a marker string, an emoji, a turn of phrase — so the remaining comments are
# read by a subagent, which reports its judgement here. The capture is not complete until every
# comment has an authorship, because phases that deduplicate findings against prior feedback treat
# agent and human comments differently.
#
# This is a separate executable from the capture for the same reason the registration is: it is the
# command that records semantic decisions beside an immutable capture.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The immutable capture whose semantic decisions are being recorded.
    [Parameter(Mandatory)]
    [string] $Path,

    # snapshotId from the bounded query that supplied the complete classification batch.
    [Parameter(Mandatory)]
    [string] $SnapshotId,

    # Classifications as a JSON array of objects carrying `threadId`, `commentId` and `kind`, where
    # kind is `agent` or `human`. Either this or -ClassificationPath is required.
    [string] $Classification,

    # A file holding the same JSON array. A capture with many comments produces a batch that exceeds
    # what a command line accepts, and the classification is then supplied as a file instead.
    [string] $ClassificationPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestIdentity.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"

if ($Classification -and $ClassificationPath) {
    throw 'Supply either -Classification or -ClassificationPath, not both.'
}
if (-not $Classification -and -not $ClassificationPath) {
    throw 'Supply the classifications with -Classification or -ClassificationPath.'
}

$classificationText = if ($ClassificationPath) {
    (Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $ClassificationPath)) -Encoding ([System.Text.Encoding]::UTF8))
} else {
    $Classification
}

$entries = ConvertFrom-JsonVerbatim -Text $classificationText
if ($entries -isnot [object[]]) {
    throw 'The classifications must be a JSON array of objects carrying threadId, commentId and kind.'
}

$lock = Open-WorkflowDataLock -Path "$Path.authorship.json"
try {
    $summary = Save-CommentAuthorship -Path $Path -SnapshotId $SnapshotId -Classification $entries
} finally { $lock.Dispose() }

[ordered]@{
    contextPath           = Resolve-PullRequestPath -Path $Path
    applied               = $summary['applied']
    unchanged             = $summary['unchanged']
    indeterminate         = $summary['indeterminate']
    isComplete            = $summary['isComplete']
} | ConvertTo-Json -Depth 32 -Compress
