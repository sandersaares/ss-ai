#Requires -Version 7.6

# Discards findings, keeping an account of why
# (docs/implementation.md, "Finding artifacts").
#
# A discarded finding is moved rather than deleted, because the pass that reports on the review's
# own quality has no other evidence of what quality improvement rejected and would otherwise only
# ever see what survived.
#
# The batch form exists because the passes that discard findings work over the whole consolidated
# set and would otherwise start one process per finding.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Errors go to the error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $ArtifactPath,

    # The finding to discard. Either this with -Reason, or -PlanPath.
    [string] $FindingId,

    # Why the finding was discarded.
    [string] $Reason,

    # A JSON file holding `{ removals: [ { findingId, reason } ] }`.
    [string] $PlanPath,

    [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$artifact = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ArtifactPath)
$arguments = @('remove-finding', "--artifact-path=$artifact", "--now-utc=$($NowUtc.ToUniversalTime().ToString('o'))")
if ($PlanPath) {
    if ($FindingId -or $Reason) { throw 'Supply either -PlanPath or -FindingId with -Reason, not both.' }
    $plan = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($PlanPath)
    Invoke-NativeReviewWorkflow -Arguments ($arguments + "--plan-path=$plan")
} else {
    if (-not $FindingId -or -not $Reason) { throw 'Supply -FindingId with -Reason, or -PlanPath.' }
    $request = [ordered]@{ schemaVersion = 1; removals = @([ordered]@{ findingId = $FindingId; reason = $Reason }) }
    Invoke-NativeReviewWorkflowWithJson -Arguments $arguments -PathOption 'plan-path' -Value $request
}
