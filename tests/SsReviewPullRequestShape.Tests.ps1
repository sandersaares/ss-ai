#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Split-Path -Parent $PSScriptRoot
    $script:Paths = [Collections.Generic.List[string]]::new()

    function Invoke-ShapeQuery {
        param([string] $Path)

        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = $script:Native
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
        foreach ($argument in @('context', 'read', '--path', $Path, '--query', 'threads')) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $out = $process.StandardOutput.ReadToEndAsync()
            $err = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $output = $out.GetAwaiter().GetResult()
            $errorOutput = $err.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0) { throw $errorOutput }
            if (@($output.Trim() -split "`r?`n").Count -ne 1) {
                throw 'Expected a single JSON response.'
            }
            $output | ConvertFrom-Json -AsHashtable -DateKind String
        } finally {
            $process.Dispose()
        }
    }
}

AfterAll {
    foreach ($path in $script:Paths) {
        if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
}

Describe 'Native capture shape' {
    It 'reads bounded thread descriptors from both provider captures' {
        foreach ($provider in @('azure-devops', 'github')) {
            $source = Join-Path $PSScriptRoot "fixtures/ss-review-native-capture/$provider-expected.json"
            $path = Join-Path $script:Root "target/shape-$([guid]::NewGuid()).json"
            [IO.File]::Copy($source, $path, $false)
            foreach ($suffix in @('', '.lock', '.authorship.json', '.authorship.json.lock')) {
                $script:Paths.Add("$path$suffix")
            }
            $page = Invoke-ShapeQuery -Path $path
            $page.complete | Should -BeTrue -Because $provider
            $page.items.Count | Should -BeGreaterThan 0 -Because $provider
            foreach ($thread in $page.items) {
                $thread.threadId | Should -Not -BeNullOrEmpty -Because $provider
                $thread.Contains('comments') | Should -BeFalse -Because $provider
                @('active', 'resolved', 'indeterminate') | Should -Contain $thread.resolution
            }
        }
    }
}
