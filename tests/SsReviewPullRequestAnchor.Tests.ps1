#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $target = Join-Path (Split-Path -Parent $PSScriptRoot) 'target'
    [void] [IO.Directory]::CreateDirectory($target)
    $script:Request = Join-Path $target (
        'anchor-cli-' + [guid]::NewGuid().ToString('N') + '.json'
    )
}

AfterAll {
    foreach ($path in @($script:Request, "$($script:Request).lock")) {
        if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    }
}

Describe 'Native anchor resolution' {
    It 'returns every batch result and downgrades a line outside the reviewed diff' {
        $request = @{
            anchors = @(
                @{ kind = 'line'; path = 'src/lib.rs'; startLine = 10 }
                @{ kind = 'line'; path = 'src/lib.rs'; startLine = 300 }
                @{ kind = 'file'; path = 'src/unchanged.rs' }
            )
            diff = @{
                files = @(
                    @{
                        path = 'src/lib.rs'; previousPath = $null; status = 'modified'
                        hunks = @(@{ oldStart = 10; oldCount = 8; newStart = 10; newCount = 8 })
                    }
                )
            }
        }
        [IO.File]::WriteAllText($script:Request, ($request | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
        $output = & $script:Native workflow resolve-anchor --request-path $script:Request
        $LASTEXITCODE | Should -Be 0
        $result = $output | ConvertFrom-Json
        $result.results.Count | Should -Be 3
        $result.results[0].anchor.kind | Should -BeExactly 'line'
        $result.results[1].anchor.kind | Should -BeExactly 'file'
        $result.results[1].anchor.wasDowngraded | Should -BeTrue
        $result.results[2].error | Should -Match 'src/unchanged.rs'
    }
}
