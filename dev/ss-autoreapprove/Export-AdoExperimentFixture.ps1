#Requires -Version 7.6

[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $StatePath,
    [Parameter(Mandatory)][string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Import-Module (Join-Path $PSScriptRoot 'AdoExperiment.psm1') -Force
if (-not [IO.Path]::IsPathFullyQualified($StatePath) -or -not [IO.Path]::IsPathFullyQualified($OutputPath)) {
    throw 'Both fixture paths must be absolute.'
}
$record = Read-ExperimentRecord $StatePath
if ($record.nextStep -cne 'Done' -or @($record.effects | Where-Object state -CNE verified).Count -ne 0) {
    throw 'Only a fully verified and cleaned experiment can produce a fixture.'
}

# Stable pseudonyms preserve identity equality without retaining service identifiers.
$identities = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
function Convert-IdentityId {
    param([string] $Id)
    if (-not $Id) { throw 'Missing identity in captured service evidence.' }
    if (-not $identities.ContainsKey($Id)) {
        $identities.Add($Id, ('10000000-0000-4000-8000-{0:D12}' -f ($identities.Count + 1)))
    }
    return $identities[$Id]
}
$account = Convert-IdentityId $record.accountId
$repository = '20000000-0000-4000-8000-000000000001'
$project = '30000000-0000-4000-8000-000000000001'
$commits = [ordered] @{
    ($record.captures[0].view.sourceCommit) = ('a' * 40)
    ($record.targetCommit) = ('b' * 40)
    ($record.effects[1].response.commits[0].commitId) = ('c' * 40)
    ($record.effects[2].response.commits[0].commitId) = ('d' * 40)
}
function Convert-CommitId {
    param([string] $Id)
    if (-not $commits.Contains($Id)) { throw "Unrecognized commit relationship in capture; do not copy $Id." }
    return $commits[$Id]
}

# Shift all timestamps together, preserving exact ordering and relative intervals.
$origin = [datetimeoffset]::Parse($record.captures[0].raw.iterations.value[0].createdDate)
$epoch = [datetimeoffset]::Parse('2026-01-01T00:00:00Z')
function Convert-EvidenceTime {
    param([string] $Value)
    return $epoch.Add([datetimeoffset]::Parse($Value) - $origin).UtcDateTime.ToString('o')
}

function Convert-RefName {
    param([string] $Name)
    if ($Name -ceq $record.sourceRef) { return 'refs/heads/fixture-source' }
    if ($Name -ceq $record.targetRef) { return 'refs/heads/fixture-target' }
    throw 'Unrecognized branch in captured evidence.'
}

# Ordinary discussion, names, email, URLs, and private code are never exported.
function Convert-EvidenceThread {
    param([Collections.IDictionary] $Thread, [int] $Id)
    $properties = [ordered] @{}
    foreach ($name in $Thread.properties.Keys) {
        $property = $Thread.properties[$name]
        $value = $property['$value']
        if ($property['$type'] -cnotin @('System.String', 'System.Int32')) { throw 'Unrecognized service property type.' }
        switch -Regex ($name) {
            '^CodeReviewRef(NewCommits|NewHeadCommit)$' { $value = Convert-CommitId $value; break }
            '^CodeReviewRefName$' { $value = Convert-RefName $value; break }
            '^CodeReviewThreadType$' {
                if ($value -cnotin @('ReviewersUpdate', 'VoteUpdate', 'RefUpdate')) { throw 'Unrecognized system thread type.' }
                break
            }
            '^CodeReview(VoteResult|VotedBy(Initiator)?Identity|ReviewersUpdated(Num(Added|Removed|Changed|Declined)|(Added|Removed|By)Identity)|Ref(NewCommitsCount|UpdatedByIdentity))$' {
                if ([string] $value -cnotmatch '^-?[0-9]+$') { throw 'Unexpected nonnumeric property; refusing to export it.' }
                break
            }
            default { throw "Unrecognized thread property $name; review its privacy and semantics before export." }
        }
        $properties[$name] = [ordered] @{ '$type' = $property['$type']; '$value' = $value }
    }
    $mapped = [ordered] @{}
    foreach ($key in $Thread.identities.Keys) { $mapped[$key] = @{ id = Convert-IdentityId $Thread.identities[$key].id } }
    @{
        id = $Id
        publishedDate = Convert-EvidenceTime $Thread.publishedDate
        lastUpdatedDate = Convert-EvidenceTime $Thread.lastUpdatedDate
        properties = $properties; identities = $mapped
        comments = @($Thread.comments | ForEach-Object {
            @{
                id = $_.id; commentType = $_.commentType
                publishedDate = Convert-EvidenceTime $_.publishedDate
                lastUpdatedDate = Convert-EvidenceTime $_.lastUpdatedDate
            }
        })
    }
}

$stages = @()
$threadIds = @{}
$policyIds = @{}
foreach ($capture in $record.captures) {
    $view = $capture.view
    $label = if ($view.sourceCommit -ceq $record.captures[0].view.sourceCommit) {
        if ($view.reviewers.Count -eq 0) { 'baseline' } else { 'approved' }
    } elseif ($view.sourceCommit -ceq $record.effects[1].response.commits[0].commitId) { 'source-updated' }
    elseif ($view.sourceCommit -ceq $record.effects[2].response.commits[0].commitId) {
        if ($view.reviewers.Count -eq 0) { 'reviewer-removed' } else { 'source-cleaned' }
    } else { throw 'Capture is outside the authorized source-commit sequence.' }
    if ($label -cin @($stages | ForEach-Object { $_.label })) { continue }
    $reviewers = @($capture.raw.reviewers.value | ForEach-Object {
        $reviewer = [ordered] @{ id = Convert-IdentityId $_.id; vote = $_.vote }
        foreach ($field in @('isRequired', 'isFlagged', 'hasDeclined', 'isContainer', 'votedFor')) {
            if ($_.Contains($field)) {
                if ($field -ceq 'votedFor' -and @($_[$field]).Count -gt 0) { throw 'Group voting is not part of this fixture.' }
                $reviewer[$field] = $_[$field]
            }
        }
        $reviewer
    })
    $iterations = @($capture.raw.iterations.value | ForEach-Object {
        @{
            id = $_.id; reason = $_.reason
            createdDate = Convert-EvidenceTime $_.createdDate; updatedDate = Convert-EvidenceTime $_.updatedDate
            author = @{ id = Convert-IdentityId $_.author.id }
            sourceRefCommit = @{ commitId = Convert-CommitId $_.sourceRefCommit.commitId }
            targetRefCommit = @{ commitId = Convert-CommitId $_.targetRefCommit.commitId }
            commonRefCommit = @{ commitId = Convert-CommitId $_.commonRefCommit.commitId }
        }
    })
    $threads = @($capture.raw.threads.value | ForEach-Object {
        $key = [string] $_.id
        if (-not $threadIds.ContainsKey($key)) { $threadIds[$key] = $threadIds.Count + 1 }
        Convert-EvidenceThread $_ $threadIds[$key]
    })
    $policies = @($capture.raw.policyPages | ForEach-Object { $_.data.value } | ForEach-Object {
        $policyKey = [string] $_.id
        if (-not $policyIds.ContainsKey($policyKey)) { $policyIds[$policyKey] = $policyIds.Count + 1 }
        $settings = [ordered] @{}
        foreach ($key in $_.settings.Keys) {
            $value = $_.settings[$key]
            if ($key -ceq 'scope') {
                $value = @($value | ForEach-Object {
                    $inputScope = $_
                    $scope = [ordered] @{}
                    foreach ($field in $inputScope.Keys) {
                        $scope[$field] = switch ($field) {
                            'repositoryId' { if ($null -eq $inputScope[$field]) { $null } elseif ($inputScope[$field] -ceq $record.repositoryId) { $repository } else { throw 'Unexpected scoped repository.' } }
                            'refName' { if ($null -eq $inputScope[$field]) { $null } else { Convert-RefName $inputScope[$field] } }
                            'matchKind' { if ($inputScope[$field] -cnotin @('exact', 'prefix')) { throw 'Unknown scope match kind.' }; $inputScope[$field] }
                            default { throw 'Unknown policy scope property.' }
                        }
                    }
                    $scope
                })
            } elseif ($null -ne $value -and $value -isnot [bool] -and $value -isnot [ValueType]) {
                throw "Policy setting $key contains potentially private content."
            }
            $settings[$key] = $value
        }
        @{
            id = $policyIds[$policyKey]; revision = $_.revision
            type = @{ id = $_.type.id }; isEnabled = $_.isEnabled; isBlocking = $_.isBlocking
            settings = $settings
        }
    })
    if (@($capture.raw.evaluationPages | ForEach-Object { $_.value }).Count -ne 0) {
        throw 'This fixture is only for the observed absence of approval-policy evaluations.'
    }
    $stages += @{
        label = $label; observedAt = Convert-EvidenceTime $capture.capturedAt
        pullRequest = @{
            pullRequestId = 1; status = $view.status; isDraft = $view.draft
            sourceRefName = Convert-RefName $view.sourceRef; targetRefName = Convert-RefName $view.targetRef
            repository = @{ id = $repository; project = @{ id = $project } }
        }
        sourceCommit = Convert-CommitId $view.sourceCommit; targetCommit = Convert-CommitId $view.targetCommit
        autoComplete = $view.autoComplete; reviewers = $reviewers; iterations = $iterations
        threads = $threads; policies = $policies; evaluations = @()
    }
}
$fixture = [ordered] @{
    schemaVersion = 1; scenario = 'ado-no-approval-policy'; accountId = $account
    provenance = @{
        source = 'Authorized ADO Services experiment captured by dev/ss-autoreapprove/Invoke-AdoExperiment.ps1.'
        actions = @('Create the authenticated reviewer with vote 10', 'Add one unique text file in a source commit', 'Delete that file in a source commit', 'Delete only the experiment-created reviewer')
        sanitization = 'Projected service fields only; identities, refs, commits, and thread IDs replaced consistently; timestamps shifted by one constant interval; names, URLs, prose, and private code omitted.'
        limitation = 'No approval policy or policy evaluations applied. Both pushes retained vote 10 without a new vote event. This does not demonstrate a reset or a required fresh-iteration renewal.'
    }
    stages = $stages
}
$json = ConvertTo-Json -InputObject $fixture -Depth 100
foreach ($private in @($record.accountId, $record.repositoryId, $record.projectId, $record.sourceRef, $record.targetRef, $record.pullRequestUrl) + @($commits.Keys)) {
    if ($json.Contains($private, [StringComparison]::Ordinal)) { throw 'Private source identifiers survived fixture projection.' }
}
Publish-ExperimentFixture $OutputPath $fixture
