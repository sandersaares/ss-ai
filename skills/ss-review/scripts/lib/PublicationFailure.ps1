#Requires -Version 7.0

# Structured failure signals shared by the pull request transports and publication
# (docs/implementation.md, "Publication").
#
# Role in the overall design: transports are the only layer that can tell whether a service
# answered a request with a definite refusal or whether the process simply lost the response.
# Publication consumes that distinction when it writes the result record, because a refused write is
# safe to retry and an unknown write is not. The signal therefore travels as exception data rather
# than as wording in a diagnostic message; messages are for people, while this key is the contract
# between libraries loaded by the composition root.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Stable key for the exception data entry that marks a service-declined request. The value is scoped
# to this skill so unrelated exceptions carrying their own metadata cannot be mistaken for a
# publication decision.
$script:PullRequestServiceRefusalDataKey = 'ss-review.pullRequestServiceRefusal'

# Builds an exception whose diagnostic text remains human-readable while its data carries the
# machine-readable publication outcome. Transport code uses this only when the service itself
# answered in a way that proves the requested write was declined.
function New-PullRequestServiceRefusalException {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Message
    )

    $exception = [System.InvalidOperationException]::new($Message)
    $exception.Data[$script:PullRequestServiceRefusalDataKey] = $true
    $exception
}

# Reports whether an exception chain carries the service-refusal signal.
#
# PowerShell and .NET callers may wrap an exception before publication catches it, so the chain is
# inspected rather than only the outermost exception. Anything without the explicit signal remains
# ambiguous, preserving the conservative recovery rule that prevents duplicate publication.
function Test-PullRequestServiceRefusalException {
    param(
        [AllowNull()]
        [System.Exception] $Exception
    )

    $current = $Exception
    while ($null -ne $current) {
        if ($current.Data.Contains($script:PullRequestServiceRefusalDataKey) -and [bool] $current.Data[$script:PullRequestServiceRefusalDataKey]) {
            return $true
        }
        $current = $current.InnerException
    }

    return $false
}

# Classifies a failed write as refused or unknown.
#
# Anything the service did not itself report is treated as unknown, which is the conservative
# reading: a request that vanished between here and the service may have been applied before the
# answer was lost, and recording it as a definite failure would invite a repeat that publishes the
# same finding twice.
#
# Both publication paths classify the same way, because both face the same question about the same
# transports. A path that read a lost response as a definite failure would rerun over a write that
# may already stand, whichever service it was writing to.
function Get-PublicationFailureOutcome {
    param(
        [AllowNull()]
        [System.Exception] $Exception
    )

    if (Test-PullRequestServiceRefusalException -Exception $Exception) { return 'failed' }

    'ambiguous'
}
