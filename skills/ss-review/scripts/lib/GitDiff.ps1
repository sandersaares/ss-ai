#Requires -Version 7.6

# Review-specific anchor lookup over the shared native Git comparison.
# Exact comparison, unified parsing and path decoding belong to ss-skills-util;
# finding membership remains part of the review workflow.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Calls the installed utility without command-line payload expansion or console decoding.
function Invoke-SkillsGit {
    param(
        [Parameter(Mandatory)]
        [string[]] $ArgumentList,

        [AllowEmptyString()]
        [string] $StandardInput = ''
    )

    $program = Get-Command ss-skills-util -CommandType Application -ErrorAction SilentlyContinue
    if (-not $program) {
        throw 'ss-skills-util is required on PATH. Run scripts\Configure-Pc.ps1 from dev-env before reviewing.'
    }

    $start = [Diagnostics.ProcessStartInfo]::new($program.Source)
    $start.WorkingDirectory = (Get-Location).ProviderPath
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($argument in @('git') + $ArgumentList) { $start.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::Start($start)
    try {
        # Drain both pipes before writing input so large diffs cannot deadlock.
        $outputTask = $process.StandardOutput.ReadToEndAsync()
        $errorTask = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($StandardInput)
        $process.StandardInput.Close()
        $output = $outputTask.GetAwaiter().GetResult()
        $errorText = $errorTask.GetAwaiter().GetResult()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) {
            throw "ss-skills-util Git operation failed: $errorText"
        }
        ConvertFrom-Json -InputObject $output -AsHashtable -Depth 100
    } finally {
        $process.Dispose()
    }
}

# Review publication also checks repository availability before contacting the service.
function Assert-GitInvokerResult {
    param($Result)

    if ($null -eq $Result -or $Result -isnot [System.Collections.IDictionary]) {
        throw 'The Git invoker must return a dictionary with exitCode, standardOutputBytes and standardErrorText.'
    }
    foreach ($key in @('exitCode', 'standardOutputBytes', 'standardErrorText')) {
        if (-not $Result.Contains($key)) { throw "The Git invoker returned no '$key' value." }
    }
}

# These presence probes belong to the review publication preconditions, not diff parsing.
function Test-GitRepositoryPresent {
    param(
        [Parameter(Mandatory)][scriptblock] $Invoker,
        [Parameter(Mandatory)][string] $RepositoryPath
    )

    $result = & $Invoker -ArgumentList @('-C', $RepositoryPath, 'rev-parse', '--git-dir')
    Assert-GitInvokerResult -Result $result
    $result['exitCode'] -eq 0
}

function Test-GitCommitPresent {
    param(
        [Parameter(Mandatory)][scriptblock] $Invoker,
        [Parameter(Mandatory)][string] $RepositoryPath,
        [Parameter(Mandatory)][string] $Commit
    )

    if ([string]::IsNullOrWhiteSpace($Commit)) { throw 'Testing for a commit requires a commit identifier.' }
    $result = & $Invoker -ArgumentList @('-C', $RepositoryPath, 'cat-file', '-e', "$Commit^{commit}")
    Assert-GitInvokerResult -Result $result
    $result['exitCode'] -eq 0
}

# Recorded diff fixtures use the same parser as real native comparisons.
function ConvertFrom-GitDiffText {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text
    )

    $result = Invoke-SkillsGit -ArgumentList @('parse') -StandardInput $Text
    return , @($result['files'])
}

# Keeps the reviewed-commit result shape expected by finding and publication logic.
function Get-GitDiff {
    param(
        [Parameter(Mandatory)][string] $RepositoryPath,
        [Parameter(Mandatory)][string] $MergeBaseCommit,
        [Parameter(Mandatory)][string] $SourceCommit
    )

    $result = Invoke-SkillsGit -ArgumentList @(
        'diff', '--repository-path', $RepositoryPath,
        '--old-commit', $MergeBaseCommit, '--new-commit', $SourceCommit
    )
    [ordered]@{
        mergeBaseCommit = $result['oldCommit']
        sourceCommit = $result['newCommit']
        files = @($result['files'])
    }
}

# Hosting services address a rename by its post-change path only.
function Get-GitDiffFile {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Diff,
        [Parameter(Mandatory)][string] $Path
    )

    $wanted = (Invoke-SkillsGit -ArgumentList @('path', '--value', $Path))['path']
    foreach ($file in $Diff['files']) {
        $entry = $file -as [System.Collections.IDictionary]
        if ($null -eq $entry) { continue }
        if ([string]::Equals([string] $entry['path'], $wanted, [StringComparison]::Ordinal)) {
            return $entry
        }
    }
    return $null
}

# A range must fit one hunk; spanning hunks includes non-commentable unchanged lines.
function Test-GitDiffLineInHunk {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $File,
        [Parameter(Mandatory)][int] $StartLine,
        [int] $EndLine = 0,
        [ValidateSet('right', 'left')][string] $Side = 'right'
    )

    $first = $StartLine
    $last = if ($EndLine -le 0) { $StartLine } else { $EndLine }
    if ($last -lt $first) { $first, $last = $last, $first }
    if ($first -lt 1 -or -not $File.Contains('hunks')) { return $false }

    foreach ($hunk in $File['hunks']) {
        $entry = $hunk -as [System.Collections.IDictionary]
        if ($null -eq $entry) { continue }
        $start = if ($Side -eq 'right') { [long] $entry['newStart'] } else { [long] $entry['oldStart'] }
        $count = if ($Side -eq 'right') { [long] $entry['newCount'] } else { [long] $entry['oldCount'] }
        if ($count -le 0) { continue }
        if ($first -ge $start -and $last -le $start + $count - 1) { return $true }
    }
    return $false
}
