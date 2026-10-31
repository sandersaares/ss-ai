BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    $script:Scripts = Join-Path $script:SkillRoot 'scripts'
    . (Join-Path $script:Scripts 'lib\JsonValue.ps1')
    . (Join-Path $script:Scripts 'lib\PullRequestModel.ps1')
    . (Join-Path $script:Scripts 'lib\ReviewWork.ps1')
    . (Join-Path $script:Scripts 'lib\WorkflowData.ps1')
    . (Join-Path $script:Scripts 'lib\FindingModel.ps1')

    function Write-WorkJson {
        param([string] $Path, $Value)
        Write-PullRequestOutputFile -Path $Path -Content (ConvertTo-JsonText -Value $Value)
    }

    # Existing queue tests model fully assessed groups; keep that prerequisite explicit while
    # separate workflow-data tests exercise partial assessments and rejected completion.
    function Set-TestGroupAssessments {
        param([string] $QueuePath)
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($null -eq $queue.inProgress) { return }
        foreach ($file in $queue.inProgress.files) {
            if (@($queue.assessments | Where-Object { $_.path -ceq $file.path }).Count) { continue }
            $queue.assessments += [ordered]@{
                groupId = $queue.inProgress.groupId; path = $file.path
                disposition = 'reviewed'; scope = ''; reason = ''
            }
        }
        Write-WorkJson -Path $QueuePath -Value $queue
    }

    # Completes a group after the fixture supplies its requested-scope analysis declarations.
    function Complete-TestReviewFileGroup {
        param([string] $QueuePath, [string] $GroupId)
        Set-TestGroupAssessments -QueuePath $QueuePath
        Complete-ReviewFileGroup -QueuePath $QueuePath -GroupId $GroupId
    }

    function Invoke-WorkScript {
        param([string] $Name, [string[]] $ArgumentList)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        foreach ($argument in (@('-NoProfile', '-NonInteractive', '-File', (Join-Path $script:Scripts $Name)) + $ArgumentList)) {
            $start.ArgumentList.Add($argument)
        }
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.RedirectStandardInput = $true
        $process = [Diagnostics.Process]::Start($start)
        try {
            $process.StandardInput.Close()
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
}

Describe 'Review work initialization' {
    BeforeAll {
        function New-CompletedReviewWork {
            $work = New-ReviewWork @script:WorkArguments
            $manifest = Read-ReviewWorkManifest -Path $work.manifestPath
            foreach ($reviewer in $manifest.reviewers) {
                Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath | Out-Null
                New-ReviewFileGroupQueue -InventoryPath $work.inventoryPath -FileGroupsPath $work.fileGroupsPath -QueuePath $reviewer.fileGroupQueuePath | Out-Null
                foreach ($group in $script:Groups) {
                    Get-NextReviewWorkItem -QueuePath $reviewer.fileGroupQueuePath | Out-Null
                    Complete-TestReviewFileGroup -QueuePath $reviewer.fileGroupQueuePath -GroupId $group.groupId | Out-Null
                }
                $document = New-FindingArtifactDocument -Kind reviewer -ReviewerId $reviewer.reviewerId -FocusArea $reviewer.focusArea
                Complete-FindingArtifactDocument -Document $document
                Write-FindingArtifact -Path (Join-Path $script:Findings "$($reviewer.reviewerId).json") -Document $document
            }
            $work
        }
    }

    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid())
        $script:Areas = Join-Path $script:Root 'focus-areas'
        New-Item -ItemType Directory -Path $script:Areas -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $script:Areas 'z.md') -Value '# Design'
        Set-Content -LiteralPath (Join-Path $script:Areas 'a.md') -Value '# Correctness'
        $script:Inventory = Join-Path $script:Root 'inventory.json'
        $script:Files = @(
            @{ path = Join-Path $script:Root 'new [file].rs'; sizeBytes = 1; change = 'new' }
            @{ path = Join-Path $script:Root 'modified.rs'; sizeBytes = 2; change = 'modified' }
            @{ path = Join-Path $script:Root 'deleted.rs'; sizeBytes = 3; change = 'deleted' }
        )
        Write-WorkJson -Path $script:Inventory -Value $script:Files
        $script:GroupedInventory = Join-Path $script:Root 'grouped-inventory.json'
        $script:Groups = @(
            @{ groupId = 'group-001'; files = @($script:Files[0], $script:Files[1]) }
            @{ groupId = 'group-002'; files = @($script:Files[2]) }
        )
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        $script:Work = Join-Path $script:Root 'work'
        $script:Findings = Join-Path $script:Root 'findings'
        $script:WorkArguments = @{
            InventoryPath = $script:Inventory
            FileGroupsPath = $script:GroupedInventory
            FocusAreaDirectory = $script:Areas
            OutputDirectory = $script:Work
        }
    }

    It 'snapshots every focus area and file without requiring deleted paths to exist' {
        $result = New-ReviewWork @script:WorkArguments
        $result.fileCount | Should -Be $script:Files.Count
        $result.groupCount | Should -Be $script:Groups.Count
        $result.reviewerCount | Should -Be 2
        $manifest = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($result.manifestPath))
        @($manifest.reviewers.reviewerId) | Should -Be @('focus-001', 'focus-002')
        @($manifest.reviewers.focusArea) | Should -Be @((Join-Path $script:Areas 'a.md'), (Join-Path $script:Areas 'z.md'))
        @($manifest.reviewers.fileGroupQueuePath | Select-Object -Unique).Count | Should -Be 2
        (Read-ReviewFileInventory -Path $result.inventoryPath).path | Should -Be $script:Files.path
        (Read-ReviewFileInventory -Path $result.inventoryPath).change | Should -Be $script:Files.change
        $groups = Read-ReviewFileGroups -Path $result.fileGroupsPath -FileInventory (Read-ReviewFileInventory -Path $result.inventoryPath)
        $groups.groupId | Should -Be $script:Groups.groupId
        @($groups | ForEach-Object { $_.files.path }) | Should -Be $script:Files.path

        Write-WorkJson -Path $script:Inventory -Value @()
        Write-WorkJson -Path $script:GroupedInventory -Value @()
        (Read-ReviewFileInventory -Path $result.inventoryPath).Count | Should -Be $script:Files.Count
        (Read-ReviewFileGroups -Path $result.fileGroupsPath -FileInventory (Read-ReviewFileInventory -Path $result.inventoryPath)).Count |
            Should -Be $script:Groups.Count
        Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath | Out-Null
        (ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($result.manifestPath))).reviewers.Count | Should -Be 2
    }

    It 'refuses reinitialization without resetting the consumed focus-area queue' {
        $result = New-ReviewWork @script:WorkArguments
        Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath | Out-Null
        { New-ReviewWork @script:WorkArguments } | Should -Throw
        (Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath).item.reviewerId | Should -Be 'focus-002'
        (Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath).exhausted | Should -BeTrue
    }

    It 'rejects an inventory that is not a nonempty array: <Json>' -ForEach @(
        @{ Json = '[]' }, @{ Json = '{}' }, @{ Json = 'null' }, @{ Json = '{broken' }
    ) {
        Set-Content -LiteralPath $script:Inventory -Value $Json
        { New-ReviewWork @script:WorkArguments } | Should -Throw
        Test-Path -LiteralPath $script:Work | Should -BeFalse
    }

    It 'rejects invalid file metadata: <Field> = <Value>' -ForEach @(
        @{ Field = 'path'; Value = 'relative.rs' }
        @{ Field = 'sizeBytes'; Value = -1 }
        @{ Field = 'sizeBytes'; Value = '1' }
        @{ Field = 'change'; Value = 'ignored' }
    ) {
        $script:Files[0][$Field] = $Value
        Write-WorkJson -Path $script:Inventory -Value $script:Files
        { Read-ReviewFileInventory -Path $script:Inventory } | Should -Throw
    }

    It 'rejects repeated file paths' {
        Write-WorkJson -Path $script:Inventory -Value @($script:Files[0], $script:Files[0])
        { Read-ReviewFileInventory -Path $script:Inventory } | Should -Throw '*repeats*'
    }

    It 'rejects malformed grouped inventories before creating work: <Json>' -ForEach @(
        @{ Json = '[]' }, @{ Json = '{}' }, @{ Json = 'null' }, @{ Json = '{broken' }
        @{ Json = '[{"groupId":"group-001","files":[]}]' }
        @{ Json = '[{"groupId":"group-001","files":{}}]' }
        @{ Json = '[{"files":[]}]' }
    ) {
        Set-Content -LiteralPath $script:GroupedInventory -Value $Json
        { New-ReviewWork @script:WorkArguments } | Should -Throw
        Test-Path -LiteralPath $script:Work | Should -BeFalse
    }

    It 'rejects groups omitting any inventory file, including deleted files' {
        Write-WorkJson -Path $script:GroupedInventory -Value @($script:Groups[0])
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*omits files*deleted.rs*'
        Test-Path -LiteralPath $script:Work | Should -BeFalse
    }

    It 'rejects repeated files within or across groups: <SameGroup>' -ForEach @(
        @{ SameGroup = $true }, @{ SameGroup = $false }
    ) {
        $index = if ($SameGroup) { 0 } else { 1 }
        $script:Groups[$index].files += $script:Files[0]
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*repeats*'
    }

    It 'rejects group identifiers that are invalid or repeated: <Id>' -ForEach @(
        @{ Id = 'group-001' }, @{ Id = '../group' }, @{ Id = '' }
    ) {
        $script:Groups[1].groupId = $Id
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        { New-ReviewWork @script:WorkArguments } | Should -Throw "*unique 'groupId'*"
    }

    It 'rejects out-of-scope paths instead of treating them as reviewed files' {
        $script:Groups[1].files = @(@{ path = Join-Path $script:Root 'unlisted.rs'; sizeBytes = 3; change = 'deleted' })
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*outside the inventory*'
    }

    It 'rejects altered metadata: <Field> = <Value>' -ForEach @(
        @{ Field = 'sizeBytes'; Value = 4 }
        @{ Field = 'sizeBytes'; Value = '3' }
        @{ Field = 'change'; Value = 'modified' }
    ) {
        $script:Groups[1].files[0][$Field] = $Value
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*changes inventory metadata*'
    }

    It 'enforces the group file-count boundary: <Count>' -ForEach @(
        @{ Count = 20; Accepted = $true }
        @{ Count = 21; Accepted = $false }
    ) {
        $files = @(foreach ($index in 1..$Count) {
            @{ path = Join-Path $script:Root "$index.rs"; sizeBytes = 1; change = 'modified' }
        })
        Write-WorkJson -Path $script:Inventory -Value $files
        Write-WorkJson -Path $script:GroupedInventory -Value @(@{ groupId = 'group-001'; files = $files })
        if ($Accepted) {
            $result = New-ReviewWork @script:WorkArguments
            $result.groupCount | Should -Be 1
            $result.fileCount | Should -Be $Count
        } else {
            { New-ReviewWork @script:WorkArguments } | Should -Throw '*file-count limit*'
        }
    }

    It 'enforces the group byte boundary: <Bytes>' -ForEach @(
        @{ Bytes = 100000; Accepted = $true }
        @{ Bytes = 100001; Accepted = $false }
    ) {
        # Place the combined size exactly at or just beyond the documented boundary.
        $script:Files[0].sizeBytes = $Bytes - $script:Files[1].sizeBytes
        Write-WorkJson -Path $script:Inventory -Value $script:Files
        Write-WorkJson -Path $script:GroupedInventory -Value $script:Groups
        if ($Accepted) {
            { New-ReviewWork @script:WorkArguments } | Should -Not -Throw
        } else {
            { New-ReviewWork @script:WorkArguments } | Should -Throw '*byte limit*'
        }
    }

    It 'preserves an oversized singleton through snapshot and queue round trips' {
        # Beyond the group byte budget, but valid because no other file shares the group.
        $script:Files[0].sizeBytes = 100001
        Write-WorkJson -Path $script:Inventory -Value @($script:Files[0])
        Write-WorkJson -Path $script:GroupedInventory -Value @(@{ groupId = 'group-001'; files = @($script:Files[0]) })
        $work = New-ReviewWork @script:WorkArguments
        $work.groupCount | Should -Be 1
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        New-ReviewFileGroupQueue -InventoryPath $work.inventoryPath -FileGroupsPath $work.fileGroupsPath -QueuePath $worker.fileGroupQueuePath | Out-Null
        $next = Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath
        $next.item.files | Should -HaveCount 1
        $next.item.files[0].sizeBytes | Should -Be $script:Files[0].sizeBytes
        $next.exhausted | Should -BeFalse
        Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $next.item.groupId | Out-Null
        (Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath).exhausted | Should -BeTrue
    }

    It 'validates group coverage again when a worker initializes its queue' {
        $work = New-ReviewWork @script:WorkArguments
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        Write-WorkJson -Path $work.fileGroupsPath -Value @($script:Groups[0])
        { New-ReviewFileGroupQueue -InventoryPath $work.inventoryPath -FileGroupsPath $work.fileGroupsPath -QueuePath $worker.fileGroupQueuePath } |
            Should -Throw '*omits files*'
        Test-Path -LiteralPath $worker.fileGroupQueuePath | Should -BeFalse
    }

    It 'rejects empty focus-area inputs rather than reporting a completed review' {
        Set-Content -LiteralPath (Join-Path $script:Areas 'a.md') -Value ' '
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*empty*'
        Remove-Item -LiteralPath (Join-Path $script:Areas 'a.md')
        Remove-Item -LiteralPath (Join-Path $script:Areas 'z.md')
        { New-ReviewWork @script:WorkArguments } | Should -Throw '*No focus areas*'
    }

    It 'keeps worker queues independent and resumes only uncompleted groups' {
        $result = New-ReviewWork @script:WorkArguments
        $first = (Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath).item
        $second = (Get-NextReviewWorkItem -QueuePath $result.focusAreaQueuePath).item
        foreach ($worker in @($first, $second)) {
            New-ReviewFileGroupQueue -InventoryPath $result.inventoryPath -FileGroupsPath $result.fileGroupsPath -QueuePath $worker.fileGroupQueuePath | Out-Null
        }
        $firstGroup = (Get-NextReviewWorkItem -QueuePath $first.fileGroupQueuePath).item
        $firstGroup.groupId | Should -Be $script:Groups[0].groupId
        $firstGroup.files.path | Should -Be $script:Groups[0].files.path
        (Read-ReviewQueueDocument -Path $second.fileGroupQueuePath).items.Count | Should -Be $script:Groups.Count
        { New-ReviewFileGroupQueue -InventoryPath $result.inventoryPath -FileGroupsPath $result.fileGroupsPath -QueuePath $first.fileGroupQueuePath } | Should -Throw '*already exists*'
        New-ReviewFileGroupQueue -InventoryPath $result.inventoryPath -FileGroupsPath $result.fileGroupsPath -QueuePath $first.fileGroupQueuePath -Resume | Out-Null
        (Get-NextReviewWorkItem -QueuePath $first.fileGroupQueuePath).item.groupId | Should -Be $script:Groups[0].groupId
        Complete-TestReviewFileGroup -QueuePath $first.fileGroupQueuePath -GroupId $firstGroup.groupId | Out-Null
        $resume = New-ReviewFileGroupQueue -InventoryPath $result.inventoryPath -FileGroupsPath $result.fileGroupsPath -QueuePath $first.fileGroupQueuePath -Resume
        $resume.completedGroupCount | Should -Be 1
        (Get-NextReviewWorkItem -QueuePath $first.fileGroupQueuePath).item.groupId | Should -Be $script:Groups[1].groupId
        (Read-ReviewQueueDocument -Path $second.fileGroupQueuePath).items.Count | Should -Be $script:Groups.Count
    }

    It 'validates the entire manifest without requiring an identifier list' {
        $work = New-CompletedReviewWork
        $run = Invoke-WorkScript -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:Findings, '-ManifestPath', $work.manifestPath
        )
        $run.exitCode | Should -Be 0
        $run.errors | Should -BeNullOrEmpty
        $result = ConvertFrom-JsonVerbatim -Text $run.output
        $result.isValid | Should -BeTrue
        $result.artifacts.Count | Should -Be 2
        $result.findingCount | Should -Be 0
    }

    It 'detects a forgotten worker even after the dispatch queue is consumed' {
        $work = New-CompletedReviewWork
        Remove-Item -LiteralPath (Join-Path $script:Findings 'focus-002.json')
        $run = Invoke-WorkScript -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:Findings, '-ManifestPath', $work.manifestPath
        )
        $run.exitCode | Should -Not -Be 0
        $result = ConvertFrom-JsonVerbatim -Text $run.output
        $result.isValid | Should -BeFalse
        $result.missingReviewers | Should -Be @('focus-002')
    }

    It 'rejects a finished artifact attributed to the wrong focus area' {
        $work = New-CompletedReviewWork
        $path = Join-Path $script:Findings 'focus-001.json'
        $document = Read-FindingArtifact -Path $path
        $document.focusArea = 'unassigned'
        Write-FindingArtifact -Path $path -Document $document
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $script:Findings
        $result.isValid | Should -BeFalse
        $result.errors | Should -Match 'different focus area'
    }

    It 'rejects duplicate worker artifacts even when both have no findings' {
        $work = New-CompletedReviewWork
        Copy-Item -LiteralPath (Join-Path $script:Findings 'focus-001.json') -Destination (Join-Path $script:Findings 'duplicate.json')
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $script:Findings
        $result.isValid | Should -BeFalse
        $result.errors | Should -Match 'More than one artifact'
    }

    It 'rejects finished artifacts whose group or dispatch queues still contain work' {
        $work = New-CompletedReviewWork
        $manifest = Read-ReviewWorkManifest -Path $work.manifestPath
        Write-WorkJson -Path $manifest.reviewers[0].fileGroupQueuePath -Value (New-ReviewQueueDocument -Kind fileGroups -Item $script:Groups)
        Write-WorkJson -Path $work.focusAreaQueuePath -Value (New-ReviewQueueDocument -Kind focusAreas -Item $manifest.reviewers)
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $script:Findings
        $result.isValid | Should -BeFalse
        @($result.errors | Where-Object { $_ -match 'dispatch queue' }).Count | Should -Be 1
        @($result.errors | Where-Object { $_ -match 'no remaining items' }).Count | Should -Be 1
    }

    It 'refuses a malformed manifest instead of validating an empty expected set' {
        $work = New-CompletedReviewWork
        Write-WorkJson -Path $work.manifestPath -Value @{ schemaVersion = 1; reviewers = @() }
        { Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $script:Findings } |
            Should -Throw '*nonempty reviewers array*'
    }

    It 'refuses a manifest with duplicate assignments: <Field>' -ForEach @(
        @{ Field = 'reviewerId' }, @{ Field = 'focusArea' }, @{ Field = 'fileGroupQueuePath' }
    ) {
        $work = New-CompletedReviewWork
        $manifest = Read-ReviewWorkManifest -Path $work.manifestPath
        $manifest.reviewers[1][$Field] = $manifest.reviewers[0][$Field]
        Write-WorkJson -Path $work.manifestPath -Value $manifest
        { Read-ReviewWorkManifest -Path $work.manifestPath } | Should -Throw '*Invalid review-work manifest*'
    }

    It 'preserves completed and partial findings while replaying only the interrupted group' {
        $work = New-ReviewWork @script:WorkArguments
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        $queueArgs = @{
            InventoryPath = $work.inventoryPath
            FileGroupsPath = $work.fileGroupsPath
            QueuePath = $worker.fileGroupQueuePath
        }
        New-ReviewFileGroupQueue @queueArgs | Out-Null
        $artifactPath = Join-Path $script:Findings "$($worker.reviewerId).json"
        $artifact = New-FindingArtifactDocument -Kind reviewer -ReviewerId $worker.reviewerId -FocusArea $worker.focusArea
        $finding = @{
            title = 'Preserve review progress'
            description = 'Review progress must survive worker replacement.'
            reasoning = 'An interrupted worker may already have written findings.'
            consequence = 'Resetting its artifact loses those findings.'
            recommendedAction = 'Retain the artifact on resume.'
            anchor = @{ kind = 'file'; path = 'review.ps1' }
        }
        foreach ($index in 0..1) {
            (Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath).item.groupId | Should -Be $script:Groups[$index].groupId
            Add-FindingToArtifact -Document $artifact -Finding $finding -Origin reviewer | Out-Null
            Write-FindingArtifact -Path $artifactPath -Document $artifact
            if ($index -eq 0) {
                Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $script:Groups[$index].groupId | Out-Null
            }
        }

        $journalBefore = [IO.File]::ReadAllText($worker.fileGroupQueuePath)
        $resume = New-ReviewFileGroupQueue @queueArgs -Resume
        $resume.completedGroupCount | Should -Be 1
        $resume.inProgressGroupId | Should -Be 'group-002'
        $resume.remainingCount | Should -Be 0
        [IO.File]::ReadAllText($worker.fileGroupQueuePath) | Should -BeExactly $journalBefore
        $artifact = Open-FindingArtifactForResume -Path $artifactPath -ReviewerId $worker.reviewerId -FocusArea $worker.focusArea
        $artifact.findings.id | Should -Be @('focus-001-1', 'focus-001-2')
        { Assert-ReviewFileGroupQueueComplete -Path $worker.fileGroupQueuePath } | Should -Throw '*in-progress*'
        $replay = Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath
        $replay.item.groupId | Should -Be 'group-002'
        Add-FindingToArtifact -Document $artifact -Finding $finding -Origin reviewer | Should -Be 'focus-001-3'
        Write-FindingArtifact -Path $artifactPath -Document $artifact
        Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $replay.item.groupId | Out-Null
        (Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath).exhausted | Should -BeTrue
        (Read-FindingArtifact -Path $artifactPath).findings.Count | Should -Be 3
        Assert-ReviewFileGroupQueueComplete -Path $worker.fileGroupQueuePath -Group $script:Groups
    }

    It 'rejects missing journal coverage at resume and final validation without resetting it' {
        $work = New-CompletedReviewWork
        $manifest = Read-ReviewWorkManifest -Path $work.manifestPath
        $queuePath = $manifest.reviewers[0].fileGroupQueuePath
        $journal = Read-ReviewQueueDocument -Path $queuePath
        $journal.completedGroupIds = @('group-001')
        Write-WorkJson -Path $queuePath -Value $journal
        $before = [IO.File]::ReadAllText($queuePath)
        { New-ReviewFileGroupQueue -InventoryPath $work.inventoryPath -FileGroupsPath $work.fileGroupsPath -QueuePath $queuePath -Resume } |
            Should -Throw '*complete grouped inventory*'
        [IO.File]::ReadAllText($queuePath) | Should -BeExactly $before
        $result = Get-ReviewWorkValidation -ManifestPath $work.manifestPath -ArtifactDirectory $script:Findings
        $result.isValid | Should -BeFalse
        $result.errors | Should -Match 'complete grouped inventory'
    }

    It 'refuses altered queued metadata on resume' {
        $work = New-ReviewWork @script:WorkArguments
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        $queueArgs = @{ InventoryPath = $work.inventoryPath; FileGroupsPath = $work.fileGroupsPath; QueuePath = $worker.fileGroupQueuePath }
        New-ReviewFileGroupQueue @queueArgs | Out-Null
        $journal = Read-ReviewQueueDocument -Path $worker.fileGroupQueuePath
        $journal.items[0].files[0].path = 'changed.rs'
        Write-WorkJson -Path $worker.fileGroupQueuePath -Value $journal
        { New-ReviewFileGroupQueue @queueArgs -Resume } | Should -Throw '*changed*path*'
    }

    It 'creates initial state on resume when initialization did not finish' {
        $work = New-ReviewWork @script:WorkArguments
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        $result = New-ReviewFileGroupQueue -InventoryPath $work.inventoryPath -FileGroupsPath $work.fileGroupsPath -QueuePath $worker.fileGroupQueuePath -Resume
        $result.completedGroupCount | Should -Be 0
        $result.inProgressGroupId | Should -BeNullOrEmpty
        $result.remainingCount | Should -Be $script:Groups.Count
    }

    It 'keeps coverage under bounded randomized claim, completion, and resume sequences' {
        # A recorded random seed makes each generated transition sequence reproducible on failure.
        $seed = Get-Random
        $seed | Write-Information -InformationAction Continue
        $random = [Random]::new($seed)
        $work = New-ReviewWork @script:WorkArguments
        $worker = (Get-NextReviewWorkItem -QueuePath $work.focusAreaQueuePath).item
        $queueArgs = @{ InventoryPath = $work.inventoryPath; FileGroupsPath = $work.fileGroupsPath; QueuePath = $worker.fileGroupQueuePath }
        New-ReviewFileGroupQueue @queueArgs | Out-Null
        $completed = [Collections.Generic.List[string]]::new()
        $current = $null
        # Bound generated operations rather than time; the fixed tail finishes any remaining groups.
        foreach ($step in 1..64) {
            switch ($random.Next(4)) {
                0 {
                    $claim = Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath
                    if ($completed.Count -eq $script:Groups.Count) {
                        $claim.exhausted | Should -BeTrue
                    } else {
                        $claim.item.groupId | Should -Be $script:Groups[$completed.Count].groupId
                        $current = $claim.item.groupId
                    }
                }
                1 {
                    if ($null -ne $current) {
                        Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $current | Out-Null
                        $completed.Add($current)
                        $current = $null
                    }
                }
                2 { New-ReviewFileGroupQueue @queueArgs -Resume | Out-Null }
                3 {
                    if ($completed.Count -gt 0) {
                        (Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $completed[0]).alreadyComplete | Should -BeTrue
                    } else {
                        { Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId 'unknown' } | Should -Throw
                    }
                }
            }
            $journal = Read-ReviewQueueDocument -Path $worker.fileGroupQueuePath
            Assert-ReviewGroupJournalCoverage -Queue $journal -Group $script:Groups
            $journal.completedGroupIds.Count | Should -Be $completed.Count
            (Get-JsonProperty -Node $journal.inProgress -Name groupId) | Should -Be $current
        }
        foreach ($step in 1..($script:Groups.Count + 1)) {
            $claim = Get-NextReviewWorkItem -QueuePath $worker.fileGroupQueuePath
            if (-not $claim.exhausted) {
                Complete-TestReviewFileGroup -QueuePath $worker.fileGroupQueuePath -GroupId $claim.item.groupId | Out-Null
            }
        }
        Assert-ReviewFileGroupQueueComplete -Path $worker.fileGroupQueuePath -Group $script:Groups
    }

    It 'runs queue and finding commands with one JSON result and refuses premature completion' {
        $run = Invoke-WorkScript -Name 'New-ReviewWork.ps1' -ArgumentList @(
            '-InventoryPath', $script:Inventory, '-FileGroupsPath', $script:GroupedInventory, '-FocusAreaDirectory', $script:Areas, '-OutputDirectory', $script:Work
        )
        $run.exitCode | Should -Be 0
        $run.errors | Should -BeNullOrEmpty
        $work = ConvertFrom-JsonVerbatim -Text $run.output
        $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $work.focusAreaQueuePath)
        $run.exitCode | Should -Be 0
        $worker = (ConvertFrom-JsonVerbatim -Text $run.output).item
        $run = Invoke-WorkScript -Name 'New-ReviewFileGroupQueue.ps1' -ArgumentList @('-InventoryPath', $work.inventoryPath, '-FileGroupsPath', $work.fileGroupsPath, '-QueuePath', $worker.fileGroupQueuePath)
        $run.exitCode | Should -Be 0
        (ConvertFrom-JsonVerbatim -Text $run.output).fileCount | Should -Be $script:Files.Count
        (ConvertFrom-JsonVerbatim -Text $run.output).groupCount | Should -Be $script:Groups.Count
        $run = Invoke-WorkScript -Name 'New-FindingArtifact.ps1' -ArgumentList @(
            '-OutputDirectory', (Join-Path $script:Root 'findings'), '-ReviewerId', $worker.reviewerId, '-FocusArea', $worker.focusArea
        )
        $run.exitCode | Should -Be 0
        $artifact = (ConvertFrom-JsonVerbatim -Text $run.output).artifactPath
        $run = Invoke-WorkScript -Name 'Complete-FindingArtifact.ps1' -ArgumentList @('-ArtifactPath', $artifact, '-FileGroupQueuePath', $worker.fileGroupQueuePath)
        $run.exitCode | Should -Not -Be 0
        $run.output | Should -BeNullOrEmpty
        (Read-FindingArtifact -Path $artifact).status | Should -Be 'open'
        foreach ($group in $script:Groups) {
            $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $worker.fileGroupQueuePath)
            $run.exitCode | Should -Be 0
            $next = ConvertFrom-JsonVerbatim -Text $run.output
            $next.exhausted | Should -BeFalse
            $next.item.groupId | Should -Be $group.groupId
            $next.item.files.path | Should -Be $group.files.path
            Set-TestGroupAssessments -QueuePath $worker.fileGroupQueuePath
            $run = Invoke-WorkScript -Name 'Complete-ReviewFileGroup.ps1' -ArgumentList @('-QueuePath', $worker.fileGroupQueuePath, '-GroupId', $group.groupId)
            $run.exitCode | Should -Be 0
            (ConvertFrom-JsonVerbatim -Text $run.output).groupId | Should -Be $group.groupId
        }
        $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $worker.fileGroupQueuePath)
        (ConvertFrom-JsonVerbatim -Text $run.output).exhausted | Should -BeTrue
        $run = Invoke-WorkScript -Name 'Complete-FindingArtifact.ps1' -ArgumentList @('-ArtifactPath', $artifact, '-FileGroupQueuePath', $worker.fileGroupQueuePath)
        $run.exitCode | Should -Be 0
        (ConvertFrom-JsonVerbatim -Text $run.output).status | Should -Be 'complete'
        (Get-FindingArtifactValidation -Path $artifact -ExpectedReviewerId @($worker.reviewerId)).isValid | Should -BeTrue
    }

    It 'runs the complete native review workflow with PowerShell-produced inputs and findings' {
        Remove-Item -LiteralPath (Join-Path $script:Areas 'z.md')
        $publishedInventory = Join-Path $script:Root 'published-inventory.json'
        $publishedGroups = Join-Path $script:Root 'published-groups.json'
        $run = Invoke-WorkScript -Name 'Set-ReviewInput.ps1' -ArgumentList @(
            '-Kind', 'Inventory', '-InputPath', $script:Inventory, '-Path', $publishedInventory
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        $run = Invoke-WorkScript -Name 'Set-ReviewInput.ps1' -ArgumentList @(
            '-Kind', 'Groups', '-InputPath', $script:GroupedInventory, '-Path', $publishedGroups, '-InventoryPath', $publishedInventory
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        (Read-ReviewFileInventory -Path $publishedInventory).Count | Should -Be $script:Files.Count
        (Read-ReviewFileGroups -Path $publishedGroups -FileInventory (Read-ReviewFileInventory -Path $publishedInventory)).Count |
            Should -Be $script:Groups.Count

        $run = Invoke-WorkScript -Name 'New-ReviewWork.ps1' -ArgumentList @(
            '-InventoryPath', $publishedInventory, '-FileGroupsPath', $publishedGroups, '-FocusAreaDirectory', $script:Areas,
            '-OutputDirectory', $script:Work, '-RepositoryRoot', $script:Root, '-ArtifactDirectory', $script:Findings
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        $work = ConvertFrom-JsonVerbatim -Text $run.output
        $manifest = Read-ReviewWorkManifest -Path $work.manifestPath
        $manifest.reviewers.Count | Should -Be 1
        $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $work.focusAreaQueuePath)
        $run.exitCode | Should -Be 0 -Because $run.errors
        $reviewer = (ConvertFrom-JsonVerbatim -Text $run.output).item
        $reviewer.reviewerId | Should -BeExactly $manifest.reviewers[0].reviewerId
        $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $work.focusAreaQueuePath)
        $run.exitCode | Should -Be 0 -Because $run.errors
        (ConvertFrom-JsonVerbatim -Text $run.output).exhausted | Should -BeTrue

        $state = Join-Path $script:Root "session O'Brien"
        [void] [IO.Directory]::CreateDirectory($state)
        $scope = Join-Path $state 'scope.md'
        [IO.File]::WriteAllText($scope, 'Review the complete inventory.')
        [IO.File]::WriteAllText((Join-Path $state 'references.md'), 'No additional references.')
        [IO.File]::WriteAllText((Join-Path $state 'content-overview.md'), 'Review the fixture.')
        $run = Invoke-WorkScript -Name 'New-ReviewWorkerAssignment.ps1' -ArgumentList @(
            '-Phase', 'review', '-SessionState', $state, '-ScopePath', $scope, '-ManifestPath', $work.manifestPath,
            '-ReviewerId', $reviewer.reviewerId, '-OutputDirectory', (Join-Path $state 'attempt')
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        $assignment = Read-WorkflowData -Kind workerAssignment -Path ((ConvertFrom-JsonVerbatim -Text $run.output).assignmentPath)
        $assignment.resume | Should -BeTrue

        $run = Invoke-WorkScript -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:Findings, '-ManifestPath', $work.manifestPath
        )
        $run.exitCode | Should -Not -Be 0
        $artifact = Read-FindingArtifact -Path $reviewer.artifactPath
        $findingId = Add-FindingToArtifact -Document $artifact -Origin reviewer -Finding @{
            title = 'Preserve data'; description = 'A concrete problem.'; reasoning = 'Evidence supports this.'
            consequence = 'Data can be lost.'; recommendedAction = 'Retain data.'
            anchor = @{ kind = 'file'; path = 'new [file].rs' }
        }
        Write-FindingArtifact -Path $reviewer.artifactPath -Document $artifact

        foreach ($group in $script:Groups) {
            $run = Invoke-WorkScript -Name 'Get-NextReviewItem.ps1' -ArgumentList @('-QueuePath', $reviewer.fileGroupQueuePath)
            $run.exitCode | Should -Be 0 -Because $run.errors
            $claim = ConvertFrom-JsonVerbatim -Text $run.output
            $claim.item.groupId | Should -BeExactly $group.groupId
            $draft = @{
                schemaVersion = 1; snapshot = $claim.snapshot; groupId = $claim.item.groupId
                assessments = @($claim.item.files | ForEach-Object {
                    @{ groupId = $claim.item.groupId; path = $_.path; disposition = 'reviewed'; scope = ''; reason = '' }
                })
                findingIds = @(if ($group.groupId -eq $script:Groups[0].groupId) { $findingId })
            }
            $draftPath = Join-Path $state "$($group.groupId).json"
            Write-WorkJson -Path $draftPath -Value $draft
            $run = Invoke-WorkScript -Name 'Set-ReviewAssessmentBatch.ps1' -ArgumentList @(
                '-QueuePath', $reviewer.fileGroupQueuePath, '-InventoryPath', $work.inventoryPath,
                '-FileGroupsPath', $work.fileGroupsPath, '-ArtifactPath', $reviewer.artifactPath, '-BatchPath', $draftPath
            )
            $run.exitCode | Should -Be 0 -Because $run.errors
            (ConvertFrom-JsonVerbatim -Text $run.output).groupId | Should -BeExactly $group.groupId
        }
        $run = Invoke-WorkScript -Name 'Complete-FindingArtifact.ps1' -ArgumentList @(
            '-ArtifactPath', $reviewer.artifactPath, '-FileGroupQueuePath', $reviewer.fileGroupQueuePath
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        $run = Invoke-WorkScript -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:Findings, '-ManifestPath', $work.manifestPath
        )
        $run.exitCode | Should -Be 0 -Because $run.errors
        (ConvertFrom-JsonVerbatim -Text $run.output).isValid | Should -BeTrue
        $finished = Read-FindingArtifact -Path $reviewer.artifactPath
        $finished.status | Should -BeExactly 'complete'
        $finished.findings[0].id | Should -BeExactly $findingId
        (Read-ReviewQueueDocument -Path $reviewer.fileGroupQueuePath).completedGroupIds.Count | Should -Be $script:Groups.Count
    }
}

Describe 'Review queue consumption' {
    BeforeEach {
        $script:Queue = Join-Path $TestDrive "$([guid]::NewGuid()).json"
        $script:Items = @('one', 'two', 'three' | ForEach-Object {
            @{ groupId = $_; files = @(@{ path = Join-Path $TestDrive "$_.rs"; sizeBytes = 1; change = 'modified' }) }
        })
        Write-WorkJson -Path $script:Queue -Value (New-ReviewQueueDocument -Kind fileGroups -Item $script:Items)
    }

    It 'returns every item once in order, including the last item before exhaustion' {
        foreach ($index in 0..($script:Items.Count - 1)) {
            $result = Get-NextReviewWorkItem -QueuePath $script:Queue
            $result.exhausted | Should -BeFalse
            $result.item.groupId | Should -Be $script:Items[$index].groupId
            $result.remainingCount | Should -Be ($script:Items.Count - $index - 1)
            Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId $result.item.groupId | Out-Null
        }
        foreach ($attempt in 1..2) {
            $result = Get-NextReviewWorkItem -QueuePath $script:Queue
            $result.exhausted | Should -BeTrue
            $result.item | Should -BeNullOrEmpty
            $result.remainingCount | Should -Be 0
        }
        Assert-ReviewFileGroupQueueComplete -Path $script:Queue
    }

    It 'preserves singleton arrays' {
        Write-WorkJson -Path $script:Queue -Value (New-ReviewQueueDocument -Kind fileGroups -Item @($script:Items[0]))
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'one'
        Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId 'one' | Out-Null
        (Get-NextReviewWorkItem -QueuePath $script:Queue).exhausted | Should -BeTrue
    }

    It 'replays an unacknowledged claim instead of advancing the queue' {
        $first = Get-NextReviewWorkItem -QueuePath $script:Queue
        $first.item.groupId | Should -Be 'one'
        $before = [IO.File]::ReadAllText($script:Queue)
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'one'
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $before
        Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId 'one' | Out-Null
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'two'
        (Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId 'one').alreadyComplete | Should -BeTrue
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'two'
        { Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId 'three' } | Should -Throw '*not the current*'
    }

    It 'keeps the last claimed group unfinished until completion is durably recorded' {
        Write-WorkJson -Path $script:Queue -Value (New-ReviewQueueDocument -Kind fileGroups -Item @($script:Items[0]))
        $claim = Get-NextReviewWorkItem -QueuePath $script:Queue
        $claim.remainingCount | Should -Be 0
        { Assert-ReviewFileGroupQueueComplete -Path $script:Queue } | Should -Throw '*in-progress*'
        (Get-NextReviewWorkItem -QueuePath $script:Queue).exhausted | Should -BeFalse
        Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId $claim.item.groupId | Out-Null
        Assert-ReviewFileGroupQueueComplete -Path $script:Queue
    }

    It 'retains the outstanding claim if completion persistence fails' {
        Get-NextReviewWorkItem -QueuePath $script:Queue | Out-Null
        Mock Write-PullRequestOutputFileAtomically { throw 'Simulated completion write failure' }
        { Complete-TestReviewFileGroup -QueuePath $script:Queue -GroupId 'one' } | Should -Throw '*completion write failure*'
        $journal = Read-ReviewQueueDocument -Path $script:Queue
        $journal.inProgress.groupId | Should -Be 'one'
        $journal.completedGroupIds.Count | Should -Be 0
        (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'one'
    }

    It 'rejects invalid state rather than reporting exhaustion: <Json>' -ForEach @(
        @{ Json = '{broken' }, @{ Json = '[]' }, @{ Json = 'null' }
        @{ Json = '{"schemaVersion":1,"kind":"fileGroups","items":null}' }
        @{ Json = '{"schemaVersion":1,"kind":"fileGroups","items":{}}' }
        @{ Json = '{"schemaVersion":1,"kind":"fileGroups","items":[null]}' }
        @{ Json = '{"schemaVersion":2,"kind":"fileGroups","items":[]}' }
        @{ Json = '{"schemaVersion":2,"kind":"fileGroups","items":[],"inProgress":null,"completedGroupIds":["one","one"]}' }
        @{ Json = '{"schemaVersion":2,"kind":"fileGroups","items":[],"inProgress":{"groupId":"one","files":[{}]},"completedGroupIds":["one"]}' }
        @{ Json = '{"schemaVersion":2,"kind":"fileGroups","items":[],"inProgress":null,"completedGroupIds":{}}' }
        @{ Json = '{"schemaVersion":1,"kind":"unknown","items":[]}' }
    ) {
        Set-Content -LiteralPath $script:Queue -Value $Json -NoNewline
        { Get-NextReviewWorkItem -QueuePath $script:Queue } | Should -Throw
        [IO.File]::ReadAllText($script:Queue) | Should -BeExactly $Json
    }

    It 'rejects a missing queue' {
        Remove-Item -LiteralPath $script:Queue
        { Get-NextReviewWorkItem -QueuePath $script:Queue } | Should -Throw
    }

    It 'does not report a claim when persisting it fails and releases its lock' {
        Mock Write-PullRequestOutputFileAtomically { throw 'Simulated write failure' }
        { Get-NextReviewWorkItem -QueuePath $script:Queue } | Should -Throw '*Simulated write failure*'
        (Read-ReviewQueueDocument -Path $script:Queue).items.Count | Should -Be $script:Items.Count
        $lock = Open-ReviewQueueLock -Path $script:Queue
        $lock.Dispose()
    }

    It 'waits for another caller before claiming an item' {
        $script:HeldQueueLock = Open-ReviewQueueLock -Path $script:Queue
        Mock Start-Sleep { $script:HeldQueueLock.Dispose() }
        try {
            (Get-NextReviewWorkItem -QueuePath $script:Queue).item.groupId | Should -Be 'one'
            Should -Invoke Start-Sleep -Times 1 -Exactly
        } finally {
            $script:HeldQueueLock.Dispose()
        }
        (Read-ReviewQueueDocument -Path $script:Queue).items.Count | Should -Be ($script:Items.Count - 1)
    }

    It 'does not accept an exhausted focus-area queue as group completion' {
        Write-WorkJson -Path $script:Queue -Value (New-ReviewQueueDocument -Kind focusAreas -Item @())
        { Assert-ReviewFileGroupQueueComplete -Path $script:Queue } | Should -Throw '*group journal*'
    }
}

Describe 'Focus-area worker instructions' {
    It 'treats guideline departures as eligible and explains consequences with proportionate remedies' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $quality = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'qualitymaxx.md') -Raw
        foreach ($prompt in @($review, $quality)) {
            $prompt | Should -Match 'verified, unjustified departure from an applicable guideline is sufficient grounds'
            $prompt | Should -Match 'implementation effort, revalidation effort, and recurring maintenance burden'
            $prompt | Should -Match 'Do not invent consequences or exaggerate their likelihood'
            $prompt | Should -Not -Match 'offers no concrete benefit|against its demonstrated benefit'
        }
        $review | Should -Match '\| `consequence` \| yes \|.*reader who does not share our guidelines'
        $review | Should -Match 'quantified harm or a demonstrated runtime defect is not required'
        $review | Should -Match 'find a smaller one; omit the finding only when no proportionate, actionable recommendation remains'
        $quality | Should -Match 'Repair an incomplete explanation'
        $quality | Should -Match 'the departure is justified'
        $quality | Should -Match 'after considering simpler alternatives'
        [regex]::Matches($quality, 'Remove a finding only when').Count | Should -Be 1
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $synthesis = [regex]::Match($skill, '(?s)# Phase 5: synthesis(.*?)## Pre-audit refresh').Groups[1].Value
        $synthesis | Should -Match 'same eligibility and proportionate-remedy rules as an individual finding'
        $synthesis | Should -Match "without independently proving the guideline's value"
    }

    It 'requires rolling concurrency, default context, passive compaction notes, and granular resume' {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $phase = [regex]::Match($skill, '(?s)# Phase 2: review(.*?)# Phase 3:').Groups[1].Value
        $phase | Should -Match 'rolling pool of up to 16 concurrent workers'
        $phase | Should -Match 'context_tier: "default"`, including retries'
        $phase | Should -Match 'Do not claim work before a slot is available'
        $phase | Should -Match 'nonblocking/background mode'
        $phase | Should -Match 'before awaiting any worker result'
        $phase | Should -Match 'completed groups are skipped'
        $phase | Should -Not -Match 'matrix|context_tier: "long_context"'
        $phase | Should -Match 'one findings artifact per focus-area worker, not per group or attempt'
        $phase | Should -Match 'all.*finding|every emitted finding'
        $phase | Should -Not -Match 'COMPACTION_ACTION|RETRY_FLAG|resetting both|retry its full pass'
        $skill | Should -Match 'Do not set optional subagent token budgets'
        $skill | Should -Match 'prepare-file-groups.md'
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'do not try to force it, wait for it, or require it'
        $review | Should -Match 'Discard the contents of files already read rather than summarizing'
        $review | Should -Match 'includes focus-area guidance'
        $review | Should -Match "re-read the review instructions and focus-area file through the assignment's prepared bounded reader"
        $review | Should -Match 'Do not recreate your queue or artifact on compaction'
        $review | Should -Match 'record-assessment-batch.md'
        $review | Should -Match 'do not launch a process per file'
        $review | Should -Match '\{\{RESUME_FLAG\}\}'
        $grouping = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'prepare-file-groups.md') -Raw
        $grouping | Should -Match '20 files or 100 000 bytes'
        $grouping | Should -Match 'Include every file exactly once'
    }

    It 'accepts diff-first review and schema-aware queries without mandatory whole-file reads' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'start with the diff and analyze the changed content in every non-excluded file'
        $review | Should -Match 'surrounding ranges, related files, or the whole file'
        $review | Should -Match 'full-file reads are optional'
        $review | Should -Match 'For a review without a diff, analyze the requested content'
        $review | Should -Match 'known schema, such as JSON'
        $review | Should -Match 'prefer read-only schema-aware queries'
        $review | Should -Match 'truncates evidence needed for the analysis, retrieve the missing evidence'
        $review | Should -Match 'reassess it using the same diff-first, targeted-read, and query approach'
        $review | Should -Match 'Targeted reads or queries are not incomplete coverage'
        $review | Should -Match 'Every PR context read must follow `pwsh-calls/query-pr-context.md`'

        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $skill | Should -Match 'full-file reads are optional'
        $skill | Should -Match 'Diffs, targeted reads, and structured-data queries can satisfy coverage'
        $skill | Should -Match 'same targeted-inspection policy'
        foreach ($relativePath in @(
            'review.md', 'SKILL.md', 'docs\design.md', 'docs\implementation.md', 'workflow-data.md',
            'data-guides\review-coverage.md',
            'pwsh-calls\get-next-review-file-group.md',
            'pwsh-calls\complete-review-file-group.md',
            'pwsh-calls\complete-finding-artifact.md'
        )) {
            $content = Get-Content -LiteralPath (Join-Path $script:SkillRoot $relativePath) -Raw
            $requiresFullRead = $content -match '\b(read (every non-excluded file|the file|it) in full|fully read|full reading|full-file\s+analysis|full-read)\b'
            $requiresFullRead | Should -BeFalse -Because "$relativePath must allow targeted inspection"
        }
    }

    It 'requires explicit scope evidence for every skipped file without bypassing group completion' {
        $review = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'review.md') -Raw
        $review | Should -Match 'only when every guideline section clearly excludes it'
        $review | Should -Match 'If any section might apply, analyze the requested scope'
        $review | Should -Match 'uncertainty requires investigation, not a skip'
        $review | Should -Match 'record-file-assessment.md'
        $review | Should -Match 'Exclusions require the path, controlling scope, and concrete reason'
        $review | Should -Match 'Do not remove files from the inventory or omit queue groups'
        $review | Should -Match 'Assess deleted files using pre-change paths, content, and removal diffs'
        $review | Should -Match 'Assess both entries of a rename independently'
        $review | Should -Not -Match 'Do not skip files or groups based on predicted relevance'

        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $skill | Should -Match 'Verify exclusions against all sections in mixed-scope focus-area files'
        $skill | Should -Match 'Normal queue exhaustion does not complete this explicit repair assignment'
        $review | Should -Match 'On resume, read the checkpoint and existing journal'
        $review | Should -Match 'Earlier assessments survive queue initialization'
        $completion = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\complete-review-file-group.md'
        ) -Raw
        $completion | Should -Match 'journaled explicit scope exclusion'
        $review | Should -Match 'groups with no findings or only excluded files'
        $completion | Should -Match 'legacy per-file recovery path'
        $completion | Should -Match 'can satisfy coverage without a whole-file read'
        $claim = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\get-next-review-file-group.md'
        ) -Raw
        $claim | Should -Match 'analyze the requested scope in every non-excluded file'
        $artifact = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\complete-finding-artifact.md'
        ) -Raw
        $artifact | Should -Match 'journaled explicit scope exclusion'
        $artifact | Should -Match 'Complete any targeted coverage repair'
        $artifact | Should -Match 'can satisfy coverage without a whole-file read'
    }

    It 'carries original scope and provenance through generation and verifies scopes before storage' {
        $preparation = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'prepare-focus-areas.md') -Raw
        $preparation | Should -Match 'Derive scope from the materialized source files'
        $preparation | Should -Match 'complete `applyTo` glob list'
        $preparation | Should -Match 'inherited by every section in that file'
        $preparation | Should -Match 'in any enclosing section'
        $preparation | Should -Match 'original scope declarations and frontmatter values verbatim'
        $preparation | Should -Match 'Alternatives within an `applyTo` list remain alternatives'
        $preparation | Should -Match 'Expanded guidance inherits the scope of its referring instruction'
        $preparation | Should -Match 'assign only one writer to each expanded target'
        $preparation | Should -Match 'including later-discovered referrers'
        $preparation | Should -Match 'Do not remove review criteria for authored agent workflows'
        $preparation | Should -Match 'complete alternative referring contexts'
        $preparation | Should -Match 'retaining all source attribution and distinct applicability contexts as alternatives'
        $preparation | Should -Match 'Scope verification happens before storage in Stage 3'
        $preparation | Should -Match 'Structural validation cannot establish these semantic properties'
    }
}

Describe 'Audit presentation instructions' {
    BeforeAll {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $script:AuditPresentation = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $script:InSessionAudit = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'audit-in-session.md') -Raw
    }

    It 'pairs the audited capture with the review clone when building the approved set' {
        $script:AuditPresentation | Should -Match 'pwsh-calls/build-approved-findings.md'
        $recipe = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\build-approved-findings.md'
        ) -Raw
        $command = [regex]::Match($recipe, '(?s)```powershell\s+(.*?)```').Groups[1].Value
        $command | Should -Match '-ContextPath "\{\{CONTEXT_PATH\}\}"'
        $command | Should -Match '-RepositoryPath "\{\{REPOSITORY_PATH\}\}"'
        $recipe | Should -Match '\| `REPOSITORY_PATH` \| Local review clone holding the commits named by the audited capture\. \|'
        $recipe | Should -Match 'pre-audit refresh or a return to audit after revalidation'
        $recipe | Should -Match 'With no pull request, omit both parameters'
        $recipe | Should -Match 'Handle any excluded anchors as phase 8 describes'
    }

    It 'repeats the audited PR identity at every audit question in either presentation mode' {
        $script:AuditPresentation | Should -Match '`id`, `title`, and `pullRequestUrl` for presentation'
        $script:AuditPresentation | Should -Match 'repeat the audited snapshot''s PR number \(`id`\) and full `title` as a named hyperlink to `pullRequestUrl`'
        $script:AuditPresentation | Should -Match 'in ordinary session text immediately before every audit question, regardless of the publication destination'
        $script:AuditPresentation | Should -Match 'Also repeat the PR number and title in the question itself'
        $script:AuditPresentation | Should -Match 'document and in-session audits, including resumed or amended presentations and the final merge question'
        $script:AuditPresentation | Should -Match 'Omit it when the review has no associated pull request'
        $script:InSessionAudit | Should -Match "Apply phase 7's PR reminder in both session text and the question UI for every per-finding and final merge question"
    }

    It 'offers an opt-in walkthrough after the applicable document approval choices' {
        $script:AuditPresentation | Should -Match 'Offer the combined document by default'
        $script:AuditPresentation | Should -Match 'Omit the merge approval choice when.*isAuthoredBySelf.*or the destination is not the pull request'
        $script:AuditPresentation | Should -Match 'selecting it approves nothing'
        $script:AuditPresentation | Should -Match 'Load that guide only when the user chooses the walkthrough or resumes one'
        $script:InSessionAudit | Should -Match 'user selects `Review findings in-session` in phase 7'
        $script:AuditPresentation | Should -Not -Match 'Do not present the findings one by one|do not ask a separate question per finding'
    }

    It 'defines one literal document-audit question and ordered choice list for every presentation' {
        $question = [regex]::Match(
            $script:AuditPresentation, '(?s)```text\r?\n(.*?)\r?\n```'
        ).Groups[1].Value -replace "`r`n", "`n"
        $question | Should -Be (
            'PR #{{PR_ID}}: {{PR_TITLE}}' + "`n`n" +
            'How would you like to proceed with the linked audit? Provide any per-finding feedback using finding IDs.'
        )
        $choices = [regex]::Matches(
            $script:AuditPresentation, '(?m)^[1-3]\. `([^`]+)`\r?$'
        ) | ForEach-Object { $_.Groups[1].Value }
        ($choices -join '|') | Should -Be 'Approve findings|Approve findings and approve merge|Review findings in-session'
        $script:AuditPresentation | Should -Match 'Replace `\{\{PR_ID\}\}` and `\{\{PR_TITLE\}\}` with the audited snapshot''s `id` and full `title`'
        $script:AuditPresentation | Should -Match 'Omit the PR reminder line and its following blank line when the review has no associated pull request'
        $script:AuditPresentation | Should -Match 'preserve the question wording, choice labels, and relative order exactly, including on resumed or amended document presentations'
        $script:AuditPresentation | Should -Match 'Do not paraphrase, reorder, add recommendation suffixes, or add explicit choices for feedback or Other'
        $script:AuditPresentation | Should -Match 'use the tool''s free-form response for feedback'
    }

    It 'uses pending identifiers and existing records rather than a second audit queue' {
        $script:InSessionAudit | Should -Match 'get-next-auto-approval-finding.md` before each iteration and on resumption'
        $script:InSessionAudit | Should -Match 'Use only its `auditFindingIds`, in their returned order'
        $script:InSessionAudit | Should -Match 'describe policy classification, not human-audit completion'
        $script:InSessionAudit | Should -Match 'settled and policy-approved findings are not presented again'
        $script:InSessionAudit | Should -Match 'Do not rerun classification or create a separate queue'
        $script:AuditPresentation | Should -Match 'continue with `audit-in-session.md` instead of repeating the document-choice question'
        $script:InSessionAudit | Should -Match "an interruption preserves the existing presentation's progress instead of resetting it"
    }

    It 'shows the complete rendered finding in session text before its own approval question' {
        $script:InSessionAudit | Should -Match 'Show `content-overview.md` in ordinary session text once before the findings'
        $script:InSessionAudit | Should -Match 'findings` entries \(`findingId` and `path`\)'
        $script:InSessionAudit | Should -Match 'complete any truncated read before presentation'
        $script:InSessionAudit | Should -Match '(?s)complete rendered Markdown verbatim in an ordinary assistant message.*Invoke the user-question tool for that finding alone'
        $script:InSessionAudit | Should -Match 'Do not summarize, truncate, paraphrase, or substitute a file link'
        $script:InSessionAudit | Should -Match 'asking "Approve finding <id>\?" with `Approve` as the explicit choice'
        $script:InSessionAudit | Should -Match "tool's built-in Other/free-form response"
    }

    It 'persists only settled findings and keeps feedback or unanswered questions from advancing' {
        $script:InSessionAudit | Should -Match 'only the displayed finding, never other findings or the merge'
        $script:InSessionAudit | Should -Match 'record-finding-decision.md` before selecting another finding'
        $script:InSessionAudit | Should -Match 'preserve verbatim feedback with `pwsh-calls/record-audit-feedback.md`'
        $script:InSessionAudit | Should -Match 'a clarification or amendment leaves the finding undecided'
        $script:InSessionAudit | Should -Match 'ask about the same finding again after clarification or amendment'
        $script:InSessionAudit | Should -Match 'An unanswered question pauses the workflow without a new verdict or advancement'
        $script:AuditPresentation | Should -Match 'an in-session `Approve` settles only the displayed finding'
        $script:AuditPresentation | Should -Match 'In in-session mode, redisplay the current finding'
    }

    It 'keeps a resumable final merge gate separate from per-finding approvals and publication' {
        $script:InSessionAudit | Should -Match 'Mark that question pending before asking about findings'
        $script:InSessionAudit | Should -Match 'destination is the pull request and `isAuthoredBySelf` is false'
        $script:InSessionAudit | Should -Match 'ask a separate final question offering "approve the pull request merge" and "do not approve the pull request merge"'
        $script:InSessionAudit | Should -Match 'even if the user rejected every finding'
        $script:InSessionAudit | Should -Match 'ask it if still pending even when `auditFindingIds` is empty'
        $script:InSessionAudit | Should -Match 'Do not offer merge approval for self-authored pull requests or other destinations'
        $script:InSessionAudit | Should -Match 'Do not write merge state for an unanswered question'
        $script:InSessionAudit | Should -Match 'only after every finding and any applicable final merge question are settled'
        $script:InSessionAudit | Should -Match 'normal phase 8 freshness check before publication'
        $script:AuditPresentation | Should -Match 'unless an in-session audit still awaits its final merge decision'
        $script:AuditPresentation | Should -Match 'In-session per-finding approvals do not settle the audit or write merge state'
    }

    It 'preserves Windows paths and keeps them out of the question UI' {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $phase = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $phase | Should -Match 'absolute `htmlPath` as inline code'
        $phase | Should -Match 'absolute `auditDocumentPath` as inline code'
        $phase | Should -Match 'separator before `\.copilot`'
        $phase | Should -Match 'do not repeat its URI or absolute path in the question UI'

        $markdownCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\render-findings.md'
        ) -Raw
        $markdownCall | Should -Match 'auditDocumentPath` separately as an inline-code fallback'

        $htmlCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\render-report-html.md'
        ) -Raw
        $htmlCall | Should -Match 'absolute HTML path separately as inline code'

        $design = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'docs\design.md') -Raw
        $design | Should -Match 'links as inline code'
        $design | Should -Match 'rather than\s+repeating local paths'

        $implementation = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'docs\implementation.md'
        ) -Raw
        $implementation | Should -Match 'absolute paths separately as inline code'
        $implementation | Should -Match 'rather than repeating their URIs or local paths'
    }
}

Describe 'Merge approval refresh instructions' {
    BeforeAll {
        $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
        $script:AuditPhase = [regex]::Match($skill, '(?s)# Phase 7: audit(.*?)# Phase 8:').Groups[1].Value
        $script:FreshnessPhase = [regex]::Match($skill, '(?s)# Phase 8: freshness check(.*?)# Phase 9:').Groups[1].Value
    }

    Describe 'Incremental freshness ordering' {
        It 'retires stale findings before policy or human presentation and retains the final gate' {
            $skill = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'SKILL.md') -Raw
            $preAudit = [regex]::Match($skill, '(?s)## Pre-audit refresh(.*?)# Phase 6:').Groups[1].Value
            $preAudit | Should -Match 'before phase 6 policy classification or phase 7 human audit'
            $preAudit | Should -Match 'every current finding, not just approved findings'
            $preAudit | Should -Match 'retire fixed or invalidated findings before presentation'
            $preAudit | Should -Match 'including force-pushes, without assuming ancestry'
            $preAudit | Should -Match 'New and revised findings proceed through phase 6 normally'
            $preAudit | Should -Match 'Measure agent execution separately from the human response interval'
            $final = [regex]::Match($skill, '(?s)# Phase 8: freshness check(.*?)# Phase 9:').Groups[1].Value
            $final | Should -Match 'including after every human wait or destination decision'
            $final | Should -Match 'last checked capture, not always the initial capture'
            $final | Should -Match 'If `hasChanges` is false and no authorship decisions need reconsideration'
            $final | Should -Match 'skip repeated semantic revalidation'
            $final | Should -Match 'never substitution of an unchecked capture or bypass'
            $final | Should -Match 'strict narrowing.*retains the existing approval'
            $final | Should -Match 'mark each materially revised finding.*as `needs-audit`'
            $skill | Should -Match 'Preserve the audited path and rendered text while waiting'
        }
    }

    It 'asks for an initial merge decision without repeating it for an empty returned audit' {
        $script:AuditPhase | Should -Match 'this run has no recorded merge decision, ask whether'
        $script:AuditPhase | Should -Match 'freshness revalidation requires no new audit, retain that decision without asking again'
        $script:AuditPhase | Should -Not -Match 'still ask whether to also approve'
    }

    It 'carries approval after movement only when no findings need audit' {
        $script:FreshnessPhase | Should -Match 'After recording revalidation decisions, execute `pwsh-calls/get-next-auto-approval-finding.md` to refresh `auditFindingIds`'
        $script:FreshnessPhase | Should -Match 'If `auditFindingIds` is empty, retain the prior merge decision and do not return to phase 7 or ask for merge approval again'
        $script:FreshnessPhase | Should -Match 'This also applies when revalidation drops every finding'
        $script:FreshnessPhase | Should -Match 'When the source commit moved, use the carry-forward command'
        $script:FreshnessPhase | Should -Match 'When the source commit is unchanged, leave the merge-decision record unchanged'

        $recordCall = Get-Content -LiteralPath (
            Join-Path $script:SkillRoot 'pwsh-calls\record-merge-approval.md'
        ) -Raw
        $recordCall | Should -Match 'carry an existing approval without asking the user again'
        $recordCall | Should -Match 'preserve the absence of\s+approval rather than asking again solely because the PR moved'
    }

    It 'requires a fresh merge decision whenever refresh produces findings needing audit' {
        $script:FreshnessPhase | Should -Match 'If that list is nonempty, return to phase 7 and require a fresh merge decision alongside the new audit, even if only discussion changed'
        $script:FreshnessPhase | Should -Match 'Do not carry an earlier merge approval in that case'
        $script:AuditPhase | Should -Match 'if the user settles that audit without explicitly approving the merge, record `DoNotApprove`'
        $script:AuditPhase | Should -Match 'Clarification or an unanswered question does not settle the audit and writes no merge state'
    }
}
