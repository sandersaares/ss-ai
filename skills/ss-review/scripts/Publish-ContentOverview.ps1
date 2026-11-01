#Requires -Version 7.6
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $ContextPath,
    [Parameter(Mandatory)][string] $OverviewPath,
    [Parameter(Mandatory)][string] $OutputDirectory,
    [AllowEmptyString()][string] $CommentPrefix = '',
    [string] $Tenant
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$skillRoot = Split-Path -Parent $PSScriptRoot
$workingDirectory = (Get-Location).ProviderPath
$outputPath = [IO.Path]::GetFullPath($OutputDirectory, $workingDirectory)
[void] [IO.Directory]::CreateDirectory($outputPath)
$prefixPath = Join-Path $outputPath ('.overview-prefix-' + [guid]::NewGuid().ToString('N'))

# Create the file with private permissions from the outset, before writing attribution text.
$created = $false
try {
    if ($IsWindows) {
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $security = [Security.AccessControl.FileSecurity]::new()
        $security.SetOwner($identity)
        $security.SetAccessRuleProtection($true, $false)
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            $identity, [Security.AccessControl.FileSystemRights]::FullControl,
            [Security.AccessControl.AccessControlType]::Allow
        )
        $security.AddAccessRule($rule)
        $stream = [IO.FileSystemAclExtensions]::Create(
            [IO.FileInfo]::new($prefixPath), [IO.FileMode]::CreateNew,
            [Security.AccessControl.FileSystemRights]::Write, [IO.FileShare]::None,
            4096, [IO.FileOptions]::None, $security
        )
    } else {
        $options = [IO.FileStreamOptions]::new()
        $options.Mode = [IO.FileMode]::CreateNew
        $options.Access = [IO.FileAccess]::Write
        $options.Share = [IO.FileShare]::None
        $options.UnixCreateMode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
        $stream = [IO.FileStream]::new($prefixPath, $options)
    }
    $created = $true
    try {
        $bytes = [Text.UTF8Encoding]::new($false, $true).GetBytes($CommentPrefix)
        $stream.Write($bytes, 0, $bytes.Length)
    } finally { $stream.Dispose() }

    $program = Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $start = [Diagnostics.ProcessStartInfo]::new($program.Source)
    $start.WorkingDirectory = $workingDirectory
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
    $arguments = [Collections.Generic.List[string]]::new()
    foreach ($argument in @(
        'review', 'publish-overview', "--context-path=$ContextPath", "--overview-path=$OverviewPath",
        "--output-directory=$OutputDirectory", "--skill-root=$skillRoot", "--comment-prefix-path=$prefixPath"
    )) { $arguments.Add($argument) }
    if ($PSBoundParameters.ContainsKey('Tenant')) { $arguments.Add("--tenant=$Tenant") }
    foreach ($argument in $arguments) { $start.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::Start($start)
    try {
        # Both redirected pipes must drain concurrently so a full pipe cannot block the child while we wait.
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $text = $output.GetAwaiter().GetResult()
        $errorText = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "ss-ai review publish-overview failed with exit code $($process.ExitCode): $errorText" }
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        [Console]::Write($text)
        [Console]::Error.Write($errorText)
    } finally { $process.Dispose() }
} finally {
    if ($created) {
        try {
            [IO.File]::Delete($prefixPath)
        } catch {
            # Cleanup must not change the publication outcome or obscure its diagnostic.
            try {
                $detail = $_.Exception.Message.Replace("`r", ' ').Replace("`n", ' ')
                # Bound exception details while keeping the residual path intact for cleanup.
                if ($detail.Length -gt 512) { $detail = $detail.Substring(0, 512) + '...' }
                [Console]::Error.WriteLine("Could not remove overview attribution file '$prefixPath': $detail")
            } catch {
                # Even an unavailable diagnostic stream must not replace the primary outcome.
            }
        }
    }
}
