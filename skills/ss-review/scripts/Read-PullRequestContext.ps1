#Requires -Version 7.6

# The read-only entry point for model-facing evidence retrieval.
# Ref: docs/implementation.md, "Bounded local query boundary".
[CmdletBinding(PositionalBinding = $false, DefaultParameterSetName = 'Direct')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Saved')] [string] $RequestPath,
    [Parameter(ParameterSetName = 'Saved')] [switch] $Next,
    [Parameter(ParameterSetName = 'Saved')] [switch] $Restart,
    [Parameter(Mandatory, ParameterSetName = 'Direct')] [string] $Path,
    [Parameter(Mandatory, ParameterSetName = 'Direct')] [string] $Query,
    [Parameter(ParameterSetName = 'Direct')] [string] $Field,
    [Parameter(ParameterSetName = 'Direct')] [string] $FilesJson,
    [Parameter(ParameterSetName = 'Direct')] [string] $FilesPath,
    [Parameter(ParameterSetName = 'Direct')] [string] $Scope = 'all',
    [Parameter(ParameterSetName = 'Direct')] [string] $Kind,
    [Parameter(ParameterSetName = 'Direct')] [string] $Resolution,
    [Parameter(ParameterSetName = 'Direct')] [string] $ThreadId,
    [Parameter(ParameterSetName = 'Direct')] [string] $CommentId,
    [Parameter(ParameterSetName = 'Direct')] [string] $Authorship,
    [Parameter(ParameterSetName = 'Direct')] [switch] $IndexOnly,
    [Parameter(ParameterSetName = 'Direct')] [string] $Text,
    [Parameter(ParameterSetName = 'Direct')] [string] $ComparePath,
    [Parameter(ParameterSetName = 'Direct')] [string] $Cursor,
    # Matches the query model's ordinary page size; the byte ceiling is independent.
    [Parameter(ParameterSetName = 'Direct')] [int] $PageSize = 20
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

# Forward bytes without PowerShell's native command output conversion.
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
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $text = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            $excerpt = Get-NativeContextErrorExcerpt -Text $errorText
            [Console]::Error.WriteLine("Context query failed: $excerpt")
            exit $process.ExitCode
        }
        [Console]::Write($text)
        [Console]::Error.Write($errorText)
    } finally { $process.Dispose() }
}

$arguments = [Collections.Generic.List[string]]::new()
if ($PSCmdlet.ParameterSetName -eq 'Saved') {
    $arguments.Add('read-saved')
    $arguments.Add("--request-path=$RequestPath")
    if ($Next) { $arguments.Add('--next') }
    if ($Restart) { $arguments.Add('--restart') }
} else {
    $arguments.Add('read')
    $arguments.Add("--path=$Path")
    $arguments.Add("--query=$Query")
    $arguments.Add("--scope=$Scope")
    $arguments.Add("--page-size=$PageSize")
    foreach ($entry in @(
        @('Field', 'field'), @('FilesJson', 'files-json'), @('FilesPath', 'files-path'),
        @('Kind', 'kind'), @('Resolution', 'resolution'), @('ThreadId', 'thread-id'),
        @('CommentId', 'comment-id'), @('Authorship', 'authorship'), @('Text', 'text'),
        @('ComparePath', 'compare-path'), @('Cursor', 'cursor')
    )) {
        if ($PSBoundParameters.ContainsKey($entry[0])) {
            $arguments.Add("--$($entry[1])=$($PSBoundParameters[$entry[0]])")
        }
    }
    if ($IndexOnly) { $arguments.Add('--index-only') }
}
Invoke-NativeContext -Arguments $arguments.ToArray()
