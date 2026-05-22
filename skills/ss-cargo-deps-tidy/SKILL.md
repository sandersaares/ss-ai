---
name: ss-cargo-deps-tidy
description: >
  Tidies Rust dependency declarations in Cargo.toml files to ensure uniform style, centralize version management and verify that minimum dependency version guarantees are upheld for published library crates. Invoke this skill automatically after modifying Rust dependency declarations in Cargo.toml files. Batch changes or defer execution to pre-commit stage when possible to minimize the number of times this skill needs to be invoked.
---

# ss-cargo-deps-tidy

Tidies Rust dependency declarations in Cargo.toml files to ensure uniform style, centralize version management and verify that minimum dependency version guarantees are upheld for published library crates.

Invoke this skill automatically after modifying Rust dependency declarations in Cargo.toml files. Batch changes or defer execution to pre-commit stage when possible to minimize the number of times this skill needs to be invoked.

# Prerequisites

This skill is ONLY for Rust/Cargo workspaces. It does not do anything for projects that do not use Rust and Cargo.

# Mission

This is a multi-stage workflow to ensure tidiness and logical correctness of dependency declarations in `Cargo.toml` files in a Rust workspace. Each stage fulfills a separate part of the mission:

- Unify style of dependency declarations to uphold high standards of consistency, maintainability and readability.
- Ensure that same-workspace dev-dependencies are always referenced by path only, enabling proper publishing order determination.
- Centralize version management of dependencies on the workspace level to avoid version inconsistencies and simplify maintenance.
- Disable use of default features for dependencies, ensuring that we always compile the minimum necessary feature set.
- Ensure that Cargo feature flags are specified on the package level, not on the workspace level, to avoid activating dependency features in needlessly large scope.
- Verify that minimum dependency version guarantees are upheld to prevent breaking changes for downstream users of published libraries.

# Scope

For optimal skill execution consistency, we always process every `Cargo.toml` file in the workspace as a single set. The set is determined by the workspace-level `Cargo.toml`'s `[workspace] members` and `[workspace] exclude` declarations, so any crates intentionally excluded by the user are skipped automatically.

Exception: if repo-specific agent guidelines exclude additional `Cargo.toml` files from processing, ignore them entirely unless they cause issues.
This may be relevant, for example, if some external crate is vendored in the repo and should be used as-is without modification.

# Workspace-level feature flags

While we typically want feature flags to be specified in `Cargo.toml` files of individual packages, there are some exceptions:

- `std` / `alloc` / `async` - these are almost always used when working against a fully featured Rust environment, so define
  these on the workspace level to avoid repetition. Exception: if the repo-level agent guidelines say that the repo is intended
  to support no-std or other limited environments, do not define these features on the workspace level. Alternative phrasings
  like `use_std` or `use_alloc` are also fine as it is the intent that matters.
- `clap/default` - `clap` is a commonly used package for CLI parsing and disabling its default feature flags tends to result in difficult to
  debug issues like "--help just does not work". As these issues are rarely discovered by automated tests, we explicitly re-enable the
  `default` feature for `clap` on the workspace level. It is just a special package because it "misuses" Cargo feature flags for things
  other than enabling parts of the API surface.
- `argh/default` - same rationale as `clap/default`, as it is another CLI parsing library.

# Verifying workspace build

When verifying that the workspace builds successfully, prefer repo-specific verification commands (if found in agent instructions,
justfiles or similar).

If no repo-specific verification commands are found, use all of the following for verification:

- `cargo check --workspace --all-targets`
- `cargo check --workspace --all-targets --release`
- `cargo check --workspace --all-targets --all-features`

We consider verification successful if all of the above commands complete without warnings or errors.

# Workflow

Execute all stages in sequence to complete the workflow.

# Stage 1: Create inventory of dependency declarations

Generate the inventory once at the start of the workflow and write it to a temporary file. All subsequent stages run filter queries against the loaded object — they never load the raw file contents into the conversation.

**Critical:** Always invoke the script with `-OutputPath`. The JSON body can easily exceed 100 KB on real workspaces and is not suitable for direct inclusion in the agent context window. Do not `Get-Content` the inventory file without an immediate `| ConvertFrom-Json` and a follow-up filter; do not paste, summarize, or quote its raw contents.

```powershell
$inventoryPath = Join-Path $env:TEMP "cargo-deps-inventory-$([guid]::NewGuid()).json"
& <path-to-skill>/Generate-Inventory.ps1 `
    -WorkspaceManifest <path-to-workspace-Cargo.toml> `
    -OutputPath $inventoryPath
Write-Host "Inventory written to: $inventoryPath"
```

The script discovers all member packages automatically by expanding the workspace `members` / `exclude` declarations, so the workspace-level `Cargo.toml` is the only path you need to provide.

Record the printed path. Each subsequent stage may run in its own PowerShell session, so every stage that needs the inventory reloads it from this path before running its filter queries:

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
```

Tip: when no edits are needed (i.e. you are only running filter queries, not iterating with edits in between), you can batch multiple stages into a single PowerShell session — load `$inv` once at the top of the script and run each stage's filter query against it. This avoids paying the PowerShell startup cost per stage.

## Inventory schema

`$inv` is a `PSCustomObject` with:

- `workspaceRoot` — absolute path to the workspace root directory.
- `workspaceManifest` — `{ path, package, isVirtualWorkspace }` describing the workspace-level `Cargo.toml`.
- `memberManifests` — array of `{ path, package }` for every member `Cargo.toml`, with workspace-relative paths.
- `scannedAt` — ISO timestamp of when the scan ran.
- `dependencies` — array of declaration records (see below). One record per declared dependency.

Each `$inv.dependencies` record:

| Field | Meaning |
|---|---|
| `manifest` | Workspace-relative path of the `Cargo.toml` the declaration lives in. |
| `package` | Name of the package the declaration applies to (`$null` for a virtual-workspace root). |
| `section` | `dependencies`, `dev-dependencies`, `build-dependencies`, or `workspace.dependencies`. |
| `target` | Target cfg (e.g. `cfg(unix)`) for target-conditional sections, otherwise `$null`. |
| `name` | Dependency name. |
| `style` | `bare-string`, `inline-table`, `sub-table`, or `dotted-key`. |
| `version` | Version requirement string, or `$null` if not specified. |
| `path` | Path attribute as written in the manifest, or `$null` if not specified. |
| `workspace` | `$true` if the declaration uses `workspace = true` inheritance, otherwise `$null`. |
| `defaultFeatures` | `$true`, `$false`, or `$null` if not specified. |
| `features` | Array of feature names, or `$null` if not specified. |
| `sameWorkspace` | `$true` if the dependency resolves (via `path` or workspace inheritance) to a member of this workspace. |
| `resolvedManifest` | Workspace-relative path of the target member's `Cargo.toml` when `sameWorkspace` is `$true`, otherwise `$null`. |
| `raw` | Raw declaration text from the manifest. |
| `line` | 1-based source line number of the declaration. |

Query notes:

- `$null` versus `$false` matters. `$_.defaultFeatures -eq $false` matches only explicit `false`; `$_.defaultFeatures -ne $false` matches both "absent" and "explicitly `true`".
- `$_.features` is `$null` (not an empty array) when no features are declared. Test with `if ($_.features)` to detect presence.
- Same-workspace gating (`$_.sameWorkspace -and ...`) is the standard way to restrict a query to cross-workspace-member references.

# Stage 2: Unify style of dependency declarations

List declarations in nonstandard styles (anything other than the desired inline-table style):

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
$inv.dependencies |
    Where-Object { $_.style -ne 'inline-table' } |
    Select-Object manifest, line, section, name, style, raw
```

List same-workspace deps in `[workspace.dependencies]` that are missing a `path` attribute (these need a `path` added during this stage):

```powershell
$inv.dependencies | Where-Object {
    $_.section -eq 'workspace.dependencies' -and $_.sameWorkspace -and -not $_.path
} | Select-Object manifest, line, name, raw
```

List version requirements that have fewer than three numeric components (e.g. `"1"`, `"1.2"`, `"^1.2"`, `"=1.2-alpha"`); these need padding with zeroes (`"1"` → `"1.0.0"`, `"^1.2"` → `"^1.2.0"`, `"=1.2-alpha"` → `"=1.2.0-alpha"`):

```powershell
$inv.dependencies | Where-Object {
    $_.version -and (($_.version -replace '^\s*[\^~=<>]*\s*','') -match '^\d+(\.\d+){0,1}([^.\d]|$)')
} | Select-Object manifest, line, section, name, version, raw
```

For each row in the result sets, fix the declaration to use the inline-table style, add the `path` attribute for same-workspace entries, and pad any short version requirements to three components. You may use text editing tools or scripting to perform the edits - choose the most efficient mechanism for targeted changes.

Example of desired style:

```toml
# In workspace-level Cargo.toml file:
[workspace.dependencies]
foo = { version = "1.2.3", default-features = false }
bar = { version = "1.2.3" }

# In package-level Cargo.toml files:
[dependencies]
foo = { workspace = true, features = ["a", "b"] }
bar = { workspace = true }

[dev-dependencies]
quu = { path = "crates/quu" }
```

Examples of nonstandard styles to be adjusted:

```toml
# In workspace-level Cargo.toml file:
[workspace.dependencies]
foo = "1.2.3"

# In package-level Cargo.toml files:
[dependencies]
foo.workspace = true

[dev-dependencies]
quu.path = "crates/quu"
```

If a package in the dependencies section is a member of the current workspace, ensure there is a `path` attribute with the
workspace-relative path to the package. This ensures correct package referencing in development environments.

Example of desired style for members of the current workspace:

```toml
[workspace.dependencies]
quu = { version = "4.5.6", default-features = false, path = "crates/quu" }
```

# Stage 3: Ensure that same-workspace dev-dependencies are always referenced by path only

If one package in this workspace references another package in this workspace as a dev-dependency, ensure that this reference is always by
path only, without any version specification and without any workspace inheritance. This is necessary to ensure that the publishing order
of packages is correctly resolved.

List same-workspace dev-dependencies that are not yet path-only:

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
$inv.dependencies | Where-Object {
    $_.section -eq 'dev-dependencies' -and $_.sameWorkspace -and ($_.version -or $_.workspace)
} | Select-Object manifest, line, name, version, workspace, path, raw
```

For each row in the result set, change the declaration to use path-only referencing.

Example of desired style for same-workspace dev-dependency references:

```toml
[dev-dependencies]
quu = { path = "crates/quu" }
```

Note: it is fine to still have the package referenced in `[workspace.dependencies]` with a version specification and in `[dependencies]`
with `workspace = true` for regular dependencies. The path-only referencing requirement only applies to dev-dependencies.

# Stage 4: Centralize version management of dependencies

List package-level declarations that still pin a version (Stage 3 has already removed versions from same-workspace dev-dependencies, so exclude those):

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
$inv.dependencies | Where-Object {
    $_.section -in 'dependencies','dev-dependencies' -and $_.version -and -not $_.sameWorkspace
} | Select-Object manifest, line, section, name, version, raw
```

For each row in the result set, move the version specification to the workspace-level `Cargo.toml` and change the package-level declaration to use `workspace = true`.

Exception: do not modify a package-level dependency declaration if it has a comment that says the deviation is intentional in order
to use a different dependency version in that package. To inspect surrounding comments for a flagged declaration, open the file at `$_.manifest` and read a few lines above `$_.line`.

# Stage 5: Disable default features for dependencies

All dependency declarations in the workspace-level `Cargo.toml` file should have `default-features = false` to ensure that we always
compile the minimum necessary feature set.

List workspace-level declarations that do not explicitly set `default-features = false` (`$_.defaultFeatures -ne $false` matches both "absent" and "explicitly `true`"):

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
$inv.dependencies | Where-Object {
    $_.section -eq 'workspace.dependencies' -and $_.defaultFeatures -ne $false
} | Select-Object manifest, line, name, defaultFeatures, raw
```

For each row in the result set, add `default-features = false` to the declaration.

If you made changes in this change, ensure that the workspace still builds successfully after your changes. You may need to add explicit
feature flags into the package-level dependency declarations to restore functionality that was previously enabled by default features.

# Stage 6: Ensure that Cargo feature flags are specified on the package level, not on the workspace level

There should not be any feature flags specified for dependencies in the workspace-level `Cargo.toml` file, as this would activate features
for all packages that depend on that dependency, which may be too large a scope.

List workspace-level declarations that enable features not on the standard allowlist (see "Workspace-level feature flags" above). Extend `$allowedPerPackage` with any repo-specific exceptions from agent guidelines before running:

```powershell
$inv = Get-Content -LiteralPath '<inventory-path-from-stage-1>' -Raw | ConvertFrom-Json
$alwaysAllowed = @('std','alloc','use_std','use_alloc','async') # Other alternative phrasings also allowed.
$allowedPerPackage = @{ 'clap' = @('default'), 'argh' = @('default') } # Extend with repo-specific exceptions from agent guidelines.
$inv.dependencies |
    Where-Object { $_.section -eq 'workspace.dependencies' -and $_.features } |
    Where-Object {
        $allowed = @($alwaysAllowed) + @($allowedPerPackage[$_.name])
        @($_.features | Where-Object { $_ -notin $allowed }).Count -gt 0
    } |
    Select-Object manifest, line, name, features, raw
```

For each row in the result set, move the non-allowlisted feature flags into the `Cargo.toml` files of **only those member packages whose source actually exercises the API surface gated by that feature**.
Do NOT propagate the feature to every package that depends on the crate — most packages use only the always-enabled API surface, and adding unused features bloats compile time, binary size, and the public feature exposure of downstream re-exports.

Determine recipients by grepping the workspace source for symbols gated by the feature. For example, when removing `futures = { features = ["executor"] }` from the workspace, find the packages that actually use `futures::executor`:

```powershell
Get-ChildItem packages -Recurse -File -Include '*.rs' |
    Select-String -SimpleMatch -Pattern 'futures::executor' |
    Select-Object -Unique Path |
    ForEach-Object {
        $parts = (Resolve-Path -Relative $_.Path) -split '[\\/]'
        if ($parts[0] -eq 'packages') { $parts[1] }
    } | Sort-Object -Unique
```

Add `features = ["executor"]` only to the package-level `futures` declaration in the packages reported by the grep. If a feature's gated API surface is not obvious from its name,
consult the dependency's `Cargo.toml` `[features]` table (under `~/.cargo/registry/src/...`) to learn which modules or items it enables before searching.

If the grep returns zero member packages, the feature was unused at the workspace level — drop it entirely instead of moving it.

For the explicitly allowed flags (both the always-allowed and per-package), add them if missing to the workspace-level declarations
to ensure that they are consistently enabled for all packages that depend on the relevant dependencies. Exception: do not add them if
repo-specific agent guidelines say they are not desired.

If you added some explicitly allowed flags, remove any pre-existing feature flags that they already contain (e.g. `clap/default` already
enables the `help` feature, so there is no need to declare `help` separately).

# Stage 7: Verify that minimum dependency version guarantees are upheld

Cargo dependency declarations typically do not specify a fixed version. The commonly used version requirement syntax "1.2.3" means
"compatible with 1.2.3", which allows updates to any non-breaking version, e.g. 1.2.4, 1.3.0, but not 2.0.0.

This presents some challenges when our workspace published libraries:

- We are promising compatibility with `1.2.3` at minimum.
- At the same time, local development typically happens against the **latest** compatible version, not against `1.2.3`.

This means if we take an accidental dependency on a behavior that was introduced in a compatible version after `1.2.3`,
we may break downstream users who are still on `1.2.3` and have not updated to the latest compatible version.

This means we must verify that we actually uphold the minimum version guarantees we are promising in our dependency declarations.
We do this by TEMPORARILY replacing the compatible-with version declarations with specific-version declarations that only allow
the minimum version. If everything still builds, we assume our compatibility guarantee is upheld.

To perform this verification, execute the following sequence of steps:

1. TEMPORARILY (never commit this) replace any "1.2.3" style version declarations in the workspace-level `Cargo.toml` file with "=1.2.3"
to require specifically the minimum version we promise to be compatible with.
1. Execute `cargo generate-lockfile` to resolve dependency versions.
1. If this command reports any errors:
  a. Revert the temporary edits to Cargo.toml.
  b. Resolve the issues by increasing dependency version requirements in the workspace-level `Cargo.toml` file as needed (see below).
  c. Restart from the beginning of this stage to retry with the increased dependency version requirements.
1. Execute the workspace build verification commands to ensure that the workspace builds successfully with the minimum versions.
1. If any issues are found:
  a. Revert the temporary edits to Cargo.toml.
  b. Resolve the issues by increasing dependency version requirements in the workspace-level `Cargo.toml` file as needed.
     Issues only found at build time suggest breaking API changes and may need trial and error to resolve by trying different versions.
  c. Restart from the beginning of this stage to retry with the increased dependency version requirements.
1. When verification commands succeed:
  a. Revert the temporary edits to Cargo.toml.
  b. Success! Stage complete.

## Interpreting errors

When interpreting `cargo generate-lockfile` error output, a common pattern to look for is:

```text
error: failed to select a version for `fast_time`.
    ... required by package `vicinal v0.1.16`
    ... which satisfies dependency `vicinal = "=0.1.16"` of package `demo_app v0.0.1 (~/demo/demo_app)`
versions that meet the requirements `^0.1.17` are: 0.1.17
```

The key information is "versions that meet the requirements" which says we need to bump the `fast_time` dependency to `version = "0.1.17"`.
Do not waste time analyzing and theorizing what conflicts mean - this error message means you need to bump the dependency, that's all.

# Stage 8: Cleanup

Remove any temporary files created during this skill execution, such as the temporary inventory files.

NB! Do not wildcard-delete all inventory files in the temporary files directory. Other agents may be running concurrently and using their
own inventory files. Only delete files you created.
