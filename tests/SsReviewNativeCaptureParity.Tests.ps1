#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:LibraryRoot = Join-Path $PSScriptRoot '..\skills\ss-review\scripts\lib'
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestContextQuery.ps1')

    $script:FixturePath = Join-Path $PSScriptRoot 'fixtures\ss-review-native-capture\azure-devops.json'
    $script:Instant = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)
    $script:Scratch = Join-Path $PSScriptRoot ('.capture-parity-' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:Scratch)
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source

    function New-OfflineAdoCapture {
        param([string] $ReplacementContent, [switch] $PreDocument)

        $fixture = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($script:FixturePath, [Text.Encoding]::UTF8))
        if ($PSBoundParameters.ContainsKey('ReplacementContent')) {
            $fixture['responses']['threads']['value'][0]['comments'][0]['content'] = $ReplacementContent
        }
        $script:Responses = $fixture['responses']
        $script:Requests = [Collections.Generic.List[string]]::new()
        $send = {
            param([string] $Method, [string] $Url, [hashtable] $Headers, $Body)
            if ($Method -cne 'GET') { throw "Unexpected provider write: $Method $Url" }
            $script:Requests.Add($Url)
            if ($script:Requests.Count -gt 20) { throw 'Fixture request limit exceeded.' }
            $key = if ($Url -match '/_apis/connectionData\?') { 'connectionData' }
            elseif ($Url -match '/threads\?') { 'threads' }
            elseif ($Url -match '/iterations\?') { 'iterations' }
            elseif ($Url -match '/reviewers\?') { 'reviewers' }
            elseif ($Url -match '/_apis/git/repositories/.*/pullRequests/4242\?') { 'pullRequest' }
            else { throw "Unexpected provider request: $Url" }
            $json = ConvertTo-Json -InputObject $script:Responses[$key] -Depth 100 -Compress
            [ordered]@{
                statusCode = 200
                headers = @{}
                contentType = 'application/json; charset=utf-8'
                contentBytes = [Text.Encoding]::UTF8.GetBytes($json)
            }
        }
        $transport = New-AdoRestTransport -CollectionUrl $fixture['target']['collectionUrl'] `
            -SendRequest $send -GetAccessToken { 'fixture-token' } -Wait { throw 'Unexpected retry' }
        $context = Get-AdoPullRequestContext -Transport $transport -Target $fixture['target']
        $name = if ($PSBoundParameters.ContainsKey('ReplacementContent')) { 'azure-devops-refresh' } else { 'azure-devops' }
        Assert-ProviderExpectation -Context $context -Name $name
        if ($PreDocument) { return $context }
        New-PullRequestContextDocument -Context $context -NowUtc $script:Instant
    }

    function New-OfflineGitHubCapture {
        param([switch] $PreDocument, [string] $ReplacementContent)

        $fixturePath = Join-Path $PSScriptRoot 'fixtures\ss-review-native-capture\github.json'
        $fixture = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($fixturePath, [Text.Encoding]::UTF8))
        if ($PSBoundParameters.ContainsKey('ReplacementContent')) {
            $fixture['responses']['reviewThreads']['data']['repository']['pullRequest']['reviewThreads']['nodes'][0]['comments']['nodes'][0]['body'] = $ReplacementContent
        }
        $script:Responses = $fixture['responses']
        $script:Requests = [Collections.Generic.List[string]]::new()
        $invoke = {
            param([string[]] $ArgumentList, [string] $StandardInput)

            $command = $ArgumentList -join ' '
            $script:Requests.Add($command)
            if ($script:Requests.Count -gt 20) { throw 'Fixture request limit exceeded.' }
            if ($command -match '--method (?!GET\b)') { throw "Unexpected provider write: $command" }
            $key = if ($command -match '^api user --method GET\b') { 'user' }
            elseif ($command -match 'pulls/7/reviews') { 'reviews' }
            elseif ($command -match 'issues/7/comments') { 'issueComments' }
            elseif ($command -match 'requested_reviewers') { 'requestedReviewers' }
            elseif ($command -match 'pulls/7 --method GET') { 'pullRequest' }
            elseif ($command -match 'compare/') { 'compare' }
            elseif ($command -match 'api graphql' -and $StandardInput -match 'reviewThreads') { 'reviewThreads' }
            elseif ($command -match 'api graphql' -and $StandardInput -match 'nodes\{ id lastEditedAt \}') { 'issueEdits' }
            elseif ($command -match 'api graphql' -and $StandardInput -match 'reviews\(first:') { 'reviewEdits' }
            elseif ($command -match 'api graphql' -and $StandardInput -match 'timelineItems') { 'timeline' }
            else { throw "Unexpected provider request: $command $StandardInput" }
            $json = ConvertTo-Json -InputObject $script:Responses[$key] -Depth 100 -Compress
            [ordered]@{
                exitCode = 0
                standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($json)
                standardErrorText = ''
            }
        }
        $transport = New-GitHubCliTransport -InvokeGh $invoke -Wait { throw 'Unexpected retry' } `
            -GetUtcNow { $script:Instant }
        $context = Get-GitHubPullRequestContext -Transport $transport -Target $fixture['target']
        $name = if ($PSBoundParameters.ContainsKey('ReplacementContent')) { 'github-refresh' } else { 'github' }
        Assert-ProviderExpectation -Context $context -Name $name
        if ($PreDocument) { return $context }
        New-PullRequestContextDocument -Context $context -NowUtc $script:Instant
    }

    function Invoke-NativeRead {
        param([string[]] $Arguments)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.Encoding]::UTF8
        $start.StandardErrorEncoding = [Text.Encoding]::UTF8
        foreach ($argument in @('context', 'read') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $text = $output.GetAwaiter().GetResult()
            $errorText = $errors.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0) { throw "Native context read failed: $errorText" }
            $text.TrimEnd("`r", "`n")
        } finally {
            $process.Dispose()
        }
    }

    function Assert-NativePage {
        param($Document, $Options, [string] $Path, [string[]] $Arguments, $Previous = $null)

        $expected = Read-PullRequestContextQuery -Document $Document -Options $Options -Previous $Previous
        $actual = Invoke-NativeRead -Arguments (@('--path', $Path) + $Arguments)
        $actual | Should -BeExactly $expected
    }

    function Assert-RetainedExpectation {
        param($Document, [string] $Name)

        $fixture = Join-Path $PSScriptRoot "fixtures\ss-review-native-capture\$Name-expected.json"
        $expected = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($fixture, [Text.Encoding]::UTF8))
        (ConvertTo-Json -InputObject $Document -Depth 100 -Compress) |
            Should -BeExactly (ConvertTo-Json -InputObject $expected -Depth 100 -Compress)
        Get-PullRequestDocumentHash -Value $Document |
            Should -BeExactly (Get-PullRequestDocumentHash -Value $expected)
        Get-PullRequestEvidenceHash -Document $Document |
            Should -BeExactly (Get-PullRequestEvidenceHash -Document $expected)
    }

    function Assert-ProviderExpectation {
        param($Context, [string] $Name)

        $fixture = Join-Path $PSScriptRoot "fixtures\ss-review-native-capture\$Name-context.json"
        $expected = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($fixture, [Text.Encoding]::UTF8))
        (ConvertTo-Json -InputObject $Context -Depth 100 -Compress) |
            Should -BeExactly (ConvertTo-Json -InputObject $expected -Depth 100 -Compress)
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Scratch)) {
        [IO.Directory]::Delete($script:Scratch, $true)
    }
}

Describe 'Offline provider capture and native reading compatibility' {
    It 'preserves GitHub provider Unicode in the complete native comments page and snapshot' {
        $document = New-OfflineGitHubCapture
        Assert-RetainedExpectation -Document $document -Name 'github'
        $content = 'Alpha ' + [char]0x2014 + ' ' + [char]0x201C + 'gamma' + [char]0x201D +
            ' ' + [char]::ConvertFromUtf32(0x1F916) + ' caf' + [char]0x00E9
        $document['threads'][0]['comments'][0]['content'] | Should -BeExactly $content
        $path = Join-Path $script:Scratch 'github.json'
        Write-PullRequestCapture -Path $path -Document $document | Out-Null
        $effective = Read-PullRequestContextDocument -Path $path
        Assert-NativePage -Document $effective -Path $path `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId 'PRRT_1') `
            -Arguments @('--query', 'comments', '--thread-id', 'PRRT_1')
    }

    It 'preserves provider Unicode and the complete native comments page and snapshot' {
        $document = New-OfflineAdoCapture
        Assert-RetainedExpectation -Document $document -Name 'azure-devops'
        $content = 'Alpha ' + [char]0x2014 + ' ' + [char]0x201C + 'gamma' + [char]0x201D +
            ' ' + [char]::ConvertFromUtf32(0x1F916) + ' caf' + [char]0x00E9
        $document['threads'][0]['comments'][0]['content'] | Should -BeExactly $content
        $document['threads'][0]['comments'][1]['content'] | Should -BeExactly '2026-08-04T10:07:33.677Z'
        $path = Join-Path $script:Scratch 'original.json'
        Write-PullRequestCapture -Path $path -Document $document | Out-Null
        $effective = Read-PullRequestContextDocument -Path $path
        Assert-NativePage -Document $effective -Path $path `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId '101') `
            -Arguments @('--query', 'comments', '--thread-id', '101')
    }

    It 'matches the native refresh change page and retains only unchanged semantic decisions' {
        $previous = New-OfflineAdoCapture
        $previousPath = Join-Path $script:Scratch 'baseline.json'
        Write-PullRequestCapture -Path $previousPath -Document $previous | Out-Null
        $snapshot = Get-PullRequestContextSnapshotId -Document $previous
        Save-CommentAuthorship -Path $previousPath -SnapshotId $snapshot -Classification @(
            @{ threadId = '101'; commentId = '1'; kind = 'human' }
            @{ threadId = '101'; commentId = '2'; kind = 'agent' }
        ) | Out-Null
        $decisions = Read-CommentAuthorship -Path $previousPath -Document $previous
        $current = New-OfflineAdoCapture -ReplacementContent ('Edited ' + [char]::ConvertFromUtf32(0x1F916))
        Assert-RetainedExpectation -Document $current -Name 'azure-devops-refresh'
        $currentPath = Join-Path $script:Scratch 'refresh.json'
        $reused = Write-PullRequestCapture -Path $currentPath -Document $current -PreviousDecisions $decisions
        $reused | Should -Be 1
        $effectivePrevious = Read-PullRequestContextDocument -Path $previousPath
        $effectiveCurrent = Read-PullRequestContextDocument -Path $currentPath
        $effectiveCurrent['threads'][0]['comments'][0]['authorship'] | Should -BeExactly 'indeterminate'
        $effectiveCurrent['threads'][0]['comments'][1]['authorship'] | Should -BeExactly 'agent'
        $delta = Get-PullRequestContextDelta -Previous $effectivePrevious -Current $effectiveCurrent
        @($delta['commentsEdited']).Count | Should -Be 1
        Assert-NativePage -Document $effectiveCurrent -Previous $effectivePrevious -Path $currentPath `
            -Options (New-PullRequestQueryOptions -Query changes) `
            -Arguments @('--query', 'changes', '--compare-path', $previousPath)
        Assert-NativePage -Document $effectiveCurrent -Path $currentPath `
            -Options (New-PullRequestQueryOptions -Query comments -ThreadId '101') `
            -Arguments @('--query', 'comments', '--thread-id', '101')
    }

    It 'keeps the fixed <Provider> refresh expectations tied to the retained model' -ForEach @(
        @{ Provider = 'github' }
        @{ Provider = 'azure-devops' }
    ) {
        $changedText = 'Edited ' + [char]::ConvertFromUtf32(0x1F916)
        if ($Provider -eq 'github') {
            $baseline = New-OfflineGitHubCapture
            $changed = New-OfflineGitHubCapture -ReplacementContent $changedText
            $classification = @(@{ threadId = 'PRRT_1'; commentId = 'PRRC_1'; kind = 'human' })
        } else {
            $baseline = New-OfflineAdoCapture
            $changed = New-OfflineAdoCapture -ReplacementContent $changedText
            $classification = @(
                @{ threadId = '101'; commentId = '1'; kind = 'human' }
                @{ threadId = '101'; commentId = '2'; kind = 'agent' }
            )
        }
        Assert-RetainedExpectation -Document $changed -Name "$Provider-refresh"
        $expectedPath = Join-Path $PSScriptRoot "fixtures\ss-review-native-capture\$Provider-refresh-parity.json"
        $expected = ConvertFrom-JsonVerbatim -Text ([IO.File]::ReadAllText($expectedPath, [Text.Encoding]::UTF8))
        $baselinePath = Join-Path $script:Scratch "$Provider-parity-baseline.json"
        Write-PullRequestCapture -Path $baselinePath -Document $baseline | Out-Null
        $snapshot = Get-PullRequestContextSnapshotId -Document $baseline
        $snapshot | Should -BeExactly $expected['baselineSnapshot']
        Get-PullRequestDocumentHash -Value $baseline | Should -BeExactly $expected['baselineRawHash']
        Get-PullRequestEvidenceHash -Document $baseline | Should -BeExactly $expected['baselineEvidenceHash']
        Save-CommentAuthorship -Path $baselinePath -SnapshotId $snapshot -Classification $classification | Out-Null
        $previous = Read-PullRequestContextDocument -Path $baselinePath
        $decisions = Read-CommentAuthorship -Path $baselinePath -Document $baseline
        Get-PullRequestContextSnapshotId -Document $previous | Should -BeExactly $expected['classifiedSnapshot']
        (ConvertTo-Json -InputObject $decisions -Depth 100 -Compress) |
            Should -BeExactly (ConvertTo-Json -InputObject $expected['baselineDecisions'] -Depth 100 -Compress)
        $refreshPath = Join-Path $script:Scratch "$Provider-parity-refresh.json"
        $reused = Write-PullRequestCapture -Path $refreshPath -Document $changed -PreviousDecisions $decisions
        $reused | Should -Be $expected['reusedClassificationCount']
        $raw = Read-PullRequestContextDocument -Path $refreshPath -WithoutAuthorship
        $effective = Read-PullRequestContextDocument -Path $refreshPath
        $refreshDecisions = Read-CommentAuthorship -Path $refreshPath -Document $raw
        Get-PullRequestDocumentHash -Value $raw | Should -BeExactly $expected['refreshRawHash']
        Get-PullRequestEvidenceHash -Document $effective | Should -BeExactly $expected['refreshEvidenceHash']
        Get-PullRequestContextSnapshotId -Document $effective | Should -BeExactly $expected['refreshSnapshot']
        foreach ($pair in @(
            @{ Actual = $effective; Expected = $expected['refreshEffective'] }
            @{ Actual = $refreshDecisions; Expected = $expected['refreshDecisions'] }
            @{ Actual = (Get-PullRequestContextDeltaSummary -Delta (Get-PullRequestContextDelta -Previous $previous -Current $effective)); Expected = $expected['delta'] }
        )) {
            (ConvertTo-Json -InputObject $pair.Actual -Depth 100 -Compress) |
                Should -BeExactly (ConvertTo-Json -InputObject $pair.Expected -Depth 100 -Compress)
        }
    }
}
