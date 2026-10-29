#Requires -Version 7.6

# Adapts the shared native renderer to the existing publication callers.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\NativeWorkflowBridge.ps1"

# Overview publication uses the same authorship marker as finding publication.
$script:FindingMarkdownAuthorshipPrefix = '[Copilot speaking]'
$script:FindingMarkdownProseSection = @(
    @{ field = 'reasoning'; label = 'Reproducible reasoning' }
    @{ field = 'consequence'; label = 'Consequence' }
    @{ field = 'recommendedAction'; label = 'Recommended action' }
)

# Requests the complete finding while retaining its parts for legacy callers.
function Invoke-FindingRendering {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Finding,
        [Parameter(Mandatory)][ValidateSet('pullRequest', 'tracker')][string] $Destination,
        [string] $FollowUpUrl,
        [string] $PullRequestUrl
    )
    Invoke-NativeReviewMarkdown -Request @{
        operation = 'findingRender'; finding = $Finding; destination = $Destination
        followUpUrl = $FollowUpUrl; pullRequestUrl = $PullRequestUrl
    }
}

# Renders a complete pull request comment.
function ConvertTo-FindingMarkdownText {
    param([Parameter(Mandatory)][System.Collections.IDictionary] $Finding)
    $result = Invoke-FindingRendering -Finding $Finding -Destination 'pullRequest'
    $result['published']
}

# Uses the links already resolved by tracker publication.
function ConvertTo-FindingTrackerItem {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Finding,
        [string] $FollowUpUrl,
        [string] $PullRequestUrl
    )
    $result = Invoke-FindingRendering -Finding $Finding -Destination 'tracker' `
        -FollowUpUrl $FollowUpUrl -PullRequestUrl $PullRequestUrl
    [ordered]@{
        title = $result['title']
        body = $result['published']
    }
}

# Renders the body used inside a multi-finding document.
function ConvertTo-FindingBodyMarkdown {
    param([Parameter(Mandatory)][System.Collections.IDictionary] $Finding)
    $result = Invoke-FindingRendering -Finding $Finding -Destination 'pullRequest'
    $result['body']
}

# Exposes the same blocks to callers that compose their own document.
function Get-FindingMarkdownBlock {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Finding,
        [Parameter(Mandatory)][ValidateSet('pullRequest', 'tracker')][string] $Destination,
        [string] $FollowUpUrl,
        [string] $PullRequestUrl
    )
    $result = Invoke-FindingRendering -Finding $Finding -Destination $Destination `
        -FollowUpUrl $FollowUpUrl -PullRequestUrl $PullRequestUrl
    , $result['blocks']
}

# Reports review counts and preparation diagnostics to publication callers.
function New-ReviewSummaryMarkdown {
    param(
        [AllowEmptyCollection()][object[]] $Finding = @(),
        [ValidateRange(0, [int]::MaxValue)][int] $FollowUpCount = 0,
        [System.Collections.IDictionary] $Diagnostics = @{},
        [Parameter(Mandatory)][ValidateSet('gitHub', 'azureDevOps')][string] $Platform
    )
    Invoke-NativeReviewMarkdown -Request @{
        operation = 'summary'; findings = @($Finding); followUpCount = $FollowUpCount
        diagnostics = $Diagnostics; platform = $Platform
    }
}

# Renders all findings for a destination without inline comments.
function ConvertTo-FindingMarkdownDocument {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]] $Finding)
    Invoke-NativeReviewMarkdown -Request @{
        operation = 'document'; findings = @($Finding); destination = 'pullRequest'
    }
}

# Applies the shared separator for composite audit and tracker documents.
function Join-FindingMarkdownDocument {
    param([Parameter(Mandatory)][AllowEmptyCollection()][string[]] $Text)
    $Text -join "`n`n---`n`n"
}

# Names per-finding outputs using the stable finding identifier.
function Get-FindingMarkdownPath {
    param(
        [Parameter(Mandatory)][string] $OutputDirectory,
        [Parameter(Mandatory)][string] $FindingId
    )
    Resolve-PullRequestPath -Path (Join-Path $OutputDirectory "$FindingId.md")
}

# Preserves synthesis-first ordering across audit and publication.
function Get-FindingPresentationOrder {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]] $Finding)
    $synthesis = [System.Collections.Generic.List[object]]::new()
    $reviewer = [System.Collections.Generic.List[object]]::new()
    foreach ($entry in $Finding) {
        if ([string] (Get-JsonProperty -Node $entry -Name 'origin') -ceq 'synthesis') {
            $synthesis.Add($entry)
        } else {
            $reviewer.Add($entry)
        }
    }
    , @($synthesis.ToArray() + $reviewer.ToArray())
}

# Keeps individual reference rendering available to composition callers.
function Get-FindingReferenceMarkdown {
    param([Parameter(Mandatory)] $Reference)
    $text = [string] (Get-JsonProperty -Node $Reference -Name 'text')
    $url = [string] (Get-JsonProperty -Node $Reference -Name 'url')
    if ([string]::IsNullOrWhiteSpace($url)) { return $text }
    "[$text]($url)"
}

# Keeps individual location rendering available to composition callers.
function Get-FindingLocationText {
    param([Parameter(Mandatory)] $Location)
    $path = [string] (Get-JsonProperty -Node $Location -Name 'path')
    $startLine = Get-JsonProperty -Node $Location -Name 'startLine'
    if ($null -eq $startLine) { return $path }
    $endLine = Get-JsonProperty -Node $Location -Name 'endLine' -Default $startLine
    if ([long] $endLine -le [long] $startLine) { return "${path}:$startLine" }
    "${path}:$startLine-$endLine"
}
