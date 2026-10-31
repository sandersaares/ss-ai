#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Scripts = Join-Path $script:Repo 'skills\ss-review\scripts'
    $script:Measure = Join-Path $script:Repo 'dev\ss-review\Measure-ReviewCoverage.ps1'
    . "$script:Scripts\lib\JsonValue.ps1"
    . "$script:Scripts\lib\PullRequestModel.ps1"
    . "$script:Scripts\lib\WorkflowData.ps1"
    . "$script:Scripts\lib\FindingModel.ps1"
    . "$script:Scripts\lib\ReviewWork.ps1"

    # Keep retained fixture files under the checkout and preserve unusual path characters.
    $script:State = Join-Path $script:Repo (".coverage-measurement O'Brien é " + [guid]::NewGuid().ToString('N'))
    $script:Results = @(& $script:Measure -StateDirectory $script:State | ConvertFrom-Json -AsHashtable)
}

AfterAll {
    if ($script:State -and (Test-Path -LiteralPath $script:State)) {
        Remove-Item -LiteralPath $script:State -Recurse -Force
    }
}

Describe 'Native coverage measurement' {
    It 'counts the launched claim and persistence operations for each complete journal' {
        $script:Results.Count | Should -Be 2
        foreach ($result in $script:Results) {
            Assert-WorkflowData -Kind coverageMeasurement -Value $result
            $root = Join-Path $script:State $result.mode
            $files = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText((Join-Path $root 'inventory.json')))
            $groups = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText((Join-Path $root 'groups.json')))
            $journal = Read-ReviewQueueDocument -Path (Join-Path $root 'queue.json')
            Assert-ReviewFileGroupQueueComplete -Path (Join-Path $root 'queue.json') -Group $groups
            $journal.assessments.Count | Should -Be $files.Count
            $journal.completedGroupIds.Count | Should -Be $groups.Count
            $expectedPersistence = if ($result.mode -ceq 'PerFile') { $files.Count + $groups.Count } else { $groups.Count }
            $result.persistenceProcessLaunches | Should -Be $expectedPersistence
            $result.processLaunches | Should -Be ($expectedPersistence + $groups.Count)
            $expectedReceipts = if ($result.mode -ceq 'Batch') { $groups.Count } else { 0 }
            $journal.batchReceipts.Count | Should -Be $expectedReceipts
        }
    }

    It 'does not report native schema timings or unobserved journal-write counts' {
        foreach ($result in $script:Results) {
            $result.schemaVersion | Should -Be 2
            $result.ContainsKey('schemaValidationMilliseconds') | Should -BeFalse
            $result.ContainsKey('journalWrites') | Should -BeFalse
            $legacy = @{
                schemaVersion = 1; mode = $result.mode; processLaunches = $result.processLaunches
                journalWrites = 0; schemaValidationMilliseconds = 0; elapsedMilliseconds = 0
            }
            { Assert-WorkflowData -Kind coverageMeasurement -Value $legacy } | Should -Throw
        }
    }

    It 'refuses an existing measurement directory without altering its journal' {
        $queue = Join-Path $script:State 'Batch\queue.json'
        $before = [IO.File]::ReadAllBytes($queue)
        { & $script:Measure -StateDirectory $script:State } | Should -Throw
        [Convert]::ToHexString([IO.File]::ReadAllBytes($queue)) | Should -BeExactly ([Convert]::ToHexString($before))
    }

    It 'fails before creating a fixture when the native executable is unavailable' {
        $emptyPath = Join-Path $script:State 'empty-path'
        [void] [IO.Directory]::CreateDirectory($emptyPath)
        $destination = Join-Path $script:State 'no-executable'
        $originalPath = $env:PATH
        try {
            $env:PATH = $emptyPath
            { & $script:Measure -StateDirectory $destination } | Should -Throw
            [IO.Directory]::Exists($destination) | Should -BeFalse
        } finally {
            $env:PATH = $originalPath
        }
    }
}
