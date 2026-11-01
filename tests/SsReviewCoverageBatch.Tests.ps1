#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.coverage-batch-' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:Root)

    # Keep arguments separate and capture errors without changing the parent process's failure policy.
    function Invoke-Workflow {
        param([string[]] $Arguments, [switch] $ExpectFailure)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $output = $stdout.GetAwaiter().GetResult()
            $errorOutput = $stderr.GetAwaiter().GetResult()
            if ($ExpectFailure) {
                if ($process.ExitCode -eq 0) { throw "Expected command failure: $output" }
                return
            }
            if ($process.ExitCode -ne 0) { throw "Native command failed: $errorOutput" }
            return (ConvertFrom-Json -InputObject $output -AsHashtable -ErrorAction Stop)
        } finally {
            $process.Dispose()
        }
    }

    function Write-TestJson {
        param([string] $Path, $Value)

        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 100), [Text.UTF8Encoding]::new($false))
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) {
        [IO.Directory]::Delete($script:Root, $true)
    }
}

Describe 'Native assessment batch lifecycle' {
    It 'preserves failure, resume, receipt retry and withdrawal <When>' -ForEach @(
        @{ When = 'before batch'; WithdrawBeforeBatch = $true }
        @{ When = 'after batch'; WithdrawBeforeBatch = $false }
    ) {
        $root = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        [void] [IO.Directory]::CreateDirectory($root)
        $inventory = Join-Path $root 'inventory.json'
        $groups = Join-Path $root 'groups.json'
        $queue = Join-Path $root 'queue.json'
        $findings = Join-Path $root 'findings'
        $draft = Join-Path $root 'batch.json'
        $findingPath = Join-Path $root 'finding.json'
        $plan = Join-Path $root 'removals.json'
        $files = @(
            @{ path = Join-Path $root 'old.rs'; sizeBytes = 1; change = 'deleted' }
            @{ path = Join-Path $root 'new.ps1'; sizeBytes = 1; change = 'new' }
            @{ path = Join-Path $root 'lib.rs'; sizeBytes = 1; change = 'modified' }
        )
        Write-TestJson -Path $inventory -Value $files
        Write-TestJson -Path $groups -Value @(
            @{ groupId = 'rename'; files = @($files[0], $files[1]) }
            @{ groupId = 'other'; files = @($files[2]) }
        )
        $queueArgs = @('--queue-path', $queue, '--inventory-path', $inventory, '--file-groups-path', $groups)
        $state = Invoke-Workflow -Arguments (@('initialize-review-queue') + $queueArgs)
        $artifact = (Invoke-Workflow -Arguments @(
            'initialize-finding', '--output-directory', $findings, '--reviewer-id', 'focus-001', '--focus-area', 'rules'
        )).artifactPath
        Write-TestJson -Path $findingPath -Value @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'; anchor = @{ kind = 'file'; path = 'old.rs' }
        }
        $id = (Invoke-Workflow -Arguments @(
            'add-finding', '--artifact-path', $artifact, '--finding-path', $findingPath, '--origin', 'reviewer'
        )).findingId
        $findingBefore = [IO.File]::ReadAllBytes($artifact)
        $claim = Invoke-Workflow -Arguments @('next-review-item', '--queue-path', $queue)
        $claim.item.groupId | Should -BeExactly 'rename'
        $batch = @{
            schemaVersion = 1; snapshot = $state.snapshot; groupId = 'rename'; findingIds = @($id)
            assessments = @($claim.item.files | ForEach-Object {
                @{ groupId = 'rename'; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
            })
        }
        Write-TestJson -Path $draft -Value $batch
        $batchArgs = @('complete-review-batch') + $queueArgs + @('--artifact-path', $artifact, '--batch-path', $draft)
        $failure = Invoke-Workflow -Arguments @('fail-review-worker', '--queue-path', $queue, '--reason', 'Worker stopped.')
        $failure.status | Should -BeExactly 'Failed'
        $failedBytes = [IO.File]::ReadAllBytes($queue)
        Invoke-Workflow -Arguments @('next-review-item', '--queue-path', $queue) -ExpectFailure
        Invoke-Workflow -Arguments $batchArgs -ExpectFailure
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($queue)) | Should -BeExactly ([Convert]::ToBase64String($failedBytes))
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($artifact)) | Should -BeExactly ([Convert]::ToBase64String($findingBefore))
        Invoke-Workflow -Arguments (@('initialize-review-queue') + $queueArgs + @('--resume')) | Out-Null
        (Invoke-Workflow -Arguments @('next-review-item', '--queue-path', $queue)).item.groupId | Should -BeExactly 'rename'

        Write-TestJson -Path $plan -Value @{ schemaVersion = 1; removals = @(@{ findingId = $id; reason = 'Evidence does not support it.' }) }
        $removeArgs = @('remove-finding', '--artifact-path', $artifact, '--plan-path', $plan, '--now-utc', '2024-05-06T07:08:09Z')
        if ($WithdrawBeforeBatch) { (Invoke-Workflow -Arguments $removeArgs).removed | Should -Be @($id) }
        $first = Invoke-Workflow -Arguments $batchArgs
        $first.groupId | Should -BeExactly 'rename'
        $first.assessmentCount | Should -Be 2
        $first.alreadyComplete | Should -BeFalse
        if (-not $WithdrawBeforeBatch) {
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($artifact)) | Should -BeExactly ([Convert]::ToBase64String($findingBefore))
        }
        if (-not $WithdrawBeforeBatch) { (Invoke-Workflow -Arguments $removeArgs).removed | Should -Be @($id) }
        $next = Invoke-Workflow -Arguments @('next-review-item', '--queue-path', $queue)
        $next.item.groupId | Should -BeExactly 'other'
        $claimedBytes = [IO.File]::ReadAllBytes($queue)
        (Invoke-Workflow -Arguments $batchArgs).alreadyComplete | Should -BeTrue
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($queue)) | Should -BeExactly ([Convert]::ToBase64String($claimedBytes))
        $other = @{
            schemaVersion = 1; snapshot = $state.snapshot; groupId = 'other'; findingIds = @()
            assessments = @(@{ groupId = 'other'; path = $files[2].path; disposition = 'reviewed'; scope = ''; reason = '' })
        }
        $otherDraft = Join-Path $root 'other.json'
        Write-TestJson -Path $otherDraft -Value $other
        Invoke-Workflow -Arguments (@('complete-review-batch') + $queueArgs + @('--artifact-path', $artifact, '--batch-path', $otherDraft)) | Out-Null
        $beforeRetry = [IO.File]::ReadAllBytes($queue)
        (Invoke-Workflow -Arguments $batchArgs).alreadyComplete | Should -BeTrue
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($queue)) | Should -BeExactly ([Convert]::ToBase64String($beforeRetry))
        $result = Invoke-Workflow -Arguments @('complete-finding', '--artifact-path', $artifact, '--queue-path', $queue)
        $result.status | Should -BeExactly 'complete'
        $result.findingCount | Should -Be 0
        $document = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($artifact)) -AsHashtable
        $document.removed[0].finding.id | Should -BeExactly $id
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($queue)) | Should -BeExactly ([Convert]::ToBase64String($beforeRetry))
    }
}
