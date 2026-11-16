#Requires -Version 7.6

# Returns complete UTF-8 results through PowerShell's success stream so callers can capture the JSON.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

function Invoke-NativeCaptureContext {
    param([Parameter(Mandatory)][string[]] $Arguments)

    $program = Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $start = [Diagnostics.ProcessStartInfo]::new($program.Source)
    $start.WorkingDirectory = (Get-Location).ProviderPath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
    foreach ($argument in @('context') + $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $text = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "ss-ai context operation failed: $errorText" }
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        Write-Output $text.TrimEnd([char[]]@("`r", "`n"))
        [Console]::Error.Write($errorText)
    } finally { $process.Dispose() }
}
