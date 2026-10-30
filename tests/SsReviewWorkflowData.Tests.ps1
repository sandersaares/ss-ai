BeforeAll {
    $script:Scripts = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review\scripts'
    . "$script:Scripts\lib\JsonValue.ps1"
    . "$script:Scripts\lib\PullRequestModel.ps1"
    . "$script:Scripts\lib\WorkflowData.ps1"
    . "$script:Scripts\lib\FindingModel.ps1"
    . "$script:Scripts\lib\ReviewWork.ps1"
    . "$script:Scripts\lib\NativeWorkflowBridge.ps1"
    . "$script:Scripts\lib\ReviewAuditState.ps1"
    . "$script:Scripts\lib\FocusAreaPreparation.ps1"
    . "$script:Scripts\lib\FocusAreaExpansion.ps1"

    # Runs actual entry points without interactive input or unbounded output-pipe waits.
    function Invoke-DataCommand {
        param([string] $Name, [string[]] $Arguments)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.RedirectStandardInput = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', "$script:Scripts\$Name") + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $process.StandardInput.Close()
            $output = $process.StandardOutput.ReadToEndAsync()
            $errorOutput = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{ exitCode = $process.ExitCode; output = $output.GetAwaiter().GetResult(); error = $errorOutput.GetAwaiter().GetResult() }
        } finally { $process.Dispose() }
    }

    # Direct source lines are frozen throughout each expansion test.
    function New-ExpansionReferrer {
        param([AllowNull()][string] $ParentId = $null, [string] $Path = 'source.md')
        $root = if ($ParentId) { 'instruction-set-raw' } else { 'instruction-set-source' }
        [ordered]@{
            parentId = if ($ParentId) { $ParentId } else { $null }
            source = [ordered]@{ root = $root; path = $Path; startLine = 1; endLine = 1 }
            scope = 'Published APIs.'
            declarations = @()
        }
    }

    # Artifact content uses the real finding validator; feedback IDs cannot name arbitrary records.
    function New-FeedbackFinding {
        [ordered]@{
            title = 'Preserve user text'; description = 'Feedback belongs to a finding.'
            reasoning = 'The user responds to a specific claim.'; consequence = 'Losing text loses the decision context.'
            recommendedAction = 'Preserve the supplied text.'; anchor = @{ kind = 'file'; path = 'review.ps1' }
        }
    }

    # Consumer fixtures use the real producer so the index contract cannot drift to ID-only records.
    function New-CandidateTestIndex {
        param([AllowEmptyCollection()][string[]] $ReviewerId = @('a', 'b'))
        $artifacts = @(foreach ($id in $ReviewerId) {
            $document = New-FindingArtifactDocument -Kind reviewer -ReviewerId $id -FocusArea 'correctness'
            Add-FindingToArtifact -Document $document -Finding (New-FeedbackFinding) -Origin reviewer | Out-Null
            @{ document = $document }
        })
        New-FindingIndexDocument -ReviewerArtifact $artifacts
    }
}

Describe 'Typed workflow plans' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        [IO.Directory]::CreateDirectory($script:Root) | Out-Null
        $script:Path = Join-Path $script:Root 'plan.json'
    }

    It 'reports the invalid field below schema references without emitting a result' {
        $document = @{
            schemaVersion = 1
            decisions = @(
                @{ findingId = 'a-1'; decision = 'approved' }
                @{ findingId = 'b-1'; decision = 'maybe' }
            )
        }
        $output = [System.Collections.Generic.List[object]]::new()
        { Assert-WorkflowData -Kind decisions -Value $document | ForEach-Object { $output.Add($_) } } |
            Should -Throw "*not valid with the schema*decisions/1/decision*"
        $output.Count | Should -Be 0
    }

    It 'rejects a malformed schema rather than accepting the document' {
        $schemaPath = Join-Path $script:Root 'malformed.schema.json'
        # A numeric type keyword is invalid schema syntax, not an invalid workflow value.
        [IO.File]::WriteAllText($schemaPath, '{"$defs":{"decisions":{"type":42}}}')
        Mock Read-PullRequestFileText { [IO.File]::ReadAllText($schemaPath) } -ParameterFilter { $Path -eq $script:WorkflowSchemaPath }
        { Assert-WorkflowData -Kind decisions -Value @{ schemaVersion = 1; decisions = @() } } |
            Should -Throw "*Invalid 'decisions' workflow schema*Expected string*"
    }

    It 'initializes explicit empty plans and rejects missing or misspelled envelopes' {
        New-WorkflowList -Kind mergePlan -Path $script:Path
        (Read-FindingMergePlan -Path $script:Path).Count | Should -Be 0
        { New-WorkflowList -Kind mergePlan -Path $script:Path } | Should -Throw '*already exists*'
        foreach ($text in @('{}', '{"groups":[]}', '{"schemaVersion":1,"group":[]}', '{"schemaVersion":1,"groups":{}}')) {
            [IO.File]::WriteAllText($script:Path, $text)
            { Read-FindingMergePlan -Path $script:Path } | Should -Throw
        }
        { Read-FindingMergePlan -Path (Join-Path $script:Root 'missing.json') } | Should -Throw '*no merge plan*'
    }

    It 'appends idempotently and rejects overlapping actual merges without changing earlier entries' {
        New-WorkflowList -Kind mergePlan -Path $script:Path
        $item = [ordered]@{ sourceFindingIds = @('a-1', 'b-1'); findingPath = 'merged.json' }
        Add-WorkflowListItem -Kind mergePlan -Path $script:Path -Item $item
        Add-WorkflowListItem -Kind mergePlan -Path $script:Path -Item $item
        (Read-WorkflowData -Kind mergePlan -Path $script:Path).groups.Count | Should -Be 1
        $before = [IO.File]::ReadAllText($script:Path)
        { Add-WorkflowListItem -Kind mergePlan -Path $script:Path -Item @{ sourceFindingIds = @('a-1', 'c-1'); findingPath = 'other.json' } } |
            Should -Throw '*multiple merge groups*'
        [IO.File]::ReadAllText($script:Path) | Should -BeExactly $before
    }

    It 'rejects malformed drafts: <Case>' -ForEach @(
        @{ Case = 'scalar IDs'; Kind = 'mergeCandidates'; Item = @{ findingIds = 'a-1' } }
        @{ Case = 'one ID'; Kind = 'mergeCandidates'; Item = @{ findingIds = @('a-1') } }
        @{ Case = 'repeated ID'; Kind = 'mergeCandidates'; Item = @{ findingIds = @('a-1', 'a-1') } }
        @{ Case = 'unknown field'; Kind = 'updates'; Item = @{ findingId = 'a-1'; findingPath = 'x'; text = 'unexpected' } }
        @{ Case = 'missing reason'; Kind = 'removals'; Item = @{ findingId = 'a-1' } }
        @{ Case = 'invalid verdict'; Kind = 'decisions'; Item = @{ findingId = 'a-1'; decision = 'maybe' } }
    ) {
        New-WorkflowList -Kind $Kind -Path $script:Path
        $before = [IO.File]::ReadAllText($script:Path)
        { Add-WorkflowListItem -Kind $Kind -Path $script:Path -Item $Item } | Should -Throw
        [IO.File]::ReadAllText($script:Path) | Should -BeExactly $before
    }

    It 'waits for a concurrent writer without truncating a plan' {
        New-WorkflowList -Kind removals -Path $script:Path
        $script:PlanLock = Open-WorkflowDataLock -Path $script:Path
        Mock Start-Sleep { $script:PlanLock.Dispose() }
        try {
            Add-WorkflowListItem -Kind removals -Path $script:Path -Item @{ findingId = 'a'; reason = 'Superseded.' }
        } finally { $script:PlanLock.Dispose() }
        Should -Invoke Start-Sleep -Times 1 -Exactly
        (Read-WorkflowData -Kind removals -Path $script:Path).removals.Count | Should -Be 1
    }

    It 'corrects one plan item without rewriting the remaining records' {
        New-WorkflowList -Kind removals -Path $script:Path
        Add-WorkflowListItem -Kind removals -Path $script:Path -Item @{ findingId = 'a'; reason = 'Superseded.' }
        Add-WorkflowListItem -Kind removals -Path $script:Path -Item @{ findingId = 'b'; reason = 'Duplicate.' }
        Add-WorkflowListItem -Kind removals -Path $script:Path -Item @{ findingId = 'a'; reason = 'Already addressed.' } -ReplaceIndex 0
        $document = Read-WorkflowData -Kind removals -Path $script:Path
        $document.removals[0].reason | Should -BeExactly 'Already addressed.'
        $document.removals[1].reason | Should -BeExactly 'Duplicate.'
        { Add-WorkflowListItem -Kind removals -Path $script:Path -Item @{ findingId = 'b'; reason = 'Conflict.' } -ReplaceIndex 0 } |
            Should -Throw '*Repeated finding*'
    }

    It 'runs prepared plan writers and rejects unknown candidate IDs before dispatch' {
        $run = Invoke-DataCommand 'New-WorkflowData.ps1' @('-Kind', 'mergeCandidates', '-Path', $script:Path)
        $run.exitCode | Should -Be 0 -Because $run.error
        $itemPath = Join-Path $script:Root 'item.json'
        [IO.File]::WriteAllText($itemPath, '{"findingIds":["a-1","b-1"]}')
        $run = Invoke-DataCommand 'Add-WorkflowDataItem.ps1' @('-Kind', 'mergeCandidates', '-Path', $script:Path, '-ItemPath', $itemPath)
        $run.exitCode | Should -Be 0 -Because $run.error
        $indexPath = Join-Path $script:Root 'index.json'
        [IO.File]::WriteAllText($indexPath, (ConvertTo-JsonText -Value (New-CandidateTestIndex)))
        $run = Invoke-DataCommand 'Test-FindingCandidates.ps1' @('-Path', $script:Path, '-IndexPath', $indexPath)
        $run.exitCode | Should -Be 0 -Because $run.error
        ($run.output | ConvertFrom-Json).clusterCount | Should -Be 1
        [IO.File]::WriteAllText($indexPath, (ConvertTo-JsonText -Value (New-CandidateTestIndex -ReviewerId @('a'))))
        $run = Invoke-DataCommand 'Test-FindingCandidates.ps1' @('-Path', $script:Path, '-IndexPath', $indexPath)
        $run.exitCode | Should -Not -Be 0
        $run.error | Should -Match 'unknown finding'
    }

    It 'rejects invalid finding indexes even when no candidate clusters are proposed: <Case>' -ForEach @(
        @{ Case = 'empty object'; Change = { param($index) $index.Clear() } }
        @{ Case = 'missing version'; Change = { param($index) $index.Remove('schemaVersion') } }
        @{ Case = 'unsupported version'; Change = { param($index) $index.schemaVersion = 2 } }
        @{ Case = 'missing findings'; Change = { param($index) $index.Remove('findings') } }
        @{ Case = 'null findings'; Change = { param($index) $index.findings = $null } }
        @{ Case = 'scalar findings'; Change = { param($index) $index.findings = $index.findings[0] } }
        @{ Case = 'incomplete entry'; Change = { param($index) $index.findings[0].Remove('reviewerId') } }
        @{ Case = 'non-array paths'; Change = { param($index) $index.findings[0].impactedPaths = 'review.ps1' } }
        @{ Case = 'unexpected field'; Change = { param($index) $index.findings[0]['priority'] = 'high' } }
        @{ Case = 'repeated identifier'; Change = { param($index) $index.findings[1].id = $index.findings[0].id } }
    ) {
        New-WorkflowList -Kind mergeCandidates -Path $script:Path
        $index = New-CandidateTestIndex
        & $Change $index
        $indexPath = Join-Path $script:Root 'index.json'
        [IO.File]::WriteAllText($indexPath, (ConvertTo-JsonText -Value $index))
        $run = Invoke-DataCommand 'Test-FindingCandidates.ps1' @('-Path', $script:Path, '-IndexPath', $indexPath)
        $run.exitCode | Should -Not -Be 0
        $run.output | Should -BeNullOrEmpty
        $run.error | Should -Not -BeNullOrEmpty
    }

    It 'accepts an explicitly empty generated index and candidate plan' {
        New-WorkflowList -Kind mergeCandidates -Path $script:Path
        $indexPath = Join-Path $script:Root 'index.json'
        [IO.File]::WriteAllText($indexPath, (ConvertTo-JsonText -Value (New-CandidateTestIndex -ReviewerId @())))
        $run = Invoke-DataCommand 'Test-FindingCandidates.ps1' @('-Path', $script:Path, '-IndexPath', $indexPath)
        $run.exitCode | Should -Be 0 -Because $run.error
        ($run.output | ConvertFrom-Json).clusterCount | Should -Be 0
        (Read-WorkflowData -Kind findingIndex -Path $indexPath).findings.Count | Should -Be 0
    }
}

Describe 'Persisted file assessments' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        [IO.Directory]::CreateDirectory($script:Root) | Out-Null
        $script:Files = @(
            @{ path = Join-Path $script:Root 'a.rs'; sizeBytes = 1; change = 'modified' }
            @{ path = Join-Path $script:Root 'deleted.rs'; sizeBytes = 1; change = 'deleted' }
        )
        $script:Groups = @(@{ groupId = 'group-001'; files = $script:Files })
        $script:Inventory = Join-Path $script:Root 'inventory.json'
        $script:Grouped = Join-Path $script:Root 'groups.json'
        Write-PullRequestOutputFileAtomically -Path $script:Inventory -Content (ConvertTo-JsonText -Value $script:Files)
        Write-PullRequestOutputFileAtomically -Path $script:Grouped -Content (ConvertTo-JsonText -Value $script:Groups)
        $script:QueuePath = Join-Path $script:Root 'queue.json'
        $script:Arguments = @{ QueuePath = $script:QueuePath; InventoryPath = $script:Inventory; FileGroupsPath = $script:Grouped }
        New-ReviewFileGroupQueue @script:Arguments | Out-Null
        Get-NextReviewWorkItem -QueuePath $script:QueuePath | Out-Null
    }

    It 'requires every file before completion and keeps exclusion evidence through resume and repair' {
        { Complete-ReviewFileGroup -QueuePath $script:QueuePath -GroupId 'group-001' } | Should -Throw '*persisted assessment*'
        Set-ReviewFileAssessment @script:Arguments -Assessment @{
            groupId = 'group-001'; path = $script:Files[0].path; disposition = 'reviewed'; scope = ''; reason = ''
        }

        { Complete-ReviewFileGroup -QueuePath $script:QueuePath -GroupId 'group-001' } | Should -Throw '*persisted assessment*'
        $excluded = @{ groupId = 'group-001'; path = $script:Files[1].path; disposition = 'excluded'; scope = 'Published scripts only.'; reason = 'Deleted Rust file.' }
        Set-ReviewFileAssessment @script:Arguments -Assessment $excluded
        Complete-ReviewFileGroup -QueuePath $script:QueuePath -GroupId 'group-001' | Out-Null
        New-ReviewFileGroupQueue @script:Arguments -Resume | Out-Null
        (Read-ReviewQueueDocument -Path $script:QueuePath).assessments.Count | Should -Be 2
        $excluded.disposition = 'reviewed'
        $excluded.scope = ''; $excluded.reason = ''
        Set-ReviewFileAssessment @script:Arguments -Assessment $excluded
        Assert-ReviewFileGroupQueueComplete -Path $script:QueuePath -Group $script:Groups
        (Read-ReviewQueueDocument -Path $script:QueuePath).completedGroupIds | Should -Be @('group-001')
    }

    It 'rejects empty exclusion evidence and out-of-inventory paths before writing' {
        $before = [IO.File]::ReadAllText($script:QueuePath)
        { Set-ReviewFileAssessment @script:Arguments -Assessment @{
            groupId = 'group-001'; path = $script:Files[0].path; disposition = 'excluded'; scope = ''; reason = ''
        } } | Should -Throw
        { Set-ReviewFileAssessment @script:Arguments -Assessment @{
            groupId = 'group-001'; path = (Join-Path $script:Root 'outside.rs'); disposition = 'reviewed'; scope = ''; reason = ''
        } } | Should -Throw '*assigned group*'
        [IO.File]::ReadAllText($script:QueuePath) | Should -BeExactly $before
    }

    It 'publishes validated inventory drafts and assessments through their entry points' {
        $published = Join-Path $script:Root 'published.json'
        $run = Invoke-DataCommand 'Set-ReviewInput.ps1' @('-Kind', 'Inventory', '-InputPath', $script:Inventory, '-Path', $published)
        $run.exitCode | Should -Be 0 -Because $run.error
        (Read-ReviewFileInventory -Path $published).Count | Should -Be 2
        $run = Invoke-DataCommand 'Set-ReviewInput.ps1' @(
            '-Kind', 'Groups', '-InputPath', $script:Grouped, '-Path', (Join-Path $script:Root 'published-groups.json'), '-InventoryPath', $published
        )
        $run.exitCode | Should -Be 0 -Because $run.error
        $assessment = Join-Path $script:Root 'assessment.json'
        Write-PullRequestOutputFileAtomically -Path $assessment -Content (ConvertTo-JsonText -Value @{
            groupId = 'group-001'; path = $script:Files[0].path; disposition = 'reviewed'; scope = ''; reason = ''
        })
        $run = Invoke-DataCommand 'Set-ReviewFileAssessment.ps1' @(
            '-QueuePath', $script:QueuePath, '-InventoryPath', $script:Inventory, '-FileGroupsPath', $script:Grouped, '-AssessmentPath', $assessment
        )
        $run.exitCode | Should -Be 0 -Because $run.error
        (Read-ReviewQueueDocument -Path $script:QueuePath).assessments.Count | Should -Be 1
    }

    It 'detects missing evidence at the final gate and permits incremental targeted repair' {
        foreach ($file in $script:Files) {
            Set-ReviewFileAssessment @script:Arguments -Assessment @{
                groupId = 'group-001'; path = $file.path; disposition = 'reviewed'; scope = ''; reason = ''
            }
        }
        Complete-ReviewFileGroup -QueuePath $script:QueuePath -GroupId 'group-001' | Out-Null
        $journal = Read-ReviewQueueDocument -Path $script:QueuePath
        $journal.assessments = @()
        Write-PullRequestOutputFileAtomically -Path $script:QueuePath -Content (ConvertTo-JsonText -Value $journal)
        { Assert-ReviewFileGroupQueueComplete -Path $script:QueuePath -Group $script:Groups } | Should -Throw '*assessment*'
        foreach ($file in $script:Files) {
            Set-ReviewFileAssessment @script:Arguments -Assessment @{
                groupId = 'group-001'; path = $file.path; disposition = 'reviewed'; scope = ''; reason = ''
            }
        }
        Assert-ReviewFileGroupQueueComplete -Path $script:QueuePath -Group $script:Groups
    }
}

Describe 'Expansion graph and completeness' {
    BeforeEach {
        $script:Session = Join-Path $TestDrive ([guid]::NewGuid())
        foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
            [IO.Directory]::CreateDirectory((Join-Path $script:Session $root)) | Out-Null
            [IO.File]::WriteAllText((Join-Path $script:Session "$root\source.md"), 'Reference and scope.')
        }
        foreach ($name in @('parent.md', 'child.md')) {
            [IO.File]::WriteAllText((Join-Path $script:Session "instruction-set-raw\$name"), "Reference $name.")
        }
        $script:ExpansionPath = Join-Path $script:Session 'focus-area-expansion-status.json'
        $script:ExpansionArgs = @{ Path = $script:ExpansionPath; SessionStateDirectory = $script:Session }
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Initialize | Out-Null
        $script:ParentInput = @{ id = 'parent'; uri = 'https://example.invalid/parent'; referrers = @((New-ExpansionReferrer)) }
    }

    It 'rejects direct raw referrers at registration without changing the manifest' {
        $before = [IO.File]::ReadAllText($script:ExpansionPath)
        $script:ParentInput.referrers[0].source.root = 'instruction-set-raw'
        $registration = Join-Path $script:Session 'registration.json'
        [IO.File]::WriteAllText($registration, (ConvertTo-JsonText -Value $script:ParentInput))
        $run = Invoke-DataCommand 'Update-FocusAreaExpansion.ps1' @(
            '-SessionStateDirectory', $script:Session, '-Operation', 'Register', '-InputPath', $registration
        )
        $run.exitCode | Should -Not -Be 0
        $run.output | Should -BeNullOrEmpty
        $run.error | Should -Match 'not valid with the schema'
        [IO.File]::ReadAllText($script:ExpansionPath) | Should -BeExactly $before
    }

    It 'rejects stored direct raw provenance at manifest consumption' {
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput | Out-Null
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId parent -OutputPath @('parent.md') | Out-Null
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize | Out-Null
        $document = Read-WorkflowData -Kind expansion -Path $script:ExpansionPath
        $document.references[0].referrers[0].source.root = 'instruction-set-raw'
        { Assert-FocusAreaExpansion -Document $document -SessionStateDirectory $script:Session -RequireFinal } | Should -Throw
        [IO.File]::WriteAllText($script:ExpansionPath, (ConvertTo-JsonText -Value $document))
        $before = [IO.File]::ReadAllText($script:ExpansionPath)
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Read -ReferenceId parent } | Should -Throw
        [IO.File]::ReadAllText($script:ExpansionPath) | Should -BeExactly $before
    }

    It 'propagates late referrers into descendants and finalizes only completed attempts' {
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput | Out-Null
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize } | Should -Throw '*pending*'
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId parent -OutputPath @('parent.md') | Out-Null
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'child'; uri = 'https://example.invalid/child'; referrers = @((New-ExpansionReferrer -ParentId parent -Path parent.md))
        } | Out-Null
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId child -OutputPath @('child.md') | Out-Null
        $extra = New-ExpansionReferrer
        $extra.scope = 'Private APIs.'
        $script:ParentInput.referrers = @($extra)
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput | Out-Null
        $read = Update-FocusAreaExpansion @script:ExpansionArgs -Operation Read -ReferenceId child
        $read.contexts.Count | Should -Be 2
        $read.contexts[0].chain.Count | Should -Be 2
        $read.contexts[1].chain[0].scope | Should -BeExactly 'Private APIs.'
        (Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize).state | Should -Be 'Complete'
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput } | Should -Throw '*immutable*'
    }

    It 'retains typed failed references and derives Degraded rather than trusting an assigned state' {
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput | Out-Null
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Fail -ReferenceId parent -Failure 'Access denied.' | Out-Null
        $result = Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize
        $result.state | Should -Be 'Degraded'
        $result.failedReferences[0].error | Should -BeExactly 'Access denied.'
        $document = Read-WorkflowData -Kind expansion -Path $script:ExpansionPath
        $document.state = 'Complete'
        { Assert-FocusAreaExpansion -Document $document -SessionStateDirectory $script:Session -RequireFinal } |
            Should -Throw '*disagrees*'
    }

    It 'rejects duplicate target IDs, missing parents, cycles and output collisions without partial writes' {
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord $script:ParentInput | Out-Null
        $before = [IO.File]::ReadAllText($script:ExpansionPath)
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'duplicate'; uri = $script:ParentInput.uri; referrers = @((New-ExpansionReferrer))
        } } | Should -Throw '*unique*'
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'child'; uri = 'https://example.invalid/child'; referrers = @((New-ExpansionReferrer -ParentId missing -Path parent.md))
        } } | Should -Throw '*parent*'
        [IO.File]::ReadAllText($script:ExpansionPath) | Should -BeExactly $before
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId parent -OutputPath @('parent.md') | Out-Null
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'parent'; uri = $script:ParentInput.uri; referrers = @((New-ExpansionReferrer -ParentId parent -Path parent.md))
        } } | Should -Throw '*cycle*'
        Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'child'; uri = 'https://example.invalid/child'; referrers = @((New-ExpansionReferrer -ParentId parent -Path parent.md))
        } | Out-Null
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId child -OutputPath @('parent.md') } |
            Should -Throw '*Multiple references*'
    }

    It 'allows an explicit empty discovery result and validates result drafts through the CLI' {
        (Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize).state | Should -Be 'Complete'
        $run = Invoke-DataCommand 'Update-FocusAreaExpansion.ps1' @('-SessionStateDirectory', $script:Session, '-Operation', 'Read')
        $run.exitCode | Should -Be 0 -Because $run.error
        ($run.output | ConvertFrom-Json).referenceCount | Should -Be 0
        ($run.output | ConvertFrom-Json).contexts.Count | Should -Be 0
        ($run.output | ConvertFrom-Json).failedReferences.Count | Should -Be 0
        ($run.output | ConvertFrom-Json).pendingReferenceIds.Count | Should -Be 0
        ($run.output.Trim() -split "`n").Count | Should -Be 1
    }

    It 'records typed worker outcomes through the CLI without array argument binding' {
        $registration = Join-Path $script:Session 'registration.json'
        Write-PullRequestOutputFileAtomically -Path $registration -Content (ConvertTo-JsonText -Value $script:ParentInput)
        $run = Invoke-DataCommand 'Update-FocusAreaExpansion.ps1' @('-SessionStateDirectory', $script:Session, '-Operation', 'Register', '-InputPath', $registration)
        $run.exitCode | Should -Be 0 -Because $run.error
        $resultPath = Join-Path $script:Session 'result.json'
        [IO.File]::WriteAllText($resultPath, '{"referenceId":"parent","outputs":["parent.md","child.md"],"error":null}')
        $run = Invoke-DataCommand 'Update-FocusAreaExpansion.ps1' @('-SessionStateDirectory', $script:Session, '-Operation', 'Complete', '-InputPath', $resultPath)
        $run.exitCode | Should -Be 0 -Because $run.error
        (Read-WorkflowData -Kind expansion -Path $script:ExpansionPath).references[0].outputs.Count | Should -Be 2
        $run = Invoke-DataCommand 'Update-FocusAreaExpansion.ps1' @('-SessionStateDirectory', $script:Session, '-Operation', 'Finalize')
        $run.exitCode | Should -Be 0 -Because $run.error
        ($run.output | ConvertFrom-Json).state | Should -Be 'Complete'
    }

    It 'rejects cycles deterministically in bounded generated reference graphs' {
        $seed = Get-Random
        $random = [Random]::new($seed)
        # A small bounded DAG exercises parent ordering and cycle checks without timing dependence.
        $count = $random.Next(4, 9)
        foreach ($index in 0..($count - 1)) {
            $name = "generated-$index.md"
            [IO.File]::WriteAllText((Join-Path $script:Session "instruction-set-raw\$name"), "Guidance $index.")
            $referrer = if ($index -eq 0) { New-ExpansionReferrer } else {
                $parent = $random.Next($index)
                New-ExpansionReferrer -ParentId "node-$parent" -Path "generated-$parent.md"
            }
            Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
                id = "node-$index"; uri = "https://example.invalid/$index"; referrers = @($referrer)
            } | Out-Null
            Update-FocusAreaExpansion @script:ExpansionArgs -Operation Complete -ReferenceId "node-$index" -OutputPath @($name) | Out-Null
        }
        $before = [IO.File]::ReadAllText($script:ExpansionPath)
        { Update-FocusAreaExpansion @script:ExpansionArgs -Operation Register -InputRecord @{
            id = 'node-0'; uri = 'https://example.invalid/0'
            referrers = @((New-ExpansionReferrer -ParentId "node-$($count - 1)" -Path "generated-$($count - 1).md"))
        } } | Should -Throw '*cycle*' -Because "seed $seed"
        [IO.File]::ReadAllText($script:ExpansionPath) | Should -BeExactly $before
        (Update-FocusAreaExpansion @script:ExpansionArgs -Operation Finalize).state | Should -Be 'Complete'
    }
}

Describe 'Human audit records' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        [IO.Directory]::CreateDirectory($script:Root) | Out-Null
        $script:FeedbackPath = Join-Path $script:Root 'audit-feedback.json'
        $script:ApprovalPath = Join-Path $script:Root 'merge-approval.json'
        $script:FindingsPath = Join-Path $script:Root 'findings.json'
        $artifact = New-FindingArtifactDocument -Kind consolidated
        $script:FindingId = Add-FindingToArtifact -Document $artifact -Finding (New-FeedbackFinding) -Origin synthesis
        Write-FindingArtifact -Path $script:FindingsPath -Document $artifact
        $script:Context = @{
            pullRequestUrl = 'https://github.com/example/repo/pull/1'
            pullRequest = @{ reviewedSourceCommit = ('a' * 40); isAuthoredBySelf = $false }
        }
    }

    It 'preserves verbatim feedback, order and exact retries without fabricating decisions' {
        $text = 'User text with $variables, "quotes", `backticks` and 2026-10-07T00:00:00Z.'
        $feedbackArguments = @{ Path = $script:FeedbackPath; FindingsPath = $script:FindingsPath; ResponseId = 'response-1'; FindingId = $script:FindingId; Text = $text }
        Add-ReviewAuditFeedback @feedbackArguments
        Add-ReviewAuditFeedback @feedbackArguments
        Add-ReviewAuditFeedback -Path $script:FeedbackPath -FindingsPath $script:FindingsPath -ResponseId response-2 -FindingId $script:FindingId -Text 'Please clarify.'
        (Get-ReviewAuditFeedback -Path $script:FeedbackPath -FindingId $script:FindingId) | Should -BeExactly "$text`n`nPlease clarify."
        Test-Path -LiteralPath (Join-Path $script:Root 'audit-decisions.json') | Should -BeFalse
        $feedbackArguments.Text = 'Different response'
        { Add-ReviewAuditFeedback @feedbackArguments } | Should -Throw '*immutable*'
        $feedbackArguments.FindingId = 'missing'
        { Add-ReviewAuditFeedback @feedbackArguments } | Should -Throw
    }

    It 'loads accumulated feedback in the decision command rather than a shell-generated argument' {
        Add-ReviewAuditFeedback -Path $script:FeedbackPath -FindingsPath $script:FindingsPath -ResponseId response-1 -FindingId $script:FindingId -Text 'Preserve $this exactly.'
        $run = Invoke-DataCommand 'Add-FindingDecision.ps1' @(
            '-OutputDirectory', $script:Root, '-FindingId', $script:FindingId, '-Decision', 'approved', '-AuditFeedbackPath', $script:FeedbackPath
        )
        $run.exitCode | Should -Be 0 -Because $run.error
        $log = Read-FindingDecisionDocument -Path (Join-Path $script:Root 'audit-decisions.json')
        (Resolve-FindingDecision -Document $log -FindingId $script:FindingId).feedback | Should -BeExactly 'Preserve $this exactly.'
    }

    It 'binds approval to the audited PR and commit and rejects malformed booleans' {
        Set-ReviewMergeApproval -Path $script:ApprovalPath -Context $script:Context -ApproveMerge $true | Should -Be 'Recorded'
        Test-ReviewMergeApproval -Path $script:ApprovalPath -PullRequestUrl $script:Context.pullRequestUrl -SourceCommit ('a' * 40) | Should -BeTrue
        { Test-ReviewMergeApproval -Path $script:ApprovalPath -PullRequestUrl $script:Context.pullRequestUrl -SourceCommit ('b' * 40) } |
            Should -Throw '*different source commit*'
        { Test-ReviewMergeApproval -Path $script:ApprovalPath -PullRequestUrl 'https://github.com/example/repo/pull/2' -SourceCommit ('a' * 40) } |
            Should -Throw '*different pull request*'
        $document = Read-WorkflowData -Kind mergeApproval -Path $script:ApprovalPath
        $document.approveMerge = 'false'
        [IO.File]::WriteAllText($script:ApprovalPath, (ConvertTo-JsonText -Value $document))
        { Test-ReviewMergeApproval -Path $script:ApprovalPath -PullRequestUrl $script:Context.pullRequestUrl -SourceCommit ('a' * 40) } | Should -Throw
    }

    It 'carries forward only an existing true baseline-bound approval' {
        $newContext = @{ pullRequestUrl = $script:Context.pullRequestUrl; pullRequest = @{ reviewedSourceCommit = ('b' * 40); isAuthoredBySelf = $false } }
        $approvalArguments = @{ Path = $script:ApprovalPath; Context = $newContext; PreviousContext = $script:Context; CarryForward = $true }
        Set-ReviewMergeApproval @approvalArguments | Should -Be 'NoApproval'
        Set-ReviewMergeApproval -Path $script:ApprovalPath -Context $script:Context -ApproveMerge $false | Out-Null
        Set-ReviewMergeApproval @approvalArguments | Should -Be 'NoApproval'
        Test-ReviewMergeApproval -Path $script:ApprovalPath -PullRequestUrl $newContext.pullRequestUrl -SourceCommit ('b' * 40) | Should -BeFalse
        Set-ReviewMergeApproval -Path $script:ApprovalPath -Context $script:Context -ApproveMerge $true | Out-Null
        Set-ReviewMergeApproval @approvalArguments | Should -Be 'Recorded'
        (Read-WorkflowData -Kind mergeApproval -Path $script:ApprovalPath).approvedSourceCommit | Should -BeExactly ('b' * 40)
        Set-ReviewMergeApproval @approvalArguments | Should -Be 'Recorded'
        $newContext.pullRequest.reviewedSourceCommit = 'c' * 40
        { Set-ReviewMergeApproval @approvalArguments } | Should -Throw '*different source commit*'
    }
}
