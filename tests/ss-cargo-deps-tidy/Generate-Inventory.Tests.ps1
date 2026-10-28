#Requires -Version 7.6
#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.5.0' }

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Manifest = Join-Path $script:Repository 'tests\ss-cargo-deps-tidy\fixtures\virtual-workspace\Cargo.toml'
    $utility = Get-Command -Name ss-ai -CommandType Application -All -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $utility) {
        throw "Native utility 'ss-ai' was not found on PATH."
    }
    $script:UtilityPath = $utility.Path
}

Describe 'Native Cargo inventory CLI contract' {
    It 'emits the complete inventory object to stdout when output is omitted' {
        $json = & $script:UtilityPath cargo inventory --workspace-manifest $script:Manifest
        $LASTEXITCODE | Should -Be 0
        $inventory = ($json | Out-String | ConvertFrom-Json)
        $inventory.PSObject.Properties.Name | Should -Contain 'workspaceRoot'
        $inventory.PSObject.Properties.Name | Should -Contain 'workspaceManifest'
        $inventory.PSObject.Properties.Name | Should -Contain 'memberManifests'
        $inventory.PSObject.Properties.Name | Should -Contain 'scannedAt'
        $inventory.PSObject.Properties.Name | Should -Contain 'dependencies'
    }

    It 'writes complete UTF-8 JSON and emits only the small file summary' {
        $output = Join-Path $TestDrive 'inventory.json'
        $summaryJson = & $script:UtilityPath cargo inventory --workspace-manifest $script:Manifest --output-path $output
        $LASTEXITCODE | Should -Be 0
        $summary = ($summaryJson | Out-String | ConvertFrom-Json)
        $summary.outputPath | Should -Be ([IO.Path]::GetFullPath($output))
        [long]$summary.memberCount | Should -BeGreaterThan 0
        [long]$summary.dependencyCount | Should -BeGreaterThan -1

        $inventory = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
        $inventory.PSObject.Properties.Name | Should -Contain 'dependencies'
        $inventory.dependencies.Count | Should -Be $summary.dependencyCount
    }

    It 'defaults the workspace manifest to Cargo.toml' {
        Push-Location -LiteralPath (Split-Path -Parent $script:Manifest)
        try {
            $json = & $script:UtilityPath cargo inventory
            $LASTEXITCODE | Should -Be 0
            $inventory = ($json | Out-String | ConvertFrom-Json)
            $inventory.workspaceManifest | Should -Not -BeNullOrEmpty
        } finally {
            Pop-Location
        }
    }

    It 'returns failure and writes diagnostics to stderr for a missing manifest' {
        $missing = Join-Path $TestDrive 'missing\Cargo.toml'
        $stderrPath = Join-Path $TestDrive 'inventory.stderr.txt'
        $previousNativeErrorPreference = $PSNativeCommandUseErrorActionPreference
        try {
            $PSNativeCommandUseErrorActionPreference = $false
            $stdout = & $script:UtilityPath cargo inventory --workspace-manifest $missing 2>$stderrPath
            $exitCode = $LASTEXITCODE
        } finally {
            $PSNativeCommandUseErrorActionPreference = $previousNativeErrorPreference
        }
        $exitCode | Should -Not -Be 0
        $stderr = Get-Content -LiteralPath $stderrPath -Raw
        $stderr.Length | Should -BeGreaterThan 0
        ($stdout | Out-String).Length | Should -Be 0
    }
}
