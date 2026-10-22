#Requires -Version 7.0

# GitHub issue adapter for tracker publication (docs/implementation.md, "Tracker adapters").
#
# Filing an issue is a single write, and this file is the whole of what GitHub needs for it: the
# repository read that settles whether issues can be filed at all, the creation itself, and the read
# back that proves what was stored.
#
# Every write goes through GraphQL, as every other GitHub write in this skill does, and for the same
# reason: the GraphQL transport is the only path that distinguishes a refusal the service stated from
# a response the process lost. A REST creation that came back 422 would be recorded as unknown, and
# an unknown creation blocks every rerun over a write that never happened.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls Get-JsonProperty and Get-JsonArrayProperty from
# JsonValue.ps1, Get-GitHubGraphQlNode from GitHubPullRequestApi.ps1 and Invoke-GitHubGraphQlQuery
# from GitHubCliTransport.ps1, which the composition root loads alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The repository read that precedes every creation. The node identifier is what the create mutation
# addresses the repository by, and the rest is what decides whether it may be addressed at all: a
# repository with issues disabled and an archived one both refuse every creation, and learning that
# from the first failed write would leave the run reporting a failure it could have predicted.
$script:GitHubIssueRepositoryQuery = @'
query($owner:String!,$repo:String!){
  repository(owner:$owner,name:$repo){ id nameWithOwner hasIssuesEnabled isArchived viewerPermission } }
'@

$script:GitHubCreateIssueMutation = @'
mutation($repositoryId:ID!,$title:String!,$body:String!){
  createIssue(input:{repositoryId:$repositoryId,title:$title,body:$body}){
    issue{ id number url title body } } }
'@

# Verification reads the issue back by node identifier rather than by number, because the identifier
# is what the creation answered with and a number is scoped to a repository the read would have to
# name again.
$script:GitHubIssueVerificationQuery = @'
query($issueId:ID!){
  node(id:$issueId){ ... on Issue { id number url title body } } }
'@

# The permissions that allow an issue to be filed. GitHub reports READ for an account that may open
# an issue in a public repository, so the check is only that a permission was reported at all; the
# service remains the authority and refuses the creation itself where it will not accept it.
$script:GitHubIssueRefusedPermission = @('NONE')

# Reads what the tracker's repository says about filing issues into it.
#
# Refusals here are the ones the service will otherwise repeat for every finding in the review. They
# are read once and reported once, before the journal exists, so a run that cannot file anything
# ends having written nothing.
function Get-GitHubIssueTrackerState {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubIssueRepositoryQuery `
        -Variables ([ordered]@{
            owner = [string] $Target['organization']
            repo  = [string] $Target['repository']
        })

    $repository = Get-GitHubGraphQlNode -Response $response -Path @('data', 'repository')

    $nameWithOwner = [string] (Get-JsonProperty -Node $repository -Name 'nameWithOwner')
    $repositoryId = [string] (Get-JsonProperty -Node $repository -Name 'id')
    if (-not $repositoryId) {
        throw "GitHub reported repository '$nameWithOwner' without an identifier, so there is nothing to file issues against."
    }

    if ([bool] (Get-JsonProperty -Node $repository -Name 'isArchived' -Default $false)) {
        throw "The repository '$nameWithOwner' is archived, so GitHub accepts no new issues in it. Name another repository as the tracker."
    }

    if (-not [bool] (Get-JsonProperty -Node $repository -Name 'hasIssuesEnabled' -Default $false)) {
        throw "The repository '$nameWithOwner' has issues disabled, so there is nowhere to file the review. Name another repository as the tracker."
    }

    $permission = [string] (Get-JsonProperty -Node $repository -Name 'viewerPermission')
    if ($script:GitHubIssueRefusedPermission -ccontains $permission) {
        throw "The authenticated account has no access to '$nameWithOwner', so it can file no issues there."
    }

    [ordered]@{
        platform     = 'gitHub'
        repositoryId = $repositoryId
        trackerName  = $nameWithOwner
    }
}

# Files one finding as an issue.
#
# The title and the body arrive already rendered, so nothing here decides how a finding reads.
function New-GitHubIssueForFinding {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item
    )

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubCreateIssueMutation `
        -Variables ([ordered]@{
            repositoryId = [string] $State['repositoryId']
            title        = [string] $Item['title']
            body         = [string] $Item['body']
        }) `
        -NoRetry

    $issue = Get-GitHubGraphQlNode -Response $response -Path @('data', 'createIssue', 'issue')

    ConvertTo-GitHubIssueRecord -Issue $issue
}

# Reads one filed issue back, so what the service stored can be compared with what was sent.
function Get-GitHubIssueRecord {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $ItemNodeId
    )

    $response = Invoke-GitHubGraphQlQuery `
        -Transport $Transport `
        -Query $script:GitHubIssueVerificationQuery `
        -Variables ([ordered]@{ issueId = $ItemNodeId })

    ConvertTo-GitHubIssueRecord -Issue (Get-GitHubGraphQlNode -Response $response -Path @('data', 'node'))
}

# Normalizes one issue into the record both adapters return.
#
# `itemId` is the number a reader quotes and `itemNodeId` the identifier the service is addressed by.
# Both are carried because neither serves the other's purpose: a number means nothing to the API and
# an opaque identifier means nothing to a person.
#
# `descriptionFormat` is stated as Markdown without asking, because GitHub renders every issue body
# as Markdown and offers no alternative. The key exists so verification can ask both services the
# same question.
function ConvertTo-GitHubIssueRecord {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Issue
    )

    $number = Get-JsonProperty -Node $Issue -Name 'number'
    if ($null -eq $number) {
        throw 'GitHub answered about an issue without reporting its number, so the run cannot tell which issue it holds.'
    }

    [ordered]@{
        itemId            = [string] $number
        itemNodeId        = [string] (Get-JsonProperty -Node $Issue -Name 'id')
        url               = [string] (Get-JsonProperty -Node $Issue -Name 'url')
        title             = [string] (Get-JsonProperty -Node $Issue -Name 'title')
        body              = [string] (Get-JsonProperty -Node $Issue -Name 'body')
        descriptionFormat = 'Markdown'
    }
}
