BeforeAll {
    $script:SkillScripts = Join-Path $PSScriptRoot '..\skills\ss-review\scripts'
    . (Join-Path $script:SkillScripts 'lib\JsonValue.ps1')
    . (Join-Path $script:SkillScripts 'lib\PullRequestModel.ps1')
    . (Join-Path $script:SkillScripts 'lib\PullRequestContextQuery.ps1')
    . (Join-Path $script:SkillScripts 'lib\WorkflowData.ps1')
    . (Join-Path $script:SkillScripts 'lib\ReviewInput.ps1')
    . (Join-Path $script:SkillScripts 'lib\PullRequestQueryRequest.ps1')

    function New-QueryComment {
        param([string] $Id, [string] $Content = 'ordinary comment', [string] $Authorship = 'indeterminate')
        [ordered]@{
            id = $Id; content = $Content; url = "https://example.invalid/comment/$Id"
            parentId = $null; author = @{ id = 'person'; displayName = 'Reviewer' }
            authorship = $Authorship; authorshipEvidence = @(); isAuthoredBySelf = $false
            publishedUtc = '2026-10-01T12:00:00Z'; lastEditedUtc = $null
        }
    }

    function New-QueryThread {
        param(
            [string] $Id, [string] $Path, [string] $Resolution = 'active',
            [string] $Kind = 'text', [object[]] $Comments, [switch] $Outdated
        )
        [ordered]@{
            id = $Id; url = "https://example.invalid/thread/$Id"; kind = $Kind
            openingCommentId = if ($Comments.Count) { $Comments[0]['id'] } else { $null }
            resolution = $Resolution; isOutdated = [bool] $Outdated; isAuthoredBySelf = $true
            context = if ($Path) { @{ filePath = $Path; side = 'right'; startLine = 4; endLine = 6 } } else { $null }
            decision = $null; eventUtc = '2026-10-01T12:00:00Z'; lastUpdatedUtc = $null
            comments = $Comments
            properties = @{ native = 'Not projected into reading responses' }
        }
    }

    function New-QueryDocument {
        [ordered]@{
            schemaVersion = $script:PullRequestContextSchemaVersion
            platform = 'gitHub'; pullRequestUrl = 'https://example.invalid/pr/1'
            repositoryUrl = 'https://example.invalid/repo'; fetchedUtc = '2026-10-01T12:00:00Z'
            target = @{ platform = 'gitHub'; pullRequestId = 1 }; authenticatedIdentity = @{ id = 'person' }
            pullRequest = [ordered]@{
                id = 1; title = 'Test change'; description = 'Author position on Widget design'
                reviewedSourceCommit = 'source'; reviewedMergeBaseCommit = 'base'
            }
            reviewers = @(@{ id = 'first' }, @{ id = 'second' })
            revisions = @(@{ id = 1 }, @{ id = 2 })
            threads = @(
                New-QueryThread -Id 'T' -Path 'src/a.rs' -Resolution resolved -Outdated -Comments @(
                    (New-QueryComment -Id 'Root' -Content 'Widget concern')
                    (New-QueryComment -Id 'Reply' -Content 'User chooses the current Widget behavior' -Authorship human)
                )
                New-QueryThread -Id 't' -Path '/src/b.rs' -Comments @(
                    (New-QueryComment -Id 'root' -Content 'Related Widget concern' -Authorship agent)
                )
                New-QueryThread -Id 'summary' -Kind reviewSummary -Resolution indeterminate -Comments @(
                    (New-QueryComment -Id 'summary-body' -Content 'Global design decision')
                )
                New-QueryThread -Id 'global' -Comments @(
                    (New-QueryComment -Id 'global-body' -Content 'PR-wide author clarification')
                )
            )
        }
    }

    function Read-QueryPage {
        param($Document, $Options, $Previous = $null, [string] $Cursor)
        $json = Read-PullRequestContextQuery -Document $Document -Options $Options -Previous $Previous -Cursor $Cursor
        [Text.Encoding]::UTF8.GetByteCount($json + [Environment]::NewLine) | Should -BeLessOrEqual 16384
        $json | Should -BeOfType ([string])
        ConvertFrom-JsonVerbatim -Text $json
    }

    function Read-QueryPages {
        param($Document, $Options, $Previous = $null)
        $cursor = ''
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        # Bounded fixtures need only a few pages; the operation bound also catches pagination mutations.
        for ($attempt = 0; $attempt -lt 100; $attempt++) {
            $page = Read-QueryPage -Document $Document -Options $Options -Previous $Previous -Cursor $cursor
            $page
            if ($page['complete']) {
                $page['nextCursor'] | Should -BeNullOrEmpty
                return
            }
            $page['items'].Count | Should -BeGreaterThan 0
            $cursor = $page['nextCursor']
            $cursor | Should -Not -BeNullOrEmpty
            $seen.Add($cursor) | Should -BeTrue -Because 'each continuation must advance'
        }
        throw 'Query exceeded the bounded fixture page count.'
    }

    function Invoke-QueryExecutable {
        param([string[]] $Arguments, [string] $ScriptName = 'Read-PullRequestContext.ps1')
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Command pwsh).Source)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.Encoding]::UTF8
        $start.StandardErrorEncoding = [Text.Encoding]::UTF8
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', (Join-Path $script:SkillScripts $ScriptName)) + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errorOutput = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{ output = $output.GetAwaiter().GetResult(); error = $errorOutput.GetAwaiter().GetResult(); exitCode = $process.ExitCode }
        } finally { $process.Dispose() }
    }
}

Describe 'Saved PR query requests' {
    BeforeEach {
        $script:Document = New-QueryDocument
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $null = New-Item -ItemType Directory -Path $root
        $script:Capture = Join-Path $root "capture space $([char]0xe9).json"
        $script:Request = Join-Path $root 'request.json'
        $script:Diagnostics = Join-Path $root 'input-diagnostics.json'
        Write-PullRequestOutputFileAtomically -Path $script:Capture -Content (ConvertTo-JsonText -Value $script:Document)
    }

    It 'preserves native arguments with backslashes, spaces, quotes, Unicode and multiline text' {
        $text = "C:\space path\quoted `"value`" O'Brien $([char]0xe9)`nnext line"
        $script:Document.threads[0].comments[0].content = $text
        Write-PullRequestOutputFileAtomically -Path $script:Capture -Content (ConvertTo-JsonText -Value $script:Document)
        $textPath = Join-Path $TestDrive 'literal search.txt'
        [IO.File]::WriteAllText($textPath, $text)
        $created = Invoke-QueryExecutable -ScriptName New-PullRequestQueryRequest.ps1 -Arguments @(
            '-Path', $script:Capture, '-RequestPath', $script:Request, '-DiagnosticsPath', $script:Diagnostics,
            '-Query', 'search', '-TextPath', $textPath
        )
        $created.exitCode | Should -Be 0 -Because $created.error
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $request.options.text | Should -BeExactly $text
        $page = Invoke-QueryExecutable -Arguments @('-RequestPath', $script:Request)
        $page.exitCode | Should -Be 0 -Because $page.error
        $result = ConvertFrom-JsonVerbatim -Text $page.output
        $result.items.Count | Should -Be 1
        $result.items[0].commentId | Should -BeExactly Root
    }

    It 'binds typed file arrays through a noninteractive PowerShell script block' {
        $path = "src\space path\quote`"$([char]0xe9).rs"
        $script:Document.threads[0].context.filePath = $path.Replace('\', '/')
        Write-PullRequestOutputFileAtomically -Path $script:Capture -Content (ConvertTo-JsonText -Value $script:Document)
        $output = & pwsh -NoProfile -NonInteractive -Command {
            param($scriptPath, $capture, $request, $diagnostics, $file)
            & $scriptPath -Path $capture -RequestPath $request -DiagnosticsPath $diagnostics `
                -Query threads -Files @($file, 'src\b.rs')
        } -args (Join-Path $script:SkillScripts 'New-PullRequestQueryRequest.ps1'), $script:Capture, $script:Request, $script:Diagnostics, $path
        $LASTEXITCODE | Should -Be 0
        $output | Should -Not -BeNullOrEmpty
        $saved = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $saved.options.files.Count | Should -Be 2
        $saved.options.files[0] | Should -BeExactly $path.Replace('\', '/')
        $page = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $script:Request)
        $page.totalCount | Should -Be 2
    }

    It 'runs the canonical PowerShell example from Bash with failure=<Fail>' -ForEach @(
        @{ Fail = $false }
        @{ Fail = $true }
    ) -Skip:(-not (Get-Command bash -CommandType Application -ErrorAction SilentlyContinue)) {
        # Exercise both shell parsers with characters that have distinct meanings in each shell.
        $root = Join-Path (Split-Path -Parent $script:Capture) "Bash space O'Brien `$literal ``tick $([char]0xe9)"
        $null = New-Item -ItemType Directory -Path $root
        $capture = Join-Path $root 'capture.json'
        $request = Join-Path $root 'request.json'
        $diagnostics = Join-Path $root 'input-diagnostics.json'
        $path = "src\space O'Brien `$literal ``tick $([char]0xe9).rs"
        $script:Document.threads[0].context.filePath = $path.Replace('\', '/')
        Write-PullRequestOutputFileAtomically -Path $capture -Content (ConvertTo-JsonText -Value $script:Document)
        $recipe = [IO.File]::ReadAllText((Join-Path $script:SkillScripts '..\pwsh-calls\query-pr-context.md'))
        $example = @([regex]::Matches($recipe, '(?s)```powershell\r?\n(?<command>.*?)\r?\n```') |
            Where-Object { $_.Groups['command'].Value.Contains("-Files @(") })
        $example.Count | Should -Be 1 -Because 'shells share one typed-array query program'
        $program = $example[0].Groups['command'].Value.Replace('{{SKILL_ROOT}}', (Split-Path -Parent $script:SkillScripts).Replace("'", "''")).
            Replace('{{CONTEXT_PATH}}', $capture.Replace("'", "''")).
            Replace('{{REQUEST_PATH}}', $request.Replace("'", "''")).
            Replace('{{DIAGNOSTICS_PATH}}', $diagnostics.Replace("'", "''")).
            Replace('src\a.rs', $path.Replace("'", "''"))
        if ($Fail) {
            $program = $program.Replace('-Query threads', '-Query not-a-query')
        }
        $programPath = Join-Path $root 'query.ps1'
        [IO.File]::WriteAllText($programPath, $program)
        $example = [regex]::Match($recipe, '(?s)```bash\r?\n(?<command>.*?)\r?\n```')
        $example.Success | Should -BeTrue
        # Bash represents an embedded apostrophe by ending, quoting the apostrophe, and reopening the literal.
        $bashApostrophe = "'" + '"' + "'" + '"' + "'"
        $command = $example.Groups['command'].Value.Replace("`r`n", "`n")
        $command = $command.Replace('{{PROGRAM_PATH}}', $programPath.Replace("'", $bashApostrophe))
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Command bash -CommandType Application | Select-Object -First 1).Source)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.Encoding]::UTF8
        $start.StandardErrorEncoding = [Text.Encoding]::UTF8
        foreach ($argument in @('--noprofile', '--norc', '-c', $command)) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errorOutput = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $errors = $errorOutput.GetAwaiter().GetResult()
            $text = $output.GetAwaiter().GetResult()
            if ($Fail) {
                $process.ExitCode | Should -Not -Be 0
                $errors | Should -Match 'Query preparation failed: Select metadata, threads, comments, search or changes\.'
                $text | Should -BeNullOrEmpty
                [IO.File]::Exists($request) | Should -BeFalse
            } else {
                $process.ExitCode | Should -Be 0 -Because $errors
                $errors | Should -BeNullOrEmpty
                (ConvertFrom-JsonVerbatim -Text $text).requestPath | Should -BeExactly $request
            }
        } finally { $process.Dispose() }
        if ($Fail) { return }
        $saved = Read-WorkflowData -Kind queryRequest -Path $request
        @($saved.options.files) | Should -BeExactly @($path.Replace('\', '/'), 'src/b.rs')
        $page = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $request)
        $page.totalCount | Should -Be 2
        @($page.items.threadId) | Should -BeExactly @('T', 't')
    }

    It 'replays the delivered page and resumes without reconstructing selectors' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*first page*'
        $first = Read-SavedPullRequestQuery -RequestPath $script:Request
        Read-SavedPullRequestQuery -RequestPath $script:Request | Should -BeExactly $first
        $secondJson = Read-SavedPullRequestQuery -RequestPath $script:Request -Next
        Read-SavedPullRequestQuery -RequestPath $script:Request | Should -BeExactly $secondJson
        $second = ConvertFrom-JsonVerbatim -Text $secondJson
        $second.items[0].commentId | Should -BeExactly Reply
        $second.complete | Should -BeTrue
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*complete*'
        (Read-WorkflowData -Kind queryRequest -Path $script:Request).options.pageSize | Should -Be 1
    }

    It 'rejects changed selectors rather than combining them with an old cursor' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $changed = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $changed.options.threadId = 't'
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $changed)
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*modified*'
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Restart } | Should -Throw '*modified*'
    }

    It 'rejects changed snapshots even on replay and explicitly restarts from the first page' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $before = [IO.File]::ReadAllText($script:Request)
        $script:Document.threads[0].comments[0].content = 'changed root'
        Write-PullRequestOutputFileAtomically -Path $script:Capture -Content (ConvertTo-JsonText -Value $script:Document)
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*capture changed*'
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*capture changed*'
        [IO.File]::ReadAllText($script:Request) | Should -BeExactly $before
        $page = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $script:Request -Restart)
        $page.items[0].content | Should -BeExactly 'changed root'
        (Read-WorkflowData -Kind inputDiagnostics -Path $script:Diagnostics).counts.cursorRestarts | Should -Be 1
    }

    It 'binds the comparison snapshot as well as the current capture' {
        $baseline = Join-Path $TestDrive 'baseline.json'
        Write-PullRequestOutputFileAtomically -Path $baseline -Content (ConvertTo-JsonText -Value $script:Document)
        $request = New-PullRequestQueryRequest -Path $script:Capture -ComparePath $baseline -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query changes)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $script:Document.threads[0].comments[0].content = 'older root'
        $script:Document.threads[0].comments[0].lastEditedUtc = '2026-09-01T12:00:00Z'
        Write-PullRequestOutputFileAtomically -Path $baseline -Content (ConvertTo-JsonText -Value $script:Document)
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*capture changed*'
        $page = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $script:Request -Restart)
        $page.items[0].change | Should -BeExactly commentsEdited
    }

    It 'invalidates saved reads when semantic authorship changes without rewriting captured evidence' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -Authorship indeterminate -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $first = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $script:Request)
        $first.snapshotId | Should -BeExactly $request.snapshotId
        $captureBefore = [IO.File]::ReadAllText($script:Capture)
        $null = Save-CommentAuthorship -Path $script:Capture -SnapshotId $first.snapshotId `
            -Classification @(@{ threadId = 'T'; commentId = 'Root'; kind = 'human' })
        [IO.File]::ReadAllText($script:Capture) | Should -BeExactly $captureBefore
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*capture changed*'
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*capture changed*'
        $restarted = ConvertFrom-JsonVerbatim -Text (Read-SavedPullRequestQuery -RequestPath $script:Request -Restart)
        $restarted.snapshotId | Should -Not -Be $first.snapshotId
        $restarted.items[0].commentId | Should -Not -BeExactly Root
    }

    It 'invalidates saved reads when authorship policy changes' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query metadata -Field title)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $policy = $script:CommentAuthorshipPolicyVersion
        try {
            $script:CommentAuthorshipPolicyVersion = "$policy-test"
            { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*capture changed*'
        } finally { $script:CommentAuthorshipPolicyVersion = $policy }
    }

    It 'records malformed requests and repeated setup without overwriting a valid request' {
        $arguments = @('-Path', $script:Capture, '-RequestPath', $script:Request, '-DiagnosticsPath', $script:Diagnostics, '-Query', 'metadata')
        $bad = Invoke-QueryExecutable -ScriptName New-PullRequestQueryRequest.ps1 -Arguments $arguments
        $bad.exitCode | Should -Not -Be 0
        Test-Path -LiteralPath $script:Request | Should -BeFalse
        $good = Invoke-QueryExecutable -ScriptName New-PullRequestQueryRequest.ps1 -Arguments ($arguments + @('-Field', 'title'))
        $good.exitCode | Should -Be 0 -Because $good.error
        $before = [IO.File]::ReadAllText($script:Request)
        $repeat = Invoke-QueryExecutable -ScriptName New-PullRequestQueryRequest.ps1 -Arguments ($arguments + @('-Field', 'description'))
        $repeat.exitCode | Should -Not -Be 0
        [IO.File]::ReadAllText($script:Request) | Should -BeExactly $before
        $counts = (Read-WorkflowData -Kind inputDiagnostics -Path $script:Diagnostics).counts
        $counts.setupCalls | Should -Be 3
        $counts.repeatedSetupCalls | Should -Be 1
        $counts.malformedQueryFailures | Should -Be 1
    }

    It 'rejects partial records, unknown fields and mixed saved/direct command arguments' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query metadata -Field title)
        $request['extra'] = 'not a selector'
        { Assert-WorkflowData -Kind queryRequest -Value $request } | Should -Throw
        $request.Remove('extra')
        $request.Remove('lastPage')
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw
        $mixed = Invoke-QueryExecutable -Arguments @('-RequestPath', $script:Request, '-Query', 'threads')
        $mixed.exitCode | Should -Not -Be 0
        $mixed.output | Should -BeNullOrEmpty
    }

    It 'keeps replay output bounded even if a saved page was edited' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query metadata -Field title)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $request.lastPage.items[0].content = 'x' * 20000
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $result = Invoke-QueryExecutable -Arguments @('-RequestPath', $script:Request)
        $result.exitCode | Should -Be 1
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match 'bounded output limit'
    }

    It 'rejects edited saved evidence before replay or continuation: <Mutation>' -ForEach @(
        @{ Mutation = 'content' }
        @{ Mutation = 'missing attribution' }
        @{ Mutation = 'extra property' }
        @{ Mutation = 'continuation' }
    ) {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        switch ($Mutation) {
            'content' { $request.lastPage.items[0].content = 'substituted evidence' }
            'missing attribution' { $request.lastPage.items[0].Remove('author') }
            'extra property' { $request.lastPage.items[0]['inventedEvidence'] = 'not in the capture' }
            'continuation' { $request.lastPage.nextCursor = $request.lastPage.nextCursor -replace ':1:0$', ':0:0' }
        }
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $before = [IO.File]::ReadAllText($script:Request)
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*does not match*'
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*does not match*'
        [IO.File]::ReadAllText($script:Request) | Should -BeExactly $before
    }

    It 'rejects a valid page substituted from another selection on the same capture' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query metadata -Field title)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $request.lastPage = ConvertFrom-JsonVerbatim -Text (Read-PullRequestContextQuery -Document $script:Document `
            -Options (New-PullRequestQueryOptions -Query metadata -Field description))
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $result = Invoke-QueryExecutable -Arguments @('-RequestPath', $script:Request)
        $result.exitCode | Should -Be 1
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match 'does not match'
    }

    It 'rejects a substituted continuation page and cursor from a different request' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $otherOptions = New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 2
        # A long root provides a continuation under both page sizes without changing the immutable snapshot.
        $script:Document.threads[0].comments[0].content = 'evidence ' * 3000
        Write-PullRequestOutputFileAtomically -Path $script:Capture -Content (ConvertTo-JsonText -Value $script:Document)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request -Restart
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $otherFirst = ConvertFrom-JsonVerbatim -Text (Read-PullRequestContextQuery -Document $script:Document -Options $otherOptions)
        $request.lastPageCursor = $otherFirst.nextCursor
        $request.lastPage = ConvertFrom-JsonVerbatim -Text (Read-PullRequestContextQuery -Document $script:Document `
            -Options $otherOptions -Cursor $otherFirst.nextCursor)
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        { Read-SavedPullRequestQuery -RequestPath $script:Request } | Should -Throw '*mismatched query cursor*'
        { Read-SavedPullRequestQuery -RequestPath $script:Request -Next } | Should -Throw '*mismatched query cursor*'
    }

    It 'requires matching page and starting-cursor presence' {
        $request = New-PullRequestQueryRequest -Path $script:Capture -DiagnosticsPath $script:Diagnostics `
            -Options (New-PullRequestQueryOptions -Query metadata -Field title)
        $request.lastPageCursor = ''
        { Assert-WorkflowData -Kind queryRequest -Value $request } | Should -Throw
        $request.lastPageCursor = $null
        Write-PullRequestOutputFileAtomically -Path $script:Request -Content (ConvertTo-JsonText -Value $request)
        $null = Read-SavedPullRequestQuery -RequestPath $script:Request
        $request = Read-WorkflowData -Kind queryRequest -Path $script:Request
        $request.lastPageCursor | Should -BeExactly ''
        $request.lastPageCursor = $null
        { Assert-WorkflowData -Kind queryRequest -Value $request } | Should -Throw
        $request.Remove('lastPageCursor')
        { Assert-WorkflowData -Kind queryRequest -Value $request } | Should -Throw
    }

    It 'registers PR evidence only as a query input and validates overview links against it' {
        $state = Split-Path -Parent $script:Capture
        $scopePath = Join-Path $state 'scope.md'
        [IO.File]::WriteAllText($scopePath, 'Review this fixture pull request.')
        $result = Invoke-QueryExecutable -ScriptName New-ReviewWorkerAssignment.ps1 -Arguments @(
            '-Phase', 'fileGroups', '-SessionState', $state, '-ScopePath', $scopePath,
            '-OutputDirectory', (Join-Path $state 'assignment'), '-ContextPath', $script:Capture
        )
        $result.exitCode | Should -Be 0 -Because $result.error
        $prepared = ConvertFrom-JsonVerbatim -Text $result.output
        $prepared.dispatchPrompt | Should -Match 'Calling shell contract in the assigned queryRecipe'
        $prepared.dispatchPrompt | Should -Match 'including for any permitted nested worker'
        $assignmentPath = $prepared.assignmentPath
        $assignment = Read-WorkflowData -Kind workerAssignment -Path $assignmentPath
        $assignment.contextPath | Should -BeExactly $script:Capture
        $assignment.documents.id | Should -Contain queryRecipe
        ($assignment.documents | Where-Object id -EQ queryRecipe).path |
            Should -BeExactly ([IO.Path]::GetFullPath((Join-Path $script:SkillScripts '..\pwsh-calls\query-pr-context.md')))
        $assignment.documents.path | Should -Not -Contain $script:Capture
        $prompt = [IO.File]::ReadAllText((Join-Path $script:SkillScripts '..\prepare-content-overview.md'))
        $overview = [regex]::Match($prompt, '(?s)```text\r?\n(?<overview>.*?)\r?\n```').Groups['overview'].Value
        $overview = $overview.Replace('# Content overview', "# Content overview`n`n**Pull request:** $($script:Document.pullRequestUrl)")
        $overviewPath = Join-Path $state 'overview.md'
        [IO.File]::WriteAllText($overviewPath, $overview)
        $valid = Invoke-QueryExecutable -ScriptName Test-ReviewOverview.ps1 -Arguments @('-Path', $overviewPath, '-ContextPath', $script:Capture)
        $valid.exitCode | Should -Be 0 -Because $valid.error
        [IO.File]::WriteAllText($overviewPath, $overview.Replace($script:Document.pullRequestUrl, 'https://example.invalid/wrong'))
        $invalid = Invoke-QueryExecutable -ScriptName Test-ReviewOverview.ps1 -Arguments @('-Path', $overviewPath, '-ContextPath', $script:Capture)
        $invalid.exitCode | Should -Not -Be 0
        $invalid.output | Should -BeNullOrEmpty
    }
}

Describe 'Selective PR context retrieval' {
    BeforeEach { $script:Document = New-QueryDocument }

    It 'selects only one metadata field' {
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query metadata -Field description)
        $page.items.Count | Should -Be 1
        $page.items[0].field | Should -BeExactly 'description'
        $page.items[0].content | Should -BeExactly $script:Document.pullRequest.description
        $page.items[0].contentComplete | Should -BeTrue
        $page.items[0].Contains('threads') | Should -BeFalse
        $page.items[0].Contains('reviewers') | Should -BeFalse
    }

    It 'paginates metadata collections rather than returning a nested unbounded list' -ForEach @('reviewers', 'revisions') {
        $pages = @(Read-QueryPages $script:Document (New-PullRequestQueryOptions -Query metadata -Field $_ -PageSize 1))
        $pages.Count | Should -Be 2
        $pages[0].items[0].value.id | Should -Not -Be $pages[1].items[0].value.id
    }

    It 'preserves empty strings, null metadata and timestamp-shaped text' {
        foreach ($text in @('', '2026-10-01T12:00:00Z')) {
            $script:Document.pullRequest.description = $text
            $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query metadata -Field description)
            $page.items[0].content | Should -BeExactly $text
            $page.items[0].content | Should -BeOfType ([string])
        }
        $script:Document.pullRequest.description = $null
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query metadata -Field description)
        $page.items[0].Contains('value') | Should -BeTrue
        $page.items[0].value | Should -BeNullOrEmpty
    }

    It 'matches exact repository paths independent of separators and provider leading slash' {
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Files @('src\a.rs', 'src\b.rs'))
        @($page.items.threadId) | Should -BeExactly @('T', 't')
        $page.items[0].resolution | Should -BeExactly 'resolved'
        $page.items[0].isOutdated | Should -BeTrue
        $empty = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Files @('src/A.rs'))
        $empty.totalCount | Should -Be 0
        $empty.complete | Should -BeTrue
    }

    It 'discovers unanchored discussion and review summaries' {
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Scope global)
        @($page.items.threadId) | Should -BeExactly @('summary', 'global')
        $filtered = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Scope global -Kind reviewSummary -Resolution indeterminate)
        $filtered.items[0].threadId | Should -BeExactly 'summary'
        $filtered.items.Count | Should -Be 1
    }

    It 'treats repository paths as identifiers rather than restricting them to host filename syntax' {
        $script:Document.threads[0].context.filePath = 'src/time:stamp.rs'
        $selected = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Files @('src/time:stamp.rs'))
        $selected.items[0].threadId | Should -BeExactly 'T'
        $other = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Files @('src/b.rs'))
        $other.items.Count | Should -Be 1
        $other.items[0].threadId | Should -BeExactly 't'
    }

    It 'uses the structural opening comment, not the first chronological comment, for discovery' {
        $script:Document.threads[0].openingCommentId = 'Reply'
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query threads -Files @('src/a.rs'))
        $page.items[0].openingCommentId | Should -BeExactly 'Reply'
        $page.items[0].excerpt | Should -BeExactly 'User chooses the current Widget behavior'
        $page.items[0].Contains('properties') | Should -BeFalse
        $page.items[0].commentCount | Should -Be 2
    }

    It 'preserves chronological replies and attribution with case-sensitive identifiers' {
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId 'T')
        @($page.items.commentId) | Should -BeExactly @('Root', 'Reply')
        $page.items[1].authorship | Should -BeExactly 'human'
        $page.items[1].openingCommentId | Should -BeExactly 'Root'
        $page.items[1].context.filePath | Should -BeExactly 'src/a.rs'
        $page.items[1].isAuthoredBySelf | Should -BeFalse
        $page.items[1].threadIsAuthoredBySelf | Should -BeTrue
        $exact = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId 't' -CommentId 'root')
        $exact.items.Count | Should -Be 1
        $exact.items[0].commentId | Should -BeExactly 'root'
        { Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId 't' -CommentId 'Root') } | Should -Throw '*no comment*'
        { Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId 'missing') } | Should -Throw '*no thread*'
    }

    It 'exposes every indeterminate comment through projected pages' {
        $pages = @(Read-QueryPages $script:Document (New-PullRequestQueryOptions -Query comments -Authorship indeterminate -PageSize 1))
        @($pages.items.commentId) | Should -BeExactly @('Root', 'summary-body', 'global-body')
        @($pages.items.content) | Should -BeExactly @('Widget concern', 'Global design decision', 'PR-wide author clarification')
    }

    It 'allocates comment batches without reading any of their bodies' {
        $script:Document.threads[0].comments[0].content = 'private body marker' * 3000
        $options = New-PullRequestQueryOptions -Query comments -Authorship indeterminate -IndexOnly -PageSize 1
        $pages = @(Read-QueryPages $script:Document $options)
        @($pages.items.commentId) | Should -BeExactly @('Root', 'summary-body', 'global-body')
        $pages.Count | Should -Be 3
        foreach ($item in $pages.items) {
            $item.Contains('content') | Should -BeFalse
            $item.Contains('excerpt') | Should -BeFalse
            $item.contentLength | Should -BeGreaterThan 0
        }
        $pages[0].items[0].contentLength | Should -Be $script:Document.threads[0].comments[0].content.Length
        $full = New-PullRequestQueryOptions -Query comments -Authorship indeterminate -PageSize 1
        { Read-QueryPage $script:Document $full -Cursor $pages[0].nextCursor } | Should -Throw '*mismatched*'
    }

    It 'searches full bodies beyond discovery excerpts and includes description and cross-file matches' {
        $script:Document.threads[0].comments[0].content = ('prefix' * 1000) + ' WiDgEt end'
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query search -Text 'widget')
        $page.totalCount | Should -Be 4
        $page.items[0].field | Should -BeExactly 'description'
        $page.items[1].matchOffset | Should -BeGreaterThan 240
        $page.items[1].excerpt | Should -Match 'WiDgEt'
        $page.items[1].excerpt.Length | Should -BeLessOrEqual 240
        $page.items[1].Contains('content') | Should -BeFalse
        $literal = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query search -Text '.*')
        $literal.totalCount | Should -Be 0
    }

    It 'combines search with file and status filters' {
        $page = Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query search -Text 'widget' -Files @('src/b.rs') -Resolution active)
        $page.totalCount | Should -Be 1
        $page.items[0].threadId | Should -BeExactly 't'
    }

    It 'bounds an enormous comment and reconstructs exact Unicode and escaped text' {
        $body = ("`"'`\`n" + [char]::ConvertFromUtf32(0x1F916) + [char] 0x03A9) * 7000
        $script:Document.threads[0].comments[0].content = $body
        $options = New-PullRequestQueryOptions -Query comments -ThreadId T -CommentId Root
        $pages = @(Read-QueryPages $script:Document $options)
        $pages.Count | Should -BeGreaterThan 1
        $rebuilt = [Text.StringBuilder]::new()
        foreach ($page in $pages) {
            $item = $page.items[0]
            $item.contentOffset | Should -Be $rebuilt.Length
            $item.contentLength | Should -Be $body.Length
            [char]::IsLowSurrogate($item.content[0]) | Should -BeFalse
            [char]::IsHighSurrogate($item.content[-1]) | Should -BeFalse
            [void] $rebuilt.Append($item.content)
        }
        $rebuilt.ToString() | Should -BeExactly $body
        $pages[-1].items[0].contentComplete | Should -BeTrue
    }

    It 'pages descriptors within the byte ceiling even before reaching the item limit' {
        $script:Document.threads = @(1..50 | ForEach-Object {
            New-QueryThread -Id "$_" -Path 'src/a.rs' -Comments @((New-QueryComment -Id "$_" -Content ('x' * 1000)))
        })
        $pages = @(Read-QueryPages $script:Document (New-PullRequestQueryOptions -Query threads -PageSize 50))
        $pages.Count | Should -BeGreaterThan 1
        @($pages.items).Count | Should -Be 50
        @($pages.items.threadId | Select-Object -Unique).Count | Should -Be 50
    }

    It 'rejects changed captures, changed filters and malformed cursors' {
        $options = New-PullRequestQueryOptions -Query comments -ThreadId T -PageSize 1
        $page = Read-QueryPage $script:Document $options
        { Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId t -PageSize 1) -Cursor $page.nextCursor } | Should -Throw '*mismatched*'
        foreach ($invalid in @('broken', ($page.nextCursor -replace ':1:0$', ':999999999999999:0'), ($page.nextCursor -replace ':1:0$', ':99:0'))) {
            { Read-QueryPage $script:Document $options -Cursor $invalid } | Should -Throw '*cursor*'
        }
        $script:Document.threads[0].comments[0].authorship = 'human'
        { Read-QueryPage $script:Document $options -Cursor $page.nextCursor } | Should -Throw '*Stale*'
    }

    It 'reports an oversized attribution envelope instead of omitting or looping on it' {
        $script:Document.threads[0].comments[0].author.displayName = 'x' * 20000
        { Read-QueryPage $script:Document (New-PullRequestQueryOptions -Query comments -ThreadId T) } | Should -Throw '*cannot fit*'
    }

    It 'rejects ambiguous or accidental bulk access' -ForEach @(
        @{ arguments = @{ Query = 'all' } }
        @{ arguments = @{ Query = 'metadata' } }
        @{ arguments = @{ Query = 'metadata'; Field = 'threads' } }
        @{ arguments = @{ Query = 'comments' } }
        @{ arguments = @{ Query = 'comments'; CommentId = 'Root' } }
        @{ arguments = @{ Query = 'search'; Text = ' ' } }
        @{ arguments = @{ Query = 'threads'; Text = 'ignored' } }
        @{ arguments = @{ Query = 'threads'; Scope = 'files' } }
        @{ arguments = @{ Query = 'threads'; Files = @('a'); Scope = 'global' } }
        @{ arguments = @{ Query = 'threads'; Files = @('../a') } }
        @{ arguments = @{ Query = 'threads'; Files = @(42) } }
        @{ arguments = @{ Query = 'metadata'; Field = 'title'; Kind = 'text' } }
        @{ arguments = @{ Query = 'threads'; IndexOnly = $true } }
        @{ arguments = @{ Query = 'threads'; PageSize = 51 } }
    ) {
        { New-PullRequestQueryOptions @arguments } | Should -Throw
    }

    It 'keeps the canonical capture unchanged while querying' {
        $before = ConvertTo-JsonText -Value $script:Document
        Read-QueryPages $script:Document (New-PullRequestQueryOptions -Query comments -Authorship indeterminate) | Out-Null
        (ConvertTo-JsonText -Value $script:Document) | Should -BeExactly $before
    }

    It 'keeps capture diagnostics small when many comments and delta entries need attention' {
        $previous = New-QueryDocument
        $previous.threads = @()
        $script:Document.threads = @(1..1000 | ForEach-Object {
            New-QueryThread -Id "$_" -Comments @((New-QueryComment -Id "$_" -Content ('x' * 1000)))
        })
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $script:Document
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'capture.json' -Delta $delta
        $result.indeterminateCommentCount | Should -Be 1000
        $result.delta.commentsAddedCount | Should -Be 1000
        $result.delta.threadsAddedCount | Should -Be 1000
        $json = ConvertTo-Json -InputObject $result -Depth 32 -Compress
        [Text.Encoding]::UTF8.GetByteCount($json) | Should -BeLessThan 16384
        $json | Should -Not -Match 'indeterminateComments|commentId|threadId'
    }
}

Describe 'Paged freshness changes' {
    BeforeEach {
        $script:Previous = New-QueryDocument
        $script:Current = New-QueryDocument
        $script:Current.threads = @($script:Current.threads[0], $script:Current.threads[2], $script:Current.threads[3])
        $script:Current.threads[0].comments[1].lastEditedUtc = '2026-10-02T12:00:00Z'
        $script:Current.threads[0].resolution = 'active'
        $script:Current.pullRequest.reviewedSourceCommit = 'new-source'
        $script:Current.threads += New-QueryThread -Id 'new' -Comments @((New-QueryComment -Id 'new-comment'))
    }

    It 'pages all change categories and identifies which capture contains the evidence' {
        $pages = @(Read-QueryPages $script:Current (New-PullRequestQueryOptions -Query changes -PageSize 1) -Previous $script:Previous)
        @($pages.items.change) | Should -BeExactly @('threadsAdded', 'threadsRemoved', 'commentsAdded', 'commentsRemoved', 'commentsEdited', 'resolutionChanged', 'reviewedSourceCommit', 'reviewEvidence')
        $pages[1].items[0].capture | Should -BeExactly 'baseline'
        $pages[3].items[0].commentId | Should -BeExactly 'root'
        $pages[4].items[0].capture | Should -BeExactly 'both'
        $pages[-2].items[0].from | Should -BeExactly 'source'
        $pages[-2].items[0].to | Should -BeExactly 'new-source'
    }

    It 'binds pagination to both captures' {
        $options = New-PullRequestQueryOptions -Query changes -PageSize 1
        $page = Read-QueryPage $script:Current $options -Previous $script:Previous
        $script:Previous.pullRequest.title = 'baseline changed'
        { Read-QueryPage $script:Current $options -Previous $script:Previous -Cursor $page.nextCursor } | Should -Throw '*Stale*'
    }

    It 'refuses missing or unrelated baselines and unused comparison arguments' {
        { Read-QueryPage $script:Current (New-PullRequestQueryOptions -Query changes) } | Should -Throw '*ComparePath*'
        { Read-QueryPage $script:Current (New-PullRequestQueryOptions -Query threads) -Previous $script:Previous } | Should -Throw '*only valid*'
        $script:Previous.pullRequestUrl = 'https://example.invalid/pr/2'
        { Read-QueryPage $script:Current (New-PullRequestQueryOptions -Query changes) -Previous $script:Previous } | Should -Throw '*different pull requests*'
    }
}

Describe 'Bounded query executable' {
    BeforeEach {
        $script:CapturePath = Join-Path $TestDrive 'pr-context.json'
        Write-PullRequestOutputFile -Path $script:CapturePath -Content (ConvertTo-JsonText -Value (New-QueryDocument))
    }

    It 'writes one bounded UTF-8 JSON response and does not modify the capture' {
        $before = (Get-FileHash -LiteralPath $script:CapturePath).Hash
        $result = Invoke-QueryExecutable @('-Path', $script:CapturePath, '-Query', 'comments', '-ThreadId', 'T')
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.error | Should -BeNullOrEmpty
        [Text.Encoding]::UTF8.GetByteCount($result.output) | Should -BeLessOrEqual 16384
        (ConvertFrom-JsonVerbatim -Text $result.output).items.Count | Should -Be 2
        (Get-FileHash -LiteralPath $script:CapturePath).Hash | Should -BeExactly $before
    }

    It 'accepts multiple files inline or through a file without positional binding' {
        $filesPath = Join-Path $TestDrive 'files.json'
        [IO.File]::WriteAllText($filesPath, '["src\\a.rs","src\\b.rs"]')
        foreach ($selector in @(@('-FilesJson', '["src\\a.rs","src\\b.rs"]'), @('-FilesPath', $filesPath))) {
            $result = Invoke-QueryExecutable (@('-Path', $script:CapturePath, '-Query', 'threads') + $selector)
            $result.exitCode | Should -Be 0 -Because $result.error
            (ConvertFrom-JsonVerbatim -Text $result.output).totalCount | Should -Be 2
        }
        $bad = Invoke-QueryExecutable @('-Path', $script:CapturePath, '-Query', 'threads', 'accidental')
        $bad.exitCode | Should -Not -Be 0
        $bad.output | Should -BeNullOrEmpty
    }

    It 'projects a body-free index through the executable' {
        $result = Invoke-QueryExecutable @('-Path', $script:CapturePath, '-Query', 'comments', '-Authorship', 'indeterminate', '-IndexOnly')
        $result.exitCode | Should -Be 0 -Because $result.error
        $page = ConvertFrom-JsonVerbatim -Text $result.output
        $page.totalCount | Should -Be 3
        $page.items[0].Contains('content') | Should -BeFalse
        $page.items[0].contentLength | Should -BeGreaterThan 0
    }

    It 'round-trips page-spanning Unicode through native stdout and a continuation argument' {
        $document = New-QueryDocument
        $document.pullRequest.description = ('"' + [char]::ConvertFromUtf32(0x1F916)) * 5000
        Write-PullRequestOutputFile -Path $script:CapturePath -Content (ConvertTo-JsonText -Value $document)
        $arguments = @('-Path', $script:CapturePath, '-Query', 'metadata', '-Field', 'description')
        $first = Invoke-QueryExecutable $arguments
        $first.exitCode | Should -Be 0 -Because $first.error
        [Text.Encoding]::UTF8.GetByteCount($first.output) | Should -BeLessOrEqual 16384
        $page = ConvertFrom-JsonVerbatim -Text $first.output
        $page.complete | Should -BeFalse
        $page.items[0].content | Should -BeExactly $document.pullRequest.description.Substring(0, $page.items[0].content.Length)
        $second = Invoke-QueryExecutable ($arguments + @('-Cursor', $page.nextCursor))
        $second.exitCode | Should -Be 0 -Because $second.error
        [Text.Encoding]::UTF8.GetByteCount($second.output) | Should -BeLessOrEqual 16384
        (ConvertFrom-JsonVerbatim -Text $second.output).items[0].contentOffset | Should -Be $page.items[0].content.Length
    }
    It 'refuses invalid file selection shapes with bounded diagnostics' -ForEach @('[]', '{}', '[null]', '[1]', '[""]') {
        $result = Invoke-QueryExecutable @('-Path', $script:CapturePath, '-Query', 'threads', '-FilesJson', $_)
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Not -BeNullOrEmpty
        [Text.Encoding]::UTF8.GetByteCount($result.error) | Should -BeLessThan 16384
    }

    It 'rejects an explicitly blank <selector> for <query> queries' -ForEach @(
        foreach ($selector in @('-FilesJson', '-FilesPath')) {
            foreach ($query in @('threads', 'search')) {
                foreach ($value in @('', ' ')) {
                    @{ selector = $selector; query = $query; value = $value }
                }
            }
        }
    ) {
        $arguments = @('-Path', $script:CapturePath, '-Query', $query, $selector, $value)
        if ($query -eq 'search') { $arguments += @('-Text', 'Widget') }
        $result = Invoke-QueryExecutable $arguments

        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match "$selector requires a nonempty"
        [Text.Encoding]::UTF8.GetByteCount($result.error) | Should -BeLessThan 16384
    }

    It 'rejects both file selectors by presence before reading either source' -ForEach @(
        @{ json = ''; emptyPath = $true }
        @{ json = ''; emptyPath = $false }
        @{ json = '["src/a.rs"]'; emptyPath = $true }
        @{ json = '["src/a.rs"]'; emptyPath = $false }
    ) {
        # Neither input file exists: argument validation must precede all evidence reads.
        $filesPath = if ($emptyPath) { '' } else { Join-Path $TestDrive 'missing-files.json' }
        $result = Invoke-QueryExecutable @(
            '-Path', (Join-Path $TestDrive 'missing-capture.json'), '-Query', 'threads',
            '-FilesJson', $json, '-FilesPath', $filesPath
        )

        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match 'Supply either -FilesJson or -FilesPath, not both'
    }

    It 'reports malformed and unsupported captures without echoing the document' {
        foreach ($content in @('{"schemaVersion":99}', ('{"bad":' + ('x' * 30000)))) {
            [IO.File]::WriteAllText($script:CapturePath, $content)
            $result = Invoke-QueryExecutable @('-Path', $script:CapturePath, '-Query', 'threads')
            $result.exitCode | Should -Not -Be 0
            $result.output | Should -BeNullOrEmpty
            $result.error | Should -Not -BeNullOrEmpty
            [Text.Encoding]::UTF8.GetByteCount($result.error) | Should -BeLessThan 16384
        }
    }
}

Describe 'Bounded randomized pagination' {
    It 'reconstructs segmented text with unique seeds and bounded operation counts' {
        $seed = [Security.Cryptography.RandomNumberGenerator]::GetInt32([int]::MaxValue)
        $random = [Random]::new($seed)
        $alphabet = @('a', '"', '\', "`r`n", [string] [char] 0x03A9, [char]::ConvertFromUtf32(0x1F916))
        # Short and page-spanning text exercise the same reconstruction contract.
        foreach ($size in @(12, 12000)) {
            foreach ($case in 1..4) {
                $body = [Text.StringBuilder]::new()
                for ($i = 0; $i -lt $size; $i++) { [void] $body.Append($alphabet[$random.Next($alphabet.Count)]) }
                $document = New-QueryDocument
                $document.pullRequest.description = $body.ToString()
                $pages = @(Read-QueryPages $document (New-PullRequestQueryOptions -Query metadata -Field description -PageSize ($random.Next(1, 4))))
                ($pages.items.content -join '') | Should -BeExactly $body.ToString() -Because "seed=$seed, size=$size, case=$case"
            }
        }
    }
}
