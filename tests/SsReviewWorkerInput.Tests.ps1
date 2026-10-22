BeforeAll {
    $script:SkillRoot = Join-Path $PSScriptRoot '..\skills\ss-review'
    $script:Scripts = Join-Path $script:SkillRoot 'scripts'
    . (Join-Path $script:Scripts 'lib\JsonValue.ps1')
    . (Join-Path $script:Scripts 'lib\PullRequestModel.ps1')
    . (Join-Path $script:Scripts 'lib\WorkflowData.ps1')
    . (Join-Path $script:Scripts 'lib\FindingModel.ps1')
    . (Join-Path $script:Scripts 'lib\ReviewWork.ps1')
    . (Join-Path $script:Scripts 'lib\ReviewInput.ps1')
    . (Join-Path $script:Scripts 'lib\OverviewPublication.ps1')
    . "$PSScriptRoot\lib\ConcurrentReviewCommand.ps1"

    # Exhausts a fixed fixture with an operation bound, checking exact reconstruction and the byte budget.
    function Read-AllSharedPages {
        param($Assignment, [string] $Id)
        $text = [Text.StringBuilder]::new()
        $offset = 0
        for ($attempt = 0; $attempt -lt 1000; $attempt++) {
            $page = Read-ReviewSharedDocument -Assignment $Assignment -DocumentId $Id -Offset $offset
            $json = ConvertTo-Json -InputObject $page -Compress
            [Text.Encoding]::UTF8.GetByteCount($json + [Environment]::NewLine) | Should -BeLessOrEqual 16384
            $page.contentOffset | Should -Be $offset
            $null = $text.Append($page.content)
            if ($page.complete) { return $text.ToString() }
            $page.nextOffset | Should -BeGreaterThan $offset
            $offset = $page.nextOffset
        }
        throw 'Fixture did not finish within its operation bound.'
    }
}

Describe 'Prepared bounded worker inputs' {
    BeforeEach {
        $script:State = Join-Path $TestDrive "session space O'Brien $([char]0xe9) $([guid]::NewGuid().ToString('N'))"
        $null = New-Item -ItemType Directory -Path $script:State -Force
        $script:Scope = Join-Path $script:State 'scope.md'
        [IO.File]::WriteAllText($script:Scope, 'Review the current diff in the fixture repository.')
        [IO.File]::WriteAllText((Join-Path $script:State 'references.md'), 'Inspect the fixture design.')
        # Both multiline guidance and a single long line exceed the viewer's default limit.
        $script:LongText = ("quoted `"text`" \ $([char]::ConvertFromUtf32(0x1f642))`n" * 2500) + ('x' * 25000)
        [IO.File]::WriteAllText((Join-Path $script:State 'content-overview.md'), $script:LongText)
        $areas = Join-Path $script:State 'focus-areas'
        $null = New-Item -ItemType Directory -Path $areas -Force
        [IO.File]::WriteAllText((Join-Path $areas 'guidance.md'), $script:LongText)
        $file = Join-Path $script:State 'source.rs'
        $inventory = @(@{ path = $file; sizeBytes = 10; change = 'modified' })
        $groups = @(@{ groupId = 'group-001'; files = $inventory })
        $inventoryPath = Join-Path $script:State 'file-inventory.json'
        $groupsPath = Join-Path $script:State 'file-inventory-grouped.json'
        Write-PullRequestOutputFileAtomically -Path $inventoryPath -Content (ConvertTo-JsonText -Value $inventory)
        Write-PullRequestOutputFileAtomically -Path $groupsPath -Content (ConvertTo-JsonText -Value $groups)
        $script:Work = New-ReviewWork -InventoryPath $inventoryPath -FileGroupsPath $groupsPath `
            -FocusAreaDirectory $areas -OutputDirectory (Join-Path $script:State 'review-work') `
            -RepositoryRoot $script:State -ArtifactDirectory (Join-Path $script:State 'findings')
    }

    It 'provides bounded initial reads and exact complete guidance for original and resumed reviewers' {
        foreach ($resume in @($false, $true)) {
            $result = & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') -Phase review `
                -SessionState $script:State -ScopePath $script:Scope -ManifestPath $script:Work.manifestPath `
                -ReviewerId focus-001 -Resume:$resume -OutputDirectory (Join-Path $script:State "attempt-$resume") |
                ConvertFrom-Json -AsHashtable
            $assignment = Read-WorkflowData -Kind workerAssignment -Path $result.assignmentPath
            $assignment.resume | Should -BeTrue -Because 'first dispatch retains the prepared artifact and journal'
            ($assignment.documents | Where-Object { $_.id -eq 'prompt' }).sizeBytes | Should -BeGreaterThan 20KB
            $result.dispatchPrompt | Should -Match 'Do not use the default file viewer'
            $result.dispatchPrompt | Should -Match "O''Brien"
            $assignment.documents.id | Should -Not -Contain 'pr-context'
            Read-AllSharedPages -Assignment $assignment -Id overview | Should -BeExactly $script:LongText
            Read-AllSharedPages -Assignment $assignment -Id focusArea | Should -BeExactly $script:LongText
            $prompt = Read-AllSharedPages -Assignment $assignment -Id prompt
            $prompt | Should -Not -Match '\{\{[^}]+\}\}'
            $prompt | Should -Match ([regex]::Escape($script:Work.inventoryPath))
            $prompt | Should -Match 'Primary orchestration, publication, and every human decision gate'
            $prompt | Should -Match 'Do not invoke the skill'
            $prompt | Should -Match 'Use `-Resume` exactly'
            $prompt | Should -Match 'Use the default context tier supplied by the orchestrator'
            $prompt | Should -Not -Match 'long-context configuration'
            $data = Read-AllSharedPages -Assignment $assignment -Id workflowData
            $expectedData = @(
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'workflow-data.md'))
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'data-guides\review-coverage.md'))
            ) -join "`n`n"
            # The shared atomic writer uses LF on every host; no other source text may change.
            $data | Should -BeExactly $expectedData.Replace("`r`n", "`n")
            $data | Should -Not -Match 'Comment authorship|Reference expansion|Finding plans|Audit records|Focus-area maps'
            $manifest = Read-ReviewWorkManifest -Path $script:Work.manifestPath
            $reviewer = $manifest.reviewers[0]
            $artifact = & (Join-Path $script:Scripts 'New-FindingArtifact.ps1') `
                -OutputDirectory (Join-Path $script:State 'findings') -ReviewerId focus-001 `
                -FocusArea $reviewer.focusArea -FileGroupQueuePath $reviewer.fileGroupQueuePath `
                -Resume:$assignment.resume | ConvertFrom-Json -AsHashtable
            $artifact.resumedExisting | Should -BeTrue
            $journalBefore = [IO.File]::ReadAllText($reviewer.fileGroupQueuePath)
            $queue = & (Join-Path $script:Scripts 'New-ReviewFileGroupQueue.ps1') `
                -InventoryPath $script:Work.inventoryPath -FileGroupsPath $script:Work.fileGroupsPath `
                -QueuePath $reviewer.fileGroupQueuePath -Resume:$assignment.resume | ConvertFrom-Json -AsHashtable
            $queue.queuePath | Should -BeExactly $reviewer.fileGroupQueuePath
            [IO.File]::ReadAllText($reviewer.fileGroupQueuePath) | Should -BeExactly $journalBefore
            $readCommand = ($result.dispatchPrompt -split "`n" | Where-Object { $_.StartsWith('pwsh ') } | Select-Object -First 1)
            $first = & ([scriptblock]::Create($readCommand.Replace('{{DOCUMENT_ID}}', 'overview'))) | ConvertFrom-Json -AsHashtable
            $first.contentOffset | Should -Be 0
            $first.complete | Should -BeFalse
        }
        $counts = (& (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $script:State | ConvertFrom-Json -AsHashtable).counts
        $counts.firstReadFailures | Should -Be 0
        $counts.setupCalls | Should -Be 2
        $counts.documentReadCalls | Should -Be 2
    }

    It 'loads only <Phase> preparation contracts without granting orchestration or human authority' -ForEach @(
        @{ Phase = 'fileGroups'; Guide = 'data-guides\file-groups.md' }
        @{ Phase = 'focusAreas'; Guide = 'data-guides\focus-areas.md' }
        @{ Phase = 'references'; Guide = $null }
        @{ Phase = 'overview'; Guide = $null }
    ) {
        $result = & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') -Phase $Phase `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State $Phase) |
            ConvertFrom-Json -AsHashtable
        $assignment = Read-WorkflowData -Kind workerAssignment -Path $result.assignmentPath
        $result.dispatchPrompt | Should -Not -Match 'assigned queryRecipe'
        $assignment.documents.id | Should -Not -Contain queryRecipe
        $prompt = Read-AllSharedPages -Assignment $assignment -Id prompt
        $prompt | Should -Match 'authorized bounded phase work'
        $prompt | Should -Match 'not invocation or delegation of ss-review itself'
        $prompt | Should -Match 'do not ask or answer it yourself'
        $prompt | Should -Match 'Repository instruction authorization: not authorized'
        $prompt | Should -Match 'Pre-existing findings authorized: False'
        if ($null -eq $Guide) {
            $assignment.documents.id | Should -Not -Contain 'workflowData'
            [IO.File]::Exists((Join-Path $script:State "$Phase\workflow-data.md")) | Should -BeFalse
            $assignment.documents += New-ReviewSharedDocument -Id workflowData -Path (Join-Path $script:SkillRoot 'workflow-data.md')
        } else {
            $data = Read-AllSharedPages -Assignment $assignment -Id workflowData
            $expectedData = @(
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'workflow-data.md'))
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot $Guide))
            ) -join "`n`n"
            $data | Should -BeExactly $expectedData.Replace("`r`n", "`n")
            $assignment.documents = @($assignment.documents | Where-Object { $_.id -ne 'workflowData' })
        }
        { Assert-ReviewWorkerAssignment -Assignment $assignment } | Should -Throw '*required phase inputs*'
    }

    It 'rejects changed <_> bytes and measures the first-read failure' -ForEach @('scope', 'workflowData') {
        $documentId = $_
        $result = & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') -Phase fileGroups `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'attempt') |
            ConvertFrom-Json -AsHashtable
        $assignment = Read-WorkflowData -Kind workerAssignment -Path $result.assignmentPath
        $document = $assignment.documents | Where-Object { $_.id -eq $documentId }
        [IO.File]::AppendAllText($document.path, ' changed guidance')
        { & (Join-Path $script:Scripts 'Read-ReviewWorkerDocument.ps1') -AssignmentPath $result.assignmentPath -DocumentId $documentId } |
            Should -Throw '*changed*'
        $counts = (& (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $script:State | ConvertFrom-Json -AsHashtable).counts
        $counts.firstReadFailures | Should -Be 1
    }

    It 'rejects an empty <_> instead of publishing an incomplete data guide' -ForEach @(
        'workflow-data.md', 'data-guides\file-groups.md'
    ) {
        $emptyGuide = $_
        # Exercise the executable's real reads without modifying installed or shared source files.
        $isolatedSkill = Join-Path $script:State 'skill'
        $null = New-Item -ItemType Directory -Path $isolatedSkill
        foreach ($relativePath in @('scripts', 'schemas', 'data-guides', 'workflow-data.md', 'prepare-file-groups.md')) {
            Copy-Item -LiteralPath (Join-Path $script:SkillRoot $relativePath) -Destination $isolatedSkill -Recurse
        }
        [IO.File]::WriteAllText((Join-Path $isolatedSkill $emptyGuide), '')
        $output = Join-Path $script:State 'empty-guide'
        {
            & (Join-Path $isolatedSkill 'scripts\New-ReviewWorkerAssignment.ps1') -Phase fileGroups `
                -SessionState $script:State -ScopePath $script:Scope -OutputDirectory $output
        } | Should -Throw '*data guide*empty*'
        [IO.File]::Exists((Join-Path $output 'assignment.json')) | Should -BeFalse
    }

    It 'preserves pages and exact counters for overlapping <_> document reads' -ForEach @('fileGroups', 'focusAreas') {
        $result = & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') -Phase $_ `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'concurrent') |
            ConvertFrom-Json -AsHashtable
        $assignment = Read-WorkflowData -Kind workerAssignment -Path $result.assignmentPath
        # Every real document and one failed read must contend before any can proceed.
        $ids = @($assignment.documents.id) + @('missing')
        $commands = @(foreach ($id in $ids) {
            @{
                name = 'Read-ReviewWorkerDocument.ps1'
                parameters = @{ AssignmentPath = $result.assignmentPath; DocumentId = $id }
            }
        })
        $reads = @(Invoke-ContendedReviewCommand -Path $assignment.diagnosticsPath -Command $commands)
        for ($index = 0; $index -lt $ids.Count; $index++) {
            $read = $reads[$index]
            if ($ids[$index] -eq 'missing') {
                $read.exitCode | Should -Not -Be 0
                $read.errors | Should -Match "Expected one shared document 'missing'"
                $read.output | Should -BeNullOrEmpty
            } else {
                $read.exitCode | Should -Be 0 -Because $read.errors
                $read.errors | Should -BeNullOrEmpty
                $page = ConvertFrom-JsonVerbatim -Text $read.output
                Assert-WorkflowData -Kind sharedReadPage -Value $page
                $expected = Read-ReviewSharedDocument -Assignment $assignment -DocumentId $ids[$index]
                (ConvertTo-JsonText -Value $page) | Should -BeExactly (ConvertTo-JsonText -Value $expected)
            }
        }
        $counts = (Read-WorkflowData -Kind inputDiagnostics -Path $assignment.diagnosticsPath).counts
        $counts.setupCalls | Should -Be 1
        $counts.documentReadCalls | Should -Be $reads.Count
        $counts.firstReadFailures | Should -Be 1
        $counts.repeatedSetupCalls | Should -Be 0
        $counts.malformedQueryFailures | Should -Be 0
        $counts.cursorRestarts | Should -Be 0
        [IO.File]::Exists("$($assignment.diagnosticsPath).lock") | Should -BeTrue
    }

    It 'rejects repeated setup without overwriting the assignment or resetting work' {
        $manifest = Read-ReviewWorkManifest -Path $script:Work.manifestPath
        $queueBefore = [IO.File]::ReadAllText($manifest.reviewers[0].fileGroupQueuePath)
        $artifactBefore = [IO.File]::ReadAllText($manifest.reviewers[0].artifactPath)
        $parameters = @{
            Phase = 'review'; SessionState = $script:State; ScopePath = $script:Scope
            ManifestPath = $script:Work.manifestPath; ReviewerId = 'focus-001'
            OutputDirectory = Join-Path $script:State 'attempt'
        }
        $result = & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') @parameters | ConvertFrom-Json -AsHashtable
        $before = [IO.File]::ReadAllText($result.assignmentPath)
        { & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') @parameters } | Should -Throw '*already exists*'
        [IO.File]::ReadAllText($result.assignmentPath) | Should -BeExactly $before
        $counts = (& (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $script:State | ConvertFrom-Json -AsHashtable).counts
        $counts.repeatedSetupCalls | Should -Be 1
        [IO.File]::ReadAllText($manifest.reviewers[0].fileGroupQueuePath) | Should -BeExactly $queueBefore
        [IO.File]::ReadAllText($manifest.reviewers[0].artifactPath) | Should -BeExactly $artifactBefore
    }

    It 'does not dispatch assignments the manifest marks entirely excluded' {
        $manifest = Read-ReviewWorkManifest -Path $script:Work.manifestPath
        $manifest.reviewers[0].dispatch = 'excluded'
        Write-PullRequestOutputFileAtomically -Path $script:Work.manifestPath -Content (ConvertTo-JsonText -Value $manifest)
        {
            & (Join-Path $script:Scripts 'New-ReviewWorkerAssignment.ps1') -Phase review `
                -SessionState $script:State -ScopePath $script:Scope -ManifestPath $script:Work.manifestPath `
                -ReviewerId focus-001 -OutputDirectory (Join-Path $script:State 'excluded-attempt')
        } | Should -Throw '*entirely excluded*'
    }

    It 'does not permit canonical JSON as a shared read even through a modified manifest' {
        { New-ReviewSharedDocument -Id scope -Path $script:Work.inventoryPath } | Should -Throw '*never canonical captures*'
        $document = New-ReviewSharedDocument -Id scope -Path $script:Scope
        $document.path = $script:Work.inventoryPath
        $assignment = @{
            schemaVersion = 1; phase = 'review'; resume = $false; contextPath = $null
            diagnosticsPath = Join-Path $script:State 'input-diagnostics.json'; documents = @($document)
        }
        { Read-ReviewSharedDocument -Assignment $assignment -DocumentId scope } | Should -Throw '*never a capture*'
    }

    It 'records measured baseline totals separately without coercing absent data to zero' {
        $baseline = Join-Path $script:State 'baseline-diagnostics.json'
        { & (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $baseline } | Should -Throw
        # Ref: issue #127's combined shared-document first-read failures.
        $data = & (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') `
            -Path $baseline -Record firstReadFailures -Count 69 | ConvertFrom-Json -AsHashtable
        $data.counts.firstReadFailures | Should -Be 69
        $data.counts.cursorRestarts | Should -Be 0
    }
}

Describe 'Diagnostic lock failure boundaries' {
    BeforeEach {
        $script:DiagnosticsPath = Join-Path $TestDrive "$([guid]::NewGuid()).json"
        Mock Start-Sleep { throw 'Unexpected wait for a non-contention failure.' }
    }

    It 'bounds persistent contention without modifying diagnostics or deleting the lock' {
        Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name setupCalls
        $before = [IO.File]::ReadAllText($script:DiagnosticsPath)
        $lock = Open-WorkflowDataLock -Path $script:DiagnosticsPath
        $script:Waits = 0
        Mock Start-Sleep {
            $Milliseconds | Should -Be 50
            $script:Waits++
            # A deterministic guard catches an unbounded retry without waiting for real time.
            if ($script:Waits -gt 600) { throw 'Retry budget exceeded.' }
        }
        try {
            { Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls } |
                Should -Throw '*remains busy after bounded contention retries*'
            $script:Waits | Should -Be 600
            [IO.File]::ReadAllText($script:DiagnosticsPath) | Should -BeExactly $before
            { [IO.File]::Open("$script:DiagnosticsPath.lock", 'Open', 'ReadWrite', 'None') } | Should -Throw
        } finally { $lock.Dispose() }
        Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls
        (Read-WorkflowData -Kind inputDiagnostics -Path $script:DiagnosticsPath).counts.documentReadCalls | Should -Be 1
        [IO.File]::Exists("$script:DiagnosticsPath.lock") | Should -BeTrue
    }

    It 'propagates non-contention I/O errors without retries' {
        Mock Open-WorkflowDataLock { throw [IO.IOException]::new('Device failure.') }
        { Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls } |
            Should -Throw '*Device failure*'
        Should -Invoke Open-WorkflowDataLock -Times 1 -Exactly
        Should -Invoke Start-Sleep -Times 0 -Exactly
        [IO.File]::Exists($script:DiagnosticsPath) | Should -BeFalse
    }

    It 'propagates unavailable lock paths without retries' {
        $null = [IO.Directory]::CreateDirectory("$script:DiagnosticsPath.lock")
        { Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls } | Should -Throw
        Should -Invoke Start-Sleep -Times 0 -Exactly
        [IO.File]::Exists($script:DiagnosticsPath) | Should -BeFalse
    }

    It 'rejects malformed diagnostics and releases ownership without retrying' {
        [IO.File]::WriteAllText($script:DiagnosticsPath, '{}')
        { Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls } | Should -Throw
        Should -Invoke Start-Sleep -Times 0 -Exactly
        [IO.File]::ReadAllText($script:DiagnosticsPath) | Should -BeExactly '{}'
        $lock = Open-WorkflowDataLock -Path $script:DiagnosticsPath
        $lock.Dispose()
    }

    It 'propagates failed counter writes without retrying or losing prior counts' {
        Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name setupCalls
        $before = [IO.File]::ReadAllText($script:DiagnosticsPath)
        Mock Write-PullRequestOutputFileAtomically { throw [IO.IOException]::new('Counter write failed.') }
        { Add-ReviewInputDiagnostic -Path $script:DiagnosticsPath -Name documentReadCalls } |
            Should -Throw '*Counter write failed*'
        Should -Invoke Write-PullRequestOutputFileAtomically -Times 1 -Exactly
        Should -Invoke Start-Sleep -Times 0 -Exactly
        [IO.File]::ReadAllText($script:DiagnosticsPath) | Should -BeExactly $before
        $lock = Open-WorkflowDataLock -Path $script:DiagnosticsPath
        $lock.Dispose()
    }
}

Describe 'Prepared overview structure validation' {
    BeforeEach {
        $script:Overview = @(
            '# Content overview', ''
            @('Mission', 'Substance', 'Design changes', 'Significant changes', 'Structure', 'Mechanism', 'Completeness assessment', 'Related history', 'Related work') |
                ForEach-Object { "## $_`n`nNot applicable for this fixture.`n" }
        ) -join "`n"
        $script:OverviewPath = Join-Path $TestDrive 'overview.md'
    }

    It 'accepts complete non-PR overviews through the executable' {
        [IO.File]::WriteAllText($script:OverviewPath, $script:Overview)
        $result = & (Join-Path $script:Scripts 'Test-ReviewOverview.ps1') -Path $script:OverviewPath | ConvertFrom-Json
        $result.isValid | Should -BeTrue
    }

    It 'rejects duplicate headings, fenced substitutes, and empty sections' {
        { Assert-ReviewOverviewStructure -Text ($script:Overview + "`n## Mission`nextra") } | Should -Throw
        { Assert-ReviewOverviewStructure -Text ($script:Overview.Replace('## Mission', "``````text`n## Mission`n``````")) } | Should -Throw
        { Assert-ReviewOverviewStructure -Text ($script:Overview.Replace("## Mission`n`nNot applicable for this fixture.", '## Mission')) } |
            Should -Throw '*empty*'
    }
}
