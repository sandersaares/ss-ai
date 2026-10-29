#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Keeps native JSON output on the success stream and diagnostics on failure.
function Invoke-NativeReviewWorkflow {
    param([Parameter(Mandatory)][string[]] $Arguments)

    $program = Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $start = [Diagnostics.ProcessStartInfo]::new($program.Source)
    $start.WorkingDirectory = (Get-Location).ProviderPath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
    foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $text = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "ss-ai workflow operation failed: $errorText" }
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        Write-Output $text.TrimEnd([char[]]@("`r", "`n"))
    } finally { $process.Dispose() }
}

# Keeps arrays intact when returning shared rendered prose to PowerShell publishers.
function Invoke-NativeReviewMarkdown {
    param([Parameter(Mandatory)][System.Collections.IDictionary] $Request)

    $output = Invoke-NativeReviewWorkflowWithJson -Arguments @('render') -PathOption 'request-path' -Value $Request
    $result = ConvertFrom-Json -InputObject $output -AsHashtable -NoEnumerate
    if ($result -is [array]) { , $result } else { $result }
}

# Keeps structured prose out of process arguments and removes each private request after use.
function Invoke-NativeReviewWorkflowWithJson {
    param(
        [Parameter(Mandatory)][string[]] $Arguments,
        [Parameter(Mandatory)][string] $PathOption,
        [Parameter(Mandatory)][System.Collections.IDictionary] $Value
    )

    $name = ".ss-ai-workflow-$([guid]::NewGuid().ToString('N')).json"
    $path = Join-Path (Get-Location).ProviderPath $name
    $options = [IO.FileStreamOptions]::new()
    $options.Mode = [IO.FileMode]::CreateNew
    $options.Access = [IO.FileAccess]::Write
    $options.Share = [IO.FileShare]::None
    if (-not $IsWindows) {
        $options.UnixCreateMode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
    }

    $created = $false
    try {
        $stream = [IO.FileStream]::new($path, $options)
        $created = $true
        try {
            $json = ConvertTo-Json -InputObject $Value -Depth 100 -Compress
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json)
            $stream.Write($bytes, 0, $bytes.Length)
        } finally { $stream.Dispose() }
        Invoke-NativeReviewWorkflow -Arguments ($Arguments + "--$PathOption=$path")
    } finally {
        if ($created) { [IO.File]::Delete($path) }
    }

}
