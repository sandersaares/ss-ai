BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'agents\.github\skills\ss-review'
    $lib = Join-Path $script:SkillRoot 'scripts\lib'
    . (Join-Path $lib 'JsonValue.ps1')
    . (Join-Path $lib 'PublicationFailure.ps1')
    . (Join-Path $lib 'PullRequestModel.ps1')
    . (Join-Path $lib 'PullRequestTarget.ps1')
    . (Join-Path $lib 'PullRequestTransportHost.ps1')
    . (Join-Path $lib 'FindingMarkdown.ps1')
    . (Join-Path $lib 'ReportTools.ps1')
    . (Join-Path $lib 'ReviewHtml.ps1')
    . (Join-Path $lib 'OverviewPublication.ps1')
    . (Join-Path $lib 'GitHubCliTransport.ps1')
    . (Join-Path $lib 'OverviewPublicationApi.ps1')

    function New-TestOverview {
        @('Mission', 'Substance', 'Design changes', 'Significant changes', 'Structure', 'Mechanism', 'Completeness assessment', 'Related history', 'Related work') |
            ForEach-Object { "## $_`n`nThis is evidence-based context.`n" } |
            Join-String -Separator "`n"
    }
    function New-TestOverviewContext {
        param([string] $Platform = 'gitHub')
        $target = [ordered]@{
            platform = $Platform
            organization = 'owner'
            repository = 'repo'
            project = 'project'
            collectionUrl = 'https://dev.azure.com/owner'
            pullRequestId = 7
        }
        [ordered]@{
            pullRequestUrl = Get-PullRequestWebUrl -Target $target
            target = $target
            pullRequest = @{
                reviewedSourceCommit = 'a' * 40
                reviewedMergeBaseCommit = 'b' * 40
            }
        }
    }
}

Describe 'Safe report presentation' {
    BeforeEach {
        Mock Get-ReportToolsLockPath { Join-Path $TestDrive 'report-tools' }
    }

    It 'renders SVG as an embedded image and escapes its source' {
        $html = ConvertTo-ReviewDiagramHtml -Source 'A["<script>"] --> B' `
            -Svg '<svg xmlns="http://www.w3.org/2000/svg"><desc>Flow &amp; result</desc><text>Result</text></svg>'
        $html | Should -Match 'data:image/svg\+xml;base64,'
        $html | Should -Match 'alt="Flow &amp; result"'
        $html | Should -Match '&lt;script&gt;'
        $html | Should -Not -Match '<script>'
        $html | Should -Match '<details>'
    }

    It 'rejects active or external SVG: <Svg>' -ForEach @(
        @{ Svg = '<svg><script>run()</script></svg>' }
        @{ Svg = '<svg><foreignObject>label</foreignObject></svg>' }
        @{ Svg = '<svg><image href="https://example.invalid/a.png"/></svg>' }
        @{ Svg = '<svg onload="run()"/>' }
        @{ Svg = '<svg><a href="file:///secret">link</a></svg>' }
        @{ Svg = '<!DOCTYPE svg [<!ENTITY test SYSTEM "file:///secret">]><svg>&test;</svg>' }
    ) {
        { ConvertTo-ReviewDiagramHtml -Source 'graph TD; A-->B' -Svg $Svg } | Should -Throw
    }

    It 'keeps fragment references in SVG styles' {
        ConvertTo-ReviewDiagramHtml -Source 'graph TD; A-->B' `
            -Svg '<svg><style>.edge {marker-end:url("#arrow");marker-start:url(#start)}</style><text>A</text></svg>' | Should -Match '<img'
    }

    It 'preserves layout dimensions rather than shrinking diagram labels' {
        ConvertTo-ReviewDiagramHtml -Source 'graph TD; A-->B' `
            -Svg '<svg viewBox="0 0 840.3 280.2"><text>A</text></svg>' |
            Should -Match 'width="841" height="281"'
    }
    It 'transforms only actual Mermaid code blocks and keeps other code intact' {
        $node = @{ blocks = @(
            @{ t = 'CodeBlock'; c = @(@('', @('powershell'), @()), '```mermaid is just text') }
            @{ t = 'CodeBlock'; c = @(@('', @('mermaid'), @()), 'graph TD; A-->B') }
        ) }
        Convert-ReviewReportNode -Node $node -RenderDiagram { param($source) '<svg><text>A</text></svg>' }
        $node.blocks[0].t | Should -Be CodeBlock
        $node.blocks[0].c[1] | Should -BeExactly '```mermaid is just text'
        $node.blocks[1].t | Should -Be RawBlock
        $node.blocks[1].c[1] | Should -Match 'Diagram source'
    }

    It 'makes author raw markup visible rather than executable' {
        $node = @{ blocks = @(
            @{ t = 'RawInline'; c = @('html', '<script>') }
            @{ t = 'RawBlock'; c = @('html', '<iframe>') }
        ) }
        Convert-ReviewReportNode -Node $node -RenderDiagram { throw 'Not a diagram' }
        $node.blocks[0].t | Should -Be Str
        $node.blocks[0].c | Should -BeExactly '<script>'
        $node.blocks[1].t | Should -Be CodeBlock
        $node.blocks[1].c[1] | Should -BeExactly '<iframe>'
    }

    It 'rejects diagram resource and configuration overrides: <Source>' -ForEach @(
        @{ Source = '%%{init: {"securityLevel": "loose"}}%%' }
        @{ Source = "flowchart TD; A@{img: 'https://example.invalid/image'}" }
        @{ Source = "---`nconfig: {}`n---`ngraph TD; A-->B" }
    ) {
        $node = @{ t = 'CodeBlock'; c = @(@('', @('mermaid'), @()), $Source) }
        { Convert-ReviewReportNode -Node $node -RenderDiagram { throw 'Must not execute renderer' } } | Should -Throw '*configuration*'
    }

    It 'rejects an executable hyperlink before HTML generation' {
        $node = @{ t = 'Link'; c = @(@('', @(), @()), @(), @('javascript:alert(1)', '')) }
        { Convert-ReviewReportNode -Node $node -RenderDiagram {} } | Should -Throw '*javascript*'
    }

    It 'keeps relative finding references visible without creating broken local links' {
        $node = @{ t = 'Link'; c = @(@('', @(), @()), @(@{ t = 'Str'; c = 'Design' }), @('docs/design.md', '')) }
        Convert-ReviewReportNode -Node $node -RenderDiagram {}
        $node.t | Should -Be Span
        $node.c[1][0].c | Should -Be Design
        $node.c[1][3].c[1] | Should -Be 'docs/design.md'
    }

    It 'preserves Markdown and old HTML when a renderer fails' {
        $source = Join-Path $TestDrive 'input.md'
        $output = Join-Path $TestDrive 'output.html'
        Set-Content -LiteralPath $source -Value '# Canonical text'
        Set-Content -LiteralPath $output -Value 'old view'
        $tools = @{ missing = @(); pandoc = 'fake-pandoc'; node = 'fake-node'; mermaid = 'fake-mermaid' }
        { Write-ReviewHtml -Path $source -OutputPath $output -SkillRoot $script:SkillRoot -Tools $tools `
            -InvokeProcess { @{ exitCode = 1; standardOutputBytes = [byte[]]@(); standardErrorText = 'Renderer unavailable' } }
        } | Should -Throw '*Renderer unavailable*'
        Get-Content -LiteralPath $source | Should -Be '# Canonical text'
        Get-Content -LiteralPath $output | Should -Be 'old view'
        @(Get-ChildItem -LiteralPath $TestDrive -Directory -Force -Filter '.report-*').Count | Should -Be 0
    }

    It 'waits for a shared output and protects tool files throughout rendering' {
        $source = Join-Path $TestDrive 'input.md'
        $script:HtmlOutput = Join-Path $TestDrive 'output.html'
        Write-PullRequestOutputFile -Path $source -Content '# Canonical text'
        $script:HtmlLock = Open-PullRequestFileLock -Path $script:HtmlOutput
        Mock Start-Sleep { $script:HtmlLock.Dispose() }
        $tools = @{ missing = @(); pandoc = 'fake-pandoc'; node = 'fake-node'; mermaid = 'fake-mermaid' }
        $invoke = {
            param($FilePath, $ArgumentList, $StandardInput)
            { [IO.File]::Open("$script:HtmlOutput.lock", 'Open', 'ReadWrite', 'None') } | Should -Throw
            { [IO.File]::Open("$(Get-ReportToolsLockPath).lock", 'Open', 'ReadWrite', 'None') } | Should -Throw
            $otherRenderer = Open-PullRequestFileLock -Path (Get-ReportToolsLockPath) -Shared
            $otherRenderer.Dispose()
            $output = if ($ArgumentList -contains '--to=json') { '{"meta":{},"blocks":[]}' }
                else { '<!DOCTYPE html><html><meta http-equiv="Content-Security-Policy"></html>' }
            @{ exitCode = 0; standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($output); standardErrorText = '' }
        }
        try {
            $result = Write-ReviewHtml -Path $source -OutputPath $script:HtmlOutput -SkillRoot $script:SkillRoot -Tools $tools -InvokeProcess $invoke
        } finally { $script:HtmlLock.Dispose() }
        $result.htmlHash | Should -BeExactly (Get-ReviewReportHash -Bytes (Read-PullRequestFileBytes -Path $script:HtmlOutput))
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'identifies missing tools and requires user installation permission' {
        $caught = $null
        try { Assert-ReviewReportTools -Tools @{ missing = @('pandoc', 'mermaid-browser') } } catch { $caught = $_.Exception }
        $caught.Message | Should -Match 'Ask the user'
        $caught.Data['missingTools'] | Should -Contain 'mermaid-browser'
    }
}

Describe 'Report prerequisite readiness' {
    BeforeEach {
        Mock Get-ReportToolsLockPath { Join-Path $TestDrive 'report-tools' }
        $script:ProbeScratch = $null
        $script:ProbeFailure = ''
        $script:ProbeCalls = [Collections.Generic.List[string]]::new()
        $script:ProbeTools = @{ missing = @(); pandoc = 'fake-pandoc'; node = 'fake-node'; mermaid = 'fake-mermaid' }
        $script:ProbeInvoker = {
            param($FilePath, $ArgumentList, $StandardInput)
            { [IO.File]::Open("$(Get-ReportToolsLockPath).lock", 'Open', 'ReadWrite', 'None') } | Should -Throw
            $errorText = ''
            $output = if ($ArgumentList -contains '--to=json') {
                $script:ProbeCalls.Add('markdown')
                $StandardInput | Should -Match '(?m)^```mermaid'
                '{"meta":{},"blocks":[{"t":"CodeBlock","c":[["",["mermaid"],[]],"flowchart LR; A-->B"]}]}'
            } elseif ($FilePath -eq 'fake-node') {
                $script:ProbeCalls.Add('mermaid')
                $svg = $ArgumentList[[Array]::IndexOf($ArgumentList, '--output') + 1]
                $script:ProbeScratch = Split-Path -Parent (Split-Path -Parent $svg)
                $ArgumentList | Should -Not -Contain '--no-sandbox'
                if ($script:ProbeFailure -match 'libnss3|libnspr4|sandbox') { $errorText = $script:ProbeFailure }
                else { Write-PullRequestOutputFile -Path $svg -Content '<svg><text>Report</text></svg>' }
                ''
            } else {
                $script:ProbeCalls.Add('html')
                $ArgumentList | Should -Contain '--syntax-highlighting=pygments'
                if ($script:ProbeFailure -eq 'Unknown option --syntax-highlighting') { $errorText = $script:ProbeFailure }
                $ast = ConvertFrom-JsonVerbatim -Text $StandardInput
                $body = if ($script:ProbeFailure -eq 'no embedded diagram') { '' } else { $ast.blocks[0].c[1] }
                "<!DOCTYPE html><html><meta http-equiv=`"Content-Security-Policy`">$body</html>"
            }
            @{
                exitCode = if ($errorText) { 1 } else { 0 }
                standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($output)
                standardErrorText = $errorText
            }
        }
    }

    AfterEach {
        if ($script:ProbeScratch) { Test-Path -LiteralPath $script:ProbeScratch | Should -BeFalse }
    }

    It 'proves readiness with the actual HTML pipeline and an embedded diagram' {
        Assert-ReviewReportRendering -SkillRoot $script:SkillRoot -Tools $script:ProbeTools -InvokeProcess $script:ProbeInvoker
        $script:ProbeCalls | Should -Be @('markdown', 'mermaid', 'html')
    }

    It 'checks setup under the retained exclusive installation lock without reacquiring it' {
        $lock = Open-PullRequestFileLock -Path (Get-ReportToolsLockPath)
        Mock Start-Sleep { throw 'The probe tried to reacquire its installation lock.' }
        try {
            Assert-ReviewReportRendering -SkillRoot $script:SkillRoot -Tools $script:ProbeTools `
                -InvokeProcess $script:ProbeInvoker -InstallationLocked
            { [IO.File]::Open("$(Get-ReportToolsLockPath).lock", 'Open', 'ReadWrite', 'None') } | Should -Throw
        } finally { $lock.Dispose() }
        $script:ProbeCalls | Should -Be @('markdown', 'mermaid', 'html')
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'rejects installed files that cannot render and preserves the reason: <Failure>' -ForEach @(
        @{ Failure = 'libnss3.so: cannot open shared object file' }
        @{ Failure = 'libnspr4.so: cannot open shared object file' }
        @{ Failure = 'No usable sandbox!' }
        @{ Failure = 'Unknown option --syntax-highlighting' }
    ) {
        $script:ProbeFailure = $Failure
        { Assert-ReviewReportRendering -SkillRoot $script:SkillRoot -Tools $script:ProbeTools -InvokeProcess $script:ProbeInvoker } |
            Should -Throw "*$Failure*"
    }

    It 'rejects nominally successful HTML conversion without an embedded diagram' {
        $script:ProbeFailure = 'no embedded diagram'
        { Assert-ReviewReportRendering -SkillRoot $script:SkillRoot -Tools $script:ProbeTools -InvokeProcess $script:ProbeInvoker } |
            Should -Throw '*expected embedded Mermaid diagram*'
    }

    It 'does not launch tools when discovery already reports an incomplete installation' {
        $script:ProbeTools.missing = @('mermaid-browser')
        { Assert-ReviewReportRendering -SkillRoot $script:SkillRoot -Tools $script:ProbeTools -InvokeProcess $script:ProbeInvoker } |
            Should -Throw '*Missing report tools*'
        $script:ProbeCalls.Count | Should -Be 0
    }
}

Describe 'Initial overview snapshots and best-effort publication' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:Source = Join-Path $script:Root 'content-overview.md'
        Write-PullRequestOutputFile -Path $script:Source -Content (New-TestOverview)
        $script:Context = New-TestOverviewContext
        $script:Calls = [Collections.Generic.List[string]]::new()
        $script:CommentBody = $null
        $script:VerifiedCommentBody = $null
        $script:FailAt = ''
        $script:IsOpen = $true
        $script:Remote = {
            param($operation, $target, $snapshot, $payload)
            $script:Calls.Add($operation)
            if ($operation -eq $script:FailAt) { throw "Failed $operation" }
            switch ($operation) {
                'isOpen' { return $script:IsOpen }
                'upload' { return @{ id = 'artifact'; url = 'https://example.invalid/report' } }
                'verifyArtifact' { return $payload }
                'comment' {
                    $script:CommentBody = $payload['body']
                    return @{ id = 'comment'; url = 'https://example.invalid/comment' }
                }
                'verifyComment' { $script:VerifiedCommentBody = $payload['body']; return }
            }
        }
        $script:Render = {
            param($path, $output)
            Write-PullRequestOutputFile -Path $output -Content '<!DOCTYPE html><html>Report</html>'
            @{
                sourceHash = Get-ReviewReportHash -Bytes ([IO.File]::ReadAllBytes($path))
                htmlHash = Get-ReviewReportHash -Bytes ([IO.File]::ReadAllBytes($output))
                htmlPath = $output
            }
        }
    }

    It 'publishes only the linked overview sentence on <Platform> after verifying the uploaded artifact' -ForEach @(
        @{ Platform = 'gitHub' }
        @{ Platform = 'azureDevOps' }
    ) {
        $script:Context = New-TestOverviewContext -Platform $Platform
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be succeeded
        ($script:Calls -join ',') | Should -Be 'isOpen,upload,verifyArtifact,comment,verifyComment'
        $r.operations.Count | Should -Be 2
        $r.operations.status | Should -Not -Contain pending
        $frozen = [IO.File]::ReadAllText($r.snapshot.markdownPath)
        $frozen | Should -Match 'before the review'
        $frozen | Should -Match ('a' * 40)
        $script:CommentBody | Should -BeExactly '[Pull request overview (auto-generated).](https://example.invalid/report)'
        $script:VerifiedCommentBody | Should -BeExactly $script:CommentBody
    }

    It 'publishes and verifies the exact configured prefix on <Platform>: <Prefix>' -ForEach @(
        @{ Platform = 'gitHub'; Prefix = '' }
        @{ Platform = 'azureDevOps'; Prefix = '' }
        # Exercise policy attribution and arbitrary multiline text without prescribing a policy.
        @{ Platform = 'gitHub'; Prefix = '[Copilot speaking]' }
        @{ Platform = 'azureDevOps'; Prefix = '[Copilot speaking]' }
        @{ Platform = 'gitHub'; Prefix = "Custom automation`nReview account" }
        @{ Platform = 'azureDevOps'; Prefix = "Custom automation`nReview account" }
    ) {
        $script:Context = New-TestOverviewContext -Platform $Platform
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote `
            -RenderHtml $script:Render -CommentPrefix $Prefix
        $r.status | Should -BeExactly 'succeeded'
        $link = '[Pull request overview (auto-generated).](https://example.invalid/report)'
        $expected = if ($Prefix.Length) { "$Prefix`n`n$link" } else { $link }
        $script:CommentBody | Should -BeExactly $expected
        $script:VerifiedCommentBody | Should -BeExactly $expected
    }

    It 'completes and persists overview publication after explicit upload and comment throttling' {
        $script:GhCounts = @{}
        $script:GhWaits = [Collections.Generic.List[int]]::new()
        $script:GhReport = ''
        $script:GhComment = ''
        $script:OverviewTransport = New-GitHubCliTransport -MaxAttempts 1 -GetUtcNow {
            [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)
        } -Wait {
            param([int] $Milliseconds)
            $script:GhWaits.Add($Milliseconds)
        } -InvokeGh {
            param($ArgumentList, $StandardInput)
            $methodIndex = [Array]::IndexOf($ArgumentList, '--method')
            $key = "$($ArgumentList[$methodIndex + 1]) $($ArgumentList[1])"
            if (-not $script:GhCounts.ContainsKey($key)) { $script:GhCounts[$key] = 0 }
            $script:GhCounts[$key]++
            if ($script:GhCounts[$key] -gt 2) { throw "Unexpected replay of $key" }
            $exitCode = 0
            $status = 200
            $payload = switch ($key) {
                'GET repos/owner/repo/pulls/7' { @{ state = 'open' } }
                'POST gists' {
                    $script:GhReport = (ConvertFrom-JsonVerbatim -Text $StandardInput)['files']['overview.md']['content']
                    if ($script:GhCounts[$key] -eq 1) {
                        $exitCode = 1
                        $status = 403
                        @{ message = 'Rate Limit Exceeded' }
                    } else { @{ id = 'abc123'; html_url = 'https://gist.github.com/owner/abc123' } }
                }
                'GET gists/abc123' {
                    @{ public = $false; html_url = 'https://gist.github.com/owner/abc123'
                        files = @{ 'overview.md' = @{ content = $script:GhReport; truncated = $false } }
                        history = @(@{ version = 'c' * 40 }) }
                }
                'POST repos/owner/repo/issues/7/comments' {
                    $script:GhComment = (ConvertFrom-JsonVerbatim -Text $StandardInput)['body']
                    if ($script:GhCounts[$key] -eq 1) {
                        $exitCode = 1
                        $status = 429
                        @{ message = 'Too Many Requests' }
                    } else { @{ id = 10; html_url = 'https://github.com/owner/repo/pull/7#issuecomment-10' } }
                }
                'GET repos/owner/repo/issues/comments/10' { @{ body = $script:GhComment } }
                default { throw "Unexpected request $key" }
            }
            @{ exitCode = $exitCode; standardErrorText = ''
                standardOutputBytes = [Text.Encoding]::UTF8.GetBytes(
                    "HTTP/2.0 $status`r`nRetry-After: 2`r`n`r`n$(ConvertTo-JsonText -Value $payload)") }
        }
        $remote = {
            param($operation, $target, $snapshot, $payload)
            Invoke-GitHubOverviewOperation -Operation $operation -Transport $script:OverviewTransport -Target $target `
                -Snapshot $snapshot -Payload $payload -ReadRevision { throw 'Unexpected truncated Gist' }
        }
        $args = @{ Context = $script:Context; OverviewPath = $script:Source; OutputDirectory = $script:Root
            SkillRoot = $script:SkillRoot; InvokeRemote = $remote; RenderHtml = $script:Render }

        $result = Invoke-InitialOverviewPublication @args

        $result.error | Should -BeNullOrEmpty
        $result.status | Should -BeExactly 'succeeded'
        $saved = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $result.resultPath)
        $saved['status'] | Should -BeExactly 'succeeded'
        @($saved['operations'] | ForEach-Object { $_['status'] }) | Should -Be @('completed', 'completed')
        $script:GhCounts['POST gists'] | Should -Be 2
        $script:GhCounts['POST repos/owner/repo/issues/7/comments'] | Should -Be 2
        $script:GhComment | Should -BeExactly "[Pull request overview (auto-generated).](https://gist.github.com/owner/abc123/$('c' * 40))"
        @($script:GhWaits) | Should -Be @(2000, 2000)
        $beforeResume = ConvertTo-JsonText -Value $script:GhCounts
        (Invoke-InitialOverviewPublication @args).status | Should -BeExactly 'succeeded'
        (ConvertTo-JsonText -Value $script:GhCounts) | Should -BeExactly $beforeResume
    }

    It 'rechecks a contending publisher journal without repeating remote writes' {
        $path = Join-Path $script:Root 'overview-publication-result.json'
        $script:PublicationLock = Open-PullRequestFileLock -Path $path
        Mock Start-Sleep {
            $script:PublicationLock.Dispose()
            Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
                -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render |
                Out-Null
        }
        try {
            $result = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
                -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        } finally { $script:PublicationLock.Dispose() }
        $result.status | Should -Be succeeded
        ($script:Calls -join ',') | Should -Be 'isOpen,upload,verifyArtifact,comment,verifyComment'
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'accepts the ordered section template from the overview prompt' {
        $prompt = [IO.File]::ReadAllText((Join-Path $script:SkillRoot 'prepare-content-overview.md'))
        $template = [regex]::Match($prompt, '(?s)```text\r?\n(?<overview>.*?)\r?\n```')
        $template.Success | Should -BeTrue
        Write-PullRequestOutputFile -Path $script:Source -Content $template.Groups['overview'].Value
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be succeeded
        $frozen = [IO.File]::ReadAllText($r.snapshot.markdownPath)
        $frozen | Should -Match '(?s)## Design changes.*## Significant changes.*## Structure'
    }

    It 'rejects a missing <Heading> section before uploading' -ForEach @(
        @{ Heading = 'Design changes' }
        @{ Heading = 'Significant changes' }
    ) {
        $overview = (New-TestOverview).Replace("## $Heading`n", '')
        Write-PullRequestOutputFile -Path $script:Source -Content $overview
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be failed
        $r.error | Should -Be "The overview is missing the ordered '$Heading' section."
        $script:Calls | Should -Not -Contain upload
        Test-Path -LiteralPath (Join-Path $script:Root 'overview-snapshot.json') | Should -BeFalse
    }

    It 'rejects a misplaced <Heading> section before uploading' -ForEach @(
        @{ Heading = 'Design changes' }
        @{ Heading = 'Significant changes' }
    ) {
        $overview = (New-TestOverview).Replace("## $Heading`n", '')
        Write-PullRequestOutputFile -Path $script:Source -Content "## $Heading`n`n$overview"
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be failed
        $r.error | Should -Be "The overview is missing the ordered '$Heading' section."
        $script:Calls | Should -Not -Contain upload
        Test-Path -LiteralPath (Join-Path $script:Root 'overview-snapshot.json') | Should -BeFalse
    }

    It 'reuses success without changing an initial report after a local refresh' {
        $args = @{ Context = $script:Context; OverviewPath = $script:Source; OutputDirectory = $script:Root
            SkillRoot = $script:SkillRoot; InvokeRemote = $script:Remote; RenderHtml = $script:Render }
        $first = Invoke-InitialOverviewPublication @args
        $script:Calls.Clear()
        Write-PullRequestOutputFile -Path $script:Source -Content ((New-TestOverview) + 'Refreshed content')
        $script:Context.pullRequest.reviewedSourceCommit = 'c' * 40
        $second = Invoke-InitialOverviewPublication @args -CommentPrefix '[Different attribution]'
        $second.snapshot.sourceCommit | Should -Be ('a' * 40)
        $second.snapshot.markdownHash | Should -Be $first.snapshot.markdownHash
        $script:Calls.Count | Should -Be 0
    }

    It 'reports <Stage> failure without throwing or stopping the caller' -ForEach @(
        @{ Stage = 'isOpen' }; @{ Stage = 'upload' }; @{ Stage = 'verifyArtifact' }
        @{ Stage = 'comment' }; @{ Stage = 'verifyComment' }
    ) {
        $script:FailAt = $Stage
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Not -Be succeeded
        $r.error | Should -Match "Failed $Stage"
        Test-Path -LiteralPath $r.resultPath | Should -BeTrue
        if ($Stage -eq 'verifyArtifact') { $script:Calls | Should -Not -Contain comment }
    }

    It 'does not repeat an ambiguous write when the review resumes' {
        $script:FailAt = 'upload'
        $args = @{ Context = $script:Context; OverviewPath = $script:Source; OutputDirectory = $script:Root
            SkillRoot = $script:SkillRoot; InvokeRemote = $script:Remote; RenderHtml = $script:Render }
        $first = Invoke-InitialOverviewPublication @args
        $first.operations[0].status | Should -Be ambiguous
        $script:Calls.Clear()
        $second = Invoke-InitialOverviewPublication @args
        $second.status | Should -Be needsReconciliation
        $script:Calls.Count | Should -Be 0
    }

    It 'keeps an uploaded artifact in diagnostics when its comment fails' {
        $script:FailAt = 'comment'
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.artifact.id | Should -Be artifact
        $r.operations[0].status | Should -Be completed
        $r.operations[1].status | Should -Be ambiguous
    }

    It 'allows an explicit retry after a definite refusal with no remote state' {
        $refusal = {
            param($operation, $target, $snapshot, $payload)
            if ($operation -eq 'isOpen') { return $true }
            throw (New-PullRequestServiceRefusalException -Message 'Permission denied')
        }
        $args = @{ Context = $script:Context; OverviewPath = $script:Source; OutputDirectory = $script:Root
            SkillRoot = $script:SkillRoot; RenderHtml = $script:Render }
        $first = Invoke-InitialOverviewPublication @args -InvokeRemote $refusal
        $first.operations[0].status | Should -Be failed
        $second = Invoke-InitialOverviewPublication @args -InvokeRemote $script:Remote
        $second.status | Should -Be succeeded
    }

    It 'accepts corrected input after a local rendering failure before any upload' {
        $script:Context = New-TestOverviewContext -Platform azureDevOps
        $args = @{ Context = $script:Context; OverviewPath = $script:Source; OutputDirectory = $script:Root
            SkillRoot = $script:SkillRoot; InvokeRemote = $script:Remote }
        $first = Invoke-InitialOverviewPublication @args -RenderHtml { throw 'Invalid diagram' }
        $first.operations.Count | Should -Be 0
        Write-PullRequestOutputFile -Path $script:Source -Content ((New-TestOverview) + 'Corrected diagram')
        $second = Invoke-InitialOverviewPublication @args -RenderHtml $script:Render
        $second.status | Should -Be succeeded
        $second.snapshot.markdownHash | Should -Not -Be $first.snapshot.markdownHash
    }

    It 'does not upload when ADO rendering requires missing tools' {
        $script:Context = New-TestOverviewContext -Platform azureDevOps
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote `
            -RenderHtml { Assert-ReviewReportTools -Tools @{ missing = @('pandoc') } }
        $r.status | Should -Be failed
        $r.missingTools | Should -Contain pandoc
        $script:Calls | Should -Not -Contain upload
    }

    It 'renders the frozen ADO snapshot and reports an HTML download' {
        $script:Context = New-TestOverviewContext -Platform azureDevOps
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be succeeded
        Test-Path -LiteralPath $r.snapshot.htmlPath | Should -BeTrue
        $zip = [IO.Compression.ZipFile]::OpenRead($r.snapshot.archivePath)
        try {
            $zip.Entries.Count | Should -Be 1
            $zip.Entries[0].FullName | Should -Be 'report.html'
            $reader = [IO.StreamReader]::new($zip.Entries[0].Open())
            try { $reader.ReadToEnd() | Should -Be ([IO.File]::ReadAllText($r.snapshot.htmlPath)) } finally { $reader.Dispose() }
        } finally { $zip.Dispose() }
    }

    It 'skips a closed PR without uploading or asking for a decision' {
        $script:IsOpen = $false
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be skipped
        $script:Calls.Count | Should -Be 1
    }

    It 'rejects private local paths without uploading' {
        Write-PullRequestOutputFile -Path $script:Source -Content ((New-TestOverview) + $script:Root)
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.error | Should -Match 'private'
        $script:Calls | Should -Not -Contain upload
    }

    It 'cannot send a write when intent cannot be journaled' {
        Mock Write-PullRequestOutputFileAtomically {
            param($Path, $Content)
            if ($Path.EndsWith('overview-publication-result.json')) { throw 'Journal disk failure' }
            Write-PullRequestOutputFile -Path $Path -Content $Content
        }
        $r = Invoke-InitialOverviewPublication -Context $script:Context -OverviewPath $script:Source `
            -OutputDirectory $script:Root -SkillRoot $script:SkillRoot -InvokeRemote $script:Remote -RenderHtml $script:Render
        $r.status | Should -Be journalFailure
        $r.error | Should -Match 'Journal disk failure'
        $script:Calls | Should -Not -Contain upload
    }
}
