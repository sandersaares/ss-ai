#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:SkillRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review'))
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Scratch = Join-Path $PSScriptRoot ('.publish-overview-caller-' + [guid]::NewGuid().ToString('N'))
    $script:Bin = Join-Path $script:Scratch 'empty bin'
    $script:Work = Join-Path $script:Scratch 'working directory é'
    [void] [IO.Directory]::CreateDirectory($script:Bin)
    [void] [IO.Directory]::CreateDirectory($script:Work)
}

AfterAll {
    if ([IO.Directory]::Exists($script:Scratch)) { [IO.Directory]::Delete($script:Scratch, $true) }
}

Describe 'Native overview publication contract' {
    It 'preserves a caller-owned UTF-8 prefix after malformed input is refused' {
        $prefix = Join-Path $script:Work 'attribution 雪.txt'
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes("café 🤖`nline two")
        [IO.File]::WriteAllBytes($prefix, $bytes)
        [IO.File]::WriteAllText((Join-Path $script:Work 'invalid-context.json'), '{')
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $script:Work
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:Bin
        foreach ($argument in @('review', 'publish-overview', '--context-path=invalid-context.json',
            '--overview-path=missing overview.md', '--output-directory=native output',
            "--skill-root=$script:SkillRoot", "--comment-prefix-path=$prefix", '--tenant=tenant name')) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $process.ExitCode | Should -Not -Be 0
            $stderr.GetAwaiter().GetResult() | Should -Not -BeNullOrEmpty
            $stdout.GetAwaiter().GetResult() | Should -BeNullOrEmpty
            [IO.File]::ReadAllBytes($prefix) | Should -Be $bytes
        } finally { $process.Dispose() }
    }
}
