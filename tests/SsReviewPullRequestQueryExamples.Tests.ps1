#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Recipe = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot '../skills/ss-review/pwsh-calls/query-pr-context.md')
    )
    $script:Fixture = [IO.File]::ReadAllText(
        (Join-Path $PSScriptRoot 'fixtures/ss-review-query-capture.json')
    )
    $script:PowerShell = (Get-Process -Id $PID).Path
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Bash = Get-Command bash -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    $script:Root = Join-Path $PSScriptRoot ('.query-examples-' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:Root)

    # Execute the checked-in shell examples as files so their quoting is parsed only by the intended shell.
    function Invoke-Recipe {
        param([string] $Shell, [string] $Program, [string] $Directory)

        $extension = if ($Shell -eq 'powershell') { 'ps1' } else { 'sh' }
        $path = Join-Path $Directory ("program.$extension")
        [IO.File]::WriteAllText($path, $Program, [Text.UTF8Encoding]::new($false))
        $executable = if ($Shell -eq 'powershell') { $script:PowerShell } else { $script:Bash.Source }
        $start = [Diagnostics.ProcessStartInfo]::new($executable)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
        $arguments = if ($Shell -eq 'powershell') { @('-NoProfile', '-NonInteractive', '-File', $path) } else { @($path) }
        foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            return @{
                Output = $stdout.GetAwaiter().GetResult()
                Error = $stderr.GetAwaiter().GetResult()
                ExitCode = $process.ExitCode
            }
        } finally {
            $process.Dispose()
        }
    }

    # Substitute literal values using the quoting rule documented for each shell.
    function Expand-Recipe {
        param([string] $Program, [string] $Shell, [hashtable] $Values)

        foreach ($key in $Values.Keys) {
            $replacement = if ($Shell -eq 'powershell') {
                $Values[$key].Replace("'", "''")
            } else {
                $Values[$key].Replace("'", "'\''")
            }
            $Program = $Program.Replace("{{$key}}", $replacement)
        }
        return $Program
    }

    # Keep each selector as a separate process argument, including explicitly empty values.
    function Invoke-NativeQuery {
        param([string[]] $Arguments, [string] $Command = 'read', [string] $Group = 'context', [hashtable] $Environment = @{})

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
        foreach ($name in $Environment.Keys) { $start.Environment[$name] = $Environment[$name] }
        foreach ($argument in @($Group, $Command) + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            return @{
                Output = $stdout.GetAwaiter().GetResult()
                Error = $stderr.GetAwaiter().GetResult()
                ExitCode = $process.ExitCode
            }
        } finally {
            $process.Dispose()
        }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) {
        [IO.Directory]::Delete($script:Root, $true)
    }
}

Describe 'Documented native pull request query examples' {
    It 'preserves literal search text from a file in the saved native request' {
        $directory = Join-Path $script:Root 'literal search'
        [void] [IO.Directory]::CreateDirectory($directory)
        $capture = Join-Path $directory 'capture.json'
        $request = Join-Path $directory 'request.json'
        $diagnostics = Join-Path $directory 'diagnostics.json'
        $textPath = Join-Path $directory 'search.txt'
        $text = "C:\space path\quoted `"value`" O'Brien 雪`nnext line"
        $document = ConvertFrom-Json -InputObject $script:Fixture -AsHashtable
        $document.threads[0].comments[0].content = $text
        [IO.File]::WriteAllText($capture, (ConvertTo-Json -InputObject $document -Depth 100), [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($textPath, $text, [Text.UTF8Encoding]::new($false))
        $values = @{ CONTEXT_PATH = $capture; REQUEST_PATH = $request; DIAGNOSTICS_PATH = $diagnostics; TEXT_PATH = $textPath }
        $header = "#Requires -Version 7.6`nSet-StrictMode -Version Latest`n`$ErrorActionPreference = 'Stop'`n`$PSNativeCommandUseErrorActionPreference = `$true`n"
        $prepare = "ss-ai context prepare --path '{{CONTEXT_PATH}}' --request-path '{{REQUEST_PATH}}' --diagnostics-path '{{DIAGNOSTICS_PATH}}' --query search --text-path '{{TEXT_PATH}}'"
        $created = Invoke-Recipe -Shell powershell -Program ($header + (Expand-Recipe -Program $prepare -Shell powershell -Values $values)) -Directory $directory
        $created.ExitCode | Should -Be 0 -Because $created.Error
        $saved = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($request))
        $saved.options.text | Should -BeExactly $text
        $read = "ss-ai context read-saved --request-path '{{REQUEST_PATH}}'"
        $result = Invoke-Recipe -Shell powershell -Program ($header + (Expand-Recipe -Program $read -Shell powershell -Values $values)) -Directory $directory
        $result.ExitCode | Should -Be 0 -Because $result.Error
        $page = ConvertFrom-Json -InputObject $result.Output
        $page.items.Count | Should -Be 1
        $page.items[0].commentId | Should -BeExactly 'C'
    }

    Describe 'Native direct query selectors' {
        BeforeEach {
            $script:Directory = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
            [void] [IO.Directory]::CreateDirectory($script:Directory)
            $script:Capture = Join-Path $script:Directory 'capture.json'
            $document = ConvertFrom-Json -InputObject $script:Fixture -AsHashtable
            $second = ConvertFrom-Json -InputObject (ConvertTo-Json -InputObject $document.threads[0] -Depth 100) -AsHashtable
            $second.id = 'second'
            $second.context.filePath = 'src/second.rs'
            $second.comments[0].id = 'second-comment'
            $second.openingCommentId = 'second-comment'
            $document.threads += $second
            $global = ConvertFrom-Json -InputObject (ConvertTo-Json -InputObject $second -Depth 100) -AsHashtable
            $global.id = 'global'
            $global.context = $null
            $global.comments[0].id = 'global-comment'
            $global.openingCommentId = 'global-comment'
            $document.threads += $global
            [IO.File]::WriteAllText($script:Capture, (ConvertTo-Json -InputObject $document -Depth 100), [Text.UTF8Encoding]::new($false))
        }

        It 'writes one bounded UTF-8 response for a conversation without changing the capture' {
            $document = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Capture)) -AsHashtable
            $reply = ConvertFrom-Json -InputObject (ConvertTo-Json -InputObject $document.threads[0].comments[0] -Depth 100) -AsHashtable
            $reply.id = 'reply'
            $document.threads[0].comments += $reply
            [IO.File]::WriteAllText($script:Capture, (ConvertTo-Json -InputObject $document -Depth 100), [Text.UTF8Encoding]::new($false))
            $before = [IO.File]::ReadAllBytes($script:Capture)
            $result = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'comments', '--thread-id', 'T')
            $result.ExitCode | Should -Be 0 -Because $result.Error
            $result.Error | Should -BeNullOrEmpty
            [Text.Encoding]::UTF8.GetByteCount($result.Output) | Should -BeLessOrEqual 16384
            (ConvertFrom-Json -InputObject $result.Output).items.Count | Should -Be 2
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($script:Capture)) | Should -BeExactly ([Convert]::ToBase64String($before))
        }

        It 'accepts multiple file identifiers inline or from a file and rejects positional input' {
            $files = '["src\\widget.rs","src\\second.rs"]'
            $filesPath = Join-Path $script:Directory 'files.json'
            [IO.File]::WriteAllText($filesPath, $files, [Text.UTF8Encoding]::new($false))
            foreach ($selector in @(@('--files-json', $files), @('--files-path', $filesPath))) {
                $result = Invoke-NativeQuery (@('--path', $script:Capture, '--query', 'threads') + $selector)
                $result.ExitCode | Should -Be 0 -Because $result.Error
                (ConvertFrom-Json -InputObject $result.Output).totalCount | Should -Be 2
            }
            $bad = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'threads', 'accidental')
            $bad.ExitCode | Should -Not -Be 0
            $bad.Output | Should -BeNullOrEmpty
        }

        It 'projects a body-free comment index through the executable' {
            $result = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'comments', '--authorship', 'indeterminate', '--index-only')
            $result.ExitCode | Should -Be 0 -Because $result.Error
            $page = ConvertFrom-Json -InputObject $result.Output -AsHashtable
            $page.totalCount | Should -Be 3
            $page.items[0].ContainsKey('content') | Should -BeFalse
            $page.items[0].contentLength | Should -BeGreaterThan 0
        }

        It 'refuses invalid file selection <Selection> with bounded diagnostics' -ForEach @(
            @{ Selection = '[]' }; @{ Selection = '{}' }; @{ Selection = '[null]' }
            @{ Selection = '[1]' }; @{ Selection = '[""]' }
        ) {
            $result = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'threads', '--files-json', $Selection)
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            $result.Error | Should -Not -BeNullOrEmpty
            [Text.Encoding]::UTF8.GetByteCount($result.Error) | Should -BeLessThan 16384
        }

        It 'rejects blank <Selector> for <Query> with bounded diagnostics' -ForEach @(
            foreach ($selector in @('--files-json', '--files-path')) {
                foreach ($query in @('threads', 'search')) {
                    foreach ($value in @('', ' ')) {
                        @{ Selector = $selector; Query = $query; Value = $value }
                    }
                }
            }
        ) {
            $arguments = @('--path', $script:Capture, '--query', $Query, $Selector, $Value)
            if ($Query -eq 'search') { $arguments += @('--text', 'Widget') }
            $result = Invoke-NativeQuery $arguments
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            $result.Error | Should -Match 'requires a nonempty'
            [Text.Encoding]::UTF8.GetByteCount($result.Error) | Should -BeLessThan 16384
        }

        It 'rejects both selectors before reading either input: blank JSON=<BlankJson>, blank path=<BlankPath>' -ForEach @(
            @{ BlankJson = $true; BlankPath = $true }; @{ BlankJson = $true; BlankPath = $false }
            @{ BlankJson = $false; BlankPath = $true }; @{ BlankJson = $false; BlankPath = $false }
        ) {
            $json = if ($BlankJson) { '' } else { '["src/widget.rs"]' }
            $filesPath = if ($BlankPath) { '' } else { Join-Path $script:Directory 'missing-files.json' }
            $result = Invoke-NativeQuery @(
                '--path', (Join-Path $script:Directory 'missing-capture.json'), '--query', 'threads',
                '--files-json', $json, '--files-path', $filesPath
            )
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            $result.Error | Should -Match 'Supply either -FilesJson or -FilesPath, not both'
        }

        It 'does not echo malformed or unsupported captures' {
            foreach ($content in @('{"schemaVersion":99}', ('{"bad":' + ('x' * 30000)))) {
                [IO.File]::WriteAllText($script:Capture, $content, [Text.UTF8Encoding]::new($false))
                $result = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'threads')
                $result.ExitCode | Should -Not -Be 0
                $result.Output | Should -BeNullOrEmpty
                $result.Error | Should -Not -BeNullOrEmpty
                [Text.Encoding]::UTF8.GetByteCount($result.Error) | Should -BeLessThan 16384
                $result.Error | Should -Not -Match ('x' * 30000)
            }
        }

        It 'refuses a saved page from another selection without emitting evidence or changing the record' {
            $request = Join-Path $script:Directory 'request.json'
            $diagnostics = Join-Path $script:Directory 'diagnostics.json'
            $created = Invoke-NativeQuery -Command prepare -Arguments @(
                '--path', $script:Capture, '--request-path', $request, '--diagnostics-path', $diagnostics,
                '--query', 'metadata', '--field', 'title'
            )
            $created.ExitCode | Should -Be 0 -Because $created.Error
            $first = Invoke-NativeQuery -Command read-saved -Arguments @('--request-path', $request)
            $first.ExitCode | Should -Be 0 -Because $first.Error
            $other = Invoke-NativeQuery @('--path', $script:Capture, '--query', 'metadata', '--field', 'description')
            $other.ExitCode | Should -Be 0 -Because $other.Error
            $record = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($request)) -AsHashtable
            $record.lastPage = ConvertFrom-Json -InputObject $other.Output -AsHashtable
            [IO.File]::WriteAllText($request, (ConvertTo-Json -InputObject $record -Depth 100), [Text.UTF8Encoding]::new($false))
            $before = [IO.File]::ReadAllBytes($request)
            $result = Invoke-NativeQuery -Command read-saved -Arguments @('--request-path', $request)
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            $result.Error | Should -Match 'does not match'
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($request)) | Should -BeExactly ([Convert]::ToBase64String($before))
        }

        It 'records failed and repeated setup without overwriting the request' {
            $request = Join-Path $script:Directory 'request.json'
            $diagnostics = Join-Path $script:Directory 'diagnostics.json'
            $arguments = @('--path', $script:Capture, '--request-path', $request, '--diagnostics-path', $diagnostics, '--query', 'metadata')
            $bad = Invoke-NativeQuery -Command prepare -Arguments $arguments
            $bad.ExitCode | Should -Not -Be 0
            [IO.File]::Exists($request) | Should -BeFalse
            $good = Invoke-NativeQuery -Command prepare -Arguments ($arguments + @('--field', 'title'))
            $good.ExitCode | Should -Be 0 -Because $good.Error
            $before = [IO.File]::ReadAllBytes($request)
            $repeat = Invoke-NativeQuery -Command prepare -Arguments ($arguments + @('--field', 'description'))
            $repeat.ExitCode | Should -Not -Be 0
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($request)) | Should -BeExactly ([Convert]::ToBase64String($before))
            $counts = (ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($diagnostics))).counts
            $counts.setupCalls | Should -Be 3
            $counts.repeatedSetupCalls | Should -Be 1
            $counts.malformedQueryFailures | Should -Be 1
        }

        It 'rejects direct selectors in a saved-query invocation' {
            $result = Invoke-NativeQuery -Command read-saved -Arguments @(
                '--request-path', (Join-Path $script:Directory 'missing-request.json'), '--query', 'threads'
            )
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
        }

        It 'rejects an oversized edited saved page without emitting it' {
            $request = Join-Path $script:Directory 'request.json'
            $diagnostics = Join-Path $script:Directory 'diagnostics.json'
            $created = Invoke-NativeQuery -Command prepare -Arguments @(
                '--path', $script:Capture, '--request-path', $request, '--diagnostics-path', $diagnostics,
                '--query', 'metadata', '--field', 'title'
            )
            $created.ExitCode | Should -Be 0 -Because $created.Error
            $first = Invoke-NativeQuery -Command read-saved -Arguments @('--request-path', $request)
            $first.ExitCode | Should -Be 0 -Because $first.Error
            $record = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($request)) -AsHashtable
            $record.lastPage.items[0].content = 'x' * 20000
            [IO.File]::WriteAllText($request, (ConvertTo-Json -InputObject $record -Depth 100), [Text.UTF8Encoding]::new($false))
            $result = Invoke-NativeQuery -Command read-saved -Arguments @('--request-path', $request)
            $result.ExitCode | Should -Not -Be 0
            $result.Output | Should -BeNullOrEmpty
            $result.Error | Should -Match 'bounded output limit'
        }

        It 'registers the capture only as query input and validates overview links' {
            $skill = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review'))
            $scope = Join-Path $script:Directory 'scope.md'
            [IO.File]::WriteAllText($scope, 'Review this fixture pull request.', [Text.UTF8Encoding]::new($false))
            $result = Invoke-NativeQuery -Group workflow -Command worker-assignment -Arguments @(
                '--phase', 'fileGroups', '--session-state', $script:Directory, '--scope-path', $scope,
                '--output-directory', (Join-Path $script:Directory 'assignment'), '--skill-root', $skill,
                '--context-path', $script:Capture
            )
            $result.ExitCode | Should -Be 0 -Because $result.Error
            $prepared = ConvertFrom-Json -InputObject $result.Output -AsHashtable
            $prepared.dispatchPrompt | Should -Match 'Calling shell contract in the assigned queryRecipe'
            $prepared.dispatchPrompt | Should -Match 'including for any permitted nested worker'
            $assignment = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($prepared.assignmentPath)) -AsHashtable
            $assignment.contextPath | Should -BeExactly $script:Capture
            @($assignment.documents.id) | Should -Contain 'queryRecipe'
            ($assignment.documents | Where-Object id -EQ queryRecipe).path |
                Should -BeExactly ([IO.Path]::GetFullPath((Join-Path $skill 'pwsh-calls/query-pr-context.md')))
            @($assignment.documents.path) | Should -Not -Contain $script:Capture

            $prompt = [IO.File]::ReadAllText((Join-Path $skill 'prepare-content-overview.md'))
            $overview = [regex]::Match($prompt, '(?s)```text\r?\n(?<body>.*?)\r?\n```').Groups['body'].Value
            $overview = $overview.Replace('# Content overview', "# Content overview`n`n**Pull request:** https://example.invalid/pr/1")
            $overviewPath = Join-Path $script:Directory 'overview.md'
            [IO.File]::WriteAllText($overviewPath, $overview, [Text.UTF8Encoding]::new($false))
            $valid = Invoke-NativeQuery -Group workflow -Command validate-overview -Arguments @('--path', $overviewPath, '--context-path', $script:Capture)
            $valid.ExitCode | Should -Be 0 -Because $valid.Error
            (ConvertFrom-Json -InputObject $valid.Output).isValid | Should -BeTrue
            [IO.File]::WriteAllText($overviewPath, $overview.Replace('https://example.invalid/pr/1', 'https://example.invalid/wrong'), [Text.UTF8Encoding]::new($false))
            $invalid = Invoke-NativeQuery -Group workflow -Command validate-overview -Arguments @('--path', $overviewPath, '--context-path', $script:Capture)
            $invalid.ExitCode | Should -Not -Be 0
            $invalid.Output | Should -BeNullOrEmpty
        }

        It 'reconstructs bounded randomized Unicode pages through the native executable' {
            $seed = [Security.Cryptography.RandomNumberGenerator]::GetInt32([int]::MaxValue)
            $random = [Random]::new($seed)
            $alphabet = @('a', '"', '\', "`r`n", [string] [char] 0x03A9, [char]::ConvertFromUtf32(0x1F916))
            # Cover a small body and a page-spanning body; repetitions balance random coverage and cost.
            foreach ($size in @(12, 12000)) {
                foreach ($case in 1..4) {
                    $body = [Text.StringBuilder]::new()
                    for ($index = 0; $index -lt $size; $index++) {
                        [void] $body.Append($alphabet[$random.Next($alphabet.Count)])
                    }
                    $document = ConvertFrom-Json -InputObject $script:Fixture -AsHashtable
                    $document.pullRequest.description = $body.ToString()
                    [IO.File]::WriteAllText($script:Capture, (ConvertTo-Json -InputObject $document -Depth 100), [Text.UTF8Encoding]::new($false))
                    $arguments = @('--path', $script:Capture, '--query', 'metadata', '--field', 'description', '--page-size', [string] $random.Next(1, 4))
                    $rebuilt = [Text.StringBuilder]::new()
                    $cursor = ''
                    $complete = $false
                    # The bound comfortably exceeds pages needed for this body and detects stalled continuations.
                    for ($attempt = 0; $attempt -lt 100; $attempt++) {
                        $selection = if ($cursor) { $arguments + @('--cursor', $cursor) } else { $arguments }
                        $result = Invoke-NativeQuery $selection
                        $result.ExitCode | Should -Be 0 -Because $result.Error
                        [Text.Encoding]::UTF8.GetByteCount($result.Output) | Should -BeLessOrEqual 16384
                        $page = ConvertFrom-Json -InputObject $result.Output
                        foreach ($item in $page.items) { [void] $rebuilt.Append($item.content) }
                        if ($page.complete) { $complete = $true; break }
                        $page.nextCursor | Should -Not -BeNullOrEmpty
                        $page.nextCursor | Should -Not -BeExactly $cursor
                        $cursor = $page.nextCursor
                    }
                    $complete | Should -BeTrue
                    $rebuilt.ToString() | Should -BeExactly $body.ToString() -Because "seed=$seed, size=$size, case=$case"
                }
            }
        }

        It 'keeps capture summary bounded when many threads and comments are added' -Skip:$IsWindows {
                # The fake CLI can only read local fixture data; it cannot reach a provider.
                $bin = Join-Path $script:Directory 'bin'
                [void] [IO.Directory]::CreateDirectory($bin)
                $fake = @'
#!/usr/bin/env pwsh
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$fixture = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($env:FAKE_GH_FIXTURE)) -AsHashtable -DateKind String
$responses = $fixture.responses
if ($args[0] -cne 'api') { throw 'Unexpected provider command.' }
$endpoint = $args[1]
if ($endpoint -ceq 'graphql') {
        $inputData = ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd()) -AsHashtable
        $query = $inputData.query
        if ($args[3] -cne 'POST' -or -not $query.StartsWith('query(')) { throw 'Unexpected provider operation.' }
        $value = if ($query.Contains('reviewThreads(')) { $responses.reviewThreads }
            elseif ($query.Contains('timelineItems(')) { $responses.timeline }
            elseif ($query.Contains('reviews(')) { $responses.reviewEdits }
            elseif ($query.Contains('comments(')) { $responses.issueEdits }
            else { throw 'Unexpected GraphQL query.' }
        [Console]::Write((ConvertTo-Json -InputObject $value -Depth 100 -Compress))
        exit 0
}
if ($args[3] -cne 'GET') { throw 'Unexpected provider method.' }
$value = switch -Wildcard ($endpoint) {
        'repos/octo/widget/pulls/7' { ConvertTo-Json -InputObject $responses.pullRequest -Depth 100 -Compress; break }
        'repos/octo/widget/pulls/7/reviews*' { ConvertTo-Json -InputObject $responses.reviews -Depth 100 -Compress; break }
        'repos/octo/widget/issues/7/comments*' { ConvertTo-Json -InputObject $responses.issueComments -Depth 100 -Compress; break }
        'repos/octo/widget/pulls/7/requested_reviewers*' { ConvertTo-Json -InputObject $responses.requestedReviewers -Depth 100 -Compress; break }
        'repos/octo/widget/compare/*' { ConvertTo-Json -InputObject $responses.compare -Depth 100 -Compress; break }
        'user' { ConvertTo-Json -InputObject $responses.user -Depth 100 -Compress; break }
        default { throw 'Unexpected provider endpoint.' }
}
if ($args -ccontains '--slurp') { $value = "[$value]" }
[Console]::Write($value)
'@
                $fakePath = Join-Path $bin 'gh'
                [IO.File]::WriteAllText($fakePath, $fake, [Text.UTF8Encoding]::new($false))
                [IO.File]::SetUnixFileMode($fakePath, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
                $fixturePath = Join-Path $script:Directory 'github-fixture.json'
                $fixture = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText((Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture/github.json'))) -AsHashtable -DateKind String
                $nodes = $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes
                $template = ConvertTo-Json -InputObject $nodes[0] -Depth 100 -Compress
                $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes = @()
                [IO.File]::WriteAllText($fixturePath, (ConvertTo-Json -InputObject $fixture -Depth 100 -Compress), [Text.UTF8Encoding]::new($false))
                $environment = @{
                    PATH = $bin + [IO.Path]::PathSeparator + [IO.Path]::GetDirectoryName($script:PowerShell)
                    FAKE_GH_FIXTURE = $fixturePath
                }
                $arguments = @('--url', 'https://github.com/octo/widget/pull/7', '--output-directory', $script:Directory, '--now-utc', '2026-08-04T12:00:00Z')
                $baseline = Invoke-NativeQuery -Group context -Command create -Arguments $arguments -Environment $environment
                $baseline.ExitCode | Should -Be 0 -Because $baseline.Error
                $baselinePath = (ConvertFrom-Json -InputObject $baseline.Output).contextPath
                # Repeated long comments would exceed the output ceiling if emitted in the summary.
                $fixture.responses.reviewThreads.data.repository.pullRequest.reviewThreads.nodes = @(
                    foreach ($index in 1..1000) {
                        $thread = ConvertFrom-Json -InputObject $template -AsHashtable -DateKind String
                        $thread.id = "thread-$index"
                        $thread.comments.nodes[0].id = "comment-$index"
                        $thread.comments.nodes[0].databaseId = $index
                        $thread.comments.nodes[0].body = 'x' * 1000
                        $thread
                    }
                )
                [IO.File]::WriteAllText($fixturePath, (ConvertTo-Json -InputObject $fixture -Depth 100 -Compress), [Text.UTF8Encoding]::new($false))
                $refresh = Invoke-NativeQuery -Group context -Command create -Arguments (
                    $arguments + @('--output-prefix', 'refresh-', '--compare-path', $baselinePath)
                ) -Environment $environment
                $refresh.ExitCode | Should -Be 0 -Because $refresh.Error
                [Text.Encoding]::UTF8.GetByteCount($refresh.Output) | Should -BeLessThan 16384
                $summary = ConvertFrom-Json -InputObject $refresh.Output
                $summary.indeterminateCommentCount | Should -Be 1000
                $summary.delta.commentsAddedCount | Should -Be 1000
                $summary.delta.threadsAddedCount | Should -Be 1000
                $refresh.Output | Should -Not -Match 'indeterminateComments|commentId|threadId'
        }
    }

    It 'recovers from using the new request path as the capture without changing the capture' {
        $directory = Join-Path $script:Root 'corrected request input'
        [void] [IO.Directory]::CreateDirectory($directory)
        $capture = Join-Path $directory 'capture.json'
        $request = Join-Path $directory 'request.json'
        $diagnostics = Join-Path $directory 'diagnostics.json'
        [IO.File]::WriteAllText($capture, $script:Fixture, [Text.UTF8Encoding]::new($false))
        $before = [IO.File]::ReadAllBytes($capture)
        $values = @{ CONTEXT_PATH = $request; REQUEST_PATH = $request; DIAGNOSTICS_PATH = $diagnostics }
        $header = "#Requires -Version 7.6`nSet-StrictMode -Version Latest`n`$ErrorActionPreference = 'Stop'`n`$PSNativeCommandUseErrorActionPreference = `$true`n"
        $command = "ss-ai context prepare --path '{{CONTEXT_PATH}}' --request-path '{{REQUEST_PATH}}' --diagnostics-path '{{DIAGNOSTICS_PATH}}' --query metadata --field title"
        $failed = Invoke-Recipe -Shell powershell -Program ($header + (Expand-Recipe -Program $command -Shell powershell -Values $values)) -Directory $directory
        $failed.ExitCode | Should -Not -Be 0
        $failed.Output | Should -BeNullOrEmpty
        [IO.File]::Exists($request) | Should -BeFalse

        $values.CONTEXT_PATH = $capture
        $corrected = Invoke-Recipe -Shell powershell -Program ($header + (Expand-Recipe -Program $command -Shell powershell -Values $values)) -Directory $directory
        $corrected.ExitCode | Should -Be 0 -Because $corrected.Error
        $read = "ss-ai context read-saved --request-path '{{REQUEST_PATH}}'"
        $result = Invoke-Recipe -Shell powershell -Program ($header + (Expand-Recipe -Program $read -Shell powershell -Values $values)) -Directory $directory
        $result.ExitCode | Should -Be 0 -Because $result.Error
        $page = ConvertFrom-Json -InputObject $result.Output
        $page.complete | Should -BeTrue
        $page.items.Count | Should -Be 1
        $page.items[0].field | Should -BeExactly 'title'
        $page.items[0].content | Should -BeExactly 'Widget'
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($capture)) | Should -BeExactly ([Convert]::ToBase64String($before))
    }

    It 'executes <Shell> quoting, bounded continuation and failed setup without a raw capture fallback' -ForEach @(
        @{ Shell = 'powershell' }
        @{ Shell = 'bash' }
    ) {
        if ($Shell -eq 'bash' -and -not $script:Bash) {
            Set-ItResult -Skipped -Because 'Bash is unavailable'
            return
        }

        $directory = Join-Path $script:Root "$Shell O'Brien `$literal ``tick 雪"
        [void] [IO.Directory]::CreateDirectory($directory)
        $captureName = if ($IsWindows) { 'capture O''Brien.json' } else { 'capture O''Brien\backslash.json' }
        $capture = [IO.Path]::Combine($directory, $captureName)
        $request = Join-Path $directory 'request O''Brien.json'
        $diagnostics = Join-Path $directory 'diagnostics O''Brien.json'
        $firstFile = "src\space O'Brien `$literal ``tick `"雪`".rs"
        $secondFile = "src/other O'Brien 雪.rs"
        $document = ConvertFrom-Json -InputObject $script:Fixture -AsHashtable
        $template = ConvertTo-Json -InputObject $document.threads[0] -Depth 100 -Compress
        # Exceed the documented default page size so continuation is required.
        $document.threads = @(
            foreach ($index in 1..25) {
                $thread = ConvertFrom-Json -InputObject $template -AsHashtable
                $thread.id = "thread-$index"
                $thread.context.filePath = if ($index % 2) { $firstFile.Replace('\', '/') } else { $secondFile }
                $thread
            }
        )
        [IO.File]::WriteAllText($capture, (ConvertTo-Json -InputObject $document -Depth 100), [Text.UTF8Encoding]::new($false))
        $before = [IO.File]::ReadAllBytes($capture)
        $values = @{
            CONTEXT_PATH = $capture; REQUEST_PATH = $request; DIAGNOSTICS_PATH = $diagnostics
            FIRST_FILE = $firstFile; SECOND_FILE = $secondFile
        }
        $pattern = '(?s)```{0}\r?\n(?<body>.*?)\r?\n```' -f $Shell
        $blocks = @([regex]::Matches($script:Recipe, $pattern))
        $blocks.Count | Should -Be 1
        $program = Expand-Recipe -Program $blocks[0].Groups['body'].Value -Shell $Shell -Values $values
        $result = Invoke-Recipe -Shell $Shell -Program $program -Directory $directory
        $result.ExitCode | Should -Be 0 -Because $result.Error
        $lines = @($result.Output -split '\r?\n' | Where-Object { $_ })
        $lines.Count | Should -Be 2
        (ConvertFrom-Json -InputObject $lines[0]).requestPath | Should -BeExactly $request
        $saved = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($request))
        $saved.options.files.Count | Should -Be 2
        $saved.options.files[0] | Should -BeExactly $firstFile.Replace('\', '/')
        $saved.options.files[1] | Should -BeExactly $secondFile
        $page = ConvertFrom-Json -InputObject $lines[1]
        $page.complete | Should -BeFalse
        $ids = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        $stageTwo = [regex]::Match($script:Recipe, '(?s)# Stage 2: read and continue.*?```text\r?\n(?<body>.*?)\r?\n```')
        $stageTwo.Success | Should -BeTrue
        $nextLines = @($stageTwo.Groups['body'].Value -split '\r?\n' | Where-Object { $_ -match ' --next$' })
        $nextLines.Count | Should -Be 1
        # Stage 2 is shell-neutral syntax; apply the calling-shell literal rules before substitution.
        $nextCommand = $nextLines[0].Replace('"{{REQUEST_PATH}}"', "'{{REQUEST_PATH}}'")
        $continuation = Expand-Recipe -Program $nextCommand -Shell $Shell -Values $values
        if ($Shell -eq 'powershell') {
            $continuation = "#Requires -Version 7.6`nSet-StrictMode -Version Latest`n`$ErrorActionPreference = 'Stop'`n`$PSNativeCommandUseErrorActionPreference = `$true`n$continuation"
        } else {
            $continuation = "set -e`n$continuation"
        }
        [IO.File]::WriteAllText($capture, '{"private":"raw capture sentinel"}', [Text.UTF8Encoding]::new($false))
        $failedRead = Invoke-Recipe -Shell $Shell -Program $continuation -Directory $directory
        $failedRead.ExitCode | Should -Not -Be 0
        $failedRead.Output | Should -BeNullOrEmpty
        $failedRead.Error | Should -Not -Match 'raw capture sentinel'
        [IO.File]::WriteAllBytes($capture, $before)

        # Even one thread per page fits within this bound for the fixture.
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            [Text.Encoding]::UTF8.GetByteCount($lines[-1] + "`n") | Should -BeLessOrEqual 16384
            foreach ($item in $page.items) { $ids.Add($item.threadId) | Should -BeTrue }
            if ($page.complete) { break }
            $page.nextCursor | Should -Not -BeNullOrEmpty
            $next = Invoke-Recipe -Shell $Shell -Program $continuation -Directory $directory
            $next.ExitCode | Should -Be 0 -Because $next.Error
            $lines = @($next.Output -split '\r?\n' | Where-Object { $_ })
            $lines.Count | Should -Be 1
            $page = ConvertFrom-Json -InputObject $lines[0]
        }
        $page.complete | Should -BeTrue
        $ids.Count | Should -Be 25
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($capture)) | Should -BeExactly ([Convert]::ToBase64String($before))

        $values.CONTEXT_PATH = Join-Path $directory 'missing capture.json'
        $values.REQUEST_PATH = Join-Path $directory 'failed request.json'
        $values.DIAGNOSTICS_PATH = Join-Path $directory 'failed diagnostics.json'
        $failed = Invoke-Recipe -Shell $Shell -Program (Expand-Recipe -Program $blocks[0].Groups['body'].Value -Shell $Shell -Values $values) -Directory $directory
        $failed.ExitCode | Should -Not -Be 0
        $failed.Output | Should -BeNullOrEmpty
        $failed.Error | Should -Not -BeNullOrEmpty
        [IO.File]::Exists($values.REQUEST_PATH) | Should -BeFalse

        $values.CONTEXT_PATH = $capture
        $invalidQuery = (Expand-Recipe -Program $blocks[0].Groups['body'].Value -Shell $Shell -Values $values).
            Replace('--query threads', '--query not-a-query')
        $failedQuery = Invoke-Recipe -Shell $Shell -Program $invalidQuery -Directory $directory
        $failedQuery.ExitCode | Should -Not -Be 0
        $failedQuery.Output | Should -BeNullOrEmpty
        $failedQuery.Error | Should -Not -BeNullOrEmpty
        [IO.File]::Exists($values.REQUEST_PATH) | Should -BeFalse
    }
}
