#Requires -Version 7.6

# These helpers translate existing PowerShell callers to the native audit operations.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot/NativeWorkflowBridge.ps1"

# Keeps caller-provided prose and context out of process arguments.
function New-ReviewAuditInputFile {
    param([Parameter(Mandatory)][AllowEmptyString()][string] $Content)

    $path = Join-Path (Get-Location).ProviderPath ".ss-ai-audit-$([guid]::NewGuid().ToString('N')).input"
    $options = [IO.FileStreamOptions]::new()
    $options.Mode = [IO.FileMode]::CreateNew
    $options.Access = [IO.FileAccess]::Write
    $options.Share = [IO.FileShare]::None
    if (-not $IsWindows) {
        $options.UnixCreateMode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
    }
    $stream = [IO.FileStream]::new($path, $options)
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Content)
        $stream.Write($bytes, 0, $bytes.Length)
    } catch {
        $stream.Dispose()
        [IO.File]::Delete($path)
        throw
    }
    $stream.Dispose()
    $path
}

# Reads verbatim feedback in arrival order without interpreting it as shell input.
function Get-ReviewAuditFeedback {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][string] $FindingId)
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $json = Invoke-NativeReviewWorkflow -Arguments @('audit-get-feedback', "--path=$path", "--finding-id=$FindingId")
    $result = ConvertFrom-Json -InputObject $json -NoEnumerate
    if ($result -isnot [string]) { throw 'Native audit feedback result is not a string.' }
    $result
}

# Records a human response without translating it into a finding decision.
function Add-ReviewAuditFeedback {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $FindingsPath,
        [Parameter(Mandatory)][string] $ResponseId,
        [Parameter(Mandatory)][string] $FindingId,
        [Parameter(Mandatory)][string] $Text
    )
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $findings = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($FindingsPath)
    $textPath = New-ReviewAuditInputFile -Content $Text
    try {
        Invoke-NativeReviewWorkflow -Arguments @('audit-add-feedback', "--path=$path", "--findings-path=$findings",
            "--response-id=$ResponseId", "--finding-id=$FindingId", "--text-path=$textPath") | Out-Null
    } finally { [IO.File]::Delete($textPath) }
}

# Publication verifies the stored approval against its own pull request and commit.
function Test-ReviewMergeApproval {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $PullRequestUrl,
        [Parameter(Mandatory)][string] $SourceCommit
    )
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $json = Invoke-NativeReviewWorkflow -Arguments @('audit-test-merge-approval', "--path=$path",
        "--pull-request-url=$PullRequestUrl", "--source-commit=$SourceCommit")
    $result = ConvertFrom-Json -InputObject $json -NoEnumerate
    if ($result -isnot [bool]) { throw 'Native merge approval result is not a boolean.' }
    $result
}

# The native operation owns the entire read-modify-write transaction.
function Set-ReviewMergeApproval {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][Collections.IDictionary] $Context,
        [bool] $ApproveMerge,
        [Collections.IDictionary] $PreviousContext,
        [switch] $CarryForward
    )
    if ($CarryForward -and $null -eq $PreviousContext) { throw 'Carry-forward requires the previous capture.' }
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $contextPath = New-ReviewAuditInputFile -Content (ConvertTo-Json -InputObject $Context -Depth 100 -Compress)
    $previousPath = $null
    try {
        $arguments = @('audit-set-merge-approval-values', "--path=$path", "--context-path=$contextPath")
        if ($CarryForward) {
            $previousPath = New-ReviewAuditInputFile -Content (ConvertTo-Json -InputObject $PreviousContext -Depth 100 -Compress)
            $arguments += @('--carry-forward', "--previous-context-path=$previousPath")
        } elseif ($ApproveMerge) {
            $arguments += '--approve'
        }
        $json = Invoke-NativeReviewWorkflow -Arguments $arguments
        $result = ConvertFrom-Json -InputObject $json -AsHashtable
        if ($result['status'] -cnotin @('Recorded', 'NoApproval')) { throw 'Invalid native merge approval result.' }
        $result['status']
    } finally {
        if ($previousPath) { [IO.File]::Delete($previousPath) }
        [IO.File]::Delete($contextPath)
    }
}
