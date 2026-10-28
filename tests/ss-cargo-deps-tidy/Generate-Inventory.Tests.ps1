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
        [IO.Path]::IsPathFullyQualified($summary.outputPath) | Should -BeTrue
        # Canonical paths can have a verbatim Windows prefix or resolve a parent symlink.
        [IO.File]::ReadAllText($summary.outputPath) | Should -BeExactly ([IO.File]::ReadAllText($output))
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

    It 'centralizes ordinary and target-specific build dependencies' {
        $workspace = Join-Path $TestDrive 'build-workspace'
        [IO.Directory]::CreateDirectory($workspace) | Out-Null
        $manifest = Join-Path $workspace 'Cargo.toml'
        $manifestText = @(
            '[package]'
            'name = "build-consumer"'
            'version = "0.1.0"'
            '[build-dependencies]'
            'build-helper = "1.0.0"'
            '[target.''cfg(unix)''.build-dependencies]'
            'conditional-helper = "1.0.0"'
        ) -join "`n"
        [IO.File]::WriteAllText($manifest, $manifestText)
        $output = Join-Path $TestDrive 'build-inventory.json'
        & $script:UtilityPath cargo inventory --workspace-manifest $manifest --output-path $output | Out-Null
        $skillPath = Join-Path $script:Repository 'skills/ss-cargo-deps-tidy/SKILL.md'
        $skill = [IO.File]::ReadAllText($skillPath)
        $query = [regex]::Match($skill, '(?s)# Stage 4:.*?```powershell\r?\n(.*?)```').Groups[1].Value
        $query | Should -Not -BeNullOrEmpty
        $query = $query.Replace('<inventory-path-from-stage-1>', $output.Replace("'", "''"))
        $dependencies = @(& ([scriptblock]::Create($query)))
        $dependencies.Count | Should -Be 2
        $dependencies.name | Should -Contain 'build-helper'
        $dependencies.name | Should -Contain 'conditional-helper'
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
