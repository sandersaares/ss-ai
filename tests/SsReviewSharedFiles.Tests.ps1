BeforeAll {
    $script:Scripts = Join-Path $PSScriptRoot '..\skills\ss-review\scripts'
    . "$script:Scripts\lib\JsonValue.ps1"
    . "$script:Scripts\lib\PullRequestModel.ps1"
    . "$script:Scripts\lib\WorkflowData.ps1"
    . "$script:Scripts\lib\FindingModel.ps1"
    . "$script:Scripts\lib\ReviewWork.ps1"
    . "$script:Scripts\lib\FocusAreaCacheDiagnostics.ps1"
    . "$PSScriptRoot\lib\ConcurrentReviewCommand.ps1"

    # A native clock signals actual contention before the parent admits each retry.
    function Invoke-ContendedAuditOperation {
        param(
            [Parameter(Mandatory)][string] $Path,
            [Parameter(Mandatory)][ValidateSet('decision', 'feedback')][string] $Operation,
            [Parameter(Mandatory)][object[]] $InputRecord,
            [string] $FindingsPath,
            [string] $FindingId
        )
        $executable = $env:SS_AI_AUDIT_TEST_EXECUTABLE
        if ([string]::IsNullOrWhiteSpace($executable) -or -not [IO.File]::Exists($executable)) {
            throw 'SS_AI_AUDIT_TEST_EXECUTABLE must name the prepared native audit test executable.'
        }
        $processes = [Collections.Generic.List[object]]::new()
        $lock = Open-PullRequestFileLock -Path $Path
        try {
            foreach ($record in $InputRecord) {
                $start = [Diagnostics.ProcessStartInfo]::new($executable)
                $start.WorkingDirectory = Split-Path -Parent $PSScriptRoot
                $start.UseShellExecute = $false
                $start.RedirectStandardInput = $true
                $start.RedirectStandardOutput = $true
                $start.RedirectStandardError = $true
                foreach ($argument in @('native_audit_lock_interop_helper', '--exact', '--nocapture')) {
                    $start.ArgumentList.Add($argument)
                }
                $start.Environment['SS_AI_AUDIT_INTEROP_OPERATION'] = $Operation
                $start.Environment['SS_AI_AUDIT_INTEROP_RECORD'] = $Path
                $start.Environment['SS_AI_AUDIT_INTEROP_INPUT'] = $record.inputPath
                if ($Operation -ceq 'feedback') {
                    $start.Environment['SS_AI_AUDIT_INTEROP_FINDINGS'] = $FindingsPath
                    $start.Environment['SS_AI_AUDIT_INTEROP_FINDING'] = $FindingId
                    $start.Environment['SS_AI_AUDIT_INTEROP_RESPONSE'] = $record.responseId
                }
                $process = [Diagnostics.Process]::Start($start)
                $processes.Add(@{ process = $process; output = $process.StandardOutput.ReadToEndAsync() })
            }
            foreach ($item in $processes) {
                $diagnostics = [Collections.Generic.List[string]]::new()
                while ($null -ne ($line = $item.process.StandardError.ReadLine())) {
                    if ($line -ceq 'lock-contended') { break }
                    $diagnostics.Add($line)
                }
                if ($null -eq $line) {
                    throw "Native child exited before lock contention: $($diagnostics -join [Environment]::NewLine)"
                }
            }
            $lock.Dispose()
            foreach ($item in $processes) {
                $item.process.StandardInput.WriteLine('retry')
                $item.process.StandardInput.Close()
                $errors = $item.process.StandardError.ReadToEnd()
                $item.process.WaitForExit()
                $item.process.ExitCode | Should -Be 0 -Because "$errors $($item.output.GetAwaiter().GetResult())"
            }
        } finally {
            $lock.Dispose()
            foreach ($item in $processes) { $item.process.StandardInput.Close() }
            foreach ($item in $processes) {
                if (-not $item.process.HasExited) {
                    try { $item.process.Kill($true) } catch [InvalidOperationException] { }
                }
                $item.process.WaitForExit()
                $item.process.Dispose()
            }
        }
    }
}

Describe 'Shared workflow file transactions' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $null = [IO.Directory]::CreateDirectory($script:Root)
        Mock Start-Sleep { throw 'Unexpected real-time wait.' }
    }

    It 'keeps an opened snapshot readable while publishing a replacement' {
        $path = Join-Path $script:Root 'snapshot.json'
        Write-PullRequestOutputFile -Path $path -Content '{"version":"original"}'
        $stream = Open-PullRequestFileRead -Path $path
        try {
            Write-PullRequestOutputFile -Path $path -Content '{"version":"replacement"}'
            $reader = [IO.StreamReader]::new($stream)
            try { $reader.ReadToEnd() | Should -BeExactly '{"version":"original"}' } finally { $reader.Dispose() }
            Read-PullRequestFileText -Path $path | Should -BeExactly '{"version":"replacement"}'
        } finally { $stream.Dispose() }
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'waits for a non-sharing reader before replacing a file without truncating it' {
        $path = Join-Path $script:Root 'snapshot.json'
        Write-PullRequestOutputFile -Path $path -Content '{"version":"original"}'
        $script:Reader = [IO.File]::Open($path, 'Open', 'Read', 'Read')
        Mock Start-Sleep { $script:Reader.Dispose() }
        try { Write-PullRequestOutputFile -Path $path -Content '{"version":"replacement"}' }
        finally { $script:Reader.Dispose() }
        Read-PullRequestFileText -Path $path | Should -BeExactly '{"version":"replacement"}'
        # Unix permits rename with an open reader; Windows waits for its missing delete share.
        Should -Invoke Start-Sleep -Times ([int]$IsWindows) -Exactly
    }

    It 'allows overlapping resource readers and makes the exclusive owner wait for both' {
        $path = Join-Path $script:Root 'installation'
        $script:FirstReader = Open-PullRequestFileLock -Path $path -Shared
        $script:SecondReader = Open-PullRequestFileLock -Path $path -Shared
        Mock Start-Sleep {
            $script:FirstReader.Dispose()
            $script:SecondReader.Dispose()
        }
        try {
            $writer = Open-PullRequestFileLock -Path $path
            $writer.Dispose()
        } finally {
            $script:FirstReader.Dispose()
            $script:SecondReader.Dispose()
        }
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'propagates unrelated read and publication failures without retrying' {
        $path = Join-Path $script:Root 'directory'
        $null = [IO.Directory]::CreateDirectory($path)
        { Read-PullRequestFileText -Path $path } | Should -Throw
        { Write-PullRequestOutputFile -Path $path -Content 'must not be published' } | Should -Throw
        Should -Invoke Start-Sleep -Times 0 -Exactly
        @(Get-ChildItem -LiteralPath $script:Root -Filter '*.tmp' -Force).Count | Should -Be 0
    }

    It 'validates the reviewer set without consuming active lock or staging files' {
        $path = Join-Path $script:Root 'reviewer.json'
        $artifact = New-FindingArtifactDocument -Kind reviewer -ReviewerId shared -FocusArea correctness
        Complete-FindingArtifactDocument -Document $artifact
        Write-FindingArtifact -Path $path -Document $artifact
        $staging = Join-Path $script:Root ".reviewer.json.$([guid]::NewGuid().ToString('N')).tmp"
        Write-PullRequestStagingFile -Path $staging -Content '{partial'
        $lock = Open-PullRequestFileLock -Path $path
        try {
            $validation = Get-FindingArtifactValidation -Path $script:Root -ExpectedReviewerId shared
            $validation.isValid | Should -BeTrue
            $validation.artifacts.Count | Should -Be 1
            [IO.File]::Exists($staging) | Should -BeTrue
        } finally { $lock.Dispose() }
    }

    It 'allocates each shared focus-area queue item exactly once across processes' {
        $path = Join-Path $script:Root 'focus-areas.json'
        # Include exhaustion alongside successful claims to catch lost and duplicated removals.
        $items = @(@{ reviewerId = 'a' }, @{ reviewerId = 'b' }, @{ reviewerId = 'c' })
        $queue = New-ReviewQueueDocument -Kind focusAreas -Item $items
        Write-PullRequestOutputFile -Path $path -Content (ConvertTo-JsonText -Value $queue)
        $commands = @(1..4 | ForEach-Object { @{ name = 'Get-NextReviewItem.ps1'; parameters = @{ QueuePath = $path } } })
        $results = @(Invoke-ContendedReviewCommand -Path $path -Command $commands)
        $pages = @(foreach ($result in $results) {
            $result.exitCode | Should -Be 0 -Because $result.errors
            $result.output | ConvertFrom-Json -AsHashtable
        })
        @($pages | Where-Object { -not $_.exhausted } | ForEach-Object { $_.item.reviewerId }) | Should -Be @('a', 'b', 'c')
        @($pages | Where-Object exhausted).Count | Should -Be 1
        (Read-ReviewQueueDocument -Path $path).items.Count | Should -Be 0
    }

    It 'preserves every concurrently appended finding and allocated identifier' {
        $path = Join-Path $script:Root 'findings.json'
        $artifact = New-FindingArtifactDocument -Kind reviewer -ReviewerId shared -FocusArea correctness
        Write-FindingArtifact -Path $path -Document $artifact
        $findings = @(foreach ($name in @('alpha', 'beta', 'gamma')) {
            $findingPath = Join-Path $script:Root "$name.json"
            $finding = @{
                title = $name; description = 'Preserve concurrent work.'
                reasoning = 'Writers share an artifact.'; consequence = 'Updates must not disappear.'
                recommendedAction = 'Serialize the transaction.'; anchor = @{ kind = 'file'; path = 'source.ps1' }
            }
            Write-PullRequestOutputFile -Path $findingPath -Content (ConvertTo-JsonText -Value $finding)
            $findingPath
        })
        # The native helper signals from its injected contention clock, not a
        # PowerShell Start-Sleep mock that cannot observe the native process.
        $nativeTestExecutable = $env:SS_AI_FINDING_TEST_EXECUTABLE
        if ([string]::IsNullOrWhiteSpace($nativeTestExecutable) -or
            -not [IO.File]::Exists($nativeTestExecutable)) {
            throw 'SS_AI_FINDING_TEST_EXECUTABLE must name the prepared native finding test executable.'
        }
        $processes = [Collections.Generic.List[object]]::new()
        $lock = Open-PullRequestFileLock -Path $path
        try {
            foreach ($findingPath in $findings) {
                $start = [Diagnostics.ProcessStartInfo]::new($nativeTestExecutable)
                $start.WorkingDirectory = Split-Path -Parent $PSScriptRoot
                $start.UseShellExecute = $false
                $start.RedirectStandardInput = $true
                $start.RedirectStandardOutput = $true
                $start.RedirectStandardError = $true
                foreach ($argument in @('native_process_lock_interop_helper', '--exact', '--nocapture')) {
                    $start.ArgumentList.Add($argument)
                }
                $start.Environment['SS_AI_TEST_INTEROP_ARTIFACT'] = $path
                $start.Environment['SS_AI_TEST_INTEROP_FINDING'] = $findingPath
                $process = [Diagnostics.Process]::Start($start)
                $processes.Add(@{ process = $process; output = $process.StandardOutput.ReadToEndAsync() })
            }
            foreach ($item in $processes) {
                $diagnostics = [Collections.Generic.List[string]]::new()
                while ($null -ne ($line = $item.process.StandardError.ReadLine())) {
                    if ($line -ceq 'lock-contended') { break }
                    $diagnostics.Add($line)
                }
                if ($null -eq $line) { throw "Native child exited before lock contention: $($diagnostics -join [Environment]::NewLine)" }
            }
            $lock.Dispose()
            foreach ($item in $processes) {
                $item.process.StandardInput.WriteLine('retry')
                $item.process.StandardInput.Close()
                $errors = $item.process.StandardError.ReadToEnd()
                $item.process.WaitForExit()
                $item.process.ExitCode | Should -Be 0 -Because "$errors $($item.output.GetAwaiter().GetResult())"
            }
        } finally {
            $lock.Dispose()
            foreach ($item in $processes) { $item.process.StandardInput.Close() }
            foreach ($item in $processes) {
                $item.process.WaitForExit()
                $item.process.Dispose()
            }
        }
        $saved = Read-FindingArtifact -Path $path
        @($saved.findings.title) | Should -Be @('alpha', 'beta', 'gamma')
        @($saved.findings.id | Sort-Object -Unique).Count | Should -Be $findings.Count
        @($saved.findings.id) | Should -Be @('shared-1', 'shared-2', 'shared-3')
    }

    It 'initializes and appends the shared decision log without losing concurrent decisions' {
        $path = Get-FindingDecisionPath -OutputDirectory $script:Root
        $inputs = @(foreach ($id in @('F1', 'F2', 'F3')) {
            $planPath = Join-Path $script:Root "$id.json"
            $plan = @{ schemaVersion = 1; decisions = @(@{
                findingId = $id; decision = 'needs-audit'; reason = 'Requires a human decision.'
            }) }
            Write-PullRequestOutputFile -Path $planPath -Content (ConvertTo-JsonText -Value $plan)
            @{ inputPath = $planPath }
        })
        Invoke-ContendedAuditOperation -Path $path -Operation decision -InputRecord $inputs
        $saved = Read-FindingDecisionDocument -Path $path
        @($saved.decisions.findingId) | Should -Be @('F1', 'F2', 'F3')
    }

    It 'merges cache diagnostics against the state committed by a contending writer' {
        $path = Get-FocusAreaCacheDiagnosticsPath -SessionStateDirectory $script:Root
        $script:Lock = Open-PullRequestFileLock -Path $path
        Mock Start-Sleep {
            $script:Lock.Dispose()
            Write-FocusAreaCacheDiagnostics -SessionStateDirectory $script:Root -Diagnostics @{ cache = 'Hit'; reason = $null }
        }
        try {
            Write-FocusAreaCacheDiagnostics -SessionStateDirectory $script:Root -Merge -Diagnostics @{ store = 'Existing' }
        } finally { $script:Lock.Dispose() }
        $record = Get-FocusAreaCacheDiagnostics -SessionStateDirectory $script:Root
        $record.cache | Should -BeExactly 'Hit'
        $record.store | Should -BeExactly 'Existing'
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'preserves concurrent first feedback responses in arrival order' {
        $path = Join-Path $script:Root 'audit-feedback.json'
        $artifactPath = Join-Path $script:Root 'findings.json'
        $artifact = New-FindingArtifactDocument -Kind reviewer -ReviewerId shared -FocusArea correctness
        $id = Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding @{
            title = 'Concurrent responses'; description = 'Preserve every response.'
            reasoning = 'Feedback can arrive concurrently.'; consequence = 'No response may disappear.'
            recommendedAction = 'Initialize and append together.'; anchor = @{ kind = 'file'; path = 'source.ps1' }
        }
        Write-FindingArtifact -Path $artifactPath -Document $artifact
        $inputs = @(foreach ($response in @('first', 'second', 'third')) {
            $textPath = Join-Path $script:Root "$response.txt"
            Write-PullRequestOutputFile -Path $textPath -Content $response
            @{ inputPath = $textPath; responseId = $response }
        })
        Invoke-ContendedAuditOperation -Path $path -Operation feedback -InputRecord $inputs `
            -FindingsPath $artifactPath -FindingId $id
        $record = Read-WorkflowData -Kind auditFeedback -Path $path
        @($record.feedback.responseId) | Should -Be @('first', 'second', 'third')
        @($record.feedback.text) | Should -Be @('first', 'second', 'third')
    }
}
