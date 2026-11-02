#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Measure = Join-Path $script:Repo 'dev\ss-review\Measure-ReviewCoverage.ps1'
    $script:MeasurementSchema = @'
{
  "type": "object",
  "additionalProperties": false,
  "required": ["schemaVersion", "mode", "processLaunches", "persistenceProcessLaunches", "elapsedMilliseconds"],
  "properties": {
    "schemaVersion": { "const": 2 },
    "mode": { "enum": ["PerFile", "Batch"] },
    "processLaunches": { "type": "integer", "minimum": 0 },
    "persistenceProcessLaunches": { "type": "integer", "minimum": 0 },
    "elapsedMilliseconds": { "type": "number", "minimum": 0 }
  }
}
'@

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
        @($script:Results.mode | Sort-Object) | Should -Be @('Batch', 'PerFile')
        foreach ($result in $script:Results) {
            Test-Json -Json (ConvertTo-Json -InputObject $result -Depth 100) -Schema $script:MeasurementSchema | Should -BeTrue
            $root = Join-Path $script:State $result.mode
            $files = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Join-Path $root 'inventory.json'))) -AsHashtable -NoEnumerate -DateKind String
            $groups = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Join-Path $root 'groups.json'))) -AsHashtable -NoEnumerate -DateKind String
            $journal = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Join-Path $root 'queue.json'))) -AsHashtable -DateKind String
            # The benchmark reproduces the reported inventory size and bounded group size.
            $files.Count | Should -Be 121
            $groups.Count | Should -Be 7
            $journal.items.Count | Should -Be 0
            $journal.inProgress | Should -BeNullOrEmpty
            $journal.failure | Should -BeNullOrEmpty
            $journal.assessments.Count | Should -Be $files.Count
            $journal.completedGroupIds.Count | Should -Be $groups.Count
            foreach ($group in $groups) {
                $journal.completedGroupIds | Should -Contain $group.groupId
                foreach ($file in $group.files) {
                    @($journal.assessments | Where-Object { $_.groupId -ceq $group.groupId -and $_.path -ceq $file.path -and $_.disposition -ceq 'reviewed' }).Count | Should -Be 1
                }
            }
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
            Test-Json -Json (ConvertTo-Json -InputObject $legacy -Depth 100) -Schema $script:MeasurementSchema -ErrorAction SilentlyContinue | Should -BeFalse
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
