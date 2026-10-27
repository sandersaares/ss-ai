# Recipes use PowerShell syntax on every supported platform.
# CommandWithArgs preserves positional recipe arguments as data, not PowerShell source.
set shell := ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-CommandWithArgs"]
set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-CommandWithArgs"]
set script-interpreter := ["pwsh"]

# List available recipes.
_default:
    @just --list

# Run every Pester test through the canonical runner.
#   just test
#   just test -FullNameFilter '*Get-SectionInfo*'
# Reconstruct named options and arrays from argv before invoking the runner.
[positional-arguments]
test *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 1); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Suite', 'Pester') + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Run Pester tests for one skill.
#   just test-skill ss-cargo-deps-tidy
#   just test-skill ss-cargo-deps-tidy -FullNameFilter '*Get-SectionInfo*'
[positional-arguments]
test-skill SKILL *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 2); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Path', "./tests/$($args[1])/") + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Run native Clippy and Cargo tests without Pester.
[positional-arguments]
test-rust *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 1); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Suite', 'Rust') + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Prepare owned Rust artifacts; pass an absolute record path via -PrerequisitePath.
[positional-arguments]
test-rust-prepare *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 1); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Suite', 'Rust', '-Phase', 'Prepare') + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Execute a matching prepared selection, including doctests when unfiltered.
[positional-arguments]
test-rust-execute *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 1); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Suite', 'Rust', '-Phase', 'Execute') + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Run all Pester and native checks.
[positional-arguments]
test-all *ARGS:
    Set-StrictMode -Version Latest; $ErrorActionPreference = 'Stop'; $PSNativeCommandUseErrorActionPreference = $true; \
    $forwarded = @($args | Select-Object -Skip 1); \
    . ./scripts/lib/TestRecipeArguments.ps1; \
    $parameters = ConvertFrom-TestRecipeArguments -ArgumentList (@('-Suite', 'All') + $forwarded); \
    & ./scripts/Invoke-Tests.ps1 @parameters; \
    exit $LASTEXITCODE

# Run a single eval spec. Extra args are forwarded to `waza run`.
#   just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml
#   just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml --task trigger-negative-unrelated
eval SPEC *ARGS:
    waza run {{SPEC}} --output-dir ./eval-results {{ARGS}}

# Run every skill's trigger-lane eval. Extra args are forwarded to `waza run`.
#   just eval-triggers
#   just eval-triggers --model claude-opus-4.8 --tags trigger
eval-triggers *ARGS:
    Get-ChildItem evals/*/trigger.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}}; if ($LASTEXITCODE -ne 0) { throw "Waza failed: $($_.FullName)" } }

# Run every skill's workflow-lane eval. Extra args are forwarded to `waza run`.
#   just eval-workflows
#   just eval-workflows --tags workflow
eval-workflows *ARGS:
    Get-ChildItem evals/*/workflow.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}}; if ($LASTEXITCODE -ne 0) { throw "Waza failed: $($_.FullName)" } }

# Run every eval (both lanes, every skill). Extra args are forwarded to `waza run`.
#   just eval-all
#   just eval-all --tags trigger
eval-all *ARGS:
    Get-ChildItem evals/*/*.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}}; if ($LASTEXITCODE -ne 0) { throw "Waza failed: $($_.FullName)" } }