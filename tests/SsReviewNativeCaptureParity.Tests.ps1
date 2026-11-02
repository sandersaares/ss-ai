#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Fixtures = Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture'
    $script:Scratch = Join-Path $PSScriptRoot ('.capture-parity-' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:Scratch)

    # Exercise the native process with the frozen capture, not a local provider.
    function Invoke-NativeContext {
        param([string[]] $Arguments)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        foreach ($argument in @('context') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $text = $stdout.GetAwaiter().GetResult()
            $errorText = $stderr.GetAwaiter().GetResult()
            $process.ExitCode | Should -Be 0 -Because $errorText
            [Text.Encoding]::UTF8.GetByteCount($text) | Should -BeLessOrEqual 16384
            ConvertFrom-Json -InputObject $text -AsHashtable -DateKind String
        } finally { $process.Dispose() }
    }

    # Compare decoded JSON recursively, preserving array order and exact scalar values.
    function Assert-PageValue {
        param($Actual, $Expected, [string] $Path = '$')

        if ($Expected -is [Collections.IDictionary]) {
            ($Actual -is [Collections.IDictionary]) | Should -BeTrue -Because $Path
            $Actual.Count | Should -Be $Expected.Count -Because $Path
            foreach ($key in $Expected.Keys) {
                @($Actual.Keys) -ccontains $key | Should -BeTrue -Because "$Path.$key"
                Assert-PageValue -Actual $Actual[$key] -Expected $Expected[$key] -Path "$Path.$key"
            }
        } elseif ($Expected -is [array]) {
            ($Actual -is [array]) | Should -BeTrue -Because $Path
            $Actual.Count | Should -Be $Expected.Count -Because $Path
            for ($index = 0; $index -lt $Expected.Count; $index++) {
                Assert-PageValue -Actual $Actual[$index] -Expected $Expected[$index] -Path "$Path[$index]"
            }
        } else {
            if ($null -eq $Expected) {
                $Actual | Should -BeNullOrEmpty -Because $Path
                ($null -eq $Actual) | Should -BeTrue -Because $Path
            } else {
                if ($Expected -is [string]) {
                    ($Actual -is [string]) | Should -BeTrue -Because $Path
                } elseif ($Expected -is [bool]) {
                    ($Actual -is [bool]) | Should -BeTrue -Because $Path
                } else {
                    ($Actual -is [long] -or $Actual -is [int] -or $Actual -is [double] -or $Actual -is [decimal]) |
                        Should -BeTrue -Because $Path
                }
                $Actual | Should -BeExactly $Expected -Because $Path
            }
        }
    }

    # Build the complete expected page from the frozen capture and effective classification.
    function New-ExpectedCommentPage {
        param($Capture, [string] $Snapshot)

        $items = @(
            foreach ($thread in $Capture.threads) {
                foreach ($comment in $thread.comments) {
                    @{
                        threadId = $thread.id; url = $thread.url; openingCommentId = $thread.openingCommentId
                        kind = $thread.kind; resolution = $thread.resolution; isOutdated = $thread.isOutdated
                        context = $thread.context; decision = $thread.decision; isAuthoredBySelf = $comment.isAuthoredBySelf
                        eventUtc = $thread.eventUtc; lastUpdatedUtc = $thread.lastUpdatedUtc
                        commentId = $comment.id; commentUrl = $comment.url; parentId = $comment.parentId
                        author = $comment.author; authorship = $comment.authorship; authorshipEvidence = $comment.authorshipEvidence
                        threadIsAuthoredBySelf = $thread.isAuthoredBySelf; publishedUtc = $comment.publishedUtc
                        lastEditedUtc = $comment.lastEditedUtc; content = $comment.content; contentOffset = 0
                        contentLength = $comment.content.Length; contentComplete = $true
                    }
                }
            }
        )
        return @{
            schemaVersion = 1; query = 'comments'; snapshotId = $Snapshot; totalCount = $items.Count
            items = $items; nextCursor = $null; complete = $true
        }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Scratch)) { [IO.Directory]::Delete($script:Scratch, $true) }
}

Describe 'Native provider capture reading' {
    It 'compares JSON values without requiring the same object order or Unicode spelling' {
        $first = ConvertFrom-Json -InputObject '{"content":"café","items":[1,2],"complete":true}' -AsHashtable -DateKind String
        $second = ConvertFrom-Json -InputObject '{ "complete": true, "items": [1, 2], "content": "caf\u00e9" }' -AsHashtable -DateKind String
        Assert-PageValue -Actual $first -Expected $second
    }

    It 'preserves <Provider> Unicode, snapshot and UTF-16 comment lengths' -ForEach @(
        @{ Provider = 'github'; Thread = 'PRRT_1'; ExpectedCount = 1 }
        @{ Provider = 'azure-devops'; Thread = '101'; ExpectedCount = 2 }
    ) {
        $path = Join-Path $script:Scratch "$Provider-baseline-雪.json"
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-expected.json"), $path, $true)
        $original = [IO.File]::ReadAllBytes($path)
        $golden = Get-Content -LiteralPath (Join-Path $script:Fixtures "$Provider-refresh-parity.json") -Raw |
            ConvertFrom-Json -AsHashtable
        $page = Invoke-NativeContext -Arguments @('read', '--path', $path, '--query', 'comments', '--thread-id', $Thread)
        $capture = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($path)) -AsHashtable -DateKind String
        Assert-PageValue -Actual $page -Expected (New-ExpectedCommentPage -Capture $capture -Snapshot $golden.baselineSnapshot)
        $page.schemaVersion | Should -Be 1
        $page.query | Should -BeExactly 'comments'
        $page.snapshotId | Should -BeExactly $golden.baselineSnapshot
        $page.totalCount | Should -Be $ExpectedCount
        $page.items.Count | Should -Be $ExpectedCount
        $page.complete | Should -BeTrue
        $page.nextCursor | Should -BeNullOrEmpty
        $content = 'Alpha — “gamma” 🤖 café'
        $page.items[0].content | Should -BeExactly $content
        $page.items[0].contentOffset | Should -Be 0
        $page.items[0].contentLength | Should -Be $content.Length
        $page.items[0].contentComplete | Should -BeTrue
        if ($Provider -eq 'azure-devops') {
            # Date-shaped comment text remains text.
            $page.items[1].content | Should -BeExactly '2026-08-04T10:07:33.677Z'
        }
        [IO.File]::ReadAllBytes($path) | Should -Be $original
    }

    It 'reports the edited <Provider> comment and preserves both captures' -ForEach @(
        @{ Provider = 'github'; Thread = 'PRRT_1'; Comment = 'PRRC_1' }
        @{ Provider = 'azure-devops'; Thread = '101'; Comment = '1' }
    ) {
        $previous = Join-Path $script:Scratch "$Provider-previous-雪.json"
        $current = Join-Path $script:Scratch "$Provider-refresh-雪.json"
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-expected.json"), $previous, $true)
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-refresh-expected.json"), $current, $true)
        $previousBytes = [IO.File]::ReadAllBytes($previous)
        $currentBytes = [IO.File]::ReadAllBytes($current)
        $changes = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'changes', '--compare-path', $previous)
        $changes.query | Should -BeExactly 'changes'
        $changes.complete | Should -BeTrue
        $edits = @($changes.items | Where-Object { $_.change -ceq 'commentsEdited' })
        $edits.Count | Should -Be 1
        [string] $edits[0].threadId | Should -BeExactly $Thread
        [string] $edits[0].commentId | Should -BeExactly $Comment
        $page = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'comments', '--thread-id', $Thread)
        $page.items[0].content | Should -BeExactly 'Edited 🤖'
        $page.items[0].contentLength | Should -Be ('Edited 🤖'.Length)
        [IO.File]::ReadAllBytes($previous) | Should -Be $previousBytes
        [IO.File]::ReadAllBytes($current) | Should -Be $currentBytes
    }

    It 'reads <Provider> classified baseline and refresh sidecars without changing either capture' -ForEach @(
        @{ Provider = 'github'; Thread = 'PRRT_1'; Comment = 'PRRC_1' }
        @{ Provider = 'azure-devops'; Thread = '101'; Comment = '1' }
    ) {
        $previous = Join-Path $script:Scratch "$Provider-classified-baseline.json"
        $current = Join-Path $script:Scratch "$Provider-classified-refresh.json"
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-expected.json"), $previous, $true)
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-refresh-expected.json"), $current, $true)
        $previousBytes = [IO.File]::ReadAllBytes($previous)
        $currentBytes = [IO.File]::ReadAllBytes($current)
        $golden = Get-Content -LiteralPath (Join-Path $script:Fixtures "$Provider-refresh-parity.json") -Raw |
            ConvertFrom-Json -AsHashtable -DateKind String
        foreach ($entry in @(
            @{ Path = "$previous.authorship.json"; Decisions = $golden.baselineDecisions }
            @{ Path = "$current.authorship.json"; Decisions = $golden.refreshDecisions }
        )) {
            $json = ConvertTo-Json -InputObject $entry.Decisions -Depth 100 -Compress
            [IO.File]::WriteAllText($entry.Path, $json, [Text.UTF8Encoding]::new($false))
        }
        $baseline = Invoke-NativeContext -Arguments @('read', '--path', $previous, '--query', 'comments', '--thread-id', $Thread)
        $refresh = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'comments', '--thread-id', $Thread)
        Assert-PageValue -Actual $refresh -Expected (New-ExpectedCommentPage -Capture $golden.refreshEffective -Snapshot $golden.refreshSnapshot)
        $baseline.snapshotId | Should -BeExactly $golden.classifiedSnapshot
        $refresh.snapshotId | Should -BeExactly $golden.refreshSnapshot
        $baseline.items[0].authorship | Should -BeExactly 'human'
        $refresh.items[0].authorship | Should -BeExactly 'indeterminate'
        if ($Provider -eq 'azure-devops') {
            $baseline.items[1].authorship | Should -BeExactly 'agent'
            $refresh.items[1].authorship | Should -BeExactly 'agent'
        }
        $changes = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'changes', '--compare-path', $previous)
        $expectedChanges = @{
            schemaVersion = 1; query = 'changes'; snapshotId = $golden.refreshSnapshot; totalCount = 2
            items = @(
                @{ change = 'commentsEdited'; capture = 'both'; threadId = $Thread; commentId = $Comment }
                @{ change = 'reviewEvidence'; capture = 'both' }
            )
            nextCursor = $null; complete = $true
        }
        Assert-PageValue -Actual $changes -Expected $expectedChanges
        @($changes.items | Where-Object { $_.change -ceq 'commentsEdited' }).Count | Should -Be 1
        [IO.File]::ReadAllBytes($previous) | Should -Be $previousBytes
        [IO.File]::ReadAllBytes($current) | Should -Be $currentBytes
    }
}
