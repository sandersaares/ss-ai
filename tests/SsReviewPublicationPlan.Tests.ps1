#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Describe 'Preflight performs no write' {
    Describe 'Native publication approval authority' {
        BeforeAll {
            $script:NativePublication = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
            $script:NativePublicationRoot = Join-Path $PSScriptRoot ('.publication-native-' + [guid]::NewGuid().ToString('N'))
            [void] [IO.Directory]::CreateDirectory($script:NativePublicationRoot)
            $script:NativePublicationPath = Join-Path $script:NativePublicationRoot 'no-provider-programs'
            [void] [IO.Directory]::CreateDirectory($script:NativePublicationPath)

            # Isolate PATH so neither a provider CLI nor Git can be launched by the native command.
            function Invoke-NativeApprovalPreflight {
                param(
                    [string] $Provider,
                    [bool] $SelfAuthored,
                    [bool] $Approve = $true,
                    [scriptblock] $Mutation,
                    [AllowEmptyString()] [string] $DiagnosticsText = '{}',
                    [switch] $OmitDiagnostics,
                    [scriptblock] $PriorMutation
                )

                $fixtureRoot = Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture'
                $capture = ConvertFrom-Json -InputObject (
                    [IO.File]::ReadAllText((Join-Path $fixtureRoot "$Provider-expected.json"))
                ) -AsHashtable -DateKind String
                $parity = ConvertFrom-Json -InputObject (
                    [IO.File]::ReadAllText((Join-Path $fixtureRoot "$Provider-refresh-parity.json"))
                ) -AsHashtable -DateKind String
                $capture.pullRequest.isAuthoredBySelf = $SelfAuthored
                $directory = Join-Path $script:NativePublicationRoot ([guid]::NewGuid().ToString('N'))
                [void] [IO.Directory]::CreateDirectory($directory)
                $capturePath = Join-Path $directory 'capture.json'
                $approvedPath = Join-Path $directory 'approved.json'
                $approvalPath = Join-Path $directory 'approval.json'
                $diagnosticsPath = Join-Path $directory 'diagnostics.json'
                $outputPath = Join-Path $directory 'output'
                $findings = if ($Approve) { @() } else {
                    @([ordered]@{
                        schemaVersion = 1
                        id = 'F'
                        sourceFindingIds = @('F')
                        origin = 'reviewer'
                        title = 'Finding'
                        description = 'Problem'
                        reasoning = 'Reason'
                        consequence = 'Impact'
                        recommendedAction = 'Fix'
                        anchor = [ordered]@{ kind = 'file'; path = 'src/lib.rs' }
                    })
                }
                $approvedTarget = ConvertFrom-Json -AsHashtable -DateKind String -InputObject (
                    ConvertTo-Json -InputObject $capture.target -Depth 100 -Compress
                )
                $approved = [ordered]@{
                    schemaVersion = 2
                    target = $approvedTarget
                    reviewedSourceCommit = $capture.pullRequest.reviewedSourceCommit
                    reviewedMergeBaseCommit = $capture.pullRequest.reviewedMergeBaseCommit
                    reviewedContextHash = $parity.baselineEvidenceHash
                    findings = @($findings)
                }
                $approval = [ordered]@{
                    schemaVersion = 1
                    pullRequestUrl = $capture.pullRequestUrl
                    approveMerge = $Approve
                    approvedSourceCommit = $capture.pullRequest.reviewedSourceCommit
                }
                if ($null -ne $Mutation) { & $Mutation $capture $approved $approval }
                foreach ($entry in @(
                    @{ Path = $capturePath; Value = $capture }
                    @{ Path = $approvedPath; Value = $approved }
                    @{ Path = $approvalPath; Value = $approval }
                )) {
                    [IO.File]::WriteAllText(
                        $entry.Path, (ConvertTo-Json -InputObject $entry.Value -Depth 100 -Compress),
                        [Text.UTF8Encoding]::new($false)
                    )
                }
                if (-not $OmitDiagnostics) {
                    [IO.File]::WriteAllText($diagnosticsPath, $DiagnosticsText, [Text.UTF8Encoding]::new($false))
                }
                $resultPath = Join-Path $outputPath 'publication-result.json'
                $priorText = $null
                if ($null -ne $PriorMutation) {
                    [void] [IO.Directory]::CreateDirectory($outputPath)
                    $operation = {
                        param($name, $outcome)
                        [ordered]@{
                            operation = $name; findingId = $null; outcome = $outcome
                            anchor = $null; wasDowngraded = $false
                            threadId = $null; commentId = $null; message = $null
                        }
                    }
                    $priorTarget = ConvertFrom-Json -AsHashtable -DateKind String -InputObject (
                        ConvertTo-Json -InputObject $capture.target -Depth 100 -Compress
                    )
                    $prior = [ordered]@{
                        schemaVersion = 1; target = $priorTarget; status = 'failed'; inFlight = $null
                        operations = @(
                            (& $operation 'startReview' 'failed')
                            (& $operation 'verifyFindings' 'pending')
                            (& $operation 'completeReview' 'pending')
                        )
                    }
                    & $PriorMutation $prior
                    $priorText = ConvertTo-Json -InputObject $prior -Depth 100 -Compress
                    [IO.File]::WriteAllText($resultPath, $priorText, [Text.UTF8Encoding]::new($false))
                }
                $start = [Diagnostics.ProcessStartInfo]::new($script:NativePublication)
                $start.UseShellExecute = $false
                $start.RedirectStandardOutput = $true
                $start.RedirectStandardError = $true
                $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
                $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
                $start.Environment['PATH'] = $script:NativePublicationPath
                foreach ($argument in @(
                    'review', 'publish-final-review', '--url', $capture.pullRequestUrl,
                    '--approved-findings-path', $approvedPath, '--context-path', $capturePath,
                    '--merge-approval-path', $approvalPath, '--diagnostics-path', $diagnosticsPath,
                    '--repository-path', $directory, '--output-directory', $outputPath
                )) {
                    $start.ArgumentList.Add($argument)
                }
                $process = [Diagnostics.Process]::Start($start)
                try {
                    $stdout = $process.StandardOutput.ReadToEndAsync()
                    $stderr = $process.StandardError.ReadToEndAsync()
                    $process.WaitForExit()
                    return @{
                        ExitCode = $process.ExitCode
                        Output = $stdout.GetAwaiter().GetResult()
                        Error = $stderr.GetAwaiter().GetResult()
                        ResultPath = $resultPath
                        PriorText = $priorText
                    }
                } finally {
                    $process.Dispose()
                }
            }
        }

        AfterAll {
            if ([IO.Directory]::Exists($script:NativePublicationRoot)) {
                [IO.Directory]::Delete($script:NativePublicationRoot, $true)
            }
        }

        It 'refuses a self-authored <Provider> pull request before provider access' -ForEach @(
            @{ Provider = 'github' }
            @{ Provider = 'azure-devops' }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider $Provider -SelfAuthored $true
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'not authored by the authenticated account'
            $result.Output | Should -BeNullOrEmpty
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'allows approval of a <Provider> pull request authored by another account through the local authority gate' -ForEach @(
            @{ Provider = 'github' }
            @{ Provider = 'azure-devops' }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider $Provider -SelfAuthored $false
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'fetch missing commits'
            $result.Error | Should -Not -Match 'not authored by the authenticated account'
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'allows review without approval for a self-authored <Provider> pull request' -ForEach @(
            @{ Provider = 'github' }
            @{ Provider = 'azure-devops' }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider $Provider -SelfAuthored $true -Approve $false
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'fetch missing commits'
            $result.Error | Should -Not -Match 'not authored by the authenticated account'
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'accepts case differences in matching GitHub coordinates' {
            $result = Invoke-NativeApprovalPreflight -Provider github -SelfAuthored $false -Approve $false -Mutation {
                param($capture, $approved, $approval)
                $capture.target.organization = 'OCTO'
                $approved.target.organization = 'OCTO'
                $capture.target.repository = 'Widget'
                $approved.target.repository = 'Widget'
            }
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'fetch missing commits'
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'refuses <Case> locally without publishing' -ForEach @(
            @{
                Case = 'an unsupported approved schema'
                Mutation = { param($capture, $approved, $approval) $approved.schemaVersion = 99 }
                Message = 'schema|version'
            }
            @{
                Case = 'a working finding artifact instead of an approved set'
                Mutation = { param($capture, $approved, $approval) $approved.kind = 'consolidated' }
                Message = 'field|schema'
            }
            @{
                Case = 'an unknown approved-set field'
                Mutation = { param($capture, $approved, $approval) $approved.reviewedBy = 'somebody' }
                Message = 'field|schema'
            }
            @{
                Case = 'a missing reviewed source commit'
                Mutation = { param($capture, $approved, $approval) $approved.reviewedSourceCommit = '' }
                Message = 'source|commit'
            }
            @{
                Case = 'a capture without a usable authenticated identity'
                Mutation = {
                    param($capture, $approved, $approval)
                    $capture.authenticatedIdentity = @{ platform = 'gitHub' }
                }
                Message = 'invalid authenticated identity'
            }
            @{
                Case = 'an approved set without a pull request target'
                Mutation = { param($capture, $approved, $approval) $approved.target = $null }
                Message = 'target|pull request'
            }
            @{
                Case = 'duplicate finding identifiers'
                Mutation = {
                    param($capture, $approved, $approval)
                    $approved.findings += $approved.findings[0]
                }
                Message = '(duplicate|more than one).*F'
            }
            @{
                Case = 'a finding without its recommended action'
                Mutation = {
                    param($capture, $approved, $approval)
                    [void] $approved.findings[0].Remove('recommendedAction')
                }
                Message = 'recommendedAction'
            }
            @{
                Case = 'an empty review without merge approval'
                Mutation = { param($capture, $approved, $approval) $approved.findings = @() }
                Message = 'empty review|no findings'
            }
            @{
                Case = 'a merge decision bound to a different source commit'
                Mutation = {
                    param($capture, $approved, $approval)
                    $approval.approveMerge = $true
                    $approval.approvedSourceCommit = 'b' * 40
                }
                Message = 'different source commit'
            }
            @{
                Case = 'a string-valued merge decision'
                Mutation = { param($capture, $approved, $approval) $approval.approveMerge = 'false' }
                Message = 'schema|boolean'
            }
            @{
                Case = 'an approved set for a different pull request'
                Mutation = { param($capture, $approved, $approval) $approved.target.pullRequestId = 9 }
                Message = 'approved finding artifact.*9'
            }
            @{
                Case = 'captured discussion from a different repository'
                Mutation = { param($capture, $approved, $approval) $capture.target.repository = 'other' }
                Message = 'captured discussion.*repository'
            }
            @{
                Case = 'a follow-up to a thread outside the capture'
                Mutation = {
                    param($capture, $approved, $approval)
                    $approved.findings[0]['followUp'] = @{ threadId = 'absent'; commentId = 'missing' }
                }
                Message = "Finding 'F'.*follow-up"
            }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider github -SelfAuthored $false -Approve $false -Mutation $Mutation
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match $Message
            $result.Output | Should -BeNullOrEmpty
            @($result.Error.TrimEnd("`r", "`n") -split "`r?`n").Count | Should -Be 1
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'refuses a <Case> diagnostics record without publishing' -ForEach @(
            @{ Case = 'missing'; Text = '{}'; Omit = $true; Message = 'cannot be read' }
            @{ Case = 'malformed'; Text = '{ not json'; Omit = $false; Message = 'cannot be read' }
            @{ Case = 'non-object'; Text = '[1,2,3]'; Omit = $false; Message = 'must be an object' }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider github -SelfAuthored $false -Approve $false `
                -DiagnosticsText $Text -OmitDiagnostics:$Omit
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'diagnostics'
            $result.Error | Should -Match $Message
            $result.Output | Should -BeNullOrEmpty
            [IO.File]::Exists($result.ResultPath) | Should -BeFalse
        }

        It 'preserves a <Case> publication journal without contacting a provider' -ForEach @(
            @{
                Case = 'different-target'
                Mutation = { param($prior) $prior.target.pullRequestId = 8 }
                Message = 'different pull request'
            }
            @{
                Case = 'partially completed'
                Mutation = {
                    param($prior)
                    $prior.operations[0].outcome = 'succeeded'
                }
                Message = 'requires reconciliation'
            }
            @{
                Case = 'ambiguous'
                Mutation = {
                    param($prior)
                    $prior.operations[0].outcome = 'ambiguous'
                }
                Message = 'requires reconciliation'
            }
        ) {
            $result = Invoke-NativeApprovalPreflight -Provider github -SelfAuthored $false -Approve $false -PriorMutation $Mutation
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match $Message
            $result.Output | Should -BeNullOrEmpty
            [IO.File]::ReadAllText($result.ResultPath) | Should -BeExactly $result.PriorText
        }

        It 'continues past a definitive failed start without replacing the prior journal' {
            $result = Invoke-NativeApprovalPreflight -Provider github -SelfAuthored $false -Approve $false `
                -PriorMutation { param($prior) }
            $result.ExitCode | Should -Not -Be 0
            $result.Error | Should -Match 'fetch missing commits'
            [IO.File]::ReadAllText($result.ResultPath) | Should -BeExactly $result.PriorText
        }
    }

}
