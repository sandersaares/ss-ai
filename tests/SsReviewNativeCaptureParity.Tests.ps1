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
        param([string[]] $Arguments, [string] $ExpectedSha256)

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
            if ($ExpectedSha256) {
                # The fixture fingerprints cover the complete compact JSON page.
                $bytes = [Text.UTF8Encoding]::new($false).GetBytes($text.TrimEnd("`r", "`n"))
                [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)) |
                    Should -BeExactly $ExpectedSha256
            }
            ConvertFrom-Json -InputObject $text -AsHashtable -DateKind String
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Scratch)) { [IO.Directory]::Delete($script:Scratch, $true) }
}

Describe 'Native provider capture reading' {
    It 'preserves <Provider> Unicode, snapshot and UTF-16 comment lengths' -ForEach @(
        @{ Provider = 'github'; Thread = 'PRRT_1'; ExpectedCount = 1; PageHash = 'A2949AA0B58F40B574432CDF603E0FF545098AFBFD15CE08F55425DDD1550032' }
        @{ Provider = 'azure-devops'; Thread = '101'; ExpectedCount = 2; PageHash = 'D249FF8E26B59B6BF1C867343E21F0A8DEA7EF48881550A9D08C30DBFD985543' }
    ) {
        $path = Join-Path $script:Scratch "$Provider-baseline-雪.json"
        [IO.File]::Copy((Join-Path $script:Fixtures "$Provider-expected.json"), $path, $true)
        $original = [IO.File]::ReadAllBytes($path)
        $golden = Get-Content -LiteralPath (Join-Path $script:Fixtures "$Provider-refresh-parity.json") -Raw |
            ConvertFrom-Json -AsHashtable
        $page = Invoke-NativeContext -Arguments @('read', '--path', $path, '--query', 'comments', '--thread-id', $Thread) `
            -ExpectedSha256 $PageHash
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
        @{ Provider = 'github'; Thread = 'PRRT_1'; CommentHash = '8886C55B2389D88197AD4EE12995BBAB4AAFCA23048D246119157CA0B535B72B'; ChangeHash = 'F9F7E5CA3ADF96462258D97F681A51E27EEAE8A634E2B756FE4F1AEEF202F8AE' }
        @{ Provider = 'azure-devops'; Thread = '101'; CommentHash = '3EF1BE9081A8ECA020ABAFE4E8B0C9F7A6281AA10115839E31B0B1BD2A468EA9'; ChangeHash = 'D4BC0E6530D107AACC0B30477072DC400C690470C6EDD522B76125558B1B850D' }
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
        $refresh = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'comments', '--thread-id', $Thread) `
            -ExpectedSha256 $CommentHash
        $baseline.snapshotId | Should -BeExactly $golden.classifiedSnapshot
        $refresh.snapshotId | Should -BeExactly $golden.refreshSnapshot
        $baseline.items[0].authorship | Should -BeExactly 'human'
        $refresh.items[0].authorship | Should -BeExactly 'indeterminate'
        if ($Provider -eq 'azure-devops') {
            $baseline.items[1].authorship | Should -BeExactly 'agent'
            $refresh.items[1].authorship | Should -BeExactly 'agent'
        }
        $changes = Invoke-NativeContext -Arguments @('read', '--path', $current, '--query', 'changes', '--compare-path', $previous) `
            -ExpectedSha256 $ChangeHash
        @($changes.items | Where-Object { $_.change -ceq 'commentsEdited' }).Count | Should -Be 1
        [IO.File]::ReadAllBytes($previous) | Should -Be $previousBytes
        [IO.File]::ReadAllBytes($current) | Should -Be $currentBytes
    }
}
