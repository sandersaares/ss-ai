# Recipes use PowerShell syntax on every supported platform.
set shell := ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
set script-interpreter := ["pwsh"]

# List available recipes.
_default:
    @just --list

# Run every Pester test through the canonical runner.
#   just test
#   just test -FullNameFilter '*Get-SectionInfo*'
test *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Suite Pester {{ARGS}}

# Run Pester tests for one skill.
#   just test-skill ss-cargo-deps-tidy
#   just test-skill ss-cargo-deps-tidy -FullNameFilter '*Get-SectionInfo*'
test-skill SKILL *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Path ./tests/{{SKILL}}/ {{ARGS}}

# Run native Clippy and Cargo tests without Pester.
test-rust *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Suite Rust {{ARGS}}

# Prepare owned Rust artifacts; pass an absolute record path via -PrerequisitePath.
test-rust-prepare *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Suite Rust -Phase Prepare {{ARGS}}

# Execute a matching prepared selection, including doctests when unfiltered.
test-rust-execute *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Suite Rust -Phase Execute {{ARGS}}

# Run all Pester and native checks.
test-all *ARGS:
    & ./scripts/Invoke-Tests.ps1 -Suite All {{ARGS}}

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