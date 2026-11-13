#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Normalizes identifier lists for both native adapters and retained PowerShell consumers.
#
# `pwsh -File` passes an array argument as one literal string, so callers may separate
# identifiers with commas. Tool-generated identifiers do not contain commas.
function ConvertTo-FindingIdentifierList {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [AllowEmptyString()]
        [string[]] $Value
    )

    $identifiers = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $Value) {
        foreach ($part in $entry.Split(',')) {
            $identifier = $part.Trim()
            if ($identifier) { $identifiers.Add($identifier) }
        }
    }

    , $identifiers.ToArray()
}
