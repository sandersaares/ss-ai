#Requires -Version 7.6

# Lists every recorded finding without its full body
# (docs/implementation.md, "Finding artifacts").
#
# Deduplication has to consider every finding against every other, and the bodies do not fit in one
# subagent's context: a few hundred findings are a few thousand tokens as an index and two orders of
# magnitude more in full. The index is what candidate pairs are proposed from, and the adjudicator
# then reads only the candidates in full.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The index goes to a file and errors to the error
# stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The directory holding the run's reviewer artifacts, as written by New-FindingArtifact.ps1.
    [Parameter(Mandatory)]
    [string] $ReviewerArtifactDirectory,

    [Parameter(Mandatory)]
    [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
$directory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ReviewerArtifactDirectory)
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
Invoke-NativeReviewWorkflow -Arguments @('finding-index', "--reviewer-directory=$directory", "--output-path=$output")
