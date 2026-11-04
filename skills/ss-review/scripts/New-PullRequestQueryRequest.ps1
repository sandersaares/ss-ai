#Requires -Version 7.6

# Typed selectors avoid embedding model-authored JSON inside shell command strings.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][string] $RequestPath,
    [Parameter(Mandatory)][string] $DiagnosticsPath,
    [Parameter(Mandatory)][string] $Query,
    [string] $Field,
    [string[]] $Files = @(),
    [string] $Scope = 'all',
    [string] $Kind,
    [string] $Resolution,
    [string] $ThreadId,
    [string] $CommentId,
    [string] $Authorship,
    [switch] $IndexOnly,
    [string] $Text,
    [string] $TextPath,
    [string] $ComparePath,
    # Matches the existing bounded query model.
    [int] $PageSize = 20
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Match the query excerpt limit because parser errors can include captured evidence.
function Get-NativeContextErrorExcerpt {
    param([AllowEmptyString()][string] $Text)
    $message = $Text.Trim() -replace '^(?:(?:invalidInput|service): )+', ''
    $length = [Math]::Min(240, $message.Length)
    if ($length -lt $message.Length -and $length -gt 0 -and [char]::IsHighSurrogate($message[$length - 1])) {
        $length--
    }
    $message.Substring(0, $length)
}

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
        if ($process.ExitCode -ne 0) {
            $excerpt = Get-NativeContextErrorExcerpt -Text $errorText
            [Console]::Error.WriteLine("Query preparation failed: $excerpt")
            exit $process.ExitCode
        }
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        Write-Output $text.TrimEnd([char[]]@("`r", "`n"))
    } finally { $process.Dispose() }
}

$arguments = [Collections.Generic.List[string]]::new()
foreach ($argument in @('prepare', "--path=$Path", "--request-path=$RequestPath",
    "--diagnostics-path=$DiagnosticsPath", "--query=$Query", "--scope=$Scope", "--page-size=$PageSize")) {
    $arguments.Add($argument)
}
foreach ($entry in @(
    @('Field', 'field'), @('Kind', 'kind'), @('Resolution', 'resolution'),
    @('ThreadId', 'thread-id'), @('CommentId', 'comment-id'), @('Authorship', 'authorship'),
    @('Text', 'text'), @('TextPath', 'text-path'), @('ComparePath', 'compare-path')
)) {
    if ($PSBoundParameters.ContainsKey($entry[0])) {
        $arguments.Add("--$($entry[1])=$($PSBoundParameters[$entry[0]])")
    }
}
if ($PSBoundParameters.ContainsKey('Files') -and $Files.Count -eq 0) {
    # An explicit empty selection must not become an unrestricted query.
    $arguments.Add('--file=')
} else {
    foreach ($file in $Files) { $arguments.Add("--file=$file") }
}
if ($IndexOnly) { $arguments.Add('--index-only') }
Invoke-NativeContext -Arguments $arguments.ToArray()
