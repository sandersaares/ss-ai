#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'

    function Invoke-WorkCommand {
        param([string[]] $ArgumentList)
        $executable = Get-Command ss-ai -CommandType Application | Select-Object -First 1
        $start = [Diagnostics.ProcessStartInfo]::new($executable.Source)
        foreach ($argument in (@('workflow') + $ArgumentList)) {
            $start.ArgumentList.Add($argument)
        }
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $output.GetAwaiter().GetResult()
                errors = $errors.GetAwaiter().GetResult()
            }
        } finally {
            $process.Dispose()
        }
    }
    function Invoke-WorkJson {
        param([string[]] $ArgumentList)
        $result = Invoke-WorkCommand -ArgumentList $ArgumentList
        $result.exitCode | Should -Be 0 -Because $result.errors
        $result.errors | Should -BeNullOrEmpty
        $result.output | ConvertFrom-Json
    }

    function Write-WorkJson {
        param([string] $Path, $Value)
        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 30))
    }
}

Describe 'Native review work CLI' {
    BeforeEach {
        $script:AlternatePaths = $false
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        $script:Areas = Join-Path $script:Root 'focus-areas'
        [void] [IO.Directory]::CreateDirectory($script:Areas)
        [IO.File]::WriteAllText((Join-Path $script:Areas 'area.md'), 'Review correctness.')
        $script:Files = @(
            @{ path = Join-Path $script:Root 'new [file].rs'; sizeBytes = 1; change = 'new' }
            @{ path = Join-Path $script:Root 'deleted.rs'; sizeBytes = 3; change = 'deleted' }
        )
        $script:Groups = @(
            @{ groupId = 'group-001'; files = @($script:Files[0]) }
            @{ groupId = 'group-002'; files = @($script:Files[1]) }
        )
        $script:Inventory = Join-Path $script:Root 'inventory.json'
        $script:GroupedInventory = Join-Path $script:Root 'groups.json'
        Write-WorkJson -Path $script:Inventory -Value $script:Files
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        $script:Work = Join-Path $script:Root 'work'
        $script:Findings = Join-Path $script:Root 'findings'
    }

    It 'requires complete preparation binding without falling back after failure' {
        $arguments = @(
            'create-review-work', '--inventory-path', $script:Inventory,
            '--file-groups-path', $script:GroupedInventory,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $partial = Invoke-WorkCommand -ArgumentList ($arguments + @('--session-state', $script:Root))
        $partial.exitCode | Should -Not -Be 0
        $partial.output | Should -BeNullOrEmpty
        $partial.errors | Should -Match 'preparation'
        Test-Path -LiteralPath $script:Work | Should -BeFalse

        $unready = Invoke-WorkCommand -ArgumentList (
            $arguments + @('--session-state', $script:Root, '--preparation-identity', 'not-ready')
        )
        $unready.exitCode | Should -Not -Be 0
        $unready.output | Should -BeNullOrEmpty
        Test-Path -LiteralPath $script:Work | Should -BeFalse
        (Invoke-WorkJson -ArgumentList $arguments).reviewerCount | Should -Be 1
    }

    It 'reports an omitted deleted path without creating review work' {
        Write-WorkJson -Path $script:GroupedInventory -Value @($script:Groups[0])
        $result = Invoke-WorkCommand -ArgumentList @(
            'create-review-work', '--inventory-path', $script:Inventory,
            '--file-groups-path', $script:GroupedInventory,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.errors | Should -Match 'deleted\.rs'
        Test-Path -LiteralPath $script:Work | Should -BeFalse
    }

    It 'publishes inputs, journals coverage and findings, and validates the finished work' {
        $inventory = Join-Path $script:Root 'published-inventory.json'
        $groups = Join-Path $script:Root 'published-groups.json'
        $input = Invoke-WorkJson -ArgumentList @(
            'set-review-input', '--kind', 'inventory', '--input-path', $script:Inventory, '--path', $inventory
        )
        $input.count | Should -Be $script:Files.Count
        $input = Invoke-WorkJson -ArgumentList @(
            'set-review-input', '--kind', 'groups', '--input-path', $script:GroupedInventory,
            '--path', $groups, '--inventory-path', $inventory
        )
        $input.count | Should -Be $script:Groups.Count
        $work = Invoke-WorkJson -ArgumentList @(
            'create-review-work', '--inventory-path', $inventory, '--file-groups-path', $groups,
            '--focus-area-directory', $script:Areas, '--output-directory', $script:Work
        )
        $work.fileCount | Should -Be $script:Files.Count
        $work.groupCount | Should -Be $script:Groups.Count
        $manifest = Get-Content -LiteralPath $work.manifestPath -Raw | ConvertFrom-Json
        $claim = Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $work.focusAreaQueuePath)
        $worker = $claim.item
        $worker.reviewerId | Should -BeExactly $manifest.reviewers[0].reviewerId
        (Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $work.focusAreaQueuePath)).exhausted | Should -BeTrue

        $queue = $worker.fileGroupQueuePath
        $queueState = Invoke-WorkJson -ArgumentList @(
            'initialize-review-queue', '--inventory-path', $work.inventoryPath,
            '--file-groups-path', $work.fileGroupsPath, '--queue-path', $queue
        )
        $queueState.groupCount | Should -Be $script:Groups.Count
        $queueState.fileCount | Should -Be $script:Files.Count
        $incomplete = Invoke-WorkJson -ArgumentList @(
            'validate-review-work', '--artifact-directory', $script:Findings, '--manifest-path', $work.manifestPath
        )
        $incomplete.isValid | Should -BeFalse
        @($incomplete.missingReviewers) | Should -Be @($worker.reviewerId)
        $created = Invoke-WorkJson -ArgumentList @(
            'initialize-finding', '--output-directory', $script:Findings,
            '--reviewer-id', $worker.reviewerId, '--focus-area', $worker.focusArea, '--queue-path', $queue
        )
        $artifact = $created.artifactPath
        $premature = Invoke-WorkCommand -ArgumentList @('complete-finding', '--artifact-path', $artifact, '--queue-path', $queue)
        $premature.exitCode | Should -Not -Be 0
        $premature.output | Should -BeNullOrEmpty
        (Get-Content -LiteralPath $artifact -Raw | ConvertFrom-Json).status | Should -Be 'open'

        $finding = Join-Path $script:Root 'finding.json'
        Write-WorkJson -Path $finding -Value @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'
            anchor = @{ kind = 'file'; path = 'new [file].rs' }
        }
        $findingId = (Invoke-WorkJson -ArgumentList @(
            'add-finding', '--artifact-path', $artifact, '--finding-path', $finding, '--origin', 'reviewer'
        )).findingId
        foreach ($group in $script:Groups) {
            $claim = Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $queue)
            $claim.exhausted | Should -BeFalse
            $claim.item.groupId | Should -BeExactly $group.groupId
            @($claim.item.files.path) | Should -Be @($group.files.path)
            $batch = @{
                schemaVersion = 1; snapshot = $claim.snapshot; groupId = $claim.item.groupId
                assessments = @($claim.item.files | ForEach-Object {
                    @{ groupId = $claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
                })
                findingIds = @(if ($group.groupId -eq 'group-001') { $findingId })
            }
            $batchPath = Join-Path $script:Root "$($group.groupId).json"
            Write-WorkJson -Path $batchPath -Value $batch
            $completed = Invoke-WorkJson -ArgumentList @(
                'complete-review-batch', '--queue-path', $queue, '--inventory-path', $work.inventoryPath,
                '--file-groups-path', $work.fileGroupsPath, '--artifact-path', $artifact, '--batch-path', $batchPath
            )
            $completed.groupId | Should -BeExactly $group.groupId
        }
        (Invoke-WorkJson -ArgumentList @('next-review-item', '--queue-path', $queue)).exhausted | Should -BeTrue
        (Invoke-WorkJson -ArgumentList @('complete-finding', '--artifact-path', $artifact, '--queue-path', $queue)).status | Should -BeExactly 'complete'
        $validation = Invoke-WorkJson -ArgumentList @(
            'validate-review-work', '--artifact-directory', $script:Findings, '--manifest-path', $work.manifestPath
        )
        $validation.isValid | Should -BeTrue
        $finished = Get-Content -LiteralPath $artifact -Raw | ConvertFrom-Json
        $finished.findings[0].id | Should -BeExactly $findingId
        (Get-Content -LiteralPath $queue -Raw | ConvertFrom-Json).completedGroupIds.Count | Should -Be $script:Groups.Count
    }
}
