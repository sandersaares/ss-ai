set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
set script-interpreter := ["pwsh"]

_default:
    @just --list

eval-triggers:
    waza run evals/ss-cargo-deps-tidy/trigger.eval.yaml

eval-workflows:
    waza run evals/ss-cargo-deps-tidy/workflow.eval.yaml

eval-all: eval-triggers eval-workflows