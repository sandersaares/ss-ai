#Requires -Version 7.6

# Reports measured input costs and records failures outside the prepared commands.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [ValidateSet('setupCalls', 'repeatedSetupCalls', 'documentReadCalls', 'firstReadFailures', 'malformedQueryFailures', 'cursorRestarts')]
    [string] $Record,
    [ValidateRange(1, [int]::MaxValue)][int] $Count = 1
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Preserve JSON on PowerShell's success stream for callers that parse it.
function Invoke-NativeContext {
    param([string[]] $Arguments)
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
    } finally { $process.Dispose() }
}

$arguments = [Collections.Generic.List[string]]::new()
$arguments.Add('diagnostics')
$arguments.Add("--path=$Path")
if ($PSBoundParameters.ContainsKey('Record')) { $arguments.Add("--record=$Record") }
if ($PSBoundParameters.ContainsKey('Count')) { $arguments.Add("--count=$Count") }
Invoke-NativeContext -Arguments $arguments.ToArray()
