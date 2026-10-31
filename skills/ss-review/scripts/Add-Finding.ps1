#Requires -Version 7.6

# Records one finding in an artifact (docs/design.md, "Findings";
# docs/implementation.md, "Finding artifacts").
#
# The finding is supplied as a file rather than on the command line, because a finding carries
# Markdown prose with newlines, quotes and backticks in it, and a command line is the one place that
# content cannot survive intact.
#
# Shared artifacts serialize the complete update, including identifier allocation.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Errors go to the error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The artifact to append to, as written by New-FindingArtifact.ps1.
    [Parameter(Mandatory)]
    [string] $ArtifactPath,

    # A JSON file holding the finding as its author wrote it, carrying no identifier and no
    # provenance.
    [Parameter(Mandatory)]
    [string] $FindingPath,

    # The captured discussion, as written by Get-PullRequestContext.ps1. Supplying it checks a
    # finding that answers existing feedback against the thread it names, so a mistyped identifier
    # fails while the finding is being recorded rather than at publication.
    [string] $ContextPath,

    # Whether the finding was found by a reviewer or produced by combining other findings.
    [ValidateSet('reviewer', 'synthesis')]
    [string] $Origin = 'reviewer',

    # The findings this one was derived from, for a finding produced by synthesis. Several are named
    # as one comma separated value.
    [string[]] $SourceFindingId = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$artifact = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ArtifactPath)
$finding = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FindingPath)
$arguments = @('add-finding', "--artifact-path=$artifact", "--finding-path=$finding", "--origin=$Origin")
if ($ContextPath) {
    $context = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ContextPath)
    $arguments += "--context-path=$context"
}
foreach ($entry in $SourceFindingId) {
    foreach ($part in $entry.Split(',')) {
        $id = $part.Trim()
        if ($id) { $arguments += "--source-id=$id" }
    }
}
Invoke-NativeReviewWorkflow -Arguments $arguments
