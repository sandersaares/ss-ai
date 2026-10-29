#Requires -Version 7.6

# Verifies that the focus-area artifacts in session state are complete and usable.
#
# prepare-focus-areas.md runs this at two points with different recovery paths: after a restore hit,
# where failure means discarding the restored artifacts and generating instead, and after generation,
# where failure means correcting the artifacts and revalidating. Both need the command to fail
# visibly, so an invalid set exits unsuccessfully rather than reporting a status.
#
# It takes no cache parameters because it inspects only session state; nothing here depends on how or
# whether the artifacts came from the cache.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. Diagnostics go to a file and errors to the error
# stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)]
    [string] $SessionStateDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\FocusAreaCache.ps1"
. "$PSScriptRoot\lib\FocusAreaCacheDiagnostics.ps1"

$transactionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-area-cache-operation')
try {
    try {
        $result = Test-FocusAreaArtifacts -SessionStateDirectory $SessionStateDirectory
    } catch {
        # A failure here must overwrite any hit already recorded, so the summary never reports reuse
        # that did not survive validation. The error is rethrown so the caller sees a failed command and
        # takes the recovery path prepare-focus-areas.md prescribes for the stage it is in.
        $previous = Get-FocusAreaCacheDiagnostics -SessionStateDirectory $SessionStateDirectory
        $diagnostics = @{ cache = 'Miss'; reason = 'ValidationFailed' }
        if ($previous.ContainsKey('userInstructionFileCount')) {
            $diagnostics.userInstructionFileCount = $previous.userInstructionFileCount
        }
        Write-FocusAreaCacheDiagnostics -SessionStateDirectory $SessionStateDirectory -Diagnostics $diagnostics
        throw
    }

    $result | ConvertTo-Json -Compress
} finally { $transactionLock.Dispose() }
