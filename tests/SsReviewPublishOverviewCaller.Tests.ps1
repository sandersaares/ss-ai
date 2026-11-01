#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Publisher = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review/scripts/Publish-ContentOverview.ps1'))
    $script:SkillRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review'))
    $script:PowerShell = (Get-Command pwsh -CommandType Application | Select-Object -First 1).Source
    $script:Scratch = Join-Path $PSScriptRoot ('.publish-overview-caller-' + [guid]::NewGuid().ToString('N'))
    $script:Bin = Join-Path $script:Scratch 'fake bin'
    $script:Work = Join-Path $script:Scratch 'working directory é'
    [void] [IO.Directory]::CreateDirectory($script:Bin)
    [void] [IO.Directory]::CreateDirectory($script:Work)
    if (-not $IsWindows) {
        $fake = @'
#!/usr/bin/env pwsh
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$prefixArgument = @($args | Where-Object { $_.StartsWith('--comment-prefix-path=') })
if ($prefixArgument.Count -ne 1) { throw 'Expected one prefix path.' }
$prefixPath = $prefixArgument[0].Substring('--comment-prefix-path='.Length)
$record = @{
    arguments = @($args)
    cwd = (Get-Location).ProviderPath
    prefixPath = $prefixPath
    prefixBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($prefixPath))
    mode = [int] [IO.File]::GetUnixFileMode($prefixPath)
}
[IO.File]::WriteAllText($env:FAKE_OVERVIEW_RECORD, (ConvertTo-Json -InputObject $record -Depth 10 -Compress))
if ($env:FAKE_OVERVIEW_CLEANUP_FAIL -eq 'yes') {
    # Replace the file with a directory so the caller's file deletion fails deterministically.
    [IO.File]::Delete($prefixPath)
    [void] [IO.Directory]::CreateDirectory($prefixPath)
}
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if ($env:FAKE_OVERVIEW_FAIL -eq 'yes') {
    [Console]::Error.Write('fixture native failure')
    exit 19
}
[Console]::Write($env:FAKE_OVERVIEW_OUTPUT)
'@
        $fakePath = Join-Path $script:Bin 'ss-ai'
        [IO.File]::WriteAllText($fakePath, $fake, [Text.UTF8Encoding]::new($false))
        [IO.File]::SetUnixFileMode($fakePath, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
    }

    # Launch a separate caller so stdout and environment match the agent-facing executable boundary.
    function Invoke-OverviewCaller {
        param([string[]] $Arguments, [string] $Output, [switch] $Fail, [switch] $MissingCli, [switch] $CleanupFail)
        $record = Join-Path $script:Scratch ('record-' + [guid]::NewGuid().ToString('N') + '.json')
        $start = [Diagnostics.ProcessStartInfo]::new($script:PowerShell)
        $start.WorkingDirectory = $script:Work
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.Environment['FAKE_OVERVIEW_RECORD'] = $record
        $start.Environment['FAKE_OVERVIEW_OUTPUT'] = $Output
        $start.Environment['FAKE_OVERVIEW_FAIL'] = if ($Fail) { 'yes' } else { 'no' }
        $start.Environment['FAKE_OVERVIEW_CLEANUP_FAIL'] = if ($CleanupFail) { 'yes' } else { 'no' }
        if ($MissingCli) { $start.Environment['PATH'] = $script:Work }
        else { $start.Environment['PATH'] = $script:Bin + [IO.Path]::PathSeparator + $env:PATH }
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', $script:Publisher) + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $result = @{
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
                exitCode = $process.ExitCode
                record = $null
            }
            if ([IO.File]::Exists($record)) {
                $result.record = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($record)) -AsHashtable -NoEnumerate
            }
            return $result
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Scratch)) { [IO.Directory]::Delete($script:Scratch, $true) }
}

Describe 'Native overview publisher caller' -Skip:$IsWindows {
    BeforeEach {
        $script:OutputDirectory = 'output directory'
        $script:Arguments = @('-ContextPath', 'missing context.json', '-OverviewPath', 'missing overview.md', '-OutputDirectory', $script:OutputDirectory)
        $script:ExpectedJson = "{`"status`":`"succeeded`",`"operations`":[`"one`"],`"message`":`"café 🤖`"}`n"
    }

    It 'forwards exact UTF-8 prefix, tenant, cwd and unchanged native JSON' {
        $prefix = "[Copilot speaking] café 🤖`nline two"
        $result = Invoke-OverviewCaller -Arguments ($script:Arguments + @('-CommentPrefix', $prefix, '-Tenant', 'tenant name')) -Output $script:ExpectedJson
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.output | Should -BeExactly $script:ExpectedJson
        $result.record.cwd | Should -BeExactly $script:Work
        $result.record.arguments[0] | Should -BeExactly 'review'
        $result.record.arguments[1] | Should -BeExactly 'publish-overview'
        $namedArguments = @($result.record.arguments | Select-Object -Skip 2)
        $expectedNamedArguments = @(
            '--context-path=missing context.json', '--overview-path=missing overview.md',
            '--output-directory=output directory', "--skill-root=$script:SkillRoot",
            "--comment-prefix-path=$($result.record.prefixPath)", '--tenant=tenant name'
        )
        $namedArguments.Count | Should -Be $expectedNamedArguments.Count
        foreach ($expected in $expectedNamedArguments) {
            @($namedArguments | Where-Object { $_ -ceq $expected }).Count | Should -Be 1
        }
        $result.record.prefixBytes | Should -BeExactly ([Convert]::ToBase64String([Text.UTF8Encoding]::new($false).GetBytes($prefix)))
        $result.record.mode | Should -Be ([int] ([IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite))
        [IO.File]::Exists($result.record.prefixPath) | Should -BeFalse
    }

    It 'writes an empty prefix and omits the optional tenant' {
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output $script:ExpectedJson
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.record.prefixBytes | Should -BeExactly ''
        @($result.record.arguments | Where-Object { $_ -like '--tenant=*' }).Count | Should -Be 0
        [IO.File]::Exists($result.record.prefixPath) | Should -BeFalse
    }

    It 'accepts an explicitly empty prefix' {
        $result = Invoke-OverviewCaller -Arguments ($script:Arguments + @('-CommentPrefix', '')) -Output $script:ExpectedJson
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.record.prefixBytes | Should -BeExactly ''
        [IO.File]::Exists($result.record.prefixPath) | Should -BeFalse
    }

    It 'preserves an explicit native failure status without converting arrays' {
        $json = '{"status":"failed","operations":["only"],"missingTools":[]}'
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output $json
        $result.exitCode | Should -Be 0 -Because $result.error
        $result.output | Should -BeExactly $json
    }

    It 'reports native process failure and removes the prefix without fallback' {
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output '' -Fail
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Match 'fixture native'
        $result.error | Should -Match 'failure'
        $result.error | Should -Match 'exit code 19'
        $result.output | Should -BeNullOrEmpty
        [IO.File]::Exists($result.record.prefixPath) | Should -BeFalse
    }

    It 'preserves successful output when attribution cleanup fails' {
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output $script:ExpectedJson -CleanupFail
        try {
            $result.exitCode | Should -Be 0 -Because $result.error
            $result.output | Should -BeExactly $script:ExpectedJson
            $result.error | Should -Match 'Could not remove overview attribution file'
            $result.error.Contains($result.record.prefixPath) | Should -BeTrue
            [IO.Directory]::Exists($result.record.prefixPath) | Should -BeTrue
        } finally {
            [IO.Directory]::Delete($result.record.prefixPath)
        }
    }

    It 'preserves the native error when attribution cleanup also fails' {
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output '' -Fail -CleanupFail
        try {
            $result.exitCode | Should -Not -Be 0
            $result.output | Should -BeNullOrEmpty
            $result.error | Should -Match 'exit code 19: fixture native'
            $result.error | Should -Match 'failure'
            $result.error | Should -Match 'Could not remove overview attribution file'
            $result.error.Contains($result.record.prefixPath) | Should -BeTrue
            [IO.Directory]::Exists($result.record.prefixPath) | Should -BeTrue
        } finally {
            [IO.Directory]::Delete($result.record.prefixPath)
        }
    }

    It 'fails without a native executable and removes the prefix without fallback' {
        $result = Invoke-OverviewCaller -Arguments $script:Arguments -Output '' -MissingCli
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.record | Should -BeNullOrEmpty
        @(Get-ChildItem -LiteralPath (Join-Path $script:Work $script:OutputDirectory) -Filter '.overview-prefix-*' -Force).Count | Should -Be 0
    }
}
