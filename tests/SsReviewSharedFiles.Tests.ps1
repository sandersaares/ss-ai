#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    # A .NET owner and native transactions must use the same retained sibling lock.
    function Invoke-ContendedNativeTransactions {
        param(
            [string] $Path,
            [string] $Executable,
            [string] $Helper,
            [object[]] $Environment
        )
        if ([string]::IsNullOrWhiteSpace($Executable) -or -not [IO.File]::Exists($Executable)) {
            throw "Prepared native test executable is missing: $Executable"
        }
        $processes = [Collections.Generic.List[object]]::new()
        $results = [Collections.Generic.List[string]]::new()
        $lock = [IO.File]::Open("$Path.lock", [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        try {
            foreach ($variables in $Environment) {
                $start = [Diagnostics.ProcessStartInfo]::new($Executable)
                $start.WorkingDirectory = Split-Path -Parent $PSScriptRoot
                $start.UseShellExecute = $false
                $start.RedirectStandardInput = $true
                $start.RedirectStandardOutput = $true
                $start.RedirectStandardError = $true
                foreach ($argument in @($Helper, '--exact', '--nocapture')) {
                    $start.ArgumentList.Add($argument)
                }
                foreach ($entry in $variables.GetEnumerator()) {
                    $start.Environment[$entry.Key] = $entry.Value
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
            # Complete each retry before releasing the next; arrival order is explicit.
            foreach ($item in $processes) {
                $item.process.StandardInput.WriteLine('retry')
                $item.process.StandardInput.Close()
                $errors = $item.process.StandardError.ReadToEnd()
                $item.process.WaitForExit()
                $item.process.ExitCode | Should -Be 0 -Because "$errors $($item.output.GetAwaiter().GetResult())"
                $results.Add($errors)
            }
            $results.ToArray()
        } finally {
            $lock.Dispose()
            foreach ($item in $processes) { $item.process.StandardInput.Close() }
            foreach ($item in $processes) {
                if (-not $item.process.HasExited) { $item.process.Kill($true) }
                $item.process.WaitForExit()
                $item.process.Dispose()
            }
        }
    }

    function Write-TestJson {
        param([string] $Path, [object] $Value)
        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 30 -Compress),
            [Text.UTF8Encoding]::new($false))
    }

    function Read-TestJson {
        param([string] $Path)
        ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($Path))
    }
}

Describe 'Shared workflow file transactions' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $null = [IO.Directory]::CreateDirectory($script:Root)
    }

    It 'allocates each shared focus-area queue item exactly once across processes' {
        $path = Join-Path $script:Root 'focus-areas.json'
        Write-TestJson -Path $path -Value @{
            schemaVersion = 1; kind = 'focusAreas'
            items = @(@{ reviewerId = 'a' }, @{ reviewerId = 'b' }, @{ reviewerId = 'c' })
        }
        $variables = @(1..4 | ForEach-Object { @{ SS_AI_REVIEW_WORK_INTEROP_QUEUE = $path } })
        $outputs = @(Invoke-ContendedNativeTransactions -Path $path -Executable $env:SS_AI_REVIEW_WORK_TEST_EXECUTABLE `
            -Helper 'native_review_work_lock_interop_helper' -Environment $variables)
        $pages = @($outputs | ForEach-Object {
            $matches = [regex]::Matches($_, '(?m)^result-json:(.+)\r?$')
            $matches.Count | Should -Be 1
            ConvertFrom-Json -InputObject $matches[0].Groups[1].Value
        })
        @($pages | Where-Object { -not $_.exhausted } | ForEach-Object { $_.item.reviewerId } | Sort-Object) | Should -Be @('a', 'b', 'c')
        @($pages | Where-Object exhausted).Count | Should -Be 1
        @( (Read-TestJson -Path $path).items ).Count | Should -Be 0
    }

    It 'preserves every concurrently appended finding and allocated identifier' {
        $path = Join-Path $script:Root 'findings.json'
        Write-TestJson -Path $path -Value @{
            schemaVersion = 1; kind = 'reviewer'; reviewerId = 'shared'
            focusArea = 'correctness'; status = 'open'; findings = @()
        }
        $variables = @(foreach ($name in @('alpha', 'beta', 'gamma')) {
            $findingPath = Join-Path $script:Root "$name.json"
            Write-TestJson -Path $findingPath -Value @{
                title = $name; description = 'Preserve concurrent work.'
                reasoning = 'Writers share an artifact.'; consequence = 'Updates must not disappear.'
                recommendedAction = 'Serialize the transaction.'; anchor = @{ kind = 'file'; path = 'source.ps1' }
            }
            @{ SS_AI_TEST_INTEROP_ARTIFACT = $path; SS_AI_TEST_INTEROP_FINDING = $findingPath }
        })
        $null = @(Invoke-ContendedNativeTransactions -Path $path -Executable $env:SS_AI_FINDING_TEST_EXECUTABLE `
            -Helper 'native_process_lock_interop_helper' -Environment $variables)
        $saved = Read-TestJson -Path $path
        @($saved.findings.title) | Should -Be @('alpha', 'beta', 'gamma')
        @($saved.findings.id) | Should -Be @('shared-1', 'shared-2', 'shared-3')
    }

    It 'initializes and appends the shared decision log without losing concurrent decisions' {
        $path = Join-Path $script:Root 'decisions.json'
        $variables = @(foreach ($id in @('F1', 'F2', 'F3')) {
            $planPath = Join-Path $script:Root "$id.json"
            Write-TestJson -Path $planPath -Value @{
                schemaVersion = 1; decisions = @(@{ findingId = $id; decision = 'needs-audit'; reason = 'Requires a human decision.' })
            }
            @{ SS_AI_AUDIT_INTEROP_OPERATION = 'decision'; SS_AI_AUDIT_INTEROP_RECORD = $path; SS_AI_AUDIT_INTEROP_INPUT = $planPath }
        })
        $null = @(Invoke-ContendedNativeTransactions -Path $path -Executable $env:SS_AI_AUDIT_TEST_EXECUTABLE `
            -Helper 'native_audit_lock_interop_helper' -Environment $variables)
        @((Read-TestJson -Path $path).decisions.findingId) | Should -Be @('F1', 'F2', 'F3')
    }

    It 'preserves concurrent first feedback responses in arrival order' {
        $path = Join-Path $script:Root 'audit-feedback.json'
        $artifactPath = Join-Path $script:Root 'findings.json'
        Write-TestJson -Path $artifactPath -Value @{
            schemaVersion = 1; kind = 'consolidated'; removed = @()
            findings = @(@{
                schemaVersion = 1; id = 'F-1'; sourceFindingIds = @(); origin = 'reviewer'
                title = 'Concurrent responses'; description = 'Preserve every response.'
                reasoning = 'Feedback can arrive concurrently.'; consequence = 'No response may disappear.'
                recommendedAction = 'Initialize and append together.'; references = @(); impactedLocations = @()
                anchor = @{ kind = 'file'; path = 'source.ps1' }
            })
        }
        $variables = @(foreach ($response in @('first', 'second', 'third')) {
            $textPath = Join-Path $script:Root "$response.txt"
            [IO.File]::WriteAllText($textPath, $response, [Text.UTF8Encoding]::new($false))
            @{
                SS_AI_AUDIT_INTEROP_OPERATION = 'feedback'; SS_AI_AUDIT_INTEROP_RECORD = $path
                SS_AI_AUDIT_INTEROP_INPUT = $textPath; SS_AI_AUDIT_INTEROP_FINDINGS = $artifactPath
                SS_AI_AUDIT_INTEROP_FINDING = 'F-1'; SS_AI_AUDIT_INTEROP_RESPONSE = $response
            }
        })
        $null = @(Invoke-ContendedNativeTransactions -Path $path -Executable $env:SS_AI_AUDIT_TEST_EXECUTABLE `
            -Helper 'native_audit_lock_interop_helper' -Environment $variables)
        $record = Read-TestJson -Path $path
        @($record.feedback.responseId) | Should -Be @('first', 'second', 'third')
        @($record.feedback.text) | Should -Be @('first', 'second', 'third')
    }
}
