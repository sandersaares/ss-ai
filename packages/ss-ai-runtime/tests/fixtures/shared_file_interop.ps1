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

. "$PSScriptRoot/../../../../skills/ss-review/scripts/lib/PullRequestModel.ps1"

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
