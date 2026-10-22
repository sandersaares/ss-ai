#Requires -Version 7.6

<#
.SYNOPSIS
Measures per-file and batch coverage persistence on the issue #125 inventory size.
.DESCRIPTION
Creates an isolated fixture in a new StateDirectory. Each persistence operation uses a fresh
PowerShell process. Reports validated coverageMeasurement records; setup is excluded from timing.
Schema validation is measured separately from worker elapsed time (which includes startup,
serialization, I/O and semantic checks). Claims run in the driver and count as journal writes.
Fixtures remain available for inspection. Child operation parameters are used by the driver.
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $StateDirectory,
    [ValidateSet('Compare', 'File', 'Batch', 'Complete')][string] $Operation = 'Compare',
    [string] $ItemPath,
    [string] $GroupId
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

$script:SchemaValidationMilliseconds = 0.0
$script:JournalWrites = 0
$script:ValidateWorkflow = ${function:Assert-WorkflowData}
$script:WriteAtomically = ${function:Write-PullRequestOutputFileAtomically}

# Instrument the real schema boundary without changing production behavior.
function Assert-WorkflowData {
    param([Parameter(Mandatory)][string] $Kind, [Parameter(Mandatory)][AllowNull()] $Value)
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try { & $script:ValidateWorkflow -Kind $Kind -Value $Value }
    finally { $script:SchemaValidationMilliseconds += $timer.Elapsed.TotalMilliseconds }
}

# Count committed journal replacements, excluding drafts, immutable inputs and finding artifacts.
function Write-PullRequestOutputFileAtomically {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][AllowEmptyString()][string] $Content)
    & $script:WriteAtomically -Path $Path -Content $Content
    if ([IO.Path]::GetFileName($Path) -ceq 'queue.json') { $script:JournalWrites++ }
}

# Native argument lists keep child measurements comparable to agent-facing process invocation.
function Invoke-CoverageMeasurementChild {
    param([string] $Root, [string] $Action, [string] $Draft = '', [string] $Group = '')
    $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', $PSCommandPath, '-StateDirectory', $Root, '-Operation', $Action)) {
        $start.ArgumentList.Add($argument)
    }
    if ($Draft) { $start.ArgumentList.Add('-ItemPath'); $start.ArgumentList.Add($Draft) }
    if ($Group) { $start.ArgumentList.Add('-GroupId'); $start.ArgumentList.Add($Group) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw $stderr.GetAwaiter().GetResult() }
        $measurement = ConvertFrom-JsonVerbatim -Text $stdout.GetAwaiter().GetResult()
        & $script:ValidateWorkflow -Kind coverageMeasurement -Value $measurement
        $measurement
    } finally { $process.Dispose() }
}

$StateDirectory = Resolve-PullRequestPath -Path $StateDirectory
if ($Operation -cne 'Compare') {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $arguments = @{
        QueuePath = Join-Path $StateDirectory 'queue.json'
        InventoryPath = Join-Path $StateDirectory 'inventory.json'
        FileGroupsPath = Join-Path $StateDirectory 'groups.json'
    }
    switch ($Operation) {
        'File' {
            $assessment = Read-WorkflowData -Kind assessment -Path $ItemPath
            Set-ReviewFileAssessment @arguments -Assessment $assessment
        }
        'Batch' {
            $batch = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($ItemPath))
            Set-ReviewAssessmentBatch @arguments -ArtifactPath (Join-Path $StateDirectory 'findings.json') -Batch $batch | Out-Null
        }
        'Complete' { Complete-ReviewFileGroup -QueuePath $arguments.QueuePath -GroupId $GroupId | Out-Null }
    }
    $measurement = [ordered]@{
        schemaVersion = 1; mode = if ($Operation -ceq 'Batch') { 'Batch' } else { 'PerFile' }
        processLaunches = 1; journalWrites = $script:JournalWrites
        schemaValidationMilliseconds = $script:SchemaValidationMilliseconds
        elapsedMilliseconds = $timer.Elapsed.TotalMilliseconds
    }
    & $script:ValidateWorkflow -Kind coverageMeasurement -Value $measurement
    ConvertTo-JsonText -Value $measurement
    return
}

New-Item -ItemType Directory -Path $StateDirectory -ErrorAction Stop | Out-Null
$measurements = @(foreach ($mode in @('PerFile', 'Batch')) {
    $root = Join-Path $StateDirectory $mode
    [void] [IO.Directory]::CreateDirectory($root)
    # Match the reported review size; include removal and rename-side entries without real files.
    $files = @(foreach ($index in 0..120) {
        @{ path = Join-Path $root "source-$index.rs"; sizeBytes = 1; change = if ($index -eq 0) { 'deleted' } elseif ($index -eq 1) { 'new' } else { 'modified' } }
    })
    $groups = @(for ($offset = 0; $offset -lt $files.Count; $offset += 20) {
        # Use the permitted file-count bound from ss-review/docs/design.md, "Review coverage".
        @{ groupId = "group-$offset"; files = @($files | Select-Object -Skip $offset -First 20) }
    })
    $arguments = @{ QueuePath = Join-Path $root 'queue.json'; InventoryPath = Join-Path $root 'inventory.json'; FileGroupsPath = Join-Path $root 'groups.json' }
    Write-PullRequestOutputFileAtomically -Path $arguments.InventoryPath -Content (ConvertTo-JsonText -Value $files)
    Write-PullRequestOutputFileAtomically -Path $arguments.FileGroupsPath -Content (ConvertTo-JsonText -Value $groups)
    New-ReviewFileGroupQueue @arguments | Out-Null
    Write-FindingArtifact -Path (Join-Path $root 'findings.json') -Document (New-FindingArtifactDocument -Kind reviewer -ReviewerId focus-001 -FocusArea benchmark)
    $script:SchemaValidationMilliseconds = 0.0
    $script:JournalWrites = 0
    $childValidation = 0.0
    $childWrites = 0
    $processes = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    foreach ($group in $groups) {
        $claim = Get-NextReviewWorkItem -QueuePath $arguments.QueuePath
        $assessments = @($claim.item.files | ForEach-Object {
            @{ groupId = $claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
        })
        $draft = Join-Path $root 'draft.json'
        if ($mode -ceq 'PerFile') {
            foreach ($assessment in $assessments) {
                [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $assessment))
                $result = Invoke-CoverageMeasurementChild -Root $root -Action File -Draft $draft
                $childValidation += $result.schemaValidationMilliseconds
                $childWrites += $result.journalWrites
                $processes += $result.processLaunches
            }
            $result = Invoke-CoverageMeasurementChild -Root $root -Action Complete -Group $group.groupId
        } else {
            $batch = @{ schemaVersion = 1; snapshot = $claim.snapshot; groupId = $claim.item.groupId; assessments = $assessments; findingIds = @() }
            [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $batch))
            $result = Invoke-CoverageMeasurementChild -Root $root -Action Batch -Draft $draft
        }
        $childValidation += $result.schemaValidationMilliseconds
        $childWrites += $result.journalWrites
        $processes += $result.processLaunches
    }
    $timer.Stop()
    $measurement = [ordered]@{
        schemaVersion = 1; mode = $mode; processLaunches = $processes
        journalWrites = $script:JournalWrites + $childWrites
        schemaValidationMilliseconds = $script:SchemaValidationMilliseconds + $childValidation
        elapsedMilliseconds = $timer.Elapsed.TotalMilliseconds
    }
    Assert-ReviewFileGroupQueueComplete -Path $arguments.QueuePath -Group $groups
    if ((Read-ReviewQueueDocument -Path $arguments.QueuePath).assessments.Count -ne $files.Count) { throw 'Benchmark lost file coverage.' }
    & $script:ValidateWorkflow -Kind coverageMeasurement -Value $measurement
    $measurement
})
ConvertTo-JsonText -Value $measurements
