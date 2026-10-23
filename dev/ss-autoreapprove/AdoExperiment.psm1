#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Development-only service captures implement implementation.md, "Required ADO
# service captures". They are not approval authority or runtime skill state.

function Invoke-ExperimentFileOperation {
    param([scriptblock] $Operation)

    # Match the repository's bounded shared-file contention budget.
    for ($attempt = 0; $attempt -le 600; $attempt++) {
        try { return & $Operation } catch [IO.IOException] {
            $code = $_.Exception.HResult -band 0xffff
            if ($code -notin @(11, 32, 33) -or $attempt -eq 600) { throw }
            Wait-ExperimentContention
        }
    }
}

# A separate wait boundary lets tests coordinate contention without elapsed time.
function Wait-ExperimentContention { Start-Sleep -Milliseconds 50 }
function Get-ExperimentTime { [datetime]::UtcNow.ToString('o') }

# Canonical property order makes evidence comparisons independent of JSON key order.
function ConvertTo-ExperimentCanonical {
    param([AllowNull()] $Value)
    if ($Value -is [Collections.IDictionary]) {
        $keys = [string[]] @($Value.Keys)
        [Array]::Sort($keys, [StringComparer]::Ordinal)
        $result = [ordered] @{}
        foreach ($key in $keys) { $result[$key] = ConvertTo-ExperimentCanonical $Value[$key] }
        return $result
    }
    if ($Value -is [array]) {
        return ,@($Value | ForEach-Object { ConvertTo-ExperimentCanonical $_ })
    }
    return $Value
}

function Get-ExperimentDigest {
    param([AllowNull()] $Value)
    $json = ConvertTo-Json -InputObject (ConvertTo-ExperimentCanonical $Value) -Depth 100 -Compress
    [Convert]::ToHexStringLower([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($json)))
}

# Strict operational schema surrounds opaque, unmodified provider JSON evidence.
function Assert-ExperimentRecord {
    param([Collections.IDictionary] $Record)
    $schema = Join-Path $PSScriptRoot 'ado-experiment.schema.json'
    $json = ConvertTo-Json -InputObject $Record -Depth 100
    if (-not (Test-Json -Json $json -SchemaFile $schema -ErrorAction Stop)) {
        throw 'Invalid ADO experiment record.'
    }
    if ($Record.filePath -cne "/.ss-autoreapprove-$($Record.operationId).txt") {
        throw 'Experiment file does not match its operation identity.'
    }
    [guid]::Parse($Record.operationId) | Out-Null
    $steps = @('Approve', 'AddFile', 'RemoveFile', 'RemoveReviewer')
    for ($index = 0; $index -lt $Record.effects.Count; $index++) {
        $effect = $Record.effects[$index]
        if ($effect.kind -cne $steps[$index] -or
            ($effect.state -cne 'verified' -and ($index -ne $Record.effects.Count - 1 -or $Record.nextStep -cne 'Stopped'))) {
            throw 'Experiment effects do not form a complete ordered prefix.'
        }
    }
    $expected = if ($Record.effects.Count -eq $steps.Count) { 'Done' } else { $steps[$Record.effects.Count] }
    if ($Record.nextStep -cne 'Stopped' -and ($Record.nextStep -cne $expected -or $null -ne $Record.error)) {
        throw 'Experiment lifecycle disagrees with its effects.'
    }
}

# The caller retains the sibling lock across validation, reads, and every effect.
function Write-ExperimentRecord {
    param([string] $Path, [Collections.IDictionary] $Record)
    Assert-ExperimentRecord $Record
    Write-ExperimentJson $Path $Record
}

# Both private records and sanitized fixtures use the same atomic file protocol.
function Write-ExperimentJson {
    param([string] $Path, [Collections.IDictionary] $Value)
    $temporary = "$Path.$([guid]::NewGuid()).tmp"
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $Value -Depth 100))
        $stream = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($bytes); $stream.Flush($true) } finally { $stream.Dispose() }
        Invoke-ExperimentFileOperation {
            if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, [NullString]::Value) }
            else { [IO.File]::Move($temporary, $Path) }
        }
    } finally {
        if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
}

# Validate both generated and consumed fixtures; opaque API subsets retain provider field names.
function Assert-ExperimentFixture {
    param([Collections.IDictionary] $Fixture)
    $schema = Join-Path $PSScriptRoot 'ado-experiment-fixture.schema.json'
    if (-not (Test-Json -Json (ConvertTo-Json -InputObject $Fixture -Depth 100) -SchemaFile $schema -ErrorAction Stop)) {
        throw 'Invalid ADO experiment fixture.'
    }
    if (($Fixture.stages.label -join ',') -cne 'baseline,approved,source-updated,source-cleaned,reviewer-removed') {
        throw 'The complete controlled sequence was not observed.'
    }
}

# Initialization is one retained transaction, including conflict and schema checks.
function Publish-ExperimentFixture {
    param([string] $Path, [Collections.IDictionary] $Fixture)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    $lock = Invoke-ExperimentFileOperation {
        [IO.File]::Open("$Path.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    }
    try {
        if ([IO.File]::Exists($Path)) { throw 'Refusing to overwrite an existing fixture.' }
        Assert-ExperimentFixture $Fixture
        Write-ExperimentJson $Path $Fixture
    } finally { $lock.Dispose() }
}

function Read-ExperimentRecord {
    param([string] $Path)
    $stream = Invoke-ExperimentFileOperation {
        [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::Read -bor [IO.FileShare]::Delete))
    }
    try {
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false, $true))
        try { $record = $reader.ReadToEnd() | ConvertFrom-Json -AsHashtable -DateKind String }
        finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
    Assert-ExperimentRecord $record
    return $record
}

# The Azure CLI owns OAuth acquisition; neither tokens nor request headers enter records.
function Get-ExperimentToken {
    param([string] $Tenant)
    $arguments = @('account', 'get-access-token', '--resource', '499b84ac-1321-427f-aa17-267ca6975798', '--query', 'accessToken', '--output', 'tsv')
    if ($Tenant) { $arguments += @('--tenant', $Tenant) }
    $token = & az @arguments
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($token)) { throw 'Azure DevOps OAuth acquisition failed.' }
    ConvertTo-SecureString $token.Trim() -AsPlainText
}

# One HTTP attempt only: an uncertain write must be inspected, never retried.
function Invoke-ExperimentRequest {
    param([string] $Method, [string] $Url, [securestring] $Token, [AllowNull()] $Body = $null)
    $arguments = @{
        Uri = $Url; Method = $Method; Authentication = 'Bearer'; Token = $Token
        MaximumRedirection = 0; SkipHttpErrorCheck = $true
    }
    if ($null -ne $Body) {
        $arguments.Body = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $Body -Depth 100 -Compress))
        $arguments.ContentType = 'application/json; charset=utf-8'
    }
    $response = Invoke-WebRequest @arguments
    if ($response.StatusCode -lt 200 -or $response.StatusCode -ge 300) {
        throw "ADO HTTP $($response.StatusCode) for $Method $Url`: $($response.Content)"
    }
    $data = if ($response.StatusCode -eq 204) { $null }
        else { $response.Content | ConvertFrom-Json -AsHashtable -DateKind String }
    $continuation = @($response.Headers['x-ms-continuationtoken'])
    @{
        data = $data
        continuation = if ($continuation.Count -gt 0 -and $continuation[0]) { [string] $continuation[0] } else { $null }
    }
}

function Get-ExperimentResource {
    param([string] $Url, [securestring] $Token)
    $response = Invoke-ExperimentRequest -Method GET -Url $Url -Token $Token
    if ($response.continuation) { throw "Unexpected continuation for $Url" }
    return $response.data
}

function Assert-ExperimentList {
    param([Collections.IDictionary] $Data)
    if (-not $Data.Contains('value') -or $Data.value -isnot [array] -or
        -not $Data.Contains('count') -or $Data.count -ne $Data.value.Count) {
        throw 'Incomplete ADO list evidence.'
    }
}

# Restrict credentials to documented ADO Services hosts, without following redirects.
function Get-ExperimentTarget {
    param([uri] $Url)
    if ($Url.Scheme -cne 'https' -or $Url.UserInfo -or $Url.Query -or $Url.Fragment -or -not $Url.IsDefaultPort) {
        throw 'Supply an HTTPS ADO pull request browser URL.'
    }
    $parts = @($Url.AbsolutePath.Trim('/').Split('/'))
    if ($Url.Host -ceq 'dev.azure.com' -and $parts.Count -eq 6) {
        $collection = "https://dev.azure.com/$($parts[0])"
        $parts = $parts[1..5]
    } elseif ($Url.Host -cmatch '^[a-zA-Z0-9-]+\.visualstudio\.com$' -and $parts.Count -eq 5) {
        $collection = "https://$($Url.Host)"
    } else { throw 'Unsupported ADO pull request URL.' }
    if ($parts[1] -cne '_git' -or $parts[3] -cne 'pullrequest' -or $parts[4] -cnotmatch '^[1-9][0-9]*$') {
        throw 'Invalid ADO pull request coordinates.'
    }
    @{
        collection = $collection
        project = "$collection/$($parts[0])"
        repository = "$collection/$($parts[0])/_apis/git/repositories/$($parts[2])"
        pr = "$collection/$($parts[0])/_apis/git/repositories/$($parts[2])/pullRequests/$($parts[4])"
        id = $parts[4]
    }
}

# Read only the exact named ref, rejecting omitted or ambiguous matches.
function Get-ExperimentRef {
    param([hashtable] $Target, [string] $Name, [securestring] $Token)
    if (-not $Name.StartsWith('refs/heads/', [StringComparison]::Ordinal)) { throw 'Only branch refs are supported.' }
    $filter = [uri]::EscapeDataString($Name.Substring('refs/'.Length))
    $response = Get-ExperimentResource "$($Target.repository)/refs?filter=$filter&api-version=7.1" $Token
    Assert-ExperimentList $response
    $matches = @($response.value | Where-Object { $_.name -ceq $Name })
    if ($matches.Count -ne 1) { throw "No unique branch ref $Name" }
    return $matches[0]
}

# Captures raw service payloads as evidence, not a reconstructed authorization decision.
function Get-AdoExperimentCapture {
    param([hashtable] $Target, [securestring] $Token)
    $identity = Get-ExperimentResource "$($Target.collection)/_apis/connectionData?api-version=7.1-preview" $Token
    $before = Get-ExperimentResource "$($Target.pr)?api-version=7.1" $Token
    if ($before['forkSource']) { throw 'This controlled experiment does not support forked source repositories.' }
    $source = Get-ExperimentRef $Target $before.sourceRefName $Token
    $targetRef = Get-ExperimentRef $Target $before.targetRefName $Token
    $reviewers = Get-ExperimentResource "$($Target.pr)/reviewers?api-version=7.1" $Token
    $iterations = Get-ExperimentResource "$($Target.pr)/iterations?api-version=7.1" $Token
    $threads = Get-ExperimentResource "$($Target.pr)/threads?api-version=7.1" $Token
    foreach ($list in @($reviewers, $iterations, $threads)) { Assert-ExperimentList $list }
    $policies = @()
    $policyPages = @()
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $policyUrl = "$($Target.project)/_apis/git/policy/configurations?repositoryId=$($before.repository.id)&refName=$([uri]::EscapeDataString($before.targetRefName))&%24top=100&api-version=7.1"
    $url = $policyUrl
    do {
        $page = Invoke-ExperimentRequest -Method GET -Url $url -Token $Token
        Assert-ExperimentList $page.data
        $policyPages += ,$page
        foreach ($policy in $page.data.value) {
            if (-not $seen.Add([string] $policy.id)) { throw 'Repeated policy identifier.' }
            $policies += ,$policy
        }
        $url = if ($page.continuation) { "$policyUrl&continuationToken=$([uri]::EscapeDataString($page.continuation))" } else { $null }
        if ($page.continuation -and $page.data.value.Count -eq 0) { throw 'Policy pagination made no progress.' }
        # Match the native capture safety limit for endlessly advancing bad pages.
        if ($policyPages.Count -ge 1000) { throw 'Policy pages exceeded the capture safety limit.' }
    } while ($url)
    $evaluationPages = @()
    $seen.Clear()
    $skip = 0
    $artifact = [uri]::EscapeDataString("vstfs:///CodeReview/CodeReviewId/$($before.repository.project.id)/$($Target.id)")
    do {
        $page = Get-ExperimentResource "$($Target.project)/_apis/policy/evaluations?artifactId=$artifact&includeNotApplicable=true&%24top=100&%24skip=$skip&api-version=7.1-preview.1" $Token
        Assert-ExperimentList $page
        $evaluationPages += ,$page
        foreach ($evaluation in $page.value) {
            if (-not $seen.Add([string] $evaluation.evaluationId)) { throw 'Repeated policy evaluation.' }
        }
        $skip += $page.value.Count
        if ($evaluationPages.Count -ge 1000) { throw 'Policy evaluations exceeded the capture safety limit.' }
    } while ($page.value.Count -gt 0)
    $after = Get-ExperimentResource "$($Target.pr)?api-version=7.1" $Token
    $sourceAfter = Get-ExperimentRef $Target $after.sourceRefName $Token
    $targetAfter = Get-ExperimentRef $Target $after.targetRefName $Token
    foreach ($field in @('sourceRefName', 'targetRefName', 'status', 'isDraft', 'autoCompleteSetBy')) {
        if ((Get-ExperimentDigest $before[$field]) -cne (Get-ExperimentDigest $after[$field])) { throw "PR $field changed during capture." }
    }
    if ($source.objectId -cne $sourceAfter.objectId -or $targetRef.objectId -cne $targetAfter.objectId) {
        throw 'A branch moved during capture.'
    }
    if ((Get-ExperimentDigest @($reviewers.value)) -cne (Get-ExperimentDigest @($after.reviewers))) {
        throw 'Reviewers changed during capture.'
    }
    @{
        capturedAt = Get-ExperimentTime
        raw = @{
            identity = $identity; prBefore = $before; prAfter = $after
            sourceRef = $source; targetRef = $targetRef; reviewers = $reviewers
            iterations = $iterations; threads = $threads
            policyPages = $policyPages; evaluationPages = $evaluationPages
        }
        view = @{
            accountId = [string] $identity.authenticatedUser.id
            repositoryId = [string] $after.repository.id
            projectId = [string] $after.repository.project.id
            authorId = [string] $after.createdBy.id
            sourceRef = [string] $after.sourceRefName; targetRef = [string] $after.targetRefName
            sourceCommit = [string] $source.objectId; targetCommit = [string] $targetRef.objectId
            status = [string] $after.status; draft = $after.isDraft
            autoComplete = ($null -ne $after['autoCompleteSetBy'])
            reviewers = @($reviewers.value)
            policyDigest = Get-ExperimentDigest @($policies)
            voteHistoryDigest = Get-ExperimentDigest @($threads.value | Where-Object {
                $null -ne $_['properties'] -and $null -ne $_['properties']['CodeReviewThreadType'] -and
                $_['properties']['CodeReviewThreadType']['$value'] -in @('VoteUpdate', 'ReviewersUpdate')
            })
        }
    }
}

function Assert-ExperimentFence {
    param([Collections.IDictionary] $Record, [Collections.IDictionary] $View)
    foreach ($field in @('accountId', 'repositoryId', 'projectId', 'sourceRef', 'targetRef', 'sourceCommit', 'targetCommit', 'policyDigest')) {
        if ($Record[$field] -cne $View[$field]) { throw "Unexpected $field movement; no mutation is permitted." }
    }
    if ($View.status -cne 'active' -or $View.draft -ne $false -or $View.autoComplete) {
        throw 'PR must remain active, non-draft, with auto-complete off.'
    }
    if ((Get-ExperimentDigest $View.reviewers) -cne (Get-ExperimentDigest $Record.expectedReviewers) -or
        $View.voteHistoryDigest -cne $Record.voteHistoryDigest) {
        throw 'Reviewer metadata or vote history changed outside the experiment.'
    }
}

# Each named invocation owns a complete retained transaction; writes require explicit opt-in.
function Invoke-AdoExperiment {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)][ValidateSet('Initialize', 'Capture', 'Approve', 'AddFile', 'RemoveFile', 'RemoveReviewer')][string] $Step,
        [Parameter(Mandatory)][uri] $PullRequestUrl,
        [Parameter(Mandatory)][string] $StatePath,
        [string] $Tenant,
        [string] $ExpectedAccountId, [string] $ExpectedSourceRef, [string] $ExpectedTargetRef,
        [string] $ExpectedSourceCommit, [string] $ExpectedTargetCommit,
        [switch] $AllowRemoteWrites
    )
    if (-not [IO.Path]::IsPathFullyQualified($StatePath)) { throw 'StatePath must be absolute.' }
    if ($Step -notin @('Initialize', 'Capture') -and -not $AllowRemoteWrites) { throw 'Explicit write authorization is required.' }
    $target = Get-ExperimentTarget $PullRequestUrl
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($StatePath)) | Out-Null
    $lock = Invoke-ExperimentFileOperation {
        [IO.File]::Open("$StatePath.lock", [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    }
    try {
        $token = Get-ExperimentToken $Tenant
        if ($Step -eq 'Initialize') {
            if ([IO.File]::Exists($StatePath)) { throw 'An experiment record already exists.' }
            if (-not $ExpectedAccountId -or -not $ExpectedSourceRef -or -not $ExpectedTargetRef -or
                $ExpectedSourceCommit -cnotmatch '^[0-9a-f]{40}$' -or $ExpectedTargetCommit -cnotmatch '^[0-9a-f]{40}$') {
                throw 'Initialize requires the explicitly authorized identity, refs, and full commits.'
            }
            $capture = Get-AdoExperimentCapture $target $token
            $view = $capture.view
            if ($view.accountId -cne $ExpectedAccountId -or $view.sourceRef -cne $ExpectedSourceRef -or
                $view.targetRef -cne $ExpectedTargetRef -or $view.sourceCommit -cne $ExpectedSourceCommit -or
                $view.targetCommit -cne $ExpectedTargetCommit -or $view.reviewers.Count -ne 0) {
                throw 'Initial state does not match the authorized experiment.'
            }
            $id = [guid]::NewGuid().ToString()
            $record = [ordered] @{
                schemaVersion = 1; operationId = $id; pullRequestUrl = $PullRequestUrl.AbsoluteUri
                tenant = $Tenant; nextStep = 'Approve'; filePath = "/.ss-autoreapprove-$id.txt"
                accountId = $view.accountId; repositoryId = $view.repositoryId; projectId = $view.projectId
                sourceRef = $view.sourceRef; targetRef = $view.targetRef
                sourceCommit = $view.sourceCommit; targetCommit = $view.targetCommit
                policyDigest = $view.policyDigest; voteHistoryDigest = $view.voteHistoryDigest
                expectedReviewers = @(); captures = @($capture); effects = @(); error = $null
            }
            Assert-ExperimentFence $record $view
            Write-ExperimentRecord $StatePath $record
        } else {
            $record = Read-ExperimentRecord $StatePath
            if ($record.pullRequestUrl -cne $PullRequestUrl.AbsoluteUri -or $record.tenant -cne $Tenant) { throw 'Experiment scope changed.' }
            if ($Step -ne 'Capture' -and $record.nextStep -cne $Step) { throw "Expected $($record.nextStep), not $Step. Do not replay an uncertain write." }
            $capture = Get-AdoExperimentCapture $target $token
            $record.captures += ,$capture
            Write-ExperimentRecord $StatePath $record
            if ($Step -ne 'Capture') {
                Assert-ExperimentFence $record $capture.view
                $method = 'POST'
                $url = "$($target.repository)/pushes?api-version=7.1"
                if ($Step -eq 'Approve') {
                    $method = 'PUT'
                    $url = "$($target.pr)/reviewers/$($record.accountId)?api-version=7.1"
                    $body = @{ id = $record.accountId; vote = 10 }
                } elseif ($Step -eq 'RemoveReviewer') {
                    $method = 'DELETE'
                    $url = "$($target.pr)/reviewers/$($record.accountId)?api-version=7.1"
                    $body = $null
                } else {
                    $change = @{ changeType = if ($Step -eq 'AddFile') { 'add' } else { 'delete' }; item = @{ path = $record.filePath } }
                    if ($Step -eq 'AddFile') {
                        $change.newContent = @{ contentType = 'rawtext'; content = "Authorized ADO reapproval evidence $($record.operationId).`n" }
                    }
                    $body = @{
                        refUpdates = @(@{ name = $record.sourceRef; oldObjectId = $record.sourceCommit })
                        commits = @(@{ comment = "[Copilot speaking] $Step ADO reapproval evidence $($record.operationId)"; changes = @($change) })
                    }
                }
                $effect = [ordered] @{ kind = $Step; state = 'intent'; request = @{ method = $method; url = $url; body = $body }; response = $null; error = $null }
                $record.effects += ,$effect
                # A crash or ambiguous response cannot leave a replayable next step.
                $record.nextStep = 'Stopped'
                Write-ExperimentRecord $StatePath $record
                try {
                    $response = Invoke-ExperimentRequest -Method $method -Url $url -Token $token -Body $body
                    if ($response.continuation) { throw 'Unexpected write continuation.' }
                    $effect.response = $response.data
                    $effect.state = 'acknowledged'
                    Write-ExperimentRecord $StatePath $record
                    if ($Step -in @('AddFile', 'RemoveFile')) {
                        $push = $response.data
                        if (@($push.refUpdates).Count -ne 1 -or @($push.commits).Count -ne 1 -or
                            $push.refUpdates[0].name -cne $record.sourceRef -or
                            $push.refUpdates[0].oldObjectId -cne $record.sourceCommit -or
                            $push.refUpdates[0].newObjectId -cnotmatch '^[0-9a-f]{40}$' -or
                            $push.commits[0].commitId -cne $push.refUpdates[0].newObjectId) {
                            throw 'Push acknowledgement does not prove the intended single-branch commit.'
                        }
                        $record.sourceCommit = $push.refUpdates[0].newObjectId
                    }
                    $after = Get-AdoExperimentCapture $target $token
                    $record.captures += ,$after
                    if ($Step -eq 'Approve') {
                        if ($response.data.id -cne $record.accountId -or $response.data.vote -ne 10 -or
                            $after.view.reviewers.Count -ne 1 -or $after.view.reviewers[0].id -cne $record.accountId -or
                            $after.view.reviewers[0].vote -ne 10 -or
                            (Get-ExperimentDigest $response.data) -cne (Get-ExperimentDigest $after.view.reviewers[0])) {
                            throw 'Approval acknowledgement or reviewer readback disagrees.'
                        }
                        $record.expectedReviewers = $after.view.reviewers
                        $record.voteHistoryDigest = $after.view.voteHistoryDigest
                    } elseif ($Step -eq 'RemoveReviewer') {
                        $record.expectedReviewers = @()
                        $record.voteHistoryDigest = $after.view.voteHistoryDigest
                    }
                    Assert-ExperimentFence $record $after.view
                    $effect.state = 'verified'
                    $record.nextStep = @{ Approve = 'AddFile'; AddFile = 'RemoveFile'; RemoveFile = 'RemoveReviewer'; RemoveReviewer = 'Done' }[$Step]
                    Write-ExperimentRecord $StatePath $record
                } catch {
                    $effect.error = $_.Exception.Message
                    if ($effect.state -eq 'intent') { $effect.state = 'unknown' }
                    $record.error = $_.Exception.Message
                    Write-ExperimentRecord $StatePath $record
                    throw
                }
            }
        }
        [pscustomobject] @{
            statePath = $StatePath; operationId = $record.operationId; nextStep = $record.nextStep
            sourceCommit = $record.sourceCommit; targetCommit = $record.targetCommit
            captureCount = $record.captures.Count; effects = @($record.effects | ForEach-Object { "$($_.kind):$($_.state)" })
        }
    } finally { $lock.Dispose() }
}

Export-ModuleMember -Function Invoke-AdoExperiment, Read-ExperimentRecord, Assert-ExperimentFixture, Publish-ExperimentFixture
