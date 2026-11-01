#Requires -Version 7.6
# Preserve the named skill interface while native code owns the transaction.
# Ref: pwsh-calls/file-tracker-items.md.
[CmdletBinding(PositionalBinding = $false)]
param(
    [string] $Url,
    [Parameter(Mandatory)][string] $ApprovedFindingsPath,
    [Parameter(Mandatory)][string] $DiagnosticsPath,
    [Parameter(Mandatory)][string] $OutputDirectory,
    [string] $ContextPath,
    [string] $WorkItemType,
    [string] $AreaPath,
    [string] $DescriptionField,
    [int] $ParentWorkItemId,
    [switch] $AcceptStoredRewrite,
    [string] $AcceptWorkItemId,
    [string] $AcceptExpectedBodyHash,
    [string] $AcceptStoredBodyHash,
    [ValidateSet('azureDevOps', 'gitHub')][string] $Platform,
    [string] $Organization,
    [string] $Project,
    [string] $Repository,
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
    'review', 'publish-tracker-items', "--approved-findings-path=$ApprovedFindingsPath",
    "--diagnostics-path=$DiagnosticsPath", "--output-directory=$OutputDirectory"
)) { $arguments.Add($argument) }

# Explicitly supplied empty values still reach native validation.
foreach ($parameter in @(
    @('Url', 'url'), @('ContextPath', 'context-path'), @('WorkItemType', 'work-item-type'),
    @('AreaPath', 'area-path'), @('DescriptionField', 'description-field'),
    @('ParentWorkItemId', 'parent-work-item-id'), @('AcceptWorkItemId', 'accept-work-item-id'),
    @('AcceptExpectedBodyHash', 'accept-expected-body-hash'), @('AcceptStoredBodyHash', 'accept-stored-body-hash'),
    @('Platform', 'platform'), @('Organization', 'organization'), @('Project', 'project'),
    @('Repository', 'repository'), @('Tenant', 'tenant')
)) {
    if ($PSBoundParameters.ContainsKey($parameter[0])) {
        $arguments.Add("--$($parameter[1])=$($PSBoundParameters[$parameter[0]])")
    }
}
if ($AcceptStoredRewrite) { $arguments.Add('--accept-stored-rewrite') }
if ($PSBoundParameters.ContainsKey('NowUtc')) {
    $arguments.Add("--now-utc=$($NowUtc.ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture))")
}
foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }

$process = [Diagnostics.Process]::Start($start)
try {
    # Drain both pipes concurrently, including when a partial outcome fails.
    $output = $process.StandardOutput.ReadToEndAsync()
    $errors = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $text = $output.GetAwaiter().GetResult()
    $errorText = $errors.GetAwaiter().GetResult()
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    if ($text.Length -gt 0) { Write-Output -NoEnumerate $text }
    [Console]::Error.Write($errorText)
    if ($process.ExitCode -ne 0) { throw "ss-ai review publish-tracker-items failed with exit code $($process.ExitCode)" }
} finally { $process.Dispose() }
