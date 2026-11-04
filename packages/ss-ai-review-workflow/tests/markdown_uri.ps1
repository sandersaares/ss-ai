#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $OutputDirectory,
    [Parameter(Mandatory)][string] $ScriptPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The spaces and parentheses exercise the URI form presented to audit readers.
$directory = Join-Path $OutputDirectory 'audit files (local)'
$artifactPath = Join-Path $OutputDirectory 'markdown-uri-artifact.json'
$auditPath = Join-Path $directory 'audit-findings.md'
$record = @{
    id = 'F1'
    origin = 'reviewer'
    title = 'Unicode and quoting'
    description = 'Résumé $HOME $(not a shell command)'
    reasoning = 'The text is preserved.'
    consequence = 'The text is readable.'
    recommendedAction = 'Keep the original text.'
    anchor = @{ kind = 'line'; path = 'src/file.rs'; startLine = 1 }
}
$artifact = @{ schemaVersion = 1; kind = 'consolidated'; findings = @($record) }
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
[IO.File]::WriteAllText($artifactPath, (ConvertTo-Json -InputObject $artifact -Depth 20), [Text.UTF8Encoding]::new($false))
$raw = & $ScriptPath -Path $artifactPath -OutputDirectory (Join-Path $OutputDirectory 'findings') -AuditDocumentPath $auditPath
$result = ConvertFrom-Json -InputObject $raw -AsHashtable
$uri = [uri]::new($result.auditDocumentUri, [UriKind]::Absolute)
if (-not $uri.IsFile -or $uri.LocalPath -cne $auditPath) { throw 'The audit URI does not identify the audit file.' }
if (-not $result.auditDocumentUri.Contains('audit%20files%20(local)')) { throw 'The audit URI did not escape spaces.' }
if (-not [IO.File]::ReadAllText($auditPath).Contains($record.description)) { throw 'The audit body changed its prose.' }
