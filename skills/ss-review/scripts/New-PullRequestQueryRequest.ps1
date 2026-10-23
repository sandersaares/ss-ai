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
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestContextQuery.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewInput.ps1"
. "$PSScriptRoot\lib\PullRequestQueryRequest.ps1"

Add-ReviewInputDiagnostic -Path $DiagnosticsPath -Name setupCalls
$lock = Open-WorkflowDataLock -Path $RequestPath
try {
    if ([IO.File]::Exists($RequestPath)) {
        Add-ReviewInputDiagnostic -Path $DiagnosticsPath -Name repeatedSetupCalls
        throw 'Query request already exists. Read it again instead of repeating setup, or use a new path for a different query.'
    }
    try {
        if ($PSBoundParameters.ContainsKey('TextPath')) {
            if ($PSBoundParameters.ContainsKey('Text')) { throw 'Supply either Text or TextPath, not both.' }
            $Text = (Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $TextPath)) -Encoding ([Text.UTF8Encoding]::new($false, $true)))
        }
        if ($PSBoundParameters.ContainsKey('Files') -and $Files.Count -eq 0) { throw 'An explicit Files selector must not be empty.' }
        $options = New-PullRequestQueryOptions -Query $Query -Field $Field -Files $Files -Scope $Scope `
            -Kind $Kind -Resolution $Resolution -ThreadId $ThreadId -CommentId $CommentId `
            -Authorship $Authorship -IndexOnly:$IndexOnly -Text $Text -PageSize $PageSize
        $request = New-PullRequestQueryRequest -Path $Path -ComparePath $ComparePath -Options $options -DiagnosticsPath $DiagnosticsPath
    } catch {
        Add-ReviewInputDiagnostic -Path $DiagnosticsPath -Name malformedQueryFailures
        throw
    }
    Write-PullRequestOutputFileAtomically -Path $RequestPath -Content (ConvertTo-JsonText -Value $request)
    [ordered]@{ requestPath = Resolve-PullRequestPath -Path $RequestPath; requestId = $request['requestId']; snapshotId = $request['snapshotId'] } |
        ConvertTo-Json -Compress
} catch {
    # A capture parser can include source text in its error; keep diagnostics bounded like the reader.
    $excerpt = Get-PullRequestQueryExcerpt -Text $_.Exception.Message
    [Console]::Error.WriteLine("Query preparation failed: $($excerpt['excerpt'])")
    exit 1
} finally { $lock.Dispose() }
