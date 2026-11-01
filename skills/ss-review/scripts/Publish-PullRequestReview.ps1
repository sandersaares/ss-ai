#Requires -Version 7.6
# Preserve the named skill interface while the native command owns validation
# and the publication transaction. Ref: pwsh-calls/publish-pull-request-review.md.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Url,
    [Parameter(Mandatory)][string] $ApprovedFindingsPath,
    [Parameter(Mandatory)][string] $ContextPath,
    [Parameter(Mandatory)][string] $DiagnosticsPath,
    [Parameter(Mandatory)][string] $RepositoryPath,
    [Parameter(Mandatory)][string] $OutputDirectory,
    [switch] $DiscardPendingReview,
    [switch] $PublishToClosedPullRequest,
    [string] $MergeApprovalPath,
    [string] $Tenant,
    [DateTimeOffset] $NowUtc
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$program = Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1
$start = [Diagnostics.ProcessStartInfo]::new($program.Source)
$start.WorkingDirectory = (Get-Location).ProviderPath
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
$start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
$arguments = [Collections.Generic.List[string]]::new()
foreach ($argument in @(
    'review', 'publish-final-review', "--url=$Url", "--approved-findings-path=$ApprovedFindingsPath",
    "--context-path=$ContextPath", "--diagnostics-path=$DiagnosticsPath", "--repository-path=$RepositoryPath",
    "--output-directory=$OutputDirectory"
)) { $arguments.Add($argument) }
if ($DiscardPendingReview) { $arguments.Add('--discard-pending-review') }
if ($PublishToClosedPullRequest) { $arguments.Add('--publish-to-closed-pull-request') }
# Forward explicitly supplied values even when empty so native validation can reject them.
if ($PSBoundParameters.ContainsKey('MergeApprovalPath')) { $arguments.Add("--merge-approval-path=$MergeApprovalPath") }
if ($PSBoundParameters.ContainsKey('Tenant')) { $arguments.Add("--tenant=$Tenant") }
if ($PSBoundParameters.ContainsKey('NowUtc')) { $arguments.Add("--now-utc=$($NowUtc.ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture))") }
foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }

$process = [Diagnostics.Process]::Start($start)
try {
    # Drain both pipes concurrently, including when the child reports failure.
    $output = $process.StandardOutput.ReadToEndAsync()
    $errors = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $text = $output.GetAwaiter().GetResult()
    $errorText = $errors.GetAwaiter().GetResult()
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    if ($text.Length -gt 0) { Write-Output -NoEnumerate $text }
    [Console]::Error.Write($errorText)
    if ($process.ExitCode -ne 0) { throw "ss-ai review publish-final-review failed with exit code $($process.ExitCode)" }
} finally { $process.Dispose() }
