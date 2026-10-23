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

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestContextQuery.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\ReviewInput.ps1"
. "$PSScriptRoot\lib\PullRequestQueryRequest.ps1"

try {
    if ($PSCmdlet.ParameterSetName -eq 'Saved') {
        $result = Read-SavedPullRequestQuery -RequestPath $RequestPath -Next:$Next -Restart:$Restart
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        [Console]::WriteLine($result)
        exit 0
    }
    # Selection depends on argument presence so invalid selectors cannot widen the query.
    $hasFilesJson = $PSBoundParameters.ContainsKey('FilesJson')
    $hasFilesPath = $PSBoundParameters.ContainsKey('FilesPath')
    if ($hasFilesJson -and $hasFilesPath) { throw 'Supply either -FilesJson or -FilesPath, not both.' }
    if ($hasFilesJson -and [string]::IsNullOrWhiteSpace($FilesJson)) { throw '-FilesJson requires a nonempty JSON array of strings.' }
    if ($hasFilesPath -and [string]::IsNullOrWhiteSpace($FilesPath)) { throw '-FilesPath requires a nonempty file path.' }
    $files = @()
    if ($hasFilesJson -or $hasFilesPath) {
        $fileText = if ($hasFilesPath) {
            (Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $FilesPath)) -Encoding ([Text.Encoding]::UTF8))
        } else { $FilesJson }
        $files = ConvertFrom-JsonVerbatim -Text $fileText
        if ($files -isnot [object[]] -or $files.Count -eq 0) { throw 'File selectors must be a nonempty JSON array of strings.' }
    }
    $options = New-PullRequestQueryOptions -Query $Query -Field $Field -Files $files -Scope $Scope `
        -Kind $Kind -Resolution $Resolution -ThreadId $ThreadId -CommentId $CommentId `
        -Authorship $Authorship -IndexOnly:$IndexOnly -Text $Text -PageSize $PageSize
    $document = Read-PullRequestContextDocument -Path $Path
    $previous = if ($ComparePath) { Read-PullRequestContextDocument -Path $ComparePath } else { $null }
    $result = Read-PullRequestContextQuery -Document $document -Options $options -Previous $previous -Cursor $Cursor
    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    [Console]::WriteLine($result)
} catch {
    # Parsing errors can quote captured text. Keep diagnostics useful without dumping evidence.
    $message = $_.Exception.Message
    $excerpt = Get-PullRequestQueryExcerpt -Text $message
    [Console]::Error.WriteLine("Context query failed: $($excerpt['excerpt'])")
    exit 1
}
