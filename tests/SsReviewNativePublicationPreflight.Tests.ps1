#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:FixtureRoot = Join-Path $PSScriptRoot ('.publication-preflight-' + [guid]::NewGuid().ToString('N'))
    $script:MissingBin = Join-Path $script:FixtureRoot 'provider probes'
    $script:Work = Join-Path $script:FixtureRoot 'working directory 雪'
    [void] [IO.Directory]::CreateDirectory($script:MissingBin)
    [void] [IO.Directory]::CreateDirectory($script:Work)

    if ($IsWindows) {
        $providerProbe = "@echo off`r`necho invoked> `"%FAKE_PROVIDER_MARKER%`"`r`nexit /b 19`r`n"
        foreach ($name in @('gh.cmd', 'az.cmd')) {
            [IO.File]::WriteAllText((Join-Path $script:MissingBin $name), $providerProbe, [Text.UTF8Encoding]::new($false))
        }
    } else {
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

    # Preflight rejects malformed local input before provider access.
    function Invoke-NativePublication {
        param([string[]] $Arguments)
        $providerMarker = Join-Path $script:FixtureRoot ('native-provider-' + [guid]::NewGuid().ToString('N'))
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $script:Work
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:MissingBin
        $start.Environment['FAKE_PROVIDER_MARKER'] = $providerMarker
        foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
                providerInvoked = [IO.File]::Exists($providerMarker)
            }
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:FixtureRoot)) { [IO.Directory]::Delete($script:FixtureRoot, $true) }
}

Describe 'Native publication preflight' {
    It 'accepts named native <Operation> options and rejects malformed local input without provider access' -ForEach @(
        @{ Operation = 'publish-final-review' }
        @{ Operation = 'publish-tracker-items' }
    ) {
        [IO.File]::WriteAllText((Join-Path $script:Work 'approved findings 雪.json'), '{')
        [IO.File]::WriteAllText((Join-Path $script:Work 'cache diagnostics.json'), '{}')
        [IO.File]::WriteAllText((Join-Path $script:Work 'captured context.json'), '{}')
        $arguments = @(
            'review', $Operation, '--approved-findings-path=approved findings 雪.json',
            '--diagnostics-path=cache diagnostics.json', '--context-path=captured context.json',
            "--output-directory=native preflight $Operation", '--now-utc=2026-03-12T08:11:12Z'
        )
        if ($Operation -eq 'publish-final-review') {
            $arguments += @('--url=https://github.com/owner/repo/pull/42',
                '--repository-path=.', '--discard-pending-review', '--publish-to-closed-pull-request',
                '--merge-approval-path=merge approval 雪.json', '--tenant=tenant name')
        } else {
            $arguments += @('--url=https://dev.azure.com/org/project', '--work-item-type=Bug',
                '--area-path=project\Area', '--description-field=Microsoft.VSTS.TCM.ReproSteps',
                '--parent-work-item-id=42', '--tenant=tenant name', '--accept-stored-rewrite',
                '--accept-work-item-id=43', ('--accept-expected-body-hash=' + ('a' * 64)),
                ('--accept-stored-body-hash=' + ('b' * 64)))
        }
        $result = Invoke-NativePublication -Arguments $arguments
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Not -BeNullOrEmpty
        $result.error | Should -Not -Match '(?i)unexpected argument'
        $result.providerInvoked | Should -BeFalse
    }
}
