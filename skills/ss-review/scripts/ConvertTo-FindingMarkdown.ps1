#Requires -Version 7.6

# Renders recorded findings as the Markdown a reader sees (SKILL.md, "Phase 7: audit"; "Phase 9:
# publication").
#
# Every body is produced here, from the structured record, so that the text an audit reads is the
# text that is published and no phase has to parse prose back into a finding. The audit renders the
# working artifact and a review whose target takes no comments renders the approved set, so the
# document read here is whichever of the two the caller holds, told apart by its own contents.
#
# The destination decides which form is rendered, because a finding filed as a tracker item is not
# laid out as a pull request comment: its title is a field of its own and it carries the links back
# to the review that a comment posted into that review does not need. The audit renders the form the
# findings will be published in, so the text the user approves stays the text that is published.
#
# Naming findings renders only those, which is what an audit does after amending one: the rest of
# the run's rendered bodies are already correct and rewriting them would obscure what changed.
#
# This is a composition root: it dot-sources the libraries and reaches no decision a library could
# reach, so every rule it enforces is exercisable offline.
#
# The only thing written to stdout is a single compressed JSON object, because the caller parses it;
# any stray output would corrupt that result. The rendered bodies go to files and errors to the
# error stream.

[CmdletBinding(PositionalBinding = $false)]
param(
    # The consolidated artifact, as written by New-ConsolidatedFindings.ps1, or the approved set, as
    # written by New-ApprovedFindings.ps1. The document states which it is.
    [Parameter(Mandatory)]
    [string] $Path,

    # Where the per-finding bodies are written, one file per finding.
    [Parameter(Mandatory)]
    [string] $OutputDirectory,

    # Also writes every finding into one document, for a review whose target takes no comments. The
    # combined document always covers the whole document read, because it is delivered as the review.
    [string] $CombinedPath,

    # Also writes the findings the audit must decide into one document for the document-review mode.
    # In-session review uses the per-finding files. Unlike the combined document, this honours -FindingId
    # and covers only the named findings, because the audit presents the findings that need a verdict
    # and not the ones it auto-approved. Each finding is labelled with its identifier so the user's
    # reply can name a specific one. When -OverviewPath is also supplied, that overview is prepended
    # here so the auditor sees what the reviewed content does before the findings.
    [string] $AuditDocumentPath,

    # The findings to render, named as one comma separated value. Rendering covers the whole
    # document when this names none.
    [string[]] $FindingId = @(),

    # Where the rendered findings are headed: 'pullRequest' for comments, 'tracker' for issues or
    # work items. A tracker rendering states the title on its own first line, because the tracker
    # holds it in a field of its own and the user approving the text has to see both.
    [ValidateSet('pullRequest', 'tracker')]
    [string] $Destination = 'pullRequest',

    # The captured discussion, supplying the links a tracker item carries back to the review. A
    # review of content that is not a pull request has none.
    [string] $ContextPath,

    # The content overview written during preparation (SKILL.md, "Phase 1: preparation"). Prepended
    # to the audit document so the auditor sees what the reviewed content does before the findings.
    # Omitted from per-finding bodies and the combined deliverable, because those are published to
    # the author of the content rather than to the auditor.
    [string] $OverviewPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FindingModel.ps1"
. "$PSScriptRoot\lib\FindingMarkdown.ps1"
. "$PSScriptRoot\lib\TrackerItem.ps1"

if ($ContextPath -and $Destination -cne 'tracker') {
    throw "A captured discussion was supplied for the '$Destination' destination, which renders no links out of it. Render with -Destination tracker, or omit -ContextPath."
}

if ($OverviewPath -and -not $AuditDocumentPath) {
    throw "A content overview was supplied without an audit document to prepend it to. Render with -AuditDocumentPath, or omit -OverviewPath."
}

# The overview is validated before any rendered body is written, because a missing or empty file
# must fail the command without leaving partial finding files, a combined document, or a stale
# audit document behind.
$overview = $null
if ($OverviewPath) {
    $overviewFullPath = Resolve-PullRequestPath -Path $OverviewPath
    if (-not (Test-Path -LiteralPath $overviewFullPath -PathType Leaf)) {
        throw "The content overview at '$OverviewPath' does not exist."
    }

    $overview = (Read-PullRequestFileText -Path ($overviewFullPath) -Encoding ([System.Text.Encoding]::UTF8)).Replace("`r`n", "`n").TrimEnd()
    if ([string]::IsNullOrWhiteSpace($overview)) {
        throw "The content overview at '$OverviewPath' is empty."
    }
}

$context = if ($ContextPath) { Read-PullRequestContextDocument -Path $ContextPath } else { $null }

$document = Read-RenderableFindingDocument -Path $Path
$findings = Get-JsonArrayProperty -Node $document -Name 'findings'
$selected = Select-FindingById -Finding $findings -FindingId (ConvertTo-FindingIdentifierList -Value $FindingId)

$rendered = [System.Collections.Generic.List[object]]::new()
foreach ($finding in $selected) {
    $renderedId = [string] (Get-JsonProperty -Node $finding -Name 'id')
    $renderedPath = Get-FindingMarkdownPath -OutputDirectory $OutputDirectory -FindingId $renderedId
    $text = ConvertTo-FindingDestinationText -Finding $finding -Destination $Destination -Context $context
    Write-PullRequestOutputFile -Path $renderedPath -Content $text
    $rendered.Add([ordered]@{ findingId = $renderedId; path = $renderedPath })
}

$combinedFullPath = $null
if ($CombinedPath) {
    $combinedFullPath = Resolve-PullRequestPath -Path $CombinedPath
    $combined = ConvertTo-FindingDestinationDocument -Finding $findings -Destination $Destination -Context $context
    Write-PullRequestOutputFile -Path $combinedFullPath -Content $combined
}

$auditDocumentFullPath = $null
if ($AuditDocumentPath) {
    $auditDocumentFullPath = Resolve-PullRequestPath -Path $AuditDocumentPath
    $audit = ConvertTo-FindingAuditDocument -Finding $selected -Destination $Destination -Context $context
    if ($null -ne $overview) {
        $audit = if ([string]::IsNullOrWhiteSpace($audit)) {
            $overview
        } else {
            Join-FindingMarkdownDocument -Text @($overview, $audit)
        }
    }

    Write-PullRequestOutputFile -Path $auditDocumentFullPath -Content $audit
}

[ordered]@{
    outputDirectory   = Resolve-PullRequestPath -Path $OutputDirectory
    destination       = $Destination
    combinedPath      = $combinedFullPath
    auditDocumentPath = $auditDocumentFullPath
    auditDocumentUri  = if ($auditDocumentFullPath) { ([uri] $auditDocumentFullPath).AbsoluteUri } else { $null }
    findings          = $rendered.ToArray()
} | ConvertTo-Json -Depth 32 -Compress
