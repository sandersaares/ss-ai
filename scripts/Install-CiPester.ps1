#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Give a transient gallery download failure two additional attempts, with time
# for the service to recover, without repeating unrelated installation failures.
$delays = @(5, 10)
for ($attempt = 0; $attempt -le $delays.Count; $attempt++) {
    try {
        Install-Module -Name Pester -RequiredVersion 5.7.1 -Scope CurrentUser -Force -SkipPublisherCheck -ErrorAction Stop
        break
    } catch {
        if ($_.Exception.Message -notmatch "^Package 'Pester' failed to download\.?$" -or $attempt -ge $delays.Count) {
            throw
        }
        Write-Warning "Pester download failed; retrying after $($delays[$attempt]) seconds. $($_.Exception.Message)"
        Start-Sleep -Seconds $delays[$attempt]
    }
}
