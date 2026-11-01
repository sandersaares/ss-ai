#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.pull-request-refusals-' + [guid]::NewGuid().ToString('N'))
    $script:EmptyBin = Join-Path $script:Root 'empty provider path'
    [void] [IO.Directory]::CreateDirectory($script:EmptyBin)

    # Refusals must happen without a usable provider on PATH.
    function Invoke-NativeContext {
        param([string[]] $Arguments)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.Environment['PATH'] = $script:EmptyBin
        foreach ($argument in @('context') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) { [IO.Directory]::Delete($script:Root, $true) }
}

Describe 'Native capture and authorship refusals' {
    BeforeEach {
        $script:Work = Join-Path $script:Root ([guid]::NewGuid().ToString('N') + ' 雪')
        [void] [IO.Directory]::CreateDirectory($script:Work)
    }

    It 'refuses a <Command> invocation without required options before provider access' -ForEach @(
        @{ Command = 'create'; Option = '--output-directory' }
        @{ Command = 'set-authorship'; Option = '--path' }
    ) {
        $result = Invoke-NativeContext -Arguments @($Command)
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match ([regex]::Escape($Option))
    }

    It 'refuses <Description> before provider access' -ForEach @(
        @{ Description = 'an unknown platform'; PlatformArguments = @('--platform', 'githup'); ErrorText = 'Unknown capture platform' }
        @{ Description = 'a missing platform'; PlatformArguments = @(); ErrorText = 'requires a URL or platform' }
    ) {
        $arguments = @('create') + $PlatformArguments + @('--organization', 'octo', '--repository', 'widget',
            '--pull-request-id', '7', '--output-directory', $script:Work)
        $result = Invoke-NativeContext -Arguments $arguments
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match ([regex]::Escape($ErrorText))
        Get-ChildItem -LiteralPath $script:Work -Force | Should -BeNullOrEmpty
    }

    It 'rejects a comparison against the path selected for output' {
        $capture = Join-Path $script:Work 'pr-context.json'
        $result = Invoke-NativeContext -Arguments @('create', '--url', 'https://github.com/octo/widget/pull/7',
            '--output-directory', $script:Work, '--compare-path', $capture)
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match '(?i)(same|distinct|alias|already exist)'
        [IO.File]::Exists($capture) | Should -BeFalse
        Get-ChildItem -LiteralPath $script:Work -Force | Should -BeNullOrEmpty
    }

    It 'rejects a baseline from an unsupported capture schema' {
        $baseline = Join-Path $script:Root ([guid]::NewGuid().ToString('N') + '-stale.json')
        [IO.File]::WriteAllText($baseline, '{"schemaVersion":99,"threads":[]}', [Text.UTF8Encoding]::new($false))
        $result = Invoke-NativeContext -Arguments @('create', '--url', 'https://github.com/octo/widget/pull/7',
            '--output-directory', $script:Work, '--compare-path', $baseline)
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match '(?i)(schema|version)'
        $result.error | Should -Match '99'
        [IO.File]::Exists((Join-Path $script:Work 'pr-context.json')) | Should -BeFalse
        Get-ChildItem -LiteralPath $script:Work -Force | Should -BeNullOrEmpty
    }

    It 'rejects classification supplied both inline and by file' {
        $capture = Join-Path $script:Work 'capture.json'
        $result = Invoke-NativeContext -Arguments @('set-authorship', '--path', $capture,
            '--snapshot-id', 'unused', '--classification', '[]',
            '--classification-path', (Join-Path $script:Work 'classification.json'))
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match '(?i)(cannot be used with|conflict)'
        [IO.File]::Exists($capture) | Should -BeFalse
    }

    It 'rejects an absent classification source' {
        $result = Invoke-NativeContext -Arguments @('set-authorship', '--path', (Join-Path $script:Work 'capture.json'),
            '--snapshot-id', 'unused')
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match '(?i)classification'
    }

    It 'rejects a classification payload that is not an array before opening the capture' {
        $capture = Join-Path $script:Work 'capture.json'
        $result = Invoke-NativeContext -Arguments @('set-authorship', '--path', $capture,
            '--snapshot-id', 'unused', '--classification', '{"threadId":1}')
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Match '(?i)JSON array'
        [IO.File]::Exists($capture) | Should -BeFalse
    }
}
