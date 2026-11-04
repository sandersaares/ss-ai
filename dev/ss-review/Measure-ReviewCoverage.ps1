#Requires -Version 7.6

<#
.SYNOPSIS
Compares native per-file and batch coverage persistence.
.DESCRIPTION
Creates an isolated fixture in a new StateDirectory using the inventory size reported in issue #125.
Measures actual native workflow process launches and elapsed time, excluding setup and final validation.
Retains the fixtures for inspection.
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $StateDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$skillScripts = Join-Path $repoRoot 'skills\ss-review\scripts'
. "$skillScripts\lib\JsonValue.ps1"
. "$skillScripts\lib\PullRequestModel.ps1"
. "$skillScripts\lib\WorkflowData.ps1"
. "$skillScripts\lib\FindingModel.ps1"
. "$skillScripts\lib\ReviewWork.ps1"

$program = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
$script:ProcessLaunches = 0
$script:PersistenceProcessLaunches = 0

# Counts only processes that actually start during the measured interval.
function Invoke-CoverageOperation {
    param(
        [Parameter(Mandatory)][string[]] $Arguments,
        [switch] $Measure,
        [switch] $Persistence
    )
    $start = [Diagnostics.ProcessStartInfo]::new($program)
    $start.WorkingDirectory = (Get-Location).ProviderPath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
    foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        if ($Measure) {
            $script:ProcessLaunches++
            if ($Persistence) { $script:PersistenceProcessLaunches++ }
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errors = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "ss-ai workflow operation failed: $errors" }
        ConvertFrom-JsonVerbatim -Text $output
    } finally { $process.Dispose() }
}

$StateDirectory = Resolve-PullRequestPath -Path $StateDirectory
New-Item -ItemType Directory -Path $StateDirectory -ErrorAction Stop | Out-Null
$measurements = @(foreach ($mode in @('PerFile', 'Batch')) {
    $root = Join-Path $StateDirectory $mode
    [void] [IO.Directory]::CreateDirectory($root)
    # Match the reported review size; removal and rename-side entries need no real files.
    $files = @(foreach ($index in 0..120) {
        @{ path = Join-Path $root "source-$index.rs"; sizeBytes = 1; change = if ($index -eq 0) { 'deleted' } elseif ($index -eq 1) { 'new' } else { 'modified' } }
    })
    $groups = @(for ($offset = 0; $offset -lt $files.Count; $offset += 20) {
        # Match the permitted file-count bound in the review coverage design.
        @{ groupId = "group-$offset"; files = @($files | Select-Object -Skip $offset -First 20) }
    })
    $inventory = Join-Path $root 'inventory.json'
    $groupPath = Join-Path $root 'groups.json'
    $queue = Join-Path $root 'queue.json'
    $artifactDirectory = Join-Path $root 'findings'
    Write-PullRequestOutputFileAtomically -Path $inventory -Content (ConvertTo-JsonText -Value $files)
    Write-PullRequestOutputFileAtomically -Path $groupPath -Content (ConvertTo-JsonText -Value $groups)
    $null = Invoke-CoverageOperation -Arguments @(
        'initialize-review-queue', "--queue-path=$queue", "--inventory-path=$inventory", "--file-groups-path=$groupPath"
    )
    $artifact = Invoke-CoverageOperation -Arguments @(
        'initialize-finding', "--output-directory=$artifactDirectory", '--reviewer-id=focus-001',
        '--focus-area=benchmark', "--queue-path=$queue"
    )
    $script:ProcessLaunches = 0
    $script:PersistenceProcessLaunches = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    foreach ($group in $groups) {
        $claim = Invoke-CoverageOperation -Arguments @('next-review-item', "--queue-path=$queue") -Measure
        if ($claim.exhausted -or $claim.item.groupId -cne $group.groupId) {
            throw 'Native claim did not return the expected group.'
        }
        $assessments = @($claim.item.files | ForEach-Object {
            @{ groupId = $claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
        })
        $draft = Join-Path $root 'draft.json'
        if ($mode -ceq 'PerFile') {
            foreach ($assessment in $assessments) {
                [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $assessment))
                $null = Invoke-CoverageOperation -Arguments @(
                    'set-review-assessment', "--queue-path=$queue", "--inventory-path=$inventory",
                    "--file-groups-path=$groupPath", "--assessment-path=$draft"
                ) -Measure -Persistence
            }
            $null = Invoke-CoverageOperation -Arguments @(
                'complete-review-group', "--queue-path=$queue", "--group-id=$($group.groupId)"
            ) -Measure -Persistence
        } else {
            $batch = @{ schemaVersion = 1; snapshot = $claim.snapshot; groupId = $claim.item.groupId; assessments = $assessments; findingIds = @() }
            [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $batch))
            $null = Invoke-CoverageOperation -Arguments @(
                'complete-review-batch', "--queue-path=$queue", "--inventory-path=$inventory",
                "--file-groups-path=$groupPath", "--artifact-path=$($artifact.artifactPath)", "--batch-path=$draft"
            ) -Measure -Persistence
        }
    }
    $timer.Stop()
    $measurement = [ordered]@{
        schemaVersion = 2; mode = $mode; processLaunches = $script:ProcessLaunches
        persistenceProcessLaunches = $script:PersistenceProcessLaunches
        elapsedMilliseconds = $timer.Elapsed.TotalMilliseconds
    }
    Assert-ReviewFileGroupQueueComplete -Path $queue -Group $groups
    if ((Read-ReviewQueueDocument -Path $queue).assessments.Count -ne $files.Count) { throw 'Benchmark lost file coverage.' }
    Assert-WorkflowData -Kind coverageMeasurement -Value $measurement
    $measurement
})
ConvertTo-JsonText -Value $measurements
