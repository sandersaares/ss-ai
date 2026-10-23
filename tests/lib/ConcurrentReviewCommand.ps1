#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Forces every executable into actual lock contention, then admits retries individually.
# Pipe messages control progress; EOF exposes a failed child without a timeout or polling loop.
function Invoke-ContendedReviewCommand {
    param([Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][object[]] $Command)
    $processes = [Collections.Generic.List[object]]::new()
    $results = [Collections.Generic.List[object]]::new()
    $lock = Open-PullRequestFileLock -Path $Path
    try {
        foreach ($item in $Command) {
            $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
            $start.UseShellExecute = $false
            $start.RedirectStandardInput = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            foreach ($argument in @('-NoProfile', '-NonInteractive', '-File',
                (Join-Path $PSScriptRoot '..\fixtures\Invoke-ContendedReviewCommand.ps1'),
                '-CommandName', $item.name, '-ParametersJson', (ConvertTo-Json -InputObject $item.parameters -Compress -Depth 10))) {
                $start.ArgumentList.Add($argument)
            }
            $process = [Diagnostics.Process]::Start($start)
            $processes.Add(@{ process = $process; errors = $process.StandardError.ReadToEndAsync() })
        }
        foreach ($item in $processes) {
            $signal = $item.process.StandardOutput.ReadLine()
            if ($null -eq $signal) { throw "Child exited before contention: $($item.errors.GetAwaiter().GetResult())" }
            if ($signal -cne 'lock-contended') { throw "Child did not wait for the held lock: $signal" }
        }
        $lock.Dispose()
        foreach ($item in $processes) {
            $item.process.StandardInput.WriteLine('retry')
            $item.process.StandardInput.Close()
            $output = $item.process.StandardOutput.ReadToEnd()
            $item.process.WaitForExit()
            $results.Add(@{ exitCode = $item.process.ExitCode; output = $output; errors = $item.errors.GetAwaiter().GetResult() })
        }
    } finally {
        $lock.Dispose()
        # Close all inputs before waiting so failed assertions cannot strand another contender.
        foreach ($item in $processes) { $item.process.StandardInput.Close() }
        foreach ($item in $processes) {
            $item.process.WaitForExit()
            $item.process.Dispose()
        }
    }
    $results.ToArray()
}
