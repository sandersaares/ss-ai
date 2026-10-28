#Requires -Version 7.6

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\AdoExperiment.psm1') -Force
}

Describe 'Controlled ADO evidence experiment' {
    InModuleScope AdoExperiment {
        BeforeEach {
            $script:source = 'a' * 40
            $script:reviewers = @()
            $script:history = '0' * 64
            $script:autoComplete = $false
            $script:writes = @()
            $script:loseResponse = $false
            $script:refusePush = $false
            Mock Get-ExperimentToken { ConvertTo-SecureString 'fixture' -AsPlainText }
            Mock Get-ExperimentTime { '2026-10-16T10:00:00Z' }
            Mock Get-AdoExperimentCapture {
                @{
                    capturedAt = Get-ExperimentTime
                    raw = @{
                        identity = @{}; prBefore = @{}; prAfter = @{}; sourceRef = @{}; targetRef = @{}
                        reviewers = @{}; iterations = @{}; threads = @{}; policyPages = @(@{}); evaluationPages = @(@{})
                    }
                    view = @{
                        accountId = 'account'; repositoryId = 'repository'; projectId = 'project'; authorId = 'account'
                        sourceRef = 'refs/heads/source'; targetRef = 'refs/heads/target'
                        sourceCommit = $script:source; targetCommit = ('b' * 40)
                        status = 'active'; draft = $false; autoComplete = $script:autoComplete
                        reviewers = @($script:reviewers); policyDigest = ('f' * 64); voteHistoryDigest = $script:history
                    }
                }
            }
            Mock Invoke-ExperimentRequest {
                param($Method, $Url, $Token, $Body)
                $script:writes += @{ method = $Method; url = $Url; body = $Body }
                if ($script:loseResponse) { throw 'Lost response after request' }
                if ($Method -ceq 'PUT') {
                    $Body.id | Should -BeExactly 'account'
                    $Body.vote | Should -Be 10
                    $script:reviewers = @(@{ id = 'account'; vote = 10; isRequired = $false })
                    $script:history = '1' * 64
                    return @{ continuation = $null; data = $script:reviewers[0] }
                }
                if ($Method -ceq 'DELETE') {
                    $Url | Should -Match '/reviewers/account\?'
                    $script:reviewers = @()
                    $script:history = '2' * 64
                    return @{ continuation = $null; data = $null }
                }
                $Method | Should -BeExactly 'POST'
                $Body.refUpdates.Count | Should -Be 1
                $Body.refUpdates[0].name | Should -BeExactly 'refs/heads/source'
                $Body.refUpdates[0].oldObjectId | Should -BeExactly $script:source
                $Body.commits.Count | Should -Be 1
                $Body.commits[0].changes.Count | Should -Be 1
                if ($script:refusePush) { throw 'Conflicting ref update' }
                $old = $script:source
                $script:source = if ($Body.commits[0].changes[0].changeType -ceq 'add') { 'c' * 40 } else { 'd' * 40 }
                @{
                    continuation = $null
                    data = @{
                        refUpdates = @(@{ name = 'refs/heads/source'; oldObjectId = $old; newObjectId = $script:source })
                        commits = @(@{ commitId = $script:source })
                    }
                }
            }
            $script:path = Join-Path $TestDrive "$([guid]::NewGuid()).json"
            $script:arguments = @{
                PullRequestUrl = [uri] 'https://dev.azure.com/example/project/_git/repository/pullrequest/1'
                StatePath = $script:path
            }
            Invoke-AdoExperiment @script:arguments -Step Initialize -ExpectedAccountId account `
                -ExpectedSourceRef refs/heads/source -ExpectedTargetRef refs/heads/target `
                -ExpectedSourceCommit ('a' * 40) -ExpectedTargetCommit ('b' * 40) | Out-Null
        }

        It 'records each scoped effect and restores reviewer absence without rewriting history' {
            foreach ($step in @('Approve', 'AddFile', 'RemoveFile', 'RemoveReviewer')) {
                Invoke-AdoExperiment @script:arguments -Step $step -AllowRemoteWrites | Out-Null
            }
            $record = Read-ExperimentRecord $script:path
            $record.nextStep | Should -BeExactly Done
            $record.sourceCommit | Should -BeExactly ('d' * 40)
            $record.expectedReviewers.Count | Should -Be 0
            $record.captures.Count | Should -Be 9
            @($record.effects | Where-Object state -CNE verified).Count | Should -Be 0
            $script:writes.Count | Should -Be 4
            $script:writes[1].body.commits[0].changes[0].item.path | Should -BeExactly $record.filePath
            $script:writes[2].body.commits[0].changes[0].item.path | Should -BeExactly $record.filePath
            $script:writes[2].body.commits[0].changes[0].changeType | Should -BeExactly delete
        }

        It 'requires explicit mutation permission and refuses an out-of-order step' {
            { Invoke-AdoExperiment @script:arguments -Step Approve } | Should -Throw '*authorization*'
            { Invoke-AdoExperiment @script:arguments -Step AddFile -AllowRemoteWrites } | Should -Throw '*Expected Approve*'
            $script:writes.Count | Should -Be 0
        }

        It 'does not overwrite source movement, reviewer changes, or enabled auto-complete' {
            $script:source = 'c' * 40
            { Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites } | Should -Throw '*sourceCommit*'
            $script:source = 'a' * 40
            $script:autoComplete = $true
            { Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites } | Should -Throw '*auto-complete*'
            $script:autoComplete = $false
            $script:reviewers = @(@{ id = 'account'; vote = 0 })
            { Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites } | Should -Throw '*Reviewer*'
            $script:writes.Count | Should -Be 0
        }

        It 'retains unknown write intent and allows only further read-only captures' {
            $script:loseResponse = $true
            { Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites } | Should -Throw '*Lost response*'
            $record = Read-ExperimentRecord $script:path
            $record.nextStep | Should -BeExactly Stopped
            $record.effects[0].state | Should -BeExactly unknown
            { Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites } | Should -Throw '*Do not replay*'
            Invoke-AdoExperiment @script:arguments -Step Capture | Out-Null
            $script:writes.Count | Should -Be 1
        }

        It 'refuses cleanup when the approved reviewer was changed by someone else' {
            Invoke-AdoExperiment @script:arguments -Step Approve -AllowRemoteWrites | Out-Null
            Invoke-AdoExperiment @script:arguments -Step AddFile -AllowRemoteWrites | Out-Null
            Invoke-AdoExperiment @script:arguments -Step RemoveFile -AllowRemoteWrites | Out-Null
            $script:reviewers[0].vote = -10
            { Invoke-AdoExperiment @script:arguments -Step RemoveReviewer -AllowRemoteWrites } | Should -Throw '*Reviewer*'
            $script:writes.Count | Should -Be 3
        }

        It 'rejects malformed lifecycle records and preserves their bytes' {
            $record = Read-ExperimentRecord $script:path
            $record.nextStep = 'RemoveReviewer'
            { Write-ExperimentRecord $script:path $record } | Should -Throw '*lifecycle*'
            (Read-ExperimentRecord $script:path).nextStep | Should -BeExactly Approve
        }

        It 'canonicalizes objects without changing array order' {
            (Get-ExperimentDigest @{ a = @(); b = @{ c = 1; d = 2 } }) |
                Should -BeExactly (Get-ExperimentDigest @{ b = @{ d = 2; c = 1 }; a = @() })
            (Get-ExperimentDigest @(1, 2)) | Should -Not -BeExactly (Get-ExperimentDigest @(2, 1))
        }

        It 'publishes complete replacements while concurrent readers retain the old record' {
            $original = [IO.File]::ReadAllText($script:path)
            $share = [IO.FileShare]::Read -bor [IO.FileShare]::Delete
            $first = [IO.File]::Open($script:path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
            $second = [IO.File]::Open($script:path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
            try {
                Invoke-AdoExperiment @script:arguments -Step Capture | Out-Null
                (Read-ExperimentRecord $script:path).captures.Count | Should -Be 2
                foreach ($stream in @($first, $second)) {
                    $reader = [IO.StreamReader]::new($stream)
                    try { $reader.ReadToEnd() | Should -BeExactly $original } finally { $reader.Dispose() }
                }
            } finally { $first.Dispose(); $second.Dispose() }
        }

        It 'waits through lock contention and preserves the complete winning initialization' {
            $output = Join-Path $TestDrive 'contended-fixture.json'
            $fixture = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\packages\ss-ai-review\tests\fixtures\ado-no-approval-policy.json') -Raw |
                ConvertFrom-Json -AsHashtable -DateKind String
            $lock = [IO.File]::Open("$output.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            $contended = [Threading.ManualResetEventSlim]::new()
            $release = [Threading.ManualResetEventSlim]::new()
            $finished = [Threading.ManualResetEventSlim]::new()
            $worker = [powershell]::Create()
            try {
                $null = $worker.AddScript({
                    param($modulePath, $output, $fixture, $contended, $release, $finished)
                    $ErrorActionPreference = 'Stop'
                    Import-Module $modulePath -Force
                    & (Get-Module AdoExperiment) {
                        param($contended, $release)
                        $script:contended = $contended
                        $script:release = $release
                        function script:Wait-ExperimentContention {
                            $script:contended.Set()
                            $script:release.Wait()
                        }
                    } $contended $release
                    try { Publish-ExperimentFixture $output $fixture }
                    finally { $finished.Set() }
                }).AddArgument((Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\AdoExperiment.psm1')).
                    AddArgument($output).AddArgument($fixture).AddArgument($contended).AddArgument($release).AddArgument($finished)
                $pending = $worker.BeginInvoke()
                [Threading.WaitHandle]::WaitAny(@($contended.WaitHandle, $finished.WaitHandle)) | Should -Be 0
                # The winner uses the actual protocol while the competing writer is paused.
                Write-ExperimentJson $output $fixture
                $lock.Dispose()
                $release.Set()
                { $worker.EndInvoke($pending) } | Should -Throw '*overwrite*'
                $actual = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json -AsHashtable -DateKind String
                (Get-ExperimentDigest $actual) | Should -BeExactly (Get-ExperimentDigest $fixture)
                [IO.File]::Exists("$output.lock") | Should -BeTrue
            } finally {
                $lock.Dispose(); $release.Set()
                $worker.Dispose()
                $contended.Dispose(); $release.Dispose(); $finished.Dispose()
            }
        }
    }
}

Describe 'ADO capture provider boundaries' {
    InModuleScope AdoExperiment {
        BeforeEach {
            $script:stage = (Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\packages\ss-ai-review\tests\fixtures\ado-no-approval-policy.json') -Raw |
                ConvertFrom-Json -AsHashtable -DateKind String).stages[1]
            $script:requests = @()
            $script:repeatPolicy = $false
            $script:badThreadCount = $false
            $script:moveSource = $false
            $script:sourceReads = 0
            Mock Invoke-ExperimentRequest {
                param($Method, $Url)
                $Method | Should -BeExactly GET
                $script:requests += $Url
                $continuation = $null
                $data = switch -Regex ($Url) {
                    '/connectionData\?' { @{ authenticatedUser = @{ id = 'account' } }; break }
                    '/refs\?' {
                        $source = $Url -match 'fixture-source'
                        if ($source) { $script:sourceReads++ }
                        $commit = if ($source) { $script:stage.sourceCommit } else { $script:stage.targetCommit }
                        if ($source -and $script:moveSource -and $script:sourceReads -gt 1) { $commit = 'e' * 40 }
                        @{
                            count = 1
                            value = @(@{ name = if ($source) { 'refs/heads/fixture-source' } else { 'refs/heads/fixture-target' }; objectId = $commit })
                        }
                        break
                    }
                    '/reviewers\?' { @{ count = $script:stage.reviewers.Count; value = $script:stage.reviewers }; break }
                    '/iterations\?' { @{ count = $script:stage.iterations.Count; value = $script:stage.iterations }; break }
                    '/threads\?' {
                        @{ count = $script:stage.threads.Count + [int] $script:badThreadCount; value = $script:stage.threads }
                        break
                    }
                    '/policy/configurations\?' {
                        $index = if ($Url -match 'continuationToken=([0-9]+)') { [int] $Matches[1] } else { 0 }
                        if ($index -lt $script:stage.policies.Count - 1) { $continuation = [string] ($index + 1) }
                        if ($script:repeatPolicy -and $index -gt 0) { $index = 0 }
                        @{ count = 1; value = @($script:stage.policies[$index]) }
                        break
                    }
                    '/policy/evaluations\?' {
                        # A short nonempty evaluation page still requires its next offset.
                        if ($Url -match '%24skip=0') { @{ count = 1; value = @(@{ evaluationId = 'fixture-evaluation' }) } }
                        else { @{ count = 0; value = @() } }
                        break
                    }
                    '/pullRequests/1\?' {
                        @{
                            repository = $script:stage.pullRequest.repository; createdBy = @{ id = 'account' }
                            sourceRefName = 'refs/heads/fixture-source'; targetRefName = 'refs/heads/fixture-target'
                            status = 'active'; isDraft = $false; autoCompleteSetBy = $null
                            reviewers = $script:stage.reviewers
                        }
                        break
                    }
                    default { throw "Unexpected fixture URL: $Url" }
                }
                @{ data = $data; continuation = $continuation }
            }
            $script:target = Get-ExperimentTarget ([uri] 'https://dev.azure.com/example/project/_git/repository/pullrequest/1')
            $script:token = ConvertTo-SecureString fixture -AsPlainText
        }

        It 'reads every policy page and fences the complete provider observation' {
            $capture = Get-AdoExperimentCapture $script:target $script:token
            $capture.raw.policyPages.Count | Should -Be $script:stage.policies.Count
            $capture.raw.evaluationPages.Count | Should -Be 2
            $capture.raw.evaluationPages[1].value.Count | Should -Be 0
            $capture.view.sourceCommit | Should -BeExactly $script:stage.sourceCommit
            $script:sourceReads | Should -Be 2
        }

        It 'rejects duplicate pages, malformed lists, and source movement explicitly' {
            $script:repeatPolicy = $true
            { Get-AdoExperimentCapture $script:target $script:token } | Should -Throw '*Repeated policy identifier*'
            $script:repeatPolicy = $false
            $script:badThreadCount = $true
            { Get-AdoExperimentCapture $script:target $script:token } | Should -Throw '*Incomplete ADO list*'
            $script:badThreadCount = $false
            $script:moveSource = $true
            $script:sourceReads = 0
            { Get-AdoExperimentCapture $script:target $script:token } | Should -Throw '*branch moved*'
        }
    }
}

Describe 'Sanitized ADO evidence fixture' {
    BeforeAll {
        $exporter = Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\Export-AdoExperimentFixture.ps1'
        $fixturePath = Join-Path $PSScriptRoot '..\packages\ss-ai-review\tests\fixtures\ado-no-approval-policy.json'
        $fixtureText = Get-Content -LiteralPath $fixturePath -Raw
        $fixture = $fixtureText | ConvertFrom-Json -AsHashtable -DateKind String

        # Derive test captures from the verified provider shape, never invent event fields.
        function New-PrivateTestRecord {
            $text = $fixtureText.Replace('10000000-0000-4000-8000-000000000001', '11111111-1111-4111-8111-111111111111').
                Replace('20000000-0000-4000-8000-000000000001', '22222222-2222-4222-8222-222222222222').
                Replace('30000000-0000-4000-8000-000000000001', '33333333-3333-4333-8333-333333333333').
                Replace('refs/heads/fixture-source', 'refs/heads/private-source').
                Replace('refs/heads/fixture-target', 'refs/heads/private-target')
            foreach ($pair in @(@('a', '1'), @('b', '2'), @('c', '3'), @('d', '4'))) {
                $text = $text.Replace(($pair[0] * 40), ($pair[1] * 40))
            }
            $private = $text | ConvertFrom-Json -AsHashtable -DateKind String
            $operation = [guid]::NewGuid().ToString()
            $captures = @($private.stages | ForEach-Object {
                $stage = $_
                foreach ($reviewer in $stage.reviewers) { $reviewer.displayName = 'DO NOT EXPORT: private display' }
                foreach ($thread in $stage.threads) {
                    foreach ($comment in $thread.comments) { $comment.content = 'DO NOT EXPORT: private discussion' }
                }
                @{
                    capturedAt = $stage.observedAt
                    raw = @{
                        identity = @{}; prBefore = $stage.pullRequest; prAfter = $stage.pullRequest
                        sourceRef = @{}; targetRef = @{}
                        reviewers = @{ value = $stage.reviewers }; iterations = @{ value = $stage.iterations }
                        threads = @{ value = $stage.threads }
                        policyPages = @(@{ data = @{ value = $stage.policies }; continuation = $null })
                        evaluationPages = @(@{ value = @() })
                    }
                    view = @{
                        accountId = $private.accountId; authorId = $private.accountId
                        repositoryId = $stage.pullRequest.repository.id; projectId = $stage.pullRequest.repository.project.id
                        sourceRef = $stage.pullRequest.sourceRefName; targetRef = $stage.pullRequest.targetRefName
                        sourceCommit = $stage.sourceCommit; targetCommit = $stage.targetCommit
                        status = 'active'; draft = $false; autoComplete = $false; reviewers = $stage.reviewers
                        policyDigest = ('0' * 64); voteHistoryDigest = ('1' * 64)
                    }
                }
            })
            $effects = @(
                @{ kind = 'Approve'; response = @{}; method = 'PUT' }
                @{ kind = 'AddFile'; response = @{ commits = @(@{ commitId = '3' * 40 }) }; method = 'POST' }
                @{ kind = 'RemoveFile'; response = @{ commits = @(@{ commitId = '4' * 40 }) }; method = 'POST' }
                @{ kind = 'RemoveReviewer'; response = $null; method = 'DELETE' }
            ) | ForEach-Object {
                @{
                    kind = $_.kind; response = $_.response; state = 'verified'; error = $null
                    request = @{ method = $_.method; url = 'https://example.invalid/private'; body = $null }
                }
            }
            @{
                schemaVersion = 1; operationId = $operation; pullRequestUrl = 'https://example.invalid/private'
                tenant = $null; nextStep = 'Done'; filePath = "/.ss-autoreapprove-$operation.txt"
                accountId = $private.accountId
                repositoryId = $captures[0].view.repositoryId; projectId = $captures[0].view.projectId
                sourceRef = $captures[0].view.sourceRef; targetRef = $captures[0].view.targetRef
                sourceCommit = ('4' * 40); targetCommit = ('2' * 40)
                policyDigest = ('0' * 64); voteHistoryDigest = ('1' * 64)
                expectedReviewers = @(); captures = $captures; effects = @($effects); error = $null
            }
        }
    }

    It 'preserves the observed retained vote and later removal, without invented reset evidence' {
        Assert-ExperimentFixture $fixture
        $fixture.stages[0].reviewers.Count | Should -Be 0
        foreach ($index in @(1, 2, 3)) {
            $fixture.stages[$index].reviewers[0].vote | Should -Be 10
            @($fixture.stages[$index].threads | Where-Object { $_.properties.CodeReviewThreadType.'$value' -ceq 'VoteUpdate' }).Count | Should -Be 1
        }
        $fixture.stages[4].reviewers.Count | Should -Be 0
        $vote = $fixture.stages[1].threads[1]
        $vote.properties.CodeReviewVoteResult.'$type' | Should -BeExactly System.String
        $vote.identities[$vote.properties.CodeReviewVotedByIdentity.'$value'].id | Should -BeExactly $fixture.accountId
        $vote.properties.ContainsKey('CodeReviewVotedByTfId') | Should -BeFalse
    }

    It 'exports through the real command, ignoring duplicate captures and excluding private fields' {
        $record = New-PrivateTestRecord
        $record.captures = @($record.captures[0]) + @($record.captures)
        $inputPath = Join-Path $TestDrive 'private.json'
        [IO.File]::WriteAllText($inputPath, (ConvertTo-Json -InputObject $record -Depth 100))
        $outputPath = Join-Path $TestDrive 'public.json'
        & $exporter -StatePath $inputPath -OutputPath $outputPath
        $exportedText = [IO.File]::ReadAllText($outputPath)
        $exported = $exportedText | ConvertFrom-Json -AsHashtable -DateKind String
        Assert-ExperimentFixture $exported
        $exportedText | Should -Not -Match 'DO NOT EXPORT|private-source|private-target|example.invalid'
        foreach ($identifier in @($record.accountId, $record.repositoryId, $record.projectId, ('1' * 40), ('2' * 40), ('3' * 40), ('4' * 40))) {
            $exportedText.Contains($identifier) | Should -BeFalse
        }
        $exported.stages[1].threads[1].properties.CodeReviewVoteResult.'$value' | Should -BeExactly '10'
        { & $exporter -StatePath $inputPath -OutputPath $outputPath } | Should -Throw '*overwrite*'
        [IO.File]::ReadAllText($outputPath) | Should -BeExactly $exportedText
    }

    It 'rejects unknown properties and incomplete or unverified sequences' {
        $record = New-PrivateTestRecord
        $inputPath = Join-Path $TestDrive 'bad-private.json'
        $outputPath = Join-Path $TestDrive 'bad-public.json'
        $record.captures[1].raw.threads.value[1].properties.PrivateField = @{ '$type' = 'System.String'; '$value' = 'private' }
        [IO.File]::WriteAllText($inputPath, (ConvertTo-Json -InputObject $record -Depth 100))
        { & $exporter -StatePath $inputPath -OutputPath $outputPath } | Should -Throw '*Unrecognized thread property*'
        [IO.File]::Exists($outputPath) | Should -BeFalse
        $record = New-PrivateTestRecord
        $record.captures = @($record.captures | Where-Object { $_.view.sourceCommit -cne ('3' * 40) })
        [IO.File]::WriteAllText($inputPath, (ConvertTo-Json -InputObject $record -Depth 100))
        { & $exporter -StatePath $inputPath -OutputPath $outputPath } | Should -Throw
        $record = New-PrivateTestRecord
        $record.effects[3].state = 'unknown'
        $record.nextStep = 'Stopped'
        [IO.File]::WriteAllText($inputPath, (ConvertTo-Json -InputObject $record -Depth 100))
        { & $exporter -StatePath $inputPath -OutputPath $outputPath } | Should -Throw '*fully verified*'
        [IO.File]::Exists($outputPath) | Should -BeFalse
    }
}
