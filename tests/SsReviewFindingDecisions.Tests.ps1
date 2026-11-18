#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.finding-decisions-' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:Root)

    # Capture streams separately so failed commands cannot masquerade as JSON success.
    function Invoke-DecisionWorkflow {
        param([string[]] $Arguments)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            # Drain both pipes while the child runs so a full pipe cannot block its exit.
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            return @{
                ExitCode = $process.ExitCode
                Output = $stdout.GetAwaiter().GetResult()
                Error = $stderr.GetAwaiter().GetResult()
            }
        } finally {
            $process.Dispose()
        }
    }

    function Write-DecisionJson {
        param([string] $Path, $Value)

        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 100), [Text.UTF8Encoding]::new($false))
    }

    function Read-Success {
        param($Result)

        if ($Result.ExitCode -ne 0) { throw "Native workflow failed: $($Result.Error)" }
        return (ConvertFrom-Json -InputObject $Result.Output -AsHashtable -ErrorAction Stop)
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) {
        [IO.Directory]::Delete($script:Root, $true)
    }
}

Describe 'Native finding decision commands' {
    BeforeEach {
        $script:Work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        [void] [IO.Directory]::CreateDirectory($script:Work)
        $script:Artifact = Join-Path $script:Work 'findings.json'
        $script:Journal = Join-Path $script:Work 'decisions.json'
        $script:Output = Join-Path $script:Work 'approved.json'
        $script:Item = Join-Path $script:Work 'item.json'
        $findings = foreach ($id in @('F1', 'F2')) {
            @{
                schemaVersion = 1; id = $id; origin = 'reviewer'; sourceFindingIds = @()
                title = 'Problem'; description = 'Details'; reasoning = 'Evidence'; consequence = 'Impact'
                recommendedAction = 'Fix'; anchor = @{ kind = 'file'; path = 'src/lib.rs' }
            }
        }
        Write-DecisionJson -Path $script:Artifact -Value @{ schemaVersion = 1; kind = 'consolidated'; findings = @($findings); removed = @() }
    }

    It 'preserves policy classification and human decisions across native calls' {
        $pollArgs = @('audit-poll-classification', '--artifact-path', $script:Artifact, '--decisions-path', $script:Journal)
        $buildArgs = @('approved', 'build', '--path', $script:Artifact, '--decisions-path', $script:Journal, '--output-path', $script:Output)
        $poll = Read-Success (Invoke-DecisionWorkflow -Arguments $pollArgs)
        $poll.finding.id | Should -BeExactly 'F1'
        $poll.remainingCount | Should -Be 2
        foreach ($entry in @(
            @{ findingId = 'F1'; decision = 'auto-approved' }
            @{ findingId = 'F2'; decision = 'needs-audit' }
        )) {
            Write-DecisionJson -Path $script:Item -Value $entry
            $result = Invoke-DecisionWorkflow -Arguments @(
                'audit-add-decisions', '--path', $script:Journal, '--item-path', $script:Item
                '--recorded-utc', '2024-05-06T09:10:11Z', '--policy-classification'
            )
            (Read-Success $result).recorded | Should -Be @($entry.findingId)
        }
        $poll = Read-Success (Invoke-DecisionWorkflow -Arguments $pollArgs)
        $poll.isComplete | Should -BeTrue
        $poll.auditFindingIds | Should -Be @('F2')
        $blocked = Invoke-DecisionWorkflow -Arguments $buildArgs
        $blocked.ExitCode | Should -Not -Be 0
        $blocked.Output | Should -BeNullOrEmpty
        Test-Path -LiteralPath $script:Output | Should -BeFalse
        Write-DecisionJson -Path $script:Item -Value @{ findingId = 'F2'; decision = 'approved' }
        $decision = Invoke-DecisionWorkflow -Arguments @(
            'audit-add-decisions', '--path', $script:Journal, '--item-path', $script:Item
            '--recorded-utc', '2024-05-06T09:10:11Z'
        )
        (Read-Success $decision).recorded | Should -Be @('F2')
        $summary = Read-Success (Invoke-DecisionWorkflow -Arguments $buildArgs)
        $summary.approved | Should -Be 2
        $summary.autoApproved | Should -Be 1
        $summary.manuallyApproved | Should -Be 1
        $summary.findingId | Should -Be @('F1', 'F2')
        $document = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Output)) -AsHashtable
        $document.findings.Count | Should -Be 2
    }

    It 'refuses incomplete audit and unmatched capture arguments without writing' {
        $buildArgs = @('approved', 'build', '--path', $script:Artifact, '--decisions-path', $script:Journal, '--output-path', $script:Output)
        foreach ($case in @(
            @{ Extra = @() }
            @{ Extra = @('--context-path', (Join-Path $script:Work 'capture.json')) }
            @{ Extra = @('--repository-path', $script:Work) }
        )) {
            $result = Invoke-DecisionWorkflow -Arguments ($buildArgs + $case.Extra)
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            Test-Path -LiteralPath $script:Output | Should -BeFalse
        }
    }

    It 'publishes an empty set without a decision journal' {
        Write-DecisionJson -Path $script:Artifact -Value @{ schemaVersion = 1; kind = 'consolidated'; findings = @(); removed = @() }
        $result = Read-Success (Invoke-DecisionWorkflow -Arguments @(
            'approved', 'build', '--path', $script:Artifact, '--decisions-path', $script:Journal, '--output-path', $script:Output
        ))
        $result.approved | Should -Be 0
        $result.findingId.Count | Should -Be 0
        $result.unanchoredFinding.Count | Should -Be 0
        $document = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Output)) -AsHashtable
        $document.findings.Count | Should -Be 0
    }

    It 'rejects simultaneous individual and plan inputs without changing the journal' {
        Write-DecisionJson -Path $script:Journal -Value @{ schemaVersion = 1; decisions = @() }
        Write-DecisionJson -Path $script:Item -Value @{ findingId = 'F1'; decision = 'approved' }
        $plan = Join-Path $script:Work 'plan.json'
        Write-DecisionJson -Path $plan -Value @{
            schemaVersion = 1; decisions = @(@{ findingId = 'F2'; decision = 'approved' })
        }
        $before = [Convert]::ToBase64String([IO.File]::ReadAllBytes($script:Journal))
        $result = Invoke-DecisionWorkflow -Arguments @(
            'audit-add-decisions', '--path', $script:Journal, '--item-path', $script:Item, '--plan-path', $plan
            '--recorded-utc', '2024-05-06T09:10:11Z'
        )
        $result.ExitCode | Should -Not -Be 0
        $result.Output | Should -BeNullOrEmpty
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($script:Journal)) | Should -BeExactly $before
    }
}
