#Requires -Version 7.6

# Combines a run's reviewer artifacts into one consolidated set
# (docs/implementation.md, "Finding artifacts").
#
# The merge plan holds only the groups that merge, and every finding it does not name is carried
# over as it stands. That inversion is what makes the phase safe: a duplicate the adjudicator missed
# reaches the author as a second comment saying much the same thing, whereas keeping only what a
# plan lists would discard a finding for every line the adjudicator forgot to write.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The consolidated artifact goes to a file and errors to
# the error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The directory holding the run's reviewer artifacts, as written by New-FindingArtifact.ps1.
    [Parameter(Mandatory)]
    [string] $ReviewerArtifactDirectory,

    # A versioned mergePlan document. Omit the argument or supply an explicit empty groups array
    # when nothing merged. A relative finding path is taken against the plan's own directory.
    [string] $MergePlanPath,

    [Parameter(Mandatory)]
    [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$directory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ReviewerArtifactDirectory)
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
$arguments = @('consolidate-findings', "--reviewer-directory=$directory", "--output-path=$output")
if ($MergePlanPath) {
    $plan = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($MergePlanPath)
    $arguments += "--plan-path=$plan"
}
Invoke-NativeReviewWorkflow -Arguments $arguments
