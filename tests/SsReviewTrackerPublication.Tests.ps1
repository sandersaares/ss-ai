#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.tracker-publication-' + [guid]::NewGuid().ToString('N'))
    $script:EmptyPath = Join-Path $script:Root 'no providers'
    [void] [IO.Directory]::CreateDirectory($script:EmptyPath)

    function Write-TrackerJson {
        param([string] $Path, [object] $Value)
        [IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 30 -Compress),
            [Text.UTF8Encoding]::new($false))
    }

    function New-TrackerFixture {
        $work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        [void] [IO.Directory]::CreateDirectory($work)
        $finding = @{
            schemaVersion = 1; id = 'F-1'; sourceFindingIds = @('F-1'); origin = 'reviewer'
            title = 'Finding 雪'; description = 'A meaningful description.'
            reasoning = 'Reason'; consequence = 'Impact'; recommendedAction = 'Fix'
            anchor = @{ kind = 'file'; path = 'src/lib.rs' }
        }
        $approved = @{
            schemaVersion = 2; target = $null; reviewedSourceCommit = $null
            reviewedMergeBaseCommit = $null; reviewedContextHash = $null; findings = @($finding)
        }
        Write-TrackerJson -Path (Join-Path $work 'approved 雪.json') -Value $approved
        Write-TrackerJson -Path (Join-Path $work 'diagnostics 雪.json') -Value @{}
        return $work
    }

    # Only the native executable is available; no provider can be launched.
    function Invoke-TrackerPublication {
        param([string] $Directory, [string[]] $Extra = @())
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $Directory
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:EmptyPath
        foreach ($arg in @('review', 'publish-tracker-items',
                '--url', 'https://github.com/acme/tracker',
                '--approved-findings-path', 'approved 雪.json', '--diagnostics-path', 'diagnostics 雪.json',
                '--output-directory', 'out 雪', '--now-utc', '2026-01-01T00:00:00Z') + $Extra) {
            $start.ArgumentList.Add($arg)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $out = $process.StandardOutput.ReadToEndAsync()
            $err = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{ exit = $process.ExitCode; output = $out.GetAwaiter().GetResult(); error = $err.GetAwaiter().GetResult() }
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) { [IO.Directory]::Delete($script:Root, $true) }
}

Describe 'Tracker publication command preflight' {
    It 'rejects malformed or duplicate approved files before any provider access' -ForEach @(
        @{ InputText = '{' }
        @{ InputText = '{"schemaVersion":2,"schemaVersion":2}' }
    ) {
        $work = New-TrackerFixture
        [IO.File]::WriteAllText((Join-Path $work 'approved 雪.json'), $InputText, [Text.UTF8Encoding]::new($false))
        $result = Invoke-TrackerPublication -Directory $work
        $result.exit | Should -Not -Be 0
        $result.output | Should -BeExactly ''
        [IO.File]::Exists((Join-Path $work 'out 雪/tracker-publication-result.json')) | Should -BeFalse
    }

    It 'rejects empty approved findings before any provider access' {
        $work = New-TrackerFixture
        $approved = [IO.File]::ReadAllText((Join-Path $work 'approved 雪.json')) | ConvertFrom-Json -AsHashtable
        $approved.findings = @()
        Write-TrackerJson -Path (Join-Path $work 'approved 雪.json') -Value $approved
        $result = Invoke-TrackerPublication -Directory $work
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike '*empty review*no tracker items*'
        [IO.File]::Exists((Join-Path $work 'out 雪/tracker-publication-result.json')) | Should -BeFalse
    }

    It 'rejects a follow-up without its captured discussion' {
        $work = New-TrackerFixture
        $approved = [IO.File]::ReadAllText((Join-Path $work 'approved 雪.json')) | ConvertFrom-Json -AsHashtable
        $approved.findings[0].followUp = @{ threadId = 'T1'; commentId = 'C1' }
        Write-TrackerJson -Path (Join-Path $work 'approved 雪.json') -Value $approved
        $result = Invoke-TrackerPublication -Directory $work
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike '*Follow-up requires a captured discussion*'
    }

    It 'rejects a missing diagnostics file' {
        $work = New-TrackerFixture
        [IO.File]::Delete((Join-Path $work 'diagnostics 雪.json'))
        $result = Invoke-TrackerPublication -Directory $work
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike '*diagnostics*missing*'
    }

    It 'preserves a durable receipt and refuses replay before reading later inputs' {
        $work = New-TrackerFixture
        $out = Join-Path $work 'out 雪'
        [void] [IO.Directory]::CreateDirectory($out)
        $path = Join-Path $out 'tracker-publication-result.json'
        $target = @{
            platform = 'gitHub'; collectionUrl = $null; organization = 'acme'; project = $null
            repository = 'tracker'; webUrl = 'https://github.com/acme/tracker/issues'
        }
        $record = @{
            schemaVersion = 1; startedUtc = '2026-01-01T00:00:00Z'; platform = 'gitHub'; target = $target
            trackerUrl = $target.webUrl; workItemType = $null; areaPath = $null; descriptionField = $null
            parentItemId = $null; planHash = ('a' * 64); status = 'failed'; message = $null
            verification = $null; adjudication = $null
            operations = @(
                @{ operation = 'createItem'; findingId = 'F-1'; title = 'Finding'; outcome = 'succeeded'
                   itemId = '1'; itemNodeId = 'I_1'; url = 'https://github.com/acme/tracker/issues/1'; message = $null }
                @{ operation = 'verifyItems'; findingId = $null; title = $null; outcome = 'pending'
                   itemId = $null; itemNodeId = $null; url = $null; message = $null }
            )
        }
        Write-TrackerJson -Path $path -Value $record
        $before = [Convert]::ToBase64String([IO.File]::ReadAllBytes($path))
        [IO.File]::Delete((Join-Path $work 'approved 雪.json'))
        $result = Invoke-TrackerPublication -Directory $work
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike '*Prior tracker publication requires reconciliation*'
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) | Should -BeExactly $before
    }

    It 'requires an explicit decision and complete acceptance coordinates' -ForEach @(
        @{ TestArgs = @('--accept-work-item-id', '1'); Expected = '*require --accept-stored-rewrite*' }
        @{ TestArgs = @('--accept-stored-rewrite', '--accept-work-item-id', '1'); Expected = '*requires the item ID and both body hashes*' }
    ) {
        $work = New-TrackerFixture
        $result = Invoke-TrackerPublication -Directory $work -Extra $TestArgs
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike $Expected
        [IO.Directory]::Exists((Join-Path $work 'out 雪')) | Should -BeFalse
    }
}
