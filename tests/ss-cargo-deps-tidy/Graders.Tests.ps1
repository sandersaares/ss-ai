BeforeAll {
    $repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Grader = Join-Path $repository 'dev\ss-cargo-deps-tidy\evals\Test-CargoTidied.ps1'
    $script:Inventory = Join-Path $repository 'skills\ss-cargo-deps-tidy\scripts\Generate-Inventory.ps1'
}

Describe 'Cargo tidiness grader' {
    It 'accepts absent optional expectation properties and rejects untidied dependencies without TEMP' {
        $workspace = Join-Path $TestDrive 'workspace'
        New-Item -ItemType Directory -Path $workspace | Out-Null
        $expectations = Join-Path $TestDrive 'expectations.json'
        [IO.File]::WriteAllText($expectations, '{}')
        $manifest = Join-Path $workspace 'Cargo.toml'
        [IO.File]::WriteAllText($manifest, "[workspace]`nmembers = []`n")
        $previous = $env:WAZA_WORKSPACE_DIR
        $previousTemp = $env:TEMP
        try {
            $env:WAZA_WORKSPACE_DIR = $workspace
            $env:TEMP = $null
            & pwsh -NoProfile -File $script:Grader -ExpectationsPath $expectations -InventoryScript $script:Inventory
            $LASTEXITCODE | Should -Be 0

            # Deliberately violates padding and default-feature policy, without needing Cargo.
            [IO.File]::AppendAllText($manifest, "[workspace.dependencies]`nserde = '1'`n")
            & pwsh -NoProfile -File $script:Grader -ExpectationsPath $expectations -InventoryScript $script:Inventory 2>$null
            $LASTEXITCODE | Should -Be 1
        } finally {
            $env:WAZA_WORKSPACE_DIR = $previous
            $env:TEMP = $previousTemp
        }
    }
}
