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
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\NativeCaptureBridge.ps1"

if ($PSBoundParameters.ContainsKey('Classification') -and $PSBoundParameters.ContainsKey('ClassificationPath')) {
    throw 'Supply either -Classification or -ClassificationPath, not both.'
}
if (-not $PSBoundParameters.ContainsKey('Classification') -and -not $PSBoundParameters.ContainsKey('ClassificationPath')) {
    throw 'Supply the classifications with -Classification or -ClassificationPath.'
}

$arguments = [Collections.Generic.List[string]]::new()
$arguments.Add('set-authorship')
$arguments.Add("--path=$Path")
$arguments.Add("--snapshot-id=$SnapshotId")
if ($PSBoundParameters.ContainsKey('Classification')) { $arguments.Add("--classification=$Classification") }
if ($PSBoundParameters.ContainsKey('ClassificationPath')) { $arguments.Add("--classification-path=$ClassificationPath") }
Invoke-NativeCaptureContext -Arguments $arguments.ToArray()
