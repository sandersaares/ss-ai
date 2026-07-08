set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
set script-interpreter := ["pwsh"]

# List available recipes.
_default:
    @just --list

# Run every Pester test. Extra args are forwarded to Invoke-Pester.
#   just test
#   just test -FullNameFilter '*Get-SectionInfo*'
test *ARGS:
    Invoke-Pester ./tests/ {{ARGS}}

# Run Pester tests for one skill. Extra args are forwarded to Invoke-Pester.
#   just test-skill ss-cargo-deps-tidy
#   just test-skill ss-cargo-deps-tidy -FullNameFilter '*Get-SectionInfo*'
test-skill SKILL *ARGS:
    Invoke-Pester ./tests/{{SKILL}}/ {{ARGS}}

# Run a single eval spec. Extra args are forwarded to `waza run`.
#   just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml
#   just eval evals/ss-cargo-deps-tidy/trigger.eval.yaml --task trigger-negative-unrelated
eval SPEC *ARGS:
    waza run {{SPEC}} --output-dir ./eval-results {{ARGS}}

# Run every skill's trigger-lane eval. Extra args are forwarded to `waza run`.
#   just eval-triggers
#   just eval-triggers --model claude-opus-4.8 --tags trigger
eval-triggers *ARGS:
    Get-ChildItem evals/*/trigger.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}} }

# Run every skill's workflow-lane eval. Extra args are forwarded to `waza run`.
#   just eval-workflows
#   just eval-workflows --tags workflow
eval-workflows *ARGS:
    Get-ChildItem evals/*/workflow.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}} }

# Run every eval (both lanes, every skill). Extra args are forwarded to `waza run`.
#   just eval-all
#   just eval-all --tags trigger
eval-all *ARGS:
    Get-ChildItem evals/*/*.eval.yaml | ForEach-Object { waza run $_.FullName --output-dir ./eval-results {{ARGS}} }