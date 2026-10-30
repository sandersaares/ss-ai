BeforeAll {
    $script:Scripts = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review\scripts'
    . "$script:Scripts\lib\JsonValue.ps1"
    . "$script:Scripts\lib\PullRequestModel.ps1"
    . "$script:Scripts\lib\WorkflowData.ps1"
    . "$script:Scripts\lib\FindingModel.ps1"
    . "$script:Scripts\lib\ReviewWork.ps1"
    . "$script:Scripts\lib\FocusAreaPreparation.ps1"

    # All fixtures are ordinary inventory entries; nonexistent paths exercise removal-side coverage.
    function New-CoverageFixture {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        [void] [IO.Directory]::CreateDirectory($script:Root)
        $script:Files = @(
            @{ path = Join-Path $script:Root 'old.rs'; sizeBytes = 1; change = 'deleted' }
            @{ path = Join-Path $script:Root 'new.ps1'; sizeBytes = 1; change = 'new' }
            @{ path = Join-Path $script:Root 'lib.rs'; sizeBytes = 1; change = 'modified' }
        )
        $script:Groups = @(
            @{ groupId = 'rename'; files = @($script:Files[0], $script:Files[1]) }
            @{ groupId = 'other'; files = @($script:Files[2]) }
        )
        $script:Inventory = Join-Path $script:Root 'inventory.json'
        $script:Grouped = Join-Path $script:Root 'groups.json'
        $script:Queue = Join-Path $script:Root 'queue.json'
        $script:Artifact = Join-Path $script:Root 'finding.json'
        Write-PullRequestOutputFileAtomically -Path $script:Inventory -Content (ConvertTo-JsonText -Value $script:Files)
        Write-PullRequestOutputFileAtomically -Path $script:Grouped -Content (ConvertTo-JsonText -Value $script:Groups)
        $script:Arguments = @{ QueuePath = $script:Queue; InventoryPath = $script:Inventory; FileGroupsPath = $script:Grouped }
    }

    # Batch drafts are typed content, not a second journal assembled by a worker.
    function New-TestBatch {
        param($Claim)
        [ordered]@{
            schemaVersion = 1; snapshot = $Claim.snapshot; groupId = $Claim.item.groupId
            assessments = @($Claim.item.files | ForEach-Object {
                [ordered]@{ groupId = $Claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
            })
            findingIds = @()
        }
    }

    # Generated fixtures use the production assembler so cached reading and section coverage agree.
    function New-TestScopeArea {
        param([AllowNull()] $Patterns = @('**/*.ps1'), [switch] $Mixed, [switch] $Uncertain)
        foreach ($name in @('instruction-set-source', 'instruction-set-raw')) {
            [void] [IO.Directory]::CreateDirectory((Join-Path $script:Root $name))
            [IO.File]::WriteAllText((Join-Path $script:Root "$name\rules.md"), "Paths only.`n# Scripts`nCheck scripts.`n# Rust`nCheck Rust.`n")
        }
        $span = @{ root = 'instruction-set-raw'; path = 'rules.md'; startLine = 1; endLine = 1 }
        $context = @{
            effectiveScope = 'Paths matched by the complete declared alternatives.'
            pathScope = if ($null -eq $Patterns) { $null } else { @{ base = '.'; include = @($Patterns) } }
            declarations = @($span); origins = @(); uncertainties = @()
        }
        $chapters = @(@{
            id = 'scripts'; categoryId = 'all'; content = @{ root = 'instruction-set-raw'; path = 'rules.md'; startLine = 2; endLine = 3 }
            sources = @($span); applicability = @($context)
        })
        if ($Mixed) {
            $chapters += @{
                id = 'rust'; categoryId = 'all'; content = @{ root = 'instruction-set-raw'; path = 'rules.md'; startLine = 4; endLine = 5 }
                sources = @($span)
                applicability = @(@{
                    effectiveScope = 'Rust paths.'; pathScope = @{ base = '.'; include = @('**/*.rs') }
                    declarations = @($span); origins = @(); uncertainties = @()
                })
            }
        }
        if ($Uncertain) {
            $context.pathScope = $null
            $context.uncertainties = @(@{ reason = 'Artifact role needs content inspection.'; evidence = @($span) })
        }
        $map = @{ schemaVersion = 1; categories = @(@{ id = 'all'; title = 'All' }); chapters = $chapters }
        $validated = Test-FocusAreaMapValue -Json (ConvertTo-JsonText -Value $map) -SessionStateDirectory $script:Root
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
        $directory = Join-Path $script:Root 'focus-areas'
        [void] [IO.Directory]::CreateDirectory($directory)
        $script:Area = Join-Path $directory 'focus-all.md'
        [IO.File]::WriteAllText($script:Area, $documents['focus-areas\focus-all.md'])
        $directory
    }
}

Describe 'Atomic assessment batches' {
    BeforeEach {
        New-CoverageFixture
        New-ReviewFileGroupQueue @script:Arguments | Out-Null
        $script:Batch = New-TestBatch -Claim (Get-NextReviewWorkItem -QueuePath $script:Queue)
        Write-FindingArtifact -Path $script:Artifact -Document (New-FindingArtifactDocument -Kind reviewer -ReviewerId focus-001 -FocusArea rules)
    }

    It 'does not complete the group when its atomic commit fails' {
        Mock Write-PullRequestOutputFileAtomically { throw 'Injected commit failure.' }
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*commit failure*'
        (Read-ReviewQueueDocument -Path $script:Queue).completedGroupIds.Count | Should -Be 0
    }

    It 'persists the complete batch and recognizes its retry after another group is claimed' {
        $first = Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch
        $first.alreadyComplete | Should -BeFalse
        $journal = Read-ReviewQueueDocument -Path $script:Queue
        $journal.assessments.path | Should -Be $script:Groups[0].files.path
        $journal.completedGroupIds | Should -Be @('rename')
        $journal.inProgress | Should -BeNullOrEmpty
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'other'
        $before = [IO.File]::ReadAllText($script:Queue)
        Mock Write-PullRequestOutputFileAtomically { throw 'Unexpected rewrite.' }
        (Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch).alreadyComplete | Should -BeTrue
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
    }

    It 'validates immutable inputs once and replaces the journal once for the entire batch' {
        $script:ReadBatchInputs = ${function:Read-ReviewWorkInputs}
        $script:CommitBatch = ${function:Write-PullRequestOutputFileAtomically}
        Mock Read-ReviewWorkInputs {
            param($InventoryPath, $FileGroupsPath)
            & $script:ReadBatchInputs -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath
        }
        Mock Write-PullRequestOutputFileAtomically {
            param($Path, $Content)
            & $script:CommitBatch -Path $Path -Content $Content
        }
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        Should -Invoke Read-ReviewWorkInputs -Exactly -Times 1
        Should -Invoke Write-PullRequestOutputFileAtomically -Exactly -Times 1
        (Read-ReviewQueueDocument -Path $script:Queue).assessments.Count | Should -Be 2
    }

    It 'rejects partial, duplicate, foreign and stale drafts without changing the journal: <Case>' -ForEach @(
        @{ Case = 'partial'; Mutate = { $script:Batch.assessments = @($script:Batch.assessments[0]) } }
        @{ Case = 'duplicate'; Mutate = { $script:Batch.assessments += $script:Batch.assessments[0] } }
        @{ Case = 'foreign path'; Mutate = { $script:Batch.assessments[0].path = $script:Files[2].path } }
        @{ Case = 'wrong group'; Mutate = { $script:Batch.assessments[0].groupId = 'other' } }
        @{ Case = 'stale identity'; Mutate = { $script:Batch.snapshot.inventorySha256 = '0' * 64 } }
        @{ Case = 'missing finding'; Mutate = { $script:Batch.findingIds = @('focus-001-1') } }
        @{ Case = 'unclaimed'; Mutate = { $script:Batch.groupId = 'other'; $script:Batch.assessments = @() } }
    ) {
        $before = [IO.File]::ReadAllText($script:Queue)
        & $Mutate
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
    }

    It 'rejects changed immutable input even when both inventory and groups are changed consistently' {
        $script:Files[0].sizeBytes = 2
        Write-PullRequestOutputFileAtomically -Path $script:Inventory -Content (ConvertTo-JsonText -Value $script:Files)
        Write-PullRequestOutputFileAtomically -Path $script:Grouped -Content (ConvertTo-JsonText -Value $script:Groups)
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*snapshot*'
        { New-ReviewFileGroupQueue @script:Arguments -Resume } | Should -Throw '*snapshot*'
    }

    It 'rejects already recorded paths on initial completion after resume: <Entry>' -ForEach @(
        @{ Entry = 'identical'; ChangeReason = $false }
        @{ Entry = 'changed'; ChangeReason = $true }
    ) {
        Set-ReviewFileAssessment @script:Arguments -Assessment $script:Batch.assessments[0]
        New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null
        if ($ChangeReason) { $script:Batch.assessments[0].reason = 'A replacement assessment.' }
        $before = [IO.File]::ReadAllText($script:Queue)
        $findingsBefore = [IO.File]::ReadAllText($script:Artifact)
        Mock Write-PullRequestOutputFileAtomically { throw 'Rejected batches must not write.' }

        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } |
            Should -Throw '*already has a persisted assessment*'

        Should -Invoke Write-PullRequestOutputFileAtomically -Exactly -Times 0
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
        [IO.File]::ReadAllText($script:Artifact) | Should -BeExactly $findingsBefore
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'rename'
    }

    It 'preserves findings through interruption, resume and batch completion' {
        $artifact = Read-FindingArtifact -Path $script:Artifact
        $id = Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'; anchor = @{ kind = 'file'; path = 'old.rs' }
        }
        Write-FindingArtifact -Path $script:Artifact -Document $artifact
        $script:Batch.findingIds = @($id)
        $findingBefore = [IO.File]::ReadAllText($script:Artifact)
        New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'rename'
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        [IO.File]::ReadAllText($script:Artifact) | Should -BeExactly $findingBefore
        $next = New-TestBatch -Claim (Get-NextReviewWorkItem -QueuePath $script:Queue)
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $next | Out-Null
        Assert-ReviewFileGroupQueueComplete -Path $script:Queue -Group $script:Groups
        $artifact.findings = @()
        Write-FindingArtifact -Path $script:Artifact -Document $artifact
        { Assert-ReviewFileGroupQueueComplete -Path $script:Queue -Group $script:Groups } | Should -Throw '*not persisted*'
    }

    It 'preserves receipts and withdrawal history through retry and completion: <Case>' -ForEach @(
        @{ Case = 'withdrawn before batch'; WithdrawBeforeBatch = $true; KeepActive = $false }
        @{ Case = 'withdrawn after batch'; WithdrawBeforeBatch = $false; KeepActive = $false }
        @{ Case = 'active and withdrawn'; WithdrawBeforeBatch = $false; KeepActive = $true }
    ) {
        $artifact = Read-FindingArtifact -Path $script:Artifact
        $finding = @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'; anchor = @{ kind = 'file'; path = 'old.rs' }
        }
        $withdrawnId = Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding $finding
        $script:Batch.findingIds = @($withdrawnId)
        if ($KeepActive) {
            $script:Batch.findingIds += Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding $finding
        }
        Write-FindingArtifact -Path $script:Artifact -Document $artifact
        if (-not $WithdrawBeforeBatch) {
            Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        }
        $removal = & "$script:Scripts\Remove-Finding.ps1" -ArtifactPath $script:Artifact -FindingId $withdrawnId `
            -Reason 'The evidence does not support this finding.' -NowUtc ([DateTimeOffset] '2024-05-06T07:08:09Z') | ConvertFrom-Json
        $removal.removed | Should -Be @($withdrawnId)
        $withdrawn = Read-FindingArtifact -Path $script:Artifact
        $historyBefore = ConvertTo-JsonText -Value $withdrawn.removed
        if ($WithdrawBeforeBatch) {
            Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        }
        $next = New-TestBatch -Claim (Get-NextReviewWorkItem -QueuePath $script:Queue)
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $next | Out-Null
        $journalBefore = [IO.File]::ReadAllText($script:Queue)
        $artifactBefore = [IO.File]::ReadAllText($script:Artifact)

        (Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch).alreadyComplete | Should -BeTrue
        Assert-ReviewFileGroupQueueComplete -Path $script:Queue -Group $script:Groups
        [IO.File]::ReadAllText($script:Artifact) | Should -BeExactly $artifactBefore
        $result = & "$script:Scripts\Complete-FindingArtifact.ps1" -ArtifactPath $script:Artifact -FileGroupQueuePath $script:Queue |
            ConvertFrom-Json

        $result.status | Should -BeExactly 'complete'
        $result.findingCount | Should -Be ([int] $KeepActive)
        $completed = Read-FindingArtifact -Path $script:Artifact
        @($completed.findings | Where-Object { $_.id -ceq $withdrawnId }).Count | Should -Be 0
        (ConvertTo-JsonText -Value $completed.removed) | Should -BeExactly $historyBefore
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $journalBefore

        # An identifier alone does not preserve the evidence that completion receipts require.
        $completed.removed[0].finding = @{ id = $withdrawnId }
        Write-FindingArtifact -Path $script:Artifact -Document $completed
        $malformedBefore = [IO.File]::ReadAllText($script:Artifact)
        { & "$script:Scripts\Complete-FindingArtifact.ps1" -ArtifactPath $script:Artifact -FileGroupQueuePath $script:Queue } |
            Should -Throw '*removal record*missing*'
        [IO.File]::ReadAllText($script:Artifact) | Should -BeExactly $malformedBefore
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $journalBefore

        # History is required evidence, not permission to accept any missing identifier.
        $completed.removed = @()
        Write-FindingArtifact -Path $script:Artifact -Document $completed
        { Assert-ReviewFileGroupQueueComplete -Path $script:Queue -Group $script:Groups } | Should -Throw '*not persisted*'
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $journalBefore
    }

    It 'rejects conflicting retries and retries of subsequently repaired assessments' {
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        $script:Batch.assessments[0].reason = 'Different judgment.'
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*Conflicting*'
        Set-ReviewFileAssessment @script:Arguments -Assessment $script:Batch.assessments[0]
        $script:Batch.assessments[0].reason = ''
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*Conflicting*'
    }

    It 'retains the previous journal when staging fails and resumes the same claim' {
        $before = [IO.File]::ReadAllText($script:Queue)
        Mock Write-PullRequestStagingFile {
            param($Path, $Content)
            [IO.File]::WriteAllText($Path, $Content.Substring(0, [Math]::Min(10, $Content.Length)))
            throw 'Injected interrupted staging write.'
        }
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*interrupted*'
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
        New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'rename'
        @(Get-ChildItem -LiteralPath $script:Root -Filter '*.tmp' -Force).Count | Should -Be 0
    }

    It 'waits for concurrent mutation under the existing sibling lock' {
        $script:ContendedLock = Open-ReviewQueueLock -Path $script:Queue
        Mock Start-Sleep { $script:ContendedLock.Dispose() }
        try {
            Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
        } finally { $script:ContendedLock.Dispose() }
        (Read-ReviewQueueDocument -Path $script:Queue).completedGroupIds | Should -Contain $script:Batch.groupId
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'requires explicit validated resume after a worker failure without losing prior work' {
        Set-ReviewFileAssessment @script:Arguments -Assessment $script:Batch.assessments[0]
        $result = & "$script:Scripts\Set-ReviewWorkerFailure.ps1" -QueuePath $script:Queue -Reason 'Worker exhausted its runtime context.' | ConvertFrom-Json
        $result.status | Should -Be 'Failed'
        $before = [IO.File]::ReadAllText($script:Queue)
        Set-ReviewWorkerFailure -QueuePath $script:Queue -Reason 'Worker exhausted its runtime context.' | Out-Null
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
        { Get-NextReviewWorkItem -QueuePath $script:Queue } | Should -Throw '*failure*'
        { Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch } | Should -Throw '*failed work*'
        { Complete-ReviewFileGroup -QueuePath $script:Queue -GroupId rename } | Should -Throw '*failed work*'
        { Assert-ReviewFileGroupQueueComplete -Path $script:Queue } | Should -Throw '*failed work*'
        New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null
        $journal = Read-ReviewQueueDocument -Path $script:Queue
        $journal.failure | Should -BeNullOrEmpty
        $journal.assessments.Count | Should -Be 1
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'rename'
        $script:Batch.assessments = @($script:Batch.assessments | Where-Object { $_.path -cne $journal.assessments[0].path })
        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $script:Batch | Out-Null
    }

    It 'runs the prepared batch command with a single JSON response' {
        $draft = Join-Path $script:Root 'batch.json'
        [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $script:Batch))
        $result = & "$script:Scripts\Set-ReviewAssessmentBatch.ps1" @script:Arguments -ArtifactPath $script:Artifact -BatchPath $draft | ConvertFrom-Json
        $result.groupId | Should -Be 'rename'
        $result.assessmentCount | Should -Be 2
    }

    It 'preserves a PowerShell journal through script failure, resume, batch retry and native artifact completion' {
        $artifact = Read-FindingArtifact -Path $script:Artifact
        $id = Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'; anchor = @{ kind = 'file'; path = 'old.rs' }
        }
        Write-FindingArtifact -Path $script:Artifact -Document $artifact
        $script:Batch.findingIds = @($id)
        $draft = Join-Path $script:Root 'batch.json'
        [IO.File]::WriteAllText($draft, (ConvertTo-JsonText -Value $script:Batch))

        & "$script:Scripts\Set-ReviewWorkerFailure.ps1" -QueuePath $script:Queue -Reason 'Worker stopped.' | Out-Null
        $failed = [IO.File]::ReadAllText($script:Queue)
        { & "$script:Scripts\Get-NextReviewItem.ps1" -QueuePath $script:Queue } | Should -Throw
        { & "$script:Scripts\Set-ReviewAssessmentBatch.ps1" @script:Arguments -ArtifactPath $script:Artifact -BatchPath $draft } | Should -Throw
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $failed

        & "$script:Scripts\New-ReviewFileGroupQueue.ps1" @script:Arguments -Resume | Out-Null
        $claim = & "$script:Scripts\Get-NextReviewItem.ps1" -QueuePath $script:Queue | ConvertFrom-Json -AsHashtable
        $claim.item.groupId | Should -BeExactly 'rename'
        $first = & "$script:Scripts\Set-ReviewAssessmentBatch.ps1" @script:Arguments -ArtifactPath $script:Artifact -BatchPath $draft |
            ConvertFrom-Json -AsHashtable
        $first.alreadyComplete | Should -BeFalse
        & "$script:Scripts\Remove-Finding.ps1" -ArtifactPath $script:Artifact -FindingId $id `
            -Reason 'The evidence does not support this finding.' -NowUtc ([DateTimeOffset] '2024-05-06T07:08:09Z') | Out-Null
        $secondClaim = & "$script:Scripts\Get-NextReviewItem.ps1" -QueuePath $script:Queue | ConvertFrom-Json -AsHashtable
        $secondClaim.item.groupId | Should -BeExactly 'other'
        $secondDraft = Join-Path $script:Root 'second-batch.json'
        [IO.File]::WriteAllText($secondDraft, (ConvertTo-JsonText -Value (New-TestBatch -Claim $secondClaim)))
        & "$script:Scripts\Set-ReviewAssessmentBatch.ps1" @script:Arguments -ArtifactPath $script:Artifact -BatchPath $secondDraft | Out-Null

        $beforeRetry = [IO.File]::ReadAllText($script:Queue)
        $retry = & "$script:Scripts\Set-ReviewAssessmentBatch.ps1" @script:Arguments -ArtifactPath $script:Artifact -BatchPath $draft |
            ConvertFrom-Json -AsHashtable
        $retry.alreadyComplete | Should -BeTrue
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $beforeRetry
        $journal = Read-ReviewQueueDocument -Path $script:Queue
        @($journal.completedGroupIds).Count | Should -Be 2
        @($journal.batchReceipts).Count | Should -Be 2
        $result = & "$script:Scripts\Complete-FindingArtifact.ps1" -ArtifactPath $script:Artifact -FileGroupQueuePath $script:Queue |
            ConvertFrom-Json -AsHashtable
        $result.status | Should -BeExactly 'complete'
        $result.findingCount | Should -Be 0
        $completed = Read-FindingArtifact -Path $script:Artifact
        $completed.removed[0].finding.id | Should -BeExactly $id
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $beforeRetry
    }

    It 'preserves coverage across bounded randomized batch, retry and resume operations' {
        $seed = Get-Random
        Write-Information "Batch sequence seed: $seed" -InformationAction Continue
        $random = [Random]::new($seed)
        $committed = [Collections.Generic.List[object]]::new()
        # Bound operations rather than clock time; a fixed tail completes any remaining groups.
        foreach ($step in 1..32) {
            switch ($random.Next(3)) {
                0 { New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null }
                1 {
                    $claim = Get-NextReviewWorkItem -QueuePath $script:Queue
                    if (-not $claim.exhausted) {
                        $batch = New-TestBatch -Claim $claim
                        Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $batch | Out-Null
                        $committed.Add($batch)
                    }
                }
                2 {
                    if ($committed.Count -gt 0) {
                        (Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch $committed[$random.Next($committed.Count)]).alreadyComplete | Should -BeTrue
                    }
                }
            }
            Assert-ReviewGroupJournalCoverage -Queue (Read-ReviewQueueDocument -Path $script:Queue) -Group $script:Groups
        }
        foreach ($step in 1..$script:Groups.Count) {
            $claim = Get-NextReviewWorkItem -QueuePath $script:Queue
            if (-not $claim.exhausted) {
                Set-ReviewAssessmentBatch @script:Arguments -ArtifactPath $script:Artifact -Batch (New-TestBatch -Claim $claim) | Out-Null
            }
        }
        Assert-ReviewFileGroupQueueComplete -Path $script:Queue -Group $script:Groups
    }
}

Describe 'Predispatch path-scope exclusions' {
    BeforeEach { New-CoverageFixture }

    It 'keeps a file included by any section or referring alternative' {
        $null = New-TestScopeArea -Mixed
        $map = Read-ReviewApplicability -Path $script:Area
        @(Get-ReviewScopeExclusions -Map $map -RepositoryRoot $script:Root -Group $script:Groups).Count | Should -Be 0
        $map.chapters[0].applicability += $map.chapters[1].applicability
        $map.chapters = @($map.chapters[0])
        @(Get-ReviewScopeExclusions -Map $map -RepositoryRoot $script:Root -Group $script:Groups).Count | Should -Be 0
    }

    It 'retains semantic, absent and uncertain applicability: <Kind>' -ForEach @(
        @{ Kind = 'semantic' }, @{ Kind = 'absent' }, @{ Kind = 'uncertain' }
    ) {
        $null = New-TestScopeArea -Patterns $null -Uncertain:($Kind -eq 'uncertain')
        $map = Read-ReviewApplicability -Path $script:Area
        if ($Kind -eq 'absent') { $map.chapters[0].applicability[0].Remove('pathScope') }
        @(Get-ReviewScopeExclusions -Map $map -RepositoryRoot $script:Root -Group $script:Groups).Count | Should -Be 0
    }

    It 'matches root and nested globs conservatively without inferring filesystem case behavior' {
        foreach ($path in @('CODE.PS1', 'src/code.ps1', 'src/deep/code.ps1')) {
            Test-ReviewPathScope -Scope @{ base = '.'; include = @('**/*.ps1') } -RelativePath $path | Should -BeTrue
        }
        Test-ReviewPathScope -Scope @{ base = 'scripts'; include = @('*.ps1') } -RelativePath 'scripts/a.ps1' | Should -BeTrue
        Test-ReviewPathScope -Scope @{ base = 'scripts'; include = @('*.ps1') } -RelativePath 'scripts/deep/a.ps1' | Should -BeFalse
        { Test-ReviewPathScope -Scope @{ base = '.'; include = @('a**b') } -RelativePath 'a.rs' } | Should -Throw '*Unsupported*'
    }

    It 'rejects damaged generated metadata rather than treating it as an empty scope' {
        $null = New-TestScopeArea
        [IO.File]::AppendAllText($script:Area, 'Changed guidance.')
        { Read-ReviewApplicability -Path $script:Area } | Should -Throw '*Stale*'
    }

    It 'retains cross-group concerns and files outside the declared repository root' {
        $null = New-TestScopeArea -Patterns @('**/Cargo.toml')
        $map = Read-ReviewApplicability -Path $script:Area
        $map.chapters[0].applicability[0].pathScope = $null
        $map.chapters[0].applicability[0].effectiveScope = 'All files influencing manifest correctness, including callers in other groups.'
        @(Get-ReviewScopeExclusions -Map $map -RepositoryRoot $script:Root -Group $script:Groups).Count | Should -Be 0
        $map = Read-ReviewApplicability -Path $script:Area
        @(Get-ReviewScopeExclusions -Map $map -RepositoryRoot (Join-Path $script:Root 'nested') -Group $script:Groups).Count | Should -Be 0
    }

    It 'keeps incomplete dispatched work distinct from completed-empty excluded work' {
        $areas = New-TestScopeArea -Mixed
        $findings = Join-Path $script:Root 'findings'
        $work = New-ReviewWork -InventoryPath $script:Inventory -FileGroupsPath $script:Grouped -FocusAreaDirectory $areas -OutputDirectory (Join-Path $script:Root 'work') -RepositoryRoot $script:Root -ArtifactDirectory $findings
        $work.excludedReviewerCount | Should -Be 0
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath | Out-Null
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings
        $result.isValid | Should -BeFalse
        $result.incompleteReviewers | Should -Be @($worker.reviewerId)
        $result.failedReviewers.Count | Should -Be 0
        Set-ReviewWorkerFailure -QueuePath $worker.fileGroupQueuePath -Reason 'Worker reported an execution failure.' | Out-Null
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings
        $result.isValid | Should -BeFalse
        $result.failedReviewers | Should -Be @($worker.reviewerId)
        Remove-Item -LiteralPath $worker.artifactPath
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings
        $result.isValid | Should -BeFalse
        $result.missingReviewers | Should -Be @($worker.reviewerId)
        { Assert-ReviewWorkerArtifactState -ArtifactPath $worker.artifactPath -QueuePath $worker.fileGroupQueuePath } | Should -Throw '*missing*'
    }

    It 'prepares partial exclusions once and lets a batch retain their provenance' {
        $areas = New-TestScopeArea
        $findings = Join-Path $script:Root 'findings'
        $work = New-ReviewWork -InventoryPath $script:Inventory -FileGroupsPath $script:Grouped -FocusAreaDirectory $areas -OutputDirectory (Join-Path $script:Root 'work') -RepositoryRoot $script:Root -ArtifactDirectory $findings
        $work.dispatchCount | Should -Be 1
        $reviewer = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        $journal = Read-ReviewQueueDocument -Path $reviewer.fileGroupQueuePath
        $journal.assessments.path | Should -Be @($script:Files[0].path, $script:Files[2].path)
        $journal.completedGroupIds | Should -Be @('other')
        $queueArguments = @{ QueuePath = $reviewer.fileGroupQueuePath; InventoryPath = $work.inventoryPath; FileGroupsPath = $work.fileGroupsPath }
        New-ReviewFileGroupQueue @queueArguments -Resume | Out-Null
        $batch = New-TestBatch -Claim (Get-NextReviewWorkItem -QueuePath $reviewer.fileGroupQueuePath)
        $before = [IO.File]::ReadAllText($reviewer.fileGroupQueuePath)
        { Set-ReviewAssessmentBatch @queueArguments -ArtifactPath $reviewer.artifactPath -Batch $batch } |
            Should -Throw '*already has a persisted assessment*'
        [IO.File]::ReadAllText($reviewer.fileGroupQueuePath) | Should -BeExactly $before
        $batch.assessments = @($batch.assessments | Where-Object { $_.path -ceq $script:Files[1].path })
        Set-ReviewAssessmentBatch @queueArguments -ArtifactPath $reviewer.artifactPath -Batch $batch | Out-Null
        $completed = [IO.File]::ReadAllText($reviewer.fileGroupQueuePath)
        (Set-ReviewAssessmentBatch @queueArguments -ArtifactPath $reviewer.artifactPath -Batch $batch).alreadyComplete | Should -BeTrue
        [IO.File]::ReadAllText($reviewer.fileGroupQueuePath) | Should -BeExactly $completed
        $artifact = Read-FindingArtifact -Path $reviewer.artifactPath
        Complete-FindingArtifactDocument -Document $artifact
        Write-FindingArtifact -Path $reviewer.artifactPath -Document $artifact
        (Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings).isValid | Should -BeTrue
        $retained = @((Read-ReviewQueueDocument -Path $reviewer.fileGroupQueuePath).assessments | Where-Object { $_.disposition -ceq 'excluded' })
        foreach ($expected in $journal.assessments) {
            $actual = @($retained | Where-Object { $_.path -ceq $expected.path })
            $actual.Count | Should -Be 1
            (ConvertTo-JsonText -Value $actual[0]) | Should -BeExactly (ConvertTo-JsonText -Value $expected)
        }
    }

    It 'completes an empty applicable set without dispatch and revalidates every exclusion at the gate' {
        $areas = New-TestScopeArea -Patterns @('**/Cargo.toml')
        $findings = Join-Path $script:Root 'findings'
        $work = New-ReviewWork -InventoryPath $script:Inventory -FileGroupsPath $script:Grouped -FocusAreaDirectory $areas -OutputDirectory (Join-Path $script:Root 'work') -RepositoryRoot $script:Root -ArtifactDirectory $findings
        $work.dispatchCount | Should -Be 0
        $work.excludedReviewerCount | Should -Be 1
        (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).exhausted | Should -BeTrue
        $reviewer = (Read-ReviewWorkManifest -Path $work.manifestPath).reviewers[0]
        $reviewer.dispatch | Should -Be 'excluded'
        (Read-FindingArtifact -Path $reviewer.artifactPath).status | Should -Be 'complete'
        (Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings).isValid | Should -BeTrue
        $journal = Read-ReviewQueueDocument -Path $reviewer.fileGroupQueuePath
        $journal.assessments.Count | Should -Be $script:Files.Count
        $journal.assessments[0].reason = 'Invented reason.'
        Write-PullRequestOutputFileAtomically -Path $reviewer.fileGroupQueuePath -Content (ConvertTo-JsonText -Value $journal)
        (Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $findings).isValid | Should -BeFalse
    }
}
