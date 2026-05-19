# Development

This document covers how to develop and test the skills in this repo. For end-user usage, see [README.md](README.md).

# Testing

Some skills ship with executable helper scripts that have their own unit tests. Tests live under `tests/<skill-name>/` (parallel to `skills/`) and use [Pester](https://pester.dev/) 5.x.

## Prerequisites

- PowerShell 7+
- Pester 5.5.0 or newer:

  ```powershell
  Install-Module Pester -Scope CurrentUser -MinimumVersion 5.5.0 -Force
  ```

## Running tests

Run every test in the repo:

```powershell
Invoke-Pester ./tests/
```

Run tests for a single skill:

```powershell
Invoke-Pester ./tests/ss-cargo-deps-tidy/
```

Filter by `Describe` or `It` name:

```powershell
Invoke-Pester ./tests/ -FullNameFilter '*Get-SectionInfo*'
```
