#Requires -Version 7.6
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $ContextPath,
    [Parameter(Mandatory)][string] $OverviewPath,
    [Parameter(Mandatory)][string] $OutputDirectory,
    [string] $CommentPrefix = '',
    [string] $Tenant
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PublicationFailure.ps1"
. "$PSScriptRoot\lib\PullRequestTarget.ps1"
. "$PSScriptRoot\lib\PullRequestIdentity.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\PullRequestTransportHost.ps1"
. "$PSScriptRoot\lib\AdoRestTransport.ps1"
. "$PSScriptRoot\lib\AdoPullRequestApi.ps1"
. "$PSScriptRoot\lib\GitHubCliTransport.ps1"
. "$PSScriptRoot\lib\GitHubPullRequestApi.ps1"
. "$PSScriptRoot\lib\PullRequestPlatform.ps1"
. "$PSScriptRoot\lib\NativeWorkflowBridge.ps1"
. "$PSScriptRoot\lib\FindingMarkdown.ps1"
. "$PSScriptRoot\lib\ReportTools.ps1"
. "$PSScriptRoot\lib\ReviewHtml.ps1"
. "$PSScriptRoot\lib\OverviewPublication.ps1"
. "$PSScriptRoot\lib\OverviewPublicationApi.ps1"

$skillRoot = Split-Path -Parent $PSScriptRoot
$context = Read-PullRequestContextDocument -Path $ContextPath
$transport = New-PullRequestTransport -Target $context['target'] -Tenant $Tenant
$processInvoker = Get-ReportProcessInvoker
$readRevision = {
    param($id, $revision)
    Read-GitHubOverviewRevision -GistId $id -Revision $revision -InvokeProcess $processInvoker
}.GetNewClosure()
$remote = {
    param($operation, $target, $snapshot, $payload)
    switch ($target['platform']) {
        'gitHub' { Invoke-GitHubOverviewOperation -Operation $operation -Transport $transport -Target $target -Snapshot $snapshot -Payload $payload -ReadRevision $readRevision }
        'azureDevOps' { Invoke-AdoOverviewOperation -Operation $operation -Transport $transport -Target $target -Snapshot $snapshot -Payload $payload }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $target['platform']) }
    }
}.GetNewClosure()
$render = {
    param($path, $output)
    Write-ReviewHtml -Path $path -OutputPath $output -SkillRoot $skillRoot -Title 'Initial review overview' `
        -Tools (Get-ReviewReportTools -SkillRoot $skillRoot) -InvokeProcess $processInvoker
}.GetNewClosure()
Invoke-InitialOverviewPublication -Context $context -OverviewPath $OverviewPath `
    -OutputDirectory (Resolve-PullRequestPath -Path $OutputDirectory) -SkillRoot $skillRoot `
    -InvokeRemote $remote -RenderHtml $render -CommentPrefix $CommentPrefix | ConvertTo-Json -Depth 64 -Compress
