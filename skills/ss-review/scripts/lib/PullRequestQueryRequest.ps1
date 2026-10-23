#Requires -Version 7.6

# Saves normalized selection and snapshot identity with continuation state.
# Ref: docs/implementation.md, "Bounded local query boundary".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Query identity includes both captures and the exact normalized selectors, not a model's reconstruction.
function Get-PullRequestQueryRequestId {
    param([Parameter(Mandatory)] $Request)
    Get-PullRequestQueryHash -Value ([ordered]@{
        path = $Request['path']; comparePath = $Request['comparePath']
        snapshotId = $Request['snapshotId']; baselineId = $Request['baselineId']; options = $Request['options']
    })
}

# Producer validation uses the same selector and comparison rules as actual evidence reads.
function New-PullRequestQueryRequest {
    param(
        [Parameter(Mandatory)][string] $Path, [string] $ComparePath,
        [Parameter(Mandatory)] $Options, [Parameter(Mandatory)][string] $DiagnosticsPath
    )
    Assert-WorkflowData -Kind queryOptions -Value $Options
    $normalized = New-PullRequestQueryOptions @Options
    $document = Read-PullRequestContextDocument -Path $Path
    $previous = if ($ComparePath) { Read-PullRequestContextDocument -Path $ComparePath } else { $null }
    # Validate even selectors whose errors require the capture, without exposing a page at creation.
    $null = Read-PullRequestContextQuery -Document $document -Options $normalized -Previous $previous
    $request = [ordered]@{
        schemaVersion = 1; path = Resolve-PullRequestPath -Path $Path
        comparePath = if ($ComparePath) { Resolve-PullRequestPath -Path $ComparePath } else { $null }
        snapshotId = Get-PullRequestContextSnapshotId -Document $document
        baselineId = if ($null -ne $previous) { Get-PullRequestContextSnapshotId -Document $previous } else { $null }
        options = $normalized; requestId = ''; diagnosticsPath = Resolve-PullRequestPath -Path $DiagnosticsPath
        lastPage = $null; lastPageCursor = $null
    }
    $request['requestId'] = Get-PullRequestQueryRequestId -Request $request
    Assert-WorkflowData -Kind queryRequest -Value $request
    $request
}

# Replay is the default so an interrupted tool call cannot silently skip a page.
function Read-SavedPullRequestQuery {
    param([Parameter(Mandatory)][string] $RequestPath, [switch] $Next, [switch] $Restart)
    if ($Next -and $Restart) { throw 'Next and Restart are mutually exclusive.' }
    $lock = Open-WorkflowDataLock -Path $RequestPath
    try {
        $request = Read-WorkflowData -Kind queryRequest -Path $RequestPath
        foreach ($path in @($request['path'], $request['comparePath'], $request['diagnosticsPath'])) {
            if ($null -ne $path -and -not [IO.Path]::IsPathFullyQualified($path)) { throw 'Saved query paths must be absolute.' }
        }
        $savedOptions = $request['options']
        $options = New-PullRequestQueryOptions @savedOptions
        if ((Get-PullRequestQueryHash -Value $options) -cne (Get-PullRequestQueryHash -Value $request['options']) -or
            (Get-PullRequestQueryRequestId -Request $request) -cne $request['requestId']) {
            throw 'Saved query selectors or identity were modified. Create a new request for different filters.'
        }
        if ($Restart) {
            Add-ReviewInputDiagnostic -Path $request['diagnosticsPath'] -Name cursorRestarts
            $request = New-PullRequestQueryRequest -Path $request['path'] -ComparePath $request['comparePath'] `
                -Options $options -DiagnosticsPath $request['diagnosticsPath']
        }
        $document = Read-PullRequestContextDocument -Path $request['path']
        $previous = if ($request['comparePath']) { Read-PullRequestContextDocument -Path $request['comparePath'] } else { $null }
        $snapshot = Get-PullRequestContextSnapshotId -Document $document
        $baseline = if ($null -ne $previous) { Get-PullRequestContextSnapshotId -Document $previous } else { $null }
        if ($snapshot -cne $request['snapshotId'] -or $baseline -cne $request['baselineId']) {
            throw 'Saved query capture changed. Report the stale request, then use -Restart against the intended capture or create a new request.'
        }
        $lastPage = $request['lastPage']
        if ($null -ne $lastPage) {
            if ($lastPage['snapshotId'] -cne $snapshot -or $lastPage['query'] -cne $options['query'] -or
                $lastPage['complete'] -ne ($null -eq $lastPage['nextCursor'])) {
                throw 'Saved query page does not match the request.'
            }
            $savedJson = ConvertTo-Json -InputObject $lastPage -Depth 100 -Compress
            if ([Text.Encoding]::UTF8.GetByteCount($savedJson + [Environment]::NewLine) -gt $script:PullRequestQueryMaxBytes) {
                throw 'Saved query page exceeds the bounded output limit.'
            }
            # Validate every item and continuation against canonical evidence, not a hash of mutable cached data.
            # The page's starting cursor binds regeneration to its original position and normalized selectors.
            $expectedJson = Read-PullRequestContextQuery -Document $document -Options $options -Previous $previous `
                -Cursor $request['lastPageCursor']
            if ($savedJson -cne $expectedJson) { throw 'Saved query page does not match the bound request and capture.' }
            if (-not $Next) { return $expectedJson }
            if ($lastPage['complete']) { throw 'The saved query is complete. There is no next page.' }
        } elseif ($Next) { throw 'Read the first page before requesting -Next.' }
        $cursor = if ($null -ne $lastPage) { $lastPage['nextCursor'] } else { '' }
        $json = Read-PullRequestContextQuery -Document $document -Options $options -Previous $previous -Cursor $cursor
        $request['lastPage'] = ConvertFrom-JsonVerbatim -Text $json
        $request['lastPageCursor'] = $cursor
        Assert-WorkflowData -Kind queryRequest -Value $request
        Write-PullRequestOutputFileAtomically -Path $RequestPath -Content (ConvertTo-JsonText -Value $request)
        $json
    } finally { $lock.Dispose() }
}
