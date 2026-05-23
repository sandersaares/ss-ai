#requires -Version 7.0
<#
.SYNOPSIS
    Program-grader script. Verifies a Rust workspace has been correctly
    tidied by the ss-cargo-deps-tidy skill, by running the skill's own
    inventory script and asserting the resulting structure.

.DESCRIPTION
    Invoked by waza's `program` grader. Reads `WAZA_WORKSPACE_DIR` from
    the environment. Validates two layers:

      Universal stage rules (always enforced):
        - Stage 2: every workspace.dependencies version is padded to
          three or more numeric components.
        - Stage 4: no package-level dependency (regular, dev, build)
          pins a version. Centralization requires `workspace = true`,
          and same-workspace dev-deps must be path-only.
        - Stage 5: every workspace.dependencies entry has
          `default-features = false`.
        - Stage 6: no workspace.dependencies entry declares features
          outside the allowlist (std, alloc, async, use_std, use_alloc,
          plus per-package clap/default and argh/default).

      Fixture-specific facts (from -ExpectationsPath JSON):
        - centralized_deps[]:  must appear in [workspace.dependencies].
        - removed_features[]:  feature must not be in the workspace dep.
        - replaced_features[]: workspace dep must have `must_have` and
          must not have `must_not_have`.
        - bumped_deps[]:       (only when -RequireBumps) workspace dep
          version must differ from `original_version` AND from
          `original_version` zero-padded to three components.

    Violations are reported one per line to stderr. Exit 0 if all pass,
    exit 1 if any fail, exit 2 on setup/configuration errors.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ExpectationsPath,

    [Parameter(Mandatory)]
    [string]$InventoryScript,

    [switch]$RequireBumps
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workspace = $env:WAZA_WORKSPACE_DIR
if (-not $workspace) {
    [Console]::Error.WriteLine('WAZA_WORKSPACE_DIR is not set'); exit 2
}
if (-not (Test-Path $workspace)) {
    [Console]::Error.WriteLine("workspace '$workspace' does not exist"); exit 2
}
if (-not (Test-Path $ExpectationsPath)) {
    [Console]::Error.WriteLine("expectations file '$ExpectationsPath' does not exist"); exit 2
}
if (-not (Test-Path $InventoryScript)) {
    [Console]::Error.WriteLine("inventory script '$InventoryScript' does not exist"); exit 2
}

$workspaceManifest = Join-Path $workspace 'Cargo.toml'
if (-not (Test-Path $workspaceManifest)) {
    [Console]::Error.WriteLine("workspace Cargo.toml not found at $workspaceManifest"); exit 2
}

$inventoryPath = Join-Path $env:TEMP "ss-cargo-deps-tidy-grader-$([guid]::NewGuid()).json"
try {
    $global:LASTEXITCODE = 0
    & $InventoryScript -WorkspaceManifest $workspaceManifest -OutputPath $inventoryPath
    if ($LASTEXITCODE -ne 0) {
        [Console]::Error.WriteLine("Generate-Inventory.ps1 exited with code $LASTEXITCODE"); exit 2
    }
    $inv = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
    $expectations = Get-Content -LiteralPath $ExpectationsPath -Raw | ConvertFrom-Json
} finally {
    Remove-Item -LiteralPath $inventoryPath -ErrorAction SilentlyContinue
}

$violations = [System.Collections.Generic.List[string]]::new()
function Add-Violation { param([string]$Message) $violations.Add($Message) }

# Helpers ---------------------------------------------------------------------

function Strip-Operator { param([string]$VersionString)
    if (-not $VersionString) { return $VersionString }
    return ($VersionString -replace '^\s*[\^~=<>]+\s*', '').Trim()
}

function Get-NumericComponents { param([string]$VersionString)
    $stripped = Strip-Operator $VersionString
    # Drop any pre-release/build suffix, then split on dots.
    $core = ($stripped -split '[-+]', 2)[0]
    $parts = $core -split '\.'
    $out = @()
    foreach ($p in $parts) {
        if ($p -match '^\d+$') { $out += $p } else { break }
    }
    return ,$out
}

function Pad-ToThree { param([string]$VersionString)
    $stripped = Strip-Operator $VersionString
    $core = ($stripped -split '[-+]', 2)[0]
    $parts = @($core -split '\.')
    while ($parts.Count -lt 3) { $parts += '0' }
    return ($parts -join '.')
}

function Get-WorkspaceDep { param([string]$Name)
    return $inv.dependencies | Where-Object {
        $_.section -eq 'workspace.dependencies' -and $_.name -eq $Name
    } | Select-Object -First 1
}

# --- Stage 2: workspace.dependencies versions padded to >=3 components -------

foreach ($dep in @($inv.dependencies | Where-Object {
    $_.section -eq 'workspace.dependencies' -and $_.version
})) {
    $comps = Get-NumericComponents $dep.version
    if ($comps.Count -lt 3) {
        Add-Violation ('Stage 2 (padding): {0} at {1}:{2} has unpadded version "{3}" (need >=3 numeric components)' -f `
            $dep.name, $dep.manifest, $dep.line, $dep.version)
    }
}

# --- Stage 4: no package-level version pins (except path-only same-ws dev) ---

foreach ($dep in @($inv.dependencies | Where-Object {
    $_.section -in 'dependencies','dev-dependencies','build-dependencies' -and $_.version
})) {
    Add-Violation ('Stage 4 (centralization): {0} at {1}:{2} pins version "{3}" at package level (should be workspace = true)' -f `
        $dep.name, $dep.manifest, $dep.line, $dep.version)
}

# --- Stage 5: every workspace.dependencies entry has default-features=false ---

foreach ($dep in @($inv.dependencies | Where-Object {
    $_.section -eq 'workspace.dependencies' -and $_.defaultFeatures -ne $false
})) {
    Add-Violation ('Stage 5 (default-features): {0} at {1}:{2} is missing default-features = false' -f `
        $dep.name, $dep.manifest, $dep.line)
}

# --- Stage 6: no non-allowlisted features at workspace level -----------------

$alwaysAllowed = @('std','alloc','use_std','use_alloc','async')
$allowedPerPackage = @{ 'clap' = @('default'); 'argh' = @('default') }

foreach ($dep in @($inv.dependencies | Where-Object {
    $_.section -eq 'workspace.dependencies' -and $_.features
})) {
    $allowed = @($alwaysAllowed) + @($allowedPerPackage[$dep.name])
    $bad = @($dep.features | Where-Object { $_ -notin $allowed })
    if ($bad.Count -gt 0) {
        Add-Violation ('Stage 6 (features): {0} at {1}:{2} declares non-allowlisted feature(s) [{3}] at workspace level' -f `
            $dep.name, $dep.manifest, $dep.line, ($bad -join ', '))
    }
}

# --- Fixture-specific facts --------------------------------------------------

function Has-Prop {
    param($Obj, [string]$Name)
    if ($null -eq $Obj) { return $false }
    return [bool]($Obj.PSObject.Properties.Match($Name))
}

if ((Has-Prop $expectations 'centralized_deps') -and $expectations.centralized_deps) {
    foreach ($spec in @($expectations.centralized_deps)) {
        $dep = Get-WorkspaceDep $spec.name
        if (-not $dep) {
            Add-Violation ('Centralization: expected ''{0}'' to appear in [workspace.dependencies] but it is missing' -f $spec.name)
        }
    }
}

if ((Has-Prop $expectations 'removed_features') -and $expectations.removed_features) {
    foreach ($spec in @($expectations.removed_features)) {
        $dep = Get-WorkspaceDep $spec.name
        if ($dep -and $dep.features -and ($dep.features -contains $spec.feature)) {
            Add-Violation ('Feature removal: ''{0}'' at workspace level still declares feature ''{1}''' -f `
                $spec.name, $spec.feature)
        }
    }
}

if ((Has-Prop $expectations 'replaced_features') -and $expectations.replaced_features) {
    foreach ($spec in @($expectations.replaced_features)) {
        $dep = Get-WorkspaceDep $spec.name
        if (-not $dep) {
            Add-Violation ('Feature replacement: ''{0}'' not found in [workspace.dependencies]' -f $spec.name)
            continue
        }
        $features = @($dep.features)
        if ((Has-Prop $spec 'must_have') -and $spec.must_have -and ($features -notcontains $spec.must_have)) {
            Add-Violation ('Feature replacement: ''{0}'' is missing required feature ''{1}''' -f `
                $spec.name, $spec.must_have)
        }
        if ((Has-Prop $spec 'must_not_have') -and $spec.must_not_have -and ($features -contains $spec.must_not_have)) {
            Add-Violation ('Feature replacement: ''{0}'' still declares forbidden feature ''{1}''' -f `
                $spec.name, $spec.must_not_have)
        }
    }
}

if ($RequireBumps -and (Has-Prop $expectations 'bumped_deps') -and $expectations.bumped_deps) {
    foreach ($spec in @($expectations.bumped_deps)) {
        $dep = Get-WorkspaceDep $spec.name
        if (-not $dep) {
            Add-Violation ('Stage 7 (bumps): ''{0}'' not found in [workspace.dependencies]' -f $spec.name)
            continue
        }
        $actual   = Strip-Operator $dep.version
        $original = Strip-Operator $spec.original_version
        $padded   = Pad-ToThree $original
        if ($actual -eq $original -or $actual -eq $padded) {
            Add-Violation ('Stage 7 (bumps): ''{0}'' version is "{1}" which matches original "{2}" (or its padded form "{3}") - expected a bump' -f `
                $spec.name, $dep.version, $spec.original_version, $padded)
        }
    }
}

# --- Report ------------------------------------------------------------------

if ($violations.Count -gt 0) {
    [Console]::Error.WriteLine("Found $($violations.Count) tidiness violation(s):")
    foreach ($v in $violations) { [Console]::Error.WriteLine("  - $v") }
    exit 1
}

$summary = "All tidiness checks passed ({0} dependencies inspected)." -f @($inv.dependencies).Count
Write-Output $summary
exit 0
