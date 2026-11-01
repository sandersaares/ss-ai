#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:SkillRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\skills\ss-review'))
    $schema = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText(
        (Join-Path $script:SkillRoot 'schemas/workflow-data.schema.json')
    )) -AsHashtable
    $schema['$ref'] = '#/$defs/sharedReadPage'
    $script:PageSchema = ConvertTo-Json -InputObject $schema -Depth 100 -Compress

    # Exhausts a fixed fixture with an operation bound, checking exact reconstruction and the byte budget.
    function Read-AllSharedPages {
        param([string] $AssignmentPath, [string] $Id)
        $text = [Text.StringBuilder]::new()
        $offset = 0
        for ($attempt = 0; $attempt -lt 1000; $attempt++) {
            $read = Invoke-WorkerRead -AssignmentPath $AssignmentPath -DocumentId $Id -Offset $offset
            $read.exitCode | Should -Be 0 -Because $read.errors
            $script:NativePageReads++
            [Text.Encoding]::UTF8.GetByteCount($read.output) | Should -BeLessOrEqual 16384
            Test-Json -Json $read.output -Schema $script:PageSchema -ErrorAction Stop | Should -BeTrue
            $page = ConvertFrom-Json -InputObject $read.output -AsHashtable
            $page.contentOffset | Should -Be $offset
            $null = $text.Append($page.content)
            if ($page.complete) { return $text.ToString() }
            $page.nextOffset | Should -BeGreaterThan $offset
            $offset = $page.nextOffset
        }
        throw 'Fixture did not finish within its operation bound.'
    }

    # Capture the native command's console output as a caller in a separate process would.
    function Invoke-NativeWorkflowCommand {
        param([string[]] $Arguments, [string] $Namespace = 'workflow')
        $native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $start = [Diagnostics.ProcessStartInfo]::new($native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        foreach ($argument in @($Namespace) + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $output.GetAwaiter().GetResult()
                errors = $errors.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }

    function Invoke-WorkerRead {
        param([string] $AssignmentPath, [string] $DocumentId, [int] $Offset)
        Invoke-NativeWorkflowCommand -Arguments @(
            'worker-document', '--assignment-path', $AssignmentPath, '--document-id', $DocumentId, '--offset', "$Offset"
        )
    }

    # Creates an assignment through the same file-based native interface used by workers.
    function New-TestWorkerAssignment {
        param(
            [string] $Phase, [string] $SessionState, [string] $ScopePath, [string] $OutputDirectory,
            [string] $ManifestPath, [string] $ReviewerId, [switch] $Resume,
            [string] $SkillRoot = $script:SkillRoot
        )
        $arguments = @(
            'worker-assignment', '--phase', $Phase, '--session-state', $SessionState,
            '--scope-path', $ScopePath, '--output-directory', $OutputDirectory, '--skill-root', $SkillRoot
        )
        if ($ManifestPath) { $arguments += @('--manifest-path', $ManifestPath) }
        if ($ReviewerId) { $arguments += @('--reviewer-id', $ReviewerId) }
        if ($Resume) { $arguments += '--resume' }
        $result = Invoke-NativeWorkflowCommand $arguments
        if ($result.exitCode -ne 0) { throw $result.errors }
        ConvertFrom-Json -InputObject $result.output -AsHashtable
    }

    # Reads or records measurements through the native diagnostics command.
    function Invoke-TestInputDiagnostics {
        param([string] $Path, [string] $Record, [int] $Count = 1)
        $arguments = @('diagnostics', '--path', $Path)
        if ($Record) { $arguments += @('--record', $Record, '--count', "$Count") }
        $result = Invoke-NativeWorkflowCommand -Namespace context -Arguments $arguments
        if ($result.exitCode -ne 0) { throw $result.errors }
        ConvertFrom-Json -InputObject $result.output -AsHashtable
    }
}

Describe 'Prepared bounded worker inputs' {
    BeforeEach {
        $script:NativePageReads = 0
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
        [IO.File]::WriteAllText($inventoryPath, (ConvertTo-Json -InputObject $inventory -Depth 20 -Compress), [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($groupsPath, (ConvertTo-Json -InputObject $groups -Depth 20 -Compress), [Text.UTF8Encoding]::new($false))
        $work = Invoke-NativeWorkflowCommand @(
            'create-review-work', '--inventory-path', $inventoryPath, '--file-groups-path', $groupsPath,
            '--focus-area-directory', $areas, '--output-directory', (Join-Path $script:State 'review-work'),
            '--repository-root', $script:State, '--artifact-directory', (Join-Path $script:State 'findings')
        )
        $work.exitCode | Should -Be 0 -Because $work.errors
        $script:Work = ConvertFrom-Json -InputObject $work.output -AsHashtable
    }

    It 'supplies canonical recovery before initial reads and compaction reloads for <Phase> with Resume=<Resume>' -ForEach @(
        @{ Phase = 'references'; Resume = $false }
        @{ Phase = 'fileGroups'; Resume = $false }
        @{ Phase = 'focusAreas'; Resume = $false }
        @{ Phase = 'overview'; Resume = $false }
        @{ Phase = 'review'; Resume = $false }
        @{ Phase = 'review'; Resume = $true }
    ) {
        $parameters = @{
            Phase = $Phase; SessionState = $script:State; ScopePath = $script:Scope
            OutputDirectory = Join-Path $script:State 'attempt'
        }
        if ($Phase -eq 'review') {
            $parameters.ManifestPath = $script:Work.manifestPath
            $parameters.ReviewerId = 'focus-001'
            $parameters.Resume = $Resume
        }
        $result = New-TestWorkerAssignment @parameters
        $dispatch = $result.dispatchPrompt.Replace("`r`n", "`n")
        $recovery = [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'error-recovery.md')).
            Replace("`r`n", "`n").TrimEnd()
        $policyOffset = $dispatch.IndexOf($recovery, [StringComparison]::Ordinal)
        $policyOffset | Should -BeGreaterOrEqual 0 -Because 'recovery must not depend on a successful shared read'
        $policyOffset | Should -BeLessThan $dispatch.IndexOf('ss-ai workflow worker-document', [StringComparison]::Ordinal)
        $dispatch | Should -Match 'Apply this recovery policy before initial shared reads and compaction reloads'
        $dispatch | Should -Match 'Keep this policy and the reader command in compaction notes'
        $dispatch | Should -Not -Match 'On errors stop and report'
    }

    It 'provides bounded initial reads and exact complete guidance for original and resumed reviewers' {
        foreach ($resume in @($false, $true)) {
            $result = New-TestWorkerAssignment -Phase review `
                -SessionState $script:State -ScopePath $script:Scope -ManifestPath $script:Work.manifestPath `
                -ReviewerId focus-001 -Resume:$resume -OutputDirectory (Join-Path $script:State "attempt-$resume")
            $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
            $assignment.resume | Should -BeTrue -Because 'first dispatch retains the prepared artifact and journal'
            ($assignment.documents | Where-Object { $_.id -eq 'prompt' }).sizeBytes | Should -BeGreaterThan 20KB
            $result.dispatchPrompt | Should -Match 'Do not use the default file viewer'
            $result.dispatchPrompt | Should -Match "O''Brien"
            $assignment.documents.id | Should -Not -Contain 'pr-context'
            Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id overview | Should -BeExactly $script:LongText
            Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id focusArea | Should -BeExactly $script:LongText
            $prompt = Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id prompt
            $prompt | Should -Not -Match '\{\{[^}]+\}\}'
            $prompt | Should -Match ([regex]::Escape($script:Work.inventoryPath))
            $prompt | Should -Match 'Primary orchestration, publication, and every human decision gate'
            $prompt | Should -Match 'Do not invoke the skill'
            $prompt | Should -Match 'Use `--resume` exactly'
            $prompt | Should -Match 'Use the default context tier supplied by the orchestrator'
            $prompt | Should -Not -Match 'long-context configuration'
            $data = Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id workflowData
            $expectedData = @(
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'workflow-data.md'))
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'data-guides\review-coverage.md'))
            ) -join "`n`n"
            # The shared atomic writer uses LF on every host; no other source text may change.
            $data | Should -BeExactly $expectedData.Replace("`r`n", "`n")
            $data | Should -Not -Match 'Comment authorship|Reference expansion|Finding plans|Audit records|Focus-area maps'
            $manifest = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Work.manifestPath)) -AsHashtable
            $reviewer = $manifest.reviewers[0]
            $artifactResult = Invoke-NativeWorkflowCommand @(
                'initialize-finding', '--output-directory', (Join-Path $script:State 'findings'),
                '--reviewer-id', 'focus-001', '--focus-area', $reviewer.focusArea,
                '--queue-path', $reviewer.fileGroupQueuePath, '--resume'
            )
            $artifactResult.exitCode | Should -Be 0 -Because $artifactResult.errors
            $artifact = ConvertFrom-Json -InputObject $artifactResult.output -AsHashtable
            $artifact.resumedExisting | Should -BeTrue
            $journalBefore = [IO.File]::ReadAllText($reviewer.fileGroupQueuePath)
            $queueResult = Invoke-NativeWorkflowCommand @(
                'initialize-review-queue', '--inventory-path', $script:Work.inventoryPath,
                '--file-groups-path', $script:Work.fileGroupsPath,
                '--queue-path', $reviewer.fileGroupQueuePath, '--resume'
            )
            $queueResult.exitCode | Should -Be 0 -Because $queueResult.errors
            $queue = ConvertFrom-Json -InputObject $queueResult.output -AsHashtable
            $queue.queuePath | Should -BeExactly $reviewer.fileGroupQueuePath
            [IO.File]::ReadAllText($reviewer.fileGroupQueuePath) | Should -BeExactly $journalBefore
            $readBlock = [regex]::Match($result.dispatchPrompt, '(?s)```powershell\n(.*?)\n```').Groups[1].Value
            $readBlock | Should -Not -BeNullOrEmpty
            $readCommand = ($readBlock -split "`n" | Where-Object { $_.StartsWith('ss-ai workflow worker-document ') } | Select-Object -First 1)
            $quotedPath = $result.assignmentPath.Replace("'", "''")
            $readCommand | Should -BeExactly "ss-ai workflow worker-document --assignment-path '$quotedPath' --document-id '{{DOCUMENT_ID}}' --offset 0"
            $readBlock | Should -Match ([regex]::Escape("if (`$PSVersionTable.PSVersion -lt [version]'7.6')"))
            $readBlock | Should -Match ([regex]::Escape('Set-StrictMode -Version Latest'))
            $readBlock | Should -Match ([regex]::Escape("`$ErrorActionPreference = 'Stop'"))
            $readBlock | Should -Match ([regex]::Escape('$PSNativeCommandUseErrorActionPreference = $true'))
            $tokens = $null
            $parseErrors = $null
            $null = [System.Management.Automation.Language.Parser]::ParseInput($readBlock, [ref]$tokens, [ref]$parseErrors)
            @($parseErrors).Count | Should -Be 0
            $ast = [System.Management.Automation.Language.Parser]::ParseInput($readCommand, [ref]$tokens, [ref]$parseErrors)
            $elements = @($ast.EndBlock.Statements[0].PipelineElements[0].CommandElements)
            $elements.Count | Should -Be 9
            $elements[4].Value | Should -BeExactly $result.assignmentPath
            $elements[6].Value | Should -BeExactly '{{DOCUMENT_ID}}'
            $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
            $start.UseShellExecute = $false
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            foreach ($argument in @('-NoProfile', '-NonInteractive', '-Command', $readBlock.Replace('{{DOCUMENT_ID}}', 'overview'))) {
                $start.ArgumentList.Add($argument)
            }
            $process = [Diagnostics.Process]::Start($start)
            try {
                $output = $process.StandardOutput.ReadToEndAsync()
                $errors = $process.StandardError.ReadToEndAsync()
                $process.WaitForExit()
                $process.ExitCode | Should -Be 0 -Because $errors.GetAwaiter().GetResult()
                $first = $output.GetAwaiter().GetResult() | ConvertFrom-Json -AsHashtable
            } finally { $process.Dispose() }
            $first.contentOffset | Should -Be 0
            $first.complete | Should -BeFalse
        }
        $counts = (Invoke-TestInputDiagnostics -Path $script:State).counts
        $counts.firstReadFailures | Should -Be 0
        $counts.setupCalls | Should -Be 2
        $counts.documentReadCalls | Should -Be ($script:NativePageReads + 2)
    }

    It 'stops dependent commands when the copied reader fails' {
        $result = New-TestWorkerAssignment -Phase references `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'failure')
        $readBlock = [regex]::Match($result.dispatchPrompt, '(?s)```powershell\n(.*?)\n```').Groups[1].Value
        $command = $readBlock.Replace('{{DOCUMENT_ID}}', 'missing') + "`nWrite-Output 'DEPENDENT COMMAND RAN'"
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-Command', $command)) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $process.ExitCode | Should -Not -Be 0
            $output.GetAwaiter().GetResult() | Should -Not -Match 'DEPENDENT COMMAND RAN'
            $errors.GetAwaiter().GetResult() | Should -Not -BeNullOrEmpty
        } finally { $process.Dispose() }
    }

    It 'reads Unicode long lines with UTF-16 offsets through the prepared command' {
        $symbol = [char]::ConvertFromUtf32(0x1f642)
        $text = ('a' * 16000) + $symbol + ('quoted "\' * 3000) + "`n" + ($symbol * 200)
        [IO.File]::WriteAllText($script:Scope, $text, [Text.UTF8Encoding]::new($false))
        $result = New-TestWorkerAssignment -Phase fileGroups `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'unicode')
        $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
        $reconstructed = [Text.StringBuilder]::new()
        $offset = 0
        for ($attempt = 0; $attempt -lt 100; $attempt++) {
            $read = Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId scope -Offset $offset
            $read.exitCode | Should -Be 0 -Because $read.errors
            [Text.Encoding]::UTF8.GetByteCount($read.output) | Should -BeLessOrEqual 16384
            Test-Json -Json $read.output -Schema $script:PageSchema -ErrorAction Stop | Should -BeTrue
            $page = ConvertFrom-Json -InputObject $read.output -AsHashtable
            $page.contentOffset | Should -Be $offset
            $page.contentLength | Should -Be $text.Length
            $null = $reconstructed.Append($page.content)
            if ($page.complete) { break }
            $page.nextOffset | Should -BeGreaterThan $offset
            [char]::IsLowSurrogate($text[$page.nextOffset]) | Should -BeFalse
            $offset = $page.nextOffset
        }
        $page.complete | Should -BeTrue
        $reconstructed.ToString() | Should -BeExactly $text
        Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id scope | Should -BeExactly $text
        $lowSurrogateOffset = $text.IndexOf($symbol, [StringComparison]::Ordinal) + 1
        $invalid = Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId scope -Offset $lowSurrogateOffset
        $invalid.exitCode | Should -Not -Be 0
        $invalid.output | Should -BeNullOrEmpty
    }

    It 'loads only <Phase> preparation contracts without granting orchestration or human authority' -ForEach @(
        @{ Phase = 'fileGroups'; Guide = 'data-guides\file-groups.md' }
        @{ Phase = 'focusAreas'; Guide = 'data-guides\focus-areas.md' }
        @{ Phase = 'references'; Guide = $null }
        @{ Phase = 'overview'; Guide = $null }
    ) {
        $result = New-TestWorkerAssignment -Phase $Phase `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State $Phase)
        $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
        $result.dispatchPrompt | Should -Not -Match 'assigned queryRecipe'
        $assignment.documents.id | Should -Not -Contain queryRecipe
        $prompt = Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id prompt
        $prompt | Should -Match 'authorized bounded phase work'
        $prompt | Should -Match 'not invocation or delegation of ss-review itself'
        $prompt | Should -Match 'do not ask or answer it yourself'
        $prompt | Should -Match 'Repository instruction authorization: not authorized'
        $prompt | Should -Match 'Pre-existing findings authorized: False'
        if ($null -eq $Guide) {
            $assignment.documents.id | Should -Not -Contain 'workflowData'
            [IO.File]::Exists((Join-Path $script:State "$Phase\workflow-data.md")) | Should -BeFalse
            $extra = $assignment.documents[0].Clone()
            $extra.id = 'workflowData'
            $assignment.documents += $extra
        } else {
            $data = Read-AllSharedPages -AssignmentPath $result.assignmentPath -Id workflowData
            $expectedData = @(
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'workflow-data.md'))
                [IO.File]::ReadAllText((Join-Path $script:SkillRoot $Guide))
            ) -join "`n`n"
            $data | Should -BeExactly $expectedData.Replace("`r`n", "`n")
            $assignment.documents = @($assignment.documents | Where-Object { $_.id -ne 'workflowData' })
        }
        [IO.File]::WriteAllText($result.assignmentPath, (ConvertTo-Json -InputObject $assignment -Depth 30))
        $invalid = Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId scope -Offset 0
        $invalid.exitCode | Should -Not -Be 0
        $invalid.output | Should -BeNullOrEmpty
    }

    It 'rejects changed <_> bytes and measures the first-read failure' -ForEach @('scope', 'workflowData') {
        $documentId = $_
        $result = New-TestWorkerAssignment -Phase fileGroups `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'attempt')
        $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
        $document = $assignment.documents | Where-Object { $_.id -eq $documentId }
        [IO.File]::AppendAllText($document.path, ' changed guidance')
        $read = Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId $documentId -Offset 0
        $read.exitCode | Should -Not -Be 0
        $read.errors | Should -Match 'changed'
        $read.output | Should -BeNullOrEmpty
        $counts = (Invoke-TestInputDiagnostics -Path $script:State).counts
        $counts.firstReadFailures | Should -Be 1
    }

    It 'rejects an empty <_> instead of publishing incomplete worker guidance' -ForEach @(
        'workflow-data.md', 'data-guides\file-groups.md', 'error-recovery.md'
    ) {
        $emptyGuide = $_
        # Exercise the executable's real reads without modifying installed or shared source files.
        $isolatedSkill = Join-Path $script:State 'skill'
        $null = New-Item -ItemType Directory -Path $isolatedSkill
        foreach ($relativePath in @('data-guides', 'workflow-data.md', 'prepare-file-groups.md', 'error-recovery.md')) {
            Copy-Item -LiteralPath (Join-Path $script:SkillRoot $relativePath) -Destination $isolatedSkill -Recurse
        }
        [IO.File]::WriteAllText((Join-Path $isolatedSkill $emptyGuide), '')
        $output = Join-Path $script:State 'empty-guide'
        $expectedError = if ($emptyGuide -eq 'error-recovery.md') { '*recovery policy*empty*' } else { '*data guide*empty*' }
        {
            New-TestWorkerAssignment -Phase fileGroups -SkillRoot $isolatedSkill `
                -SessionState $script:State -ScopePath $script:Scope -OutputDirectory $output
        } | Should -Throw $expectedError
        [IO.File]::Exists((Join-Path $output 'assignment.json')) | Should -BeFalse
    }

    It 'preserves pages and exact counters for <_> document reads' -ForEach @('fileGroups', 'focusAreas') {
        $result = New-TestWorkerAssignment -Phase $_ `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'concurrent')
        $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
        $ids = @($assignment.documents.id) + @('missing')
        $reads = @(foreach ($id in $ids) {
            Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId $id -Offset 0
        })
        for ($index = 0; $index -lt $ids.Count; $index++) {
            $read = $reads[$index]
            if ($ids[$index] -eq 'missing') {
                $read.exitCode | Should -Not -Be 0
                $read.errors | Should -Match "(?s)Expected.*one shared document 'missing'"
                $read.output | Should -BeNullOrEmpty
            } else {
                $read.exitCode | Should -Be 0 -Because $read.errors
                $read.errors | Should -BeNullOrEmpty
                Test-Json -Json $read.output -Schema $script:PageSchema -ErrorAction Stop | Should -BeTrue
                $page = ConvertFrom-Json -InputObject $read.output -AsHashtable
                $document = $assignment.documents | Where-Object { $_.id -eq $ids[$index] }
                $expected = [IO.File]::ReadAllText($document.path)
                $page.contentOffset | Should -Be 0
                $page.contentLength | Should -Be $expected.Length
                $page.content | Should -BeExactly $expected.Substring(0, $page.content.Length)
                if ($page.complete) {
                    $page.nextOffset | Should -BeNullOrEmpty
                    $page.content | Should -BeExactly $expected
                } else {
                    $page.nextOffset | Should -Be $page.content.Length
                }
            }
        }
        $counts = (Invoke-TestInputDiagnostics -Path $assignment.diagnosticsPath).counts
        $counts.setupCalls | Should -Be 1
        $counts.documentReadCalls | Should -Be $reads.Count
        $counts.firstReadFailures | Should -Be 1
        $counts.repeatedSetupCalls | Should -Be 0
        $counts.malformedQueryFailures | Should -Be 0
        $counts.cursorRestarts | Should -Be 0
        [IO.File]::Exists("$($assignment.diagnosticsPath).lock") | Should -BeTrue
    }

    It 'preserves counters under controlled native reader contention' {
        $executable = $env:SS_AI_WORKER_INPUT_TEST_EXECUTABLE
        if ([string]::IsNullOrWhiteSpace($executable) -or -not [IO.File]::Exists($executable)) {
            throw 'SS_AI_WORKER_INPUT_TEST_EXECUTABLE must name the prepared native worker input test executable.'
        }
        # The native fixture holds the diagnostics lock and signals each observed conflict
        # before releasing contenders; the PowerShell wait mock cannot control native retries.
        $output = & $executable concurrent_reads_preserve_diagnostics --exact
        $LASTEXITCODE | Should -Be 0
        ($output -join "`n") | Should -Match '1 passed'
    }

    It 'rejects repeated setup without overwriting the assignment or resetting work' {
        $manifest = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Work.manifestPath)) -AsHashtable
        $queueBefore = [IO.File]::ReadAllText($manifest.reviewers[0].fileGroupQueuePath)
        $artifactBefore = [IO.File]::ReadAllText($manifest.reviewers[0].artifactPath)
        $parameters = @{
            Phase = 'review'; SessionState = $script:State; ScopePath = $script:Scope
            ManifestPath = $script:Work.manifestPath; ReviewerId = 'focus-001'
            OutputDirectory = Join-Path $script:State 'attempt'
        }
        $result = New-TestWorkerAssignment @parameters
        $before = [IO.File]::ReadAllText($result.assignmentPath)
        { New-TestWorkerAssignment @parameters } | Should -Throw '*already exists*'
        [IO.File]::ReadAllText($result.assignmentPath) | Should -BeExactly $before
        $counts = (Invoke-TestInputDiagnostics -Path $script:State).counts
        $counts.repeatedSetupCalls | Should -Be 1
        [IO.File]::ReadAllText($manifest.reviewers[0].fileGroupQueuePath) | Should -BeExactly $queueBefore
        [IO.File]::ReadAllText($manifest.reviewers[0].artifactPath) | Should -BeExactly $artifactBefore
    }

    It 'does not dispatch assignments the manifest marks entirely excluded' {
        $manifest = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Work.manifestPath)) -AsHashtable
        $manifest.reviewers[0].dispatch = 'excluded'
        [IO.File]::WriteAllText($script:Work.manifestPath, (ConvertTo-Json -InputObject $manifest -Depth 30))
        {
            New-TestWorkerAssignment -Phase review `
                -SessionState $script:State -ScopePath $script:Scope -ManifestPath $script:Work.manifestPath `
                -ReviewerId focus-001 -OutputDirectory (Join-Path $script:State 'excluded-attempt')
        } | Should -Throw '*entirely excluded*'
    }

    It 'does not permit canonical JSON as a shared read even through a modified manifest' {
        $result = New-TestWorkerAssignment -Phase references `
            -SessionState $script:State -ScopePath $script:Scope -OutputDirectory (Join-Path $script:State 'canonical')
        $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($result.assignmentPath)) -AsHashtable
        ($assignment.documents | Where-Object { $_.id -eq 'scope' }).path = $script:Work.inventoryPath
        [IO.File]::WriteAllText($result.assignmentPath, (ConvertTo-Json -InputObject $assignment -Depth 30))
        $read = Invoke-WorkerRead -AssignmentPath $result.assignmentPath -DocumentId scope -Offset 0
        $read.exitCode | Should -Not -Be 0
        $read.output | Should -BeNullOrEmpty
    }

    It 'records measured baseline totals separately without coercing absent data to zero' {
        $baseline = Join-Path $script:State 'baseline-diagnostics.json'
        { Invoke-TestInputDiagnostics -Path $baseline } | Should -Throw
        # Ref: issue #127's combined shared-document first-read failures.
        $data = Invoke-TestInputDiagnostics -Path $baseline -Record firstReadFailures -Count 69
        $data.counts.firstReadFailures | Should -Be 69
        $data.counts.cursorRestarts | Should -Be 0
    }
}

Describe 'Diagnostic lock failure boundaries' {
    BeforeEach {
        $script:DiagnosticsPath = Join-Path $TestDrive "$([guid]::NewGuid()).json"
    }

    It 'propagates unavailable lock paths without retries' {
        $null = [IO.Directory]::CreateDirectory("$script:DiagnosticsPath.lock")
        $result = Invoke-NativeWorkflowCommand -Namespace context -Arguments @(
            'diagnostics', '--path', $script:DiagnosticsPath, '--record', 'documentReadCalls', '--count', '1'
        )
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        [IO.File]::Exists($script:DiagnosticsPath) | Should -BeFalse
    }

    It 'rejects malformed diagnostics and releases ownership without retrying' {
        [IO.File]::WriteAllText($script:DiagnosticsPath, '{}')
        $result = Invoke-NativeWorkflowCommand -Namespace context -Arguments @(
            'diagnostics', '--path', $script:DiagnosticsPath, '--record', 'documentReadCalls', '--count', '1'
        )
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        [IO.File]::ReadAllText($script:DiagnosticsPath) | Should -BeExactly '{}'
        $lock = [IO.File]::Open("$script:DiagnosticsPath.lock", 'Open', 'ReadWrite', 'None')
        try { $lock | Should -Not -BeNullOrEmpty } finally { $lock.Dispose() }
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
        [IO.File]::WriteAllText($script:OverviewPath, $script:Overview + "`nUnicode: café 雪", [Text.UTF8Encoding]::new($true))
        $result = Invoke-NativeWorkflowCommand -Arguments @('validate-overview', '--path', $script:OverviewPath)
        $result.exitCode | Should -Be 0 -Because $result.errors
        (ConvertFrom-Json -InputObject $result.output).isValid | Should -BeTrue
    }

    It 'rejects duplicate headings, fenced substitutes, and empty sections' {
        $invalidOverviews = @(
            @{ text = $script:Overview + "`n## Mission`nextra"; error = $null }
            @{ text = $script:Overview.Replace('## Mission', "``````text`n## Mission`n``````"); error = $null }
            @{ text = $script:Overview.Replace("## Mission`n`nNot applicable for this fixture.", '## Mission'); error = 'empty' }
        )
        foreach ($overview in $invalidOverviews) {
            [IO.File]::WriteAllText($script:OverviewPath, $overview.text, [Text.UTF8Encoding]::new($false))
            $result = Invoke-NativeWorkflowCommand -Arguments @('validate-overview', '--path', $script:OverviewPath)
            $result.exitCode | Should -Not -Be 0
            $result.output | Should -BeNullOrEmpty
            $result.errors | Should -Not -BeNullOrEmpty
            if ($overview.error) { $result.errors | Should -Match $overview.error }
        }
    }
}
