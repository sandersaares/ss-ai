#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.tracker-azure-' + [guid]::NewGuid().ToString('N'))
    $script:EmptyPath = Join-Path $script:Root 'no providers'
    [void] [IO.Directory]::CreateDirectory($script:EmptyPath)

    # Local validation cannot fall through to a real Azure CLI.
    function Invoke-TrackerAzure {
        param([string] $Directory, [string[]] $Extra)
        [void] [IO.Directory]::CreateDirectory($Directory)
        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.WorkingDirectory = $Directory
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:EmptyPath
        foreach ($arg in @('review', 'publish-tracker-items',
                '--url', 'https://dev.azure.com/contoso/Example%20Project',
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

Describe 'Azure tracker command preflight' {
    It 'rejects <Case> before output or provider access' -ForEach @(
        @{ Case = 'missing type'; TestArgs = @('--area-path', 'Example Project'); Expected = '*requires a work item type*' }
        @{ Case = 'missing area'; TestArgs = @('--work-item-type', 'Bug'); Expected = '*requires an area path*' }
        @{ Case = 'another project root'; TestArgs = @('--work-item-type', 'Bug', '--area-path', 'Other\Core'); Expected = '*rooted at the tracker project*' }
        @{ Case = 'empty area segments'; TestArgs = @('--work-item-type', 'Bug', '--area-path', '\\'); Expected = '*area path*' }
    ) {
        $work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        $result = Invoke-TrackerAzure -Directory $work -Extra $TestArgs
        $result.exit | Should -Not -Be 0
        $result.output | Should -BeExactly ''
        $result.error | Should -BeLike $Expected
        [IO.Directory]::Exists((Join-Path $work 'out 雪')) | Should -BeFalse
    }

    It 'accepts a case-insensitive project root before reading named input' {
        $work = Join-Path $script:Root ([guid]::NewGuid().ToString('N'))
        $result = Invoke-TrackerAzure -Directory $work -Extra @('--work-item-type', 'Bug', '--area-path', 'EXAMPLE PROJECT\Core')
        $result.exit | Should -Not -Be 0
        $result.error | Should -Not -BeLike '*area path*'
        $result.error | Should -Match 'approved|missing|open|read|file|No such'
    }
}
