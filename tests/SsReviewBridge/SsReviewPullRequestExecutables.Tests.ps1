#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
    $script:RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Scripts = Join-Path $script:RepoRoot 'skills/ss-review/scripts'
    $script:Root = Join-Path $script:RepoRoot "target/capture-bridge-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $script:Root -Force | Out-Null
    New-PSDrive -Name CaptureTest -PSProvider FileSystem -Root $script:Root | Out-Null

    # Native output uses Console directly, rather than PowerShell's success stream.
    function Invoke-Bridge {
        param([string] $Name, [hashtable] $Parameters)
        $previous = [Console]::Out
        $output = [IO.StringWriter]::new()
        try {
            [Console]::SetOut($output)
            & (Join-Path $script:Scripts $Name) @Parameters
            $output.ToString()
        } finally {
            [Console]::SetOut($previous)
            $output.Dispose()
        }
    }
}

AfterAll {
    Remove-PSDrive -Name CaptureTest
    Remove-Item -LiteralPath $script:Root -Recurse -Force
}

Describe 'Capture bridge PowerShell paths' {
    BeforeEach {
        $script:Directory = Join-Path $script:Root ([guid]::NewGuid().ToString('N') + ' é🤖')
        New-Item -ItemType Directory -Path $script:Directory | Out-Null
        $script:Capture = Join-Path $script:Directory 'capture é🤖.json'
        $document = Get-Content -Raw -LiteralPath (Join-Path $script:RepoRoot 'tests/fixtures/ss-review-query-capture.json') | ConvertFrom-Json -AsHashtable
        $document.threads[0].id = 'Té🤖'
        $document.threads[0].openingCommentId = 'Cé🤖'
        $document.threads[0].comments[0].id = 'Cé🤖'
        [IO.File]::WriteAllText($script:Capture, (ConvertTo-Json -InputObject $document -Depth 50), [Text.UTF8Encoding]::new($false))
        $script:Classification = Join-Path $script:Directory 'classification é🤖.json'
        [IO.File]::WriteAllText($script:Classification, '[{"threadId":"Té🤖","commentId":"Cé🤖","kind":"human"}]', [Text.UnicodeEncoding]::new($false, $true))
        $read = & ss-ai context read "--path=$script:Capture" --query=comments '--thread-id=Té🤖'
        $script:Snapshot = ($read | ConvertFrom-Json).snapshotId
    }

    Describe 'Query bridge PowerShell paths' {
        BeforeEach {
            $script:Directory = Join-Path $script:Root ([guid]::NewGuid().ToString('N') + ' é🤖')
            New-Item -ItemType Directory -Path $script:Directory | Out-Null
            $script:Capture = Join-Path $script:Directory 'capture é🤖.json'
            [IO.File]::Copy((Join-Path $script:RepoRoot 'tests/fixtures/ss-review-query-capture.json'), $script:Capture)
            [IO.File]::WriteAllText((Join-Path $script:Directory 'files.json'), '["src/widget.rs"]')
            [IO.File]::WriteAllText((Join-Path $script:Directory 'search.txt'), 'widget')
        }

        It 'resolves <spelling> paths for direct, saved, preparation and diagnostics commands' -ForEach @(
            @{ spelling = 'relative' }
            @{ spelling = 'drive-qualified' }
            @{ spelling = 'backslash' }
            @{ spelling = 'home-relative' }
        ) {
            $relativeFromHome = [IO.Path]::GetRelativePath($HOME, $script:Directory)
            if ($spelling -eq 'home-relative' -and [IO.Path]::IsPathRooted($relativeFromHome)) {
                Set-ItResult -Skipped -Because 'The checkout and home are on different filesystem roots.'
                return
            }
            Push-Location -LiteralPath $script:Directory
            try {
                $directory = switch ($spelling) {
                    'relative' { '.' }
                    'drive-qualified' { 'CaptureTest:\' + (Split-Path -Leaf $script:Directory) }
                    'backslash' { '.\' }
                    'home-relative' { "~/$relativeFromHome" }
                }
                $capture = Join-Path $directory 'capture é🤖.json'
                $request = Join-Path $directory 'request.json'
                $diagnostics = Join-Path $directory 'diagnostics.json'
                $files = Join-Path $directory 'files.json'
                $text = Join-Path $directory 'search.txt'

                $page = Invoke-Bridge -Name 'Read-PullRequestContext.ps1' -Parameters @{
                    Path = $capture; Query = 'comments'; FilesPath = $files
                } | ConvertFrom-Json
                $page.items[0].commentId | Should -BeExactly 'C'
                $changes = Invoke-Bridge -Name 'Read-PullRequestContext.ps1' -Parameters @{
                    Path = $script:Capture; Query = 'changes'; ComparePath = $capture
                } | ConvertFrom-Json
                $changes.totalCount | Should -Be 0

                $prepared = & (Join-Path $script:Scripts 'New-PullRequestQueryRequest.ps1') `
                    -Path $capture -RequestPath $request -DiagnosticsPath $diagnostics -Query search -TextPath $text
                $prepared | ConvertFrom-Json | Should -Not -BeNullOrEmpty
                $saved = Invoke-Bridge -Name 'Read-PullRequestContext.ps1' -Parameters @{ RequestPath = $request } | ConvertFrom-Json
                $saved.totalCount | Should -BeGreaterThan 0
                $before = & (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $diagnostics
                $recorded = & (Join-Path $script:Scripts 'Get-ReviewInputDiagnostics.ps1') -Path $diagnostics -Record documentReadCalls
                ($recorded | ConvertFrom-Json).counts.documentReadCalls | Should -Be (($before | ConvertFrom-Json).counts.documentReadCalls + 1)

                $changesRequest = Join-Path $directory 'changes.json'
                $null = & (Join-Path $script:Scripts 'New-PullRequestQueryRequest.ps1') `
                    -Path $script:Capture -RequestPath $changesRequest -DiagnosticsPath $diagnostics -Query changes -ComparePath $capture
                $savedChanges = Invoke-Bridge -Name 'Read-PullRequestContext.ps1' -Parameters @{ RequestPath = $changesRequest } | ConvertFrom-Json
                $savedChanges.totalCount | Should -Be 0
            } finally {
                Pop-Location
            }
        }

        It 'rejects non-filesystem provider paths for <name> <parameter>' -ForEach @(
            @{ name = 'Read-PullRequestContext.ps1'; parameter = 'Path' }
            @{ name = 'Read-PullRequestContext.ps1'; parameter = 'FilesPath' }
            @{ name = 'Read-PullRequestContext.ps1'; parameter = 'ComparePath' }
            @{ name = 'Read-PullRequestContext.ps1'; parameter = 'RequestPath' }
            @{ name = 'New-PullRequestQueryRequest.ps1'; parameter = 'Path' }
            @{ name = 'New-PullRequestQueryRequest.ps1'; parameter = 'RequestPath' }
            @{ name = 'New-PullRequestQueryRequest.ps1'; parameter = 'DiagnosticsPath' }
            @{ name = 'New-PullRequestQueryRequest.ps1'; parameter = 'TextPath' }
            @{ name = 'New-PullRequestQueryRequest.ps1'; parameter = 'ComparePath' }
            @{ name = 'Get-ReviewInputDiagnostics.ps1'; parameter = 'Path' }
        ) {
            $arguments = if ($name -eq 'Get-ReviewInputDiagnostics.ps1') {
                @{ Path = (Join-Path $script:Directory 'diagnostics.json') }
            } elseif ($name -eq 'New-PullRequestQueryRequest.ps1') {
                @{ Path = $script:Capture; RequestPath = (Join-Path $script:Directory 'request.json')
                    DiagnosticsPath = (Join-Path $script:Directory 'diagnostics.json'); Query = 'metadata' }
            } elseif ($parameter -eq 'RequestPath') {
                @{ RequestPath = 'unused' }
            } else {
                @{ Path = $script:Capture; Query = 'metadata' }
            }
            $arguments[$parameter] = 'Env:PATH'
            { & (Join-Path $script:Scripts $name) @arguments } | Should -Throw '*FileSystem provider*'
        }
    }

    It 'resolves <spelling> paths for authorship and comparison' -ForEach @(
        @{ spelling = 'relative' }
        @{ spelling = 'drive-qualified' }
        @{ spelling = 'backslash' }
        @{ spelling = 'home-relative' }
    ) {
        $relativeFromHome = [IO.Path]::GetRelativePath($HOME, $script:Directory)
        if ($spelling -eq 'home-relative' -and [IO.Path]::IsPathRooted($relativeFromHome)) {
            Set-ItResult -Skipped -Because 'The checkout and home are on different filesystem roots.'
            return
        }
        Push-Location -LiteralPath $script:Directory
        try {
            $directory = switch ($spelling) {
                'relative' { '.' }
                'drive-qualified' { 'CaptureTest:\' + (Split-Path -Leaf $script:Directory) }
                'backslash' { '.\' }
                'home-relative' { "~/$relativeFromHome" }
            }
            $capture = Join-Path $directory 'capture é🤖.json'
            $classification = Join-Path $directory 'classification é🤖.json'
            $result = Invoke-Bridge -Name 'Set-CommentAuthorship.ps1' -Parameters @{
                Path = $capture; SnapshotId = $script:Snapshot; ClassificationPath = $classification
            }
            $result | ConvertFrom-Json | Should -Not -BeNullOrEmpty
            $read = & ss-ai context read "--path=$script:Capture" --query=comments '--thread-id=Té🤖'
            ($read | ConvertFrom-Json).items[0].authorship | Should -BeExactly 'human'

            # Comparing the intended output to itself fails before any provider access.
            foreach ($pair in @(
                @{ Output = $directory; Compare = (Join-Path $script:Directory 'pr-context.json') }
                @{ Output = $script:Directory; Compare = (Join-Path $directory 'pr-context.json') }
            )) {
                {
                    Invoke-Bridge -Name 'Get-PullRequestContext.ps1' -Parameters @{
                        Url = 'https://github.com/octo/widget/pull/7'
                        OutputDirectory = $pair.Output; ComparePath = $pair.Compare
                    }
                } | Should -Throw '*distinct -OutputPrefix*'
            }
            Test-Path -LiteralPath (Join-Path $script:Directory 'pr-context.json') | Should -BeFalse
        } finally {
            Pop-Location
        }
    }

    It 'rejects non-filesystem provider paths for <parameter>' -ForEach @(
        @{ parameter = 'Path' }
        @{ parameter = 'ClassificationPath' }
        @{ parameter = 'OutputDirectory' }
        @{ parameter = 'ComparePath' }
    ) {
        if ($parameter -in @('Path', 'ClassificationPath')) {
            $arguments = @{ Path = $script:Capture; SnapshotId = $script:Snapshot; ClassificationPath = $script:Classification }
            $arguments[$parameter] = 'Env:PATH'
            $name = 'Set-CommentAuthorship.ps1'
        } else {
            $arguments = @{ Url = 'https://github.com/octo/widget/pull/7'; OutputDirectory = $script:Directory; ComparePath = $script:Capture }
            $arguments[$parameter] = 'Env:PATH'
            $name = 'Get-PullRequestContext.ps1'
        }
        { Invoke-Bridge -Name $name -Parameters $arguments } | Should -Throw '*filesystem path*'
    }
}
