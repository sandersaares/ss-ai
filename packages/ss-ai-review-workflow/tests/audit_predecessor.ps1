#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Root,
    [Parameter(Mandatory)][string] $Mode,
    [string] $Decisions,
    [string] $TextPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$library = Join-Path $Root 'skills/ss-review/scripts/lib'
. (Join-Path $library 'JsonValue.ps1')
. (Join-Path $library 'PullRequestModel.ps1')
. (Join-Path $library 'WorkflowData.ps1')
. (Join-Path $library 'FindingModel.ps1')

# Contending operations wait for the test to finish the other transaction.
$script:contentionCount = 0
function Start-Sleep {
    param([int] $Milliseconds)
    $script:contentionCount++
    if ($script:contentionCount -ne 1) { throw 'Unexpected repeated contention.' }
    [Console]::WriteLine('contended')
    if ([Console]::ReadLine() -cne 'retry') { throw 'Expected retry signal.' }
}

if ($Mode -ceq 'read-text') {
    ConvertTo-Json -InputObject (Read-PullRequestFileText -Path $TextPath) -Compress
    return
}

switch ($Mode) {
    'feedback-contender' {
        Add-WorkflowListItem -Kind auditFeedback -Path $Decisions -Initialize -Item ([ordered]@{
            responseId = 'powershell'; findingId = 'F-1'; text = 'PowerShell response'
        })
        [Console]::WriteLine('complete')
    }
    'decision-contender' {
        $lock = Open-WorkflowDataLock -Path $Decisions
        try {
            $document = Open-FindingDecisionDocument -Path $Decisions
            Add-FindingDecisionRecord -Document $document -FindingId 'F-2' -Decision 'approved' `
                -NowUtc ([DateTimeOffset]::Parse('2024-05-06T09:10:11Z')) | Out-Null
            Write-FindingDecisionDocument -Path $Decisions -Document $document
        } finally { $lock.Dispose() }
        [Console]::WriteLine('complete')
    }
    default { throw 'Unexpected test mode.' }
}
