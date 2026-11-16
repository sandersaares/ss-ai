#Requires -Version 7.6

# Translate PowerShell filesystem paths before passing them to a native process.
function Resolve-NativeContextFilePath {
    param([Parameter(Mandatory)][string] $Path)

    $provider = $null
    $drive = $null
    $resolved = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
        $Path, [ref] $provider, [ref] $drive
    )
    if ($provider.Name -ne 'FileSystem') {
        throw "Native context paths require the FileSystem provider: $Path"
    }
    return $resolved
}
