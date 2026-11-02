#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:OutputRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'target'
    $script:Fixture = Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture/github-expected.json'
    $script:Owned = [Collections.Generic.List[string]]::new()

    function New-ContextFixture {
        param([switch] $UnsupportedSchema)

        $document = [IO.File]::ReadAllText($script:Fixture) | ConvertFrom-Json -AsHashtable -DateKind String
        $document.threads[0].comments[0].content = "2026-08-04T10:33:54.677Z`r`nReview café 🤖"
        if ($UnsupportedSchema) { $document.schemaVersion = 99 }
        $path = Join-Path $script:OutputRoot "context-cli-$([guid]::NewGuid()).json"
        $stream = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($document | ConvertTo-Json -Depth 100 -Compress))
            $stream.Write($bytes, 0, $bytes.Length)
        } finally {
            $stream.Dispose()
        }
        foreach ($suffix in @('', '.lock', '.authorship.json', '.authorship.json.lock')) {
            $script:Owned.Add("$path$suffix")
        }
        $path
    }

    # Exercise relative capture paths by passing a filename from the child's working directory.
    function Invoke-ContextQuery {
        param([string] $Path)

        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = $script:Native
        $start.WorkingDirectory = $script:OutputRoot
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
        foreach ($argument in @('context', 'read', '--path', [IO.Path]::GetFileName($Path),
                '--query', 'comments', '--thread-id', 'PRRT_1', '--comment-id', 'PRRC_1')) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            # Drain both pipes while the child runs so a full pipe cannot block its exit.
            $out = $process.StandardOutput.ReadToEndAsync()
            $err = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            [pscustomobject]@{
                ExitCode = $process.ExitCode
                StandardOutput = $out.GetAwaiter().GetResult()
                StandardError = $err.GetAwaiter().GetResult()
            }
        } finally {
            $process.Dispose()
        }
    }
}

AfterAll {
    foreach ($path in $script:Owned) {
        if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
}

Describe 'Native capture context' {
    It 'reads a relative capture path and preserves the selected body exactly' {
        $path = New-ContextFixture
        $before = [IO.File]::ReadAllBytes($path)
        $result = Invoke-ContextQuery -Path $path
        $result.ExitCode | Should -Be 0 -Because $result.StandardError
        @($result.StandardOutput.Trim() -split "`r?`n").Count | Should -Be 1
        $page = $result.StandardOutput | ConvertFrom-Json -AsHashtable -DateKind String
        $page.complete | Should -BeTrue
        $page.items.Count | Should -Be 1
        $page.items[0].content | Should -BeExactly "2026-08-04T10:33:54.677Z`r`nReview café 🤖"
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) | Should -BeExactly ([Convert]::ToBase64String($before))
    }

    It 'refuses a capture with an unsupported schema without replacing it' {
        $path = New-ContextFixture -UnsupportedSchema
        $before = [IO.File]::ReadAllBytes($path)
        $result = Invoke-ContextQuery -Path $path
        $result.ExitCode | Should -Not -Be 0
        $result.StandardOutput.Trim() | Should -BeNullOrEmpty
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) | Should -BeExactly ([Convert]::ToBase64String($before))
    }
}
