#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][ValidateSet('exclusive-owner', 'shared-owner', 'exclusive-contender', 'shared-contender', 'shared-after-signal', 'publication')]
    [string] $Scenario
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The independent .NET participant exercises the shared sibling-lock protocol
# without depending on any particular skill's file helpers.
function Open-PullRequestFileLock {
    param([string] $Path, [switch] $Shared)
    while ($true) {
        try {
            if ($Shared) {
                return [IO.File]::Open("$Path.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::Read, [IO.FileShare]::Read)
            }
            return [IO.File]::Open("$Path.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        } catch [IO.IOException] {
            $cause = $_.Exception.GetBaseException()
            $contended = ($IsWindows -and $cause.HResult -eq [int]0x80070020) -or
                ($IsLinux -and $cause.HResult -eq 11) -or ($IsMacOS -and $cause.HResult -eq 35)
            if (-not $contended) { throw }
            Start-Sleep -Milliseconds 1
        }
    }
}

function Open-PullRequestFileRead {
    param([string] $Path)
    [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        ([IO.FileShare]::Read -bor [IO.FileShare]::Delete))
}

function Read-PullRequestFileText {
    param([string] $Path)
    $stream = Open-PullRequestFileRead -Path $Path
    try {
        $reader = [IO.StreamReader]::new($stream)
        try { $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

function Write-PullRequestOutputFileAtomically {
    param([string] $Path, [string] $Content)
    $staging = "$Path.$([guid]::NewGuid().ToString('N')).staging"
    try {
        [IO.File]::WriteAllText($staging, $Content, [Text.UTF8Encoding]::new($false))
        [IO.File]::Replace($staging, $Path, [NullString]::Value)
    } finally {
        if ([IO.File]::Exists($staging)) { [IO.File]::Delete($staging) }
    }
}

# Yield the retry to the parent so it can release the real native lock.
$script:waitCount = 0
function Start-Sleep {
    param([int] $Milliseconds)
    $script:waitCount++
    if ($script:waitCount -gt 1) { throw 'Unexpected repeated contention.' }
    [Console]::WriteLine('contended')
    if ([Console]::ReadLine() -cne 'retry') { throw 'Expected retry signal.' }
}

switch ($Scenario) {
    { $_ -in 'exclusive-owner', 'shared-owner' } {
        $lock = Open-PullRequestFileLock -Path $Path -Shared:($_ -eq 'shared-owner')
        try {
            [Console]::WriteLine('held')
            if ([Console]::ReadLine() -cne 'release') { throw 'Expected release signal.' }
        } finally { $lock.Dispose() }
        [Console]::WriteLine('released')
    }
    { $_ -in 'exclusive-contender', 'shared-contender' } {
        [Console]::WriteLine('ready')
        if ([Console]::ReadLine() -cne 'acquire') { throw 'Expected acquire signal.' }
        $lock = Open-PullRequestFileLock -Path $Path -Shared:($_ -eq 'shared-contender')
        try { [Console]::WriteLine('acquired') } finally { $lock.Dispose() }
    }
    'shared-after-signal' {
        [Console]::WriteLine('ready')
        if ([Console]::ReadLine() -cne 'acquire') { throw 'Expected acquire signal.' }
        $lock = Open-PullRequestFileLock -Path $Path -Shared
        try { [Console]::WriteLine('acquired') } finally { $lock.Dispose() }
    }
    'publication' {
        $stream = Open-PullRequestFileRead -Path $Path
        try {
            [Console]::WriteLine('reader-open')
            if ([Console]::ReadLine() -cne 'published') { throw 'Expected publication signal.' }
            $reader = [IO.StreamReader]::new($stream)
            try {
                if ($reader.ReadToEnd() -cne '{"version":"original"}') { throw 'Opened snapshot changed.' }
            } finally { $reader.Dispose() }
        } finally { $stream.Dispose() }
        if ((Read-PullRequestFileText -Path $Path) -cne '{"version":"native"}') { throw 'Native publication was incomplete.' }
        $lock = Open-PullRequestFileLock -Path $Path
        try { Write-PullRequestOutputFileAtomically -Path $Path -Content '{"version":"powershell"}' }
        finally { $lock.Dispose() }
        [Console]::WriteLine('published')
    }
}
