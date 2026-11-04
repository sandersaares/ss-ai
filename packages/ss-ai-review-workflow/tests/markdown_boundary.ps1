#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param([Parameter(Mandatory)][string] $RepositoryRoot)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$library = Join-Path $RepositoryRoot 'skills/ss-review/scripts/lib'
. (Join-Path $library 'JsonValue.ps1')
. (Join-Path $library 'PullRequestModel.ps1')
. (Join-Path $library 'FindingMarkdown.ps1')
. (Join-Path $library 'TrackerItem.ps1')

# Recording requests proves local composition does not cross the process boundary.
$script:requests = [System.Collections.Generic.List[object]]::new()
function Invoke-NativeReviewMarkdown {
    param([Parameter(Mandatory)][System.Collections.IDictionary] $Request)
    $script:requests.Add($Request)
    if ($Request.operation -ceq 'findingRender') {
        return @{ title = 'Title'; body = 'Body'; published = '[Copilot speaking]'; blocks = @('Block') }
    }
    if ($Request.operation -ceq 'document') { return 'Destination document' }
    if ($Request.operation -ceq 'auditDocument') { return 'Audit document' }
    throw 'Unexpected rendering request.'
}

$findings = @(
    @{ id = 'R'; origin = 'reviewer' }
    @{ id = 'S'; origin = 'synthesis' }
)
if ((Join-FindingMarkdownDocument -Text @('a', 'b')) -cne "a`n`n---`n`nb") { throw 'Incorrect separator.' }
if ((Get-FindingPresentationOrder -Finding $findings)[0].id -cne 'S') { throw 'Incorrect order.' }
if ((Get-FindingReferenceMarkdown -Reference @{ text = 'reference'; url = 'url' }) -cne '[reference](url)') { throw 'Incorrect reference.' }
if ((Get-FindingLocationText -Location @{ path = 'file'; startLine = 2; endLine = 3 }) -cne 'file:2-3') { throw 'Incorrect location.' }
if ($script:requests.Count -ne 0) { throw 'A local helper invoked native rendering.' }

$context = @{ pullRequestUrl = 'https://example.invalid/pr' }
$destination = ConvertTo-FindingDestinationDocument -Finding $findings -Destination 'tracker' -Context $context
$audit = ConvertTo-FindingAuditDocument -Finding $findings -Destination 'tracker' -Context $context
if ($destination -cne 'Destination document' -or $audit -cne 'Audit document') {
    throw 'Document adapters must return their renderer results.'
}
if ($script:requests.Count -ne 2 -or $script:requests[0].operation -cne 'document' -or
    $script:requests[1].operation -cne 'auditDocument') {
    throw 'Each document must use one complete rendering request.'
}
if ($script:requests[0].findings.Count -ne 2 -or $script:requests[1].findings.Count -ne 2) {
    throw 'A complete document request must carry all its findings.'
}

$body = ConvertTo-FindingBodyMarkdown -Finding $findings[0]
$blocks = @(Get-FindingMarkdownBlock -Finding $findings[0] -Destination 'tracker')
if ($body -cne 'Body' -or $blocks.Count -ne 1 -or $blocks[0] -cne 'Block') {
    throw 'Finding adapters must return the selected renderer fields.'
}
if ($script:requests.Count -ne 4 -or $script:requests[2].operation -cne 'findingRender' -or
    $script:requests[3].operation -cne 'findingRender') {
    throw 'Compatibility callers must request a complete finding.'
}
