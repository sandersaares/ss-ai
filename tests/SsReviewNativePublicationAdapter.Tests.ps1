#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Publisher = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review/scripts/Publish-PullRequestReview.ps1'))
    $script:TrackerPublisher = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review/scripts/Publish-ReviewTrackerItems.ps1'))
    $script:PowerShell = (Get-Process -Id $PID).Path
    $script:FixtureRoot = Join-Path $PSScriptRoot ('.publication-adapter-' + [guid]::NewGuid().ToString('N'))
    $script:Bin = Join-Path $script:FixtureRoot 'fake bin'
    $script:MissingBin = Join-Path $script:FixtureRoot 'missing native bin'
    $script:Work = Join-Path $script:FixtureRoot 'working directory 雪'
    [void] [IO.Directory]::CreateDirectory($script:Bin)
    [void] [IO.Directory]::CreateDirectory($script:MissingBin)
    [void] [IO.Directory]::CreateDirectory($script:Work)

    if ($IsWindows) {
        # PowerShell can discover these launchers if a fallback tries a provider.
        $providerProbe = "@echo off`r`necho invoked> `"%FAKE_PROVIDER_MARKER%`"`r`nexit /b 19`r`n"
        foreach ($name in @('gh.cmd', 'az.cmd')) {
            [IO.File]::WriteAllText((Join-Path $script:MissingBin $name), $providerProbe, [Text.UTF8Encoding]::new($false))
        }
    } else {
        # The stand-in is prepared once and records its actual process arguments.
        $fake = @'
#!/usr/bin/env pwsh
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
# PowerShell's script argument parser splits colon-containing tokens; the native
# process command line preserves the exact arguments received from the adapter.
$record = @{ arguments = @([Environment]::GetCommandLineArgs() | Select-Object -Skip 2); cwd = (Get-Location).ProviderPath }
[IO.File]::WriteAllText($env:FAKE_PUBLICATION_RECORD, (ConvertTo-Json -InputObject $record -Depth 10 -Compress), [Text.UTF8Encoding]::new($false))
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::Error.Write($env:FAKE_PUBLICATION_ERROR)
[Console]::Write($env:FAKE_PUBLICATION_OUTPUT)
exit [int] $env:FAKE_PUBLICATION_EXIT
'@
        $fakePath = Join-Path $script:Bin 'ss-ai'
        [IO.File]::WriteAllText($fakePath, $fake, [Text.UTF8Encoding]::new($false))
        [IO.File]::SetUnixFileMode($fakePath, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
        # A fallback to a provider command would leave a marker without contacting a service.
        $providerProbe = @'
#!/bin/sh
printf invoked > "$FAKE_PROVIDER_MARKER"
exit 19
'@
        foreach ($name in @('gh', 'az')) {
            $path = Join-Path $script:MissingBin $name
            [IO.File]::WriteAllText($path, $providerProbe, [Text.UTF8Encoding]::new($false))
            [IO.File]::SetUnixFileMode($path, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
        }
    }

    # A separate PowerShell process exercises executable discovery, streams and cwd.
    function Invoke-PublicationAdapter {
        param(
            [string[]] $Arguments,
            [string] $Output,
            [string] $ErrorText = '',
            [int] $NativeExit = 0,
            [switch] $MissingCli,
            [string] $PublisherPath = $script:Publisher
        )

        $record = Join-Path $script:FixtureRoot ('record-' + [guid]::NewGuid().ToString('N') + '.json')
        $providerMarker = Join-Path $script:FixtureRoot ('provider-' + [guid]::NewGuid().ToString('N'))
        $start = [Diagnostics.ProcessStartInfo]::new($script:PowerShell)
        $start.WorkingDirectory = $script:Work
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.Environment['FAKE_PUBLICATION_RECORD'] = $record
        $start.Environment['FAKE_PUBLICATION_OUTPUT'] = $Output
        $start.Environment['FAKE_PUBLICATION_ERROR'] = $ErrorText
        $start.Environment['FAKE_PUBLICATION_EXIT'] = [string] $NativeExit
        $start.Environment['FAKE_PROVIDER_MARKER'] = $providerMarker
        $start.Environment['PATH'] = if ($MissingCli) {
            $script:MissingBin
        } else {
            $script:Bin + [IO.Path]::PathSeparator + $env:PATH
        }
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', $PublisherPath) + $Arguments) {
            $start.ArgumentList.Add($argument)
        }

        $process = [Diagnostics.Process]::Start($start)
        try {
            # Concurrent reads prevent either redirected pipe from blocking the child.
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $result = @{
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
                exitCode = $process.ExitCode
                record = $null
                providerInvoked = [IO.File]::Exists($providerMarker)
            }
            if ([IO.File]::Exists($record)) {
                $result.record = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($record)) -AsHashtable -NoEnumerate
            }
            return $result
        } finally { $process.Dispose() }
    }

    # Each invocation uses the public named-parameter surface, independently of files.
    function Get-PublicationArguments {
        @(
            '-Url', 'https://github.com/owner/repo/pull/42',
            '-ApprovedFindingsPath', 'approved findings 雪.json',
            '-ContextPath', 'captured context.json',
            '-DiagnosticsPath', 'cache diagnostics.json',
            '-RepositoryPath', 'repository with spaces',
            '-OutputDirectory', 'publication output'
        )
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:FixtureRoot)) { [IO.Directory]::Delete($script:FixtureRoot, $true) }
}

# This launcher needs a real executable because the adapter starts it directly.
# The repository's prepared provider fixture implements provider commands, not
# the publication CLI, and its Windows .cmd launchers require a command shell.
Describe 'Native final review publication adapter' -Skip:$IsWindows {
    BeforeEach {
        $script:Arguments = @(Get-PublicationArguments)
    }

    It 'forwards named inputs, explicit decisions, UTF-8 and the working directory' {
        $json = '{"status":"succeeded","message":"café 🤖","operations":["one"],"missing":[]}'
        $arguments = $script:Arguments + @(
            '-DiscardPendingReview', '-PublishToClosedPullRequest',
            '-MergeApprovalPath', 'merge approval 雪.json', '-Tenant', 'tenant name',
            '-NowUtc', '2026-03-12T10:11:12+02:00'
        )
        $result = Invoke-PublicationAdapter -Arguments $arguments -Output $json
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.output.TrimEnd("`r", "`n") | Should -BeExactly $json
        $result.record.cwd | Should -BeExactly $script:Work
        $result.record.arguments[0] | Should -BeExactly 'review'
        $result.record.arguments[1] | Should -BeExactly 'publish-final-review'
        $named = @($result.record.arguments | Select-Object -Skip 2)
        $nowArguments = @($named | Where-Object { $_ -like '--now-utc=*' })
        $nowArguments.Count | Should -Be 1
        $forwardedNow = [DateTimeOffset]::Parse(
            $nowArguments[0].Substring('--now-utc='.Length),
            [Globalization.CultureInfo]::InvariantCulture
        )
        $inputNow = [DateTimeOffset]::Parse(
            '2026-03-12T10:11:12+02:00',
            [Globalization.CultureInfo]::InvariantCulture
        )
        ($forwardedNow - $inputNow).Ticks | Should -Be 0
        $otherNamed = @($named | Where-Object { $_ -notlike '--now-utc=*' })
        $expected = @(
            '--url=https://github.com/owner/repo/pull/42',
            '--approved-findings-path=approved findings 雪.json',
            '--context-path=captured context.json',
            '--diagnostics-path=cache diagnostics.json',
            '--repository-path=repository with spaces',
            '--output-directory=publication output',
            '--discard-pending-review', '--publish-to-closed-pull-request',
            '--merge-approval-path=merge approval 雪.json', '--tenant=tenant name'
        )
        $otherNamed.Count | Should -Be $expected.Count -Because ($otherNamed | ConvertTo-Json -Compress)
        foreach ($argument in $expected) {
            @($otherNamed | Where-Object { $_ -ceq $argument }).Count | Should -Be 1 -Because $argument
        }
        $parsed = ConvertFrom-Json -InputObject $result.output -AsHashtable
        $parsed.message | Should -BeExactly 'café 🤖'
        ($parsed.operations -is [array]) | Should -BeTrue
        $parsed.operations.Count | Should -Be 1
        ($parsed.missing -is [array]) | Should -BeTrue
        $parsed.missing.Count | Should -Be 0
    }

    It 'omits decisions and optional inputs that were not supplied' {
        $result = Invoke-PublicationAdapter -Arguments $script:Arguments -Output '{"status":"succeeded"}'
        $result.exitCode | Should -Be 0 -Because $result.error
        $named = @($result.record.arguments | Select-Object -Skip 2)
        $named.Count | Should -Be 6 -Because ($named | ConvertTo-Json -Compress)
        @($named | Where-Object { $_ -match '^--(discard-pending-review|publish-to-closed-pull-request|merge-approval-path|tenant|now-utc)(=|$)' }).Count | Should -Be 0
    }

    It 'retains a partial result and native stderr while propagating failure' {
        $json = '{"status":"publishedWithFollowUpFailure","message":"réouverture 雪","operations":["published"],"remaining":[]}  '
        $result = Invoke-PublicationAdapter -Arguments $script:Arguments -Output $json -ErrorText 'fixture failure 雪' -NativeExit 19
        $result.exitCode | Should -Not -Be 0
        $result.output.TrimEnd("`r", "`n") | Should -BeExactly $json
        $result.error | Should -Match 'fixture failure 雪'
        $result.error | Should -Match 'exit code 19'
        $parsed = ConvertFrom-Json -InputObject $result.output -AsHashtable
        $parsed.status | Should -BeExactly 'publishedWithFollowUpFailure'
        ($parsed.operations -is [array]) | Should -BeTrue
        $parsed.operations.Count | Should -Be 1
        ($parsed.remaining -is [array]) | Should -BeTrue
        $parsed.remaining.Count | Should -Be 0
    }

}

Describe 'Missing native final review publication command' {
    It 'fails without the native command instead of falling back' {
        $result = Invoke-PublicationAdapter -Arguments @(Get-PublicationArguments) -Output '' -MissingCli
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match 'ss-ai'
        $result.record | Should -BeNullOrEmpty
        $result.providerInvoked | Should -BeFalse
        [IO.Directory]::Exists((Join-Path $script:Work 'publication output')) | Should -BeFalse
    }
}

Describe 'Native tracker publication adapter' -Skip:$IsWindows {
    It 'forwards destination, adjudication coordinates and Unicode without changing the result' {
        $arguments = @(
            '-Url', 'https://dev.azure.com/org/project',
            '-ApprovedFindingsPath', 'approved findings 雪.json',
            '-DiagnosticsPath', 'cache diagnostics.json',
            '-OutputDirectory', 'publication output',
            '-ContextPath', 'checked capture.json',
            '-WorkItemType', 'Bug', '-AreaPath', 'project\Area',
            '-DescriptionField', 'Microsoft.VSTS.TCM.ReproSteps',
            '-ParentWorkItemId', '42', '-AcceptStoredRewrite',
            '-AcceptWorkItemId', '43', '-AcceptExpectedBodyHash', 'expected',
            '-AcceptStoredBodyHash', 'stored', '-Tenant', 'tenant name'
        )
        $json = '{"status":"needsAdjudication","message":"café 🤖","items":[]}'
        $result = Invoke-PublicationAdapter -PublisherPath $script:TrackerPublisher -Arguments $arguments -Output $json -ErrorText 'decision needed' -NativeExit 19
        $result.exitCode | Should -Not -Be 0
        $result.output.TrimEnd("`r", "`n") | Should -BeExactly $json
        $result.error | Should -Match 'decision needed'
        $result.record.cwd | Should -BeExactly $script:Work
        $result.record.arguments[0] | Should -BeExactly 'review'
        $result.record.arguments[1] | Should -BeExactly 'publish-tracker-items'
        $named = @($result.record.arguments | Select-Object -Skip 2)
        foreach ($argument in @(
            '--url=https://dev.azure.com/org/project',
            '--approved-findings-path=approved findings 雪.json',
            '--diagnostics-path=cache diagnostics.json',
            '--output-directory=publication output',
            '--context-path=checked capture.json', '--work-item-type=Bug',
            '--area-path=project\Area', '--description-field=Microsoft.VSTS.TCM.ReproSteps',
            '--parent-work-item-id=42', '--accept-stored-rewrite',
            '--accept-work-item-id=43', '--accept-expected-body-hash=expected',
            '--accept-stored-body-hash=stored', '--tenant=tenant name'
        )) {
            @($named | Where-Object { $_ -ceq $argument }).Count | Should -Be 1 -Because $argument
        }
    }

    It 'does not fall back to a provider when the native command is missing' {
        $arguments = @('-ApprovedFindingsPath', 'approved.json', '-DiagnosticsPath', 'diagnostics.json', '-OutputDirectory', 'output')
        $result = Invoke-PublicationAdapter -PublisherPath $script:TrackerPublisher -Arguments $arguments -Output '' -MissingCli
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.providerInvoked | Should -BeFalse
    }
}
