#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.tracker-github-' + [guid]::NewGuid().ToString('N'))
    $script:EmptyPath = Join-Path $script:Root 'no providers'
    [void] [IO.Directory]::CreateDirectory($script:EmptyPath)

    # An empty search path prevents any regression from reaching a real provider.
    function Invoke-TrackerGitHub {
        param([string] $Directory, [string[]] $Extra)
        [void] [IO.Directory]::CreateDirectory($Directory)
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $Directory
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:EmptyPath
        foreach ($arg in @('review', 'publish-tracker-items',
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

Describe 'GitHub tracker command preflight' {
    It 'refuses incompatible metadata <Flag> before creating output' -ForEach @(
        @{ Flag = '--work-item-type'; Value = 'Bug' }
        @{ Flag = '--area-path'; Value = 'Project\Core' }
        @{ Flag = '--description-field'; Value = 'System.Description' }
        @{ Flag = '--parent-work-item-id'; Value = '12' }
    ) {
        $work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        $result = Invoke-TrackerGitHub -Directory $work -Extra @('--url', 'https://github.com/acme/tracker', $Flag, $Value)
        $result.exit | Should -Not -Be 0
        $result.output | Should -BeExactly ''
        $result.error | Should -BeLike '*Azure DevOps metadata*GitHub tracker*'
        [IO.Directory]::Exists((Join-Path $work 'out 雪')) | Should -BeFalse
    }

    It 'refuses a URL without a repository before creating output' {
        $work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        $result = Invoke-TrackerGitHub -Directory $work -Extra @('--url', 'https://github.com/acme')
        $result.exit | Should -Not -Be 0
        $result.error | Should -BeLike '*GitHub tracker requires a repository*'
        [IO.Directory]::Exists((Join-Path $work 'out 雪')) | Should -BeFalse
    }
}
