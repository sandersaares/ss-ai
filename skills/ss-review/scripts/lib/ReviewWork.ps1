#Requires -Version 7.6

# Durable focus-area dispatch and private file-group iteration.
# Ref: docs/implementation.md, "Review work queues".
# Executables load JsonValue.ps1 and PullRequestModel.ps1 before this library.
# Artifact initialization and the coverage gate also load FindingModel.ps1.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

function Read-ReviewFileInventory {
    param(
        [Parameter(Mandatory)]
        [string] $Path,
        [string] $Json
    )

    if (-not $PSBoundParameters.ContainsKey('Json')) { $Json = Read-PullRequestFileText -Path $Path }
    $files = ConvertFrom-JsonVerbatim -Text $Json
    if ($files -isnot [object[]] -or $files.Count -eq 0) {
        throw "The file inventory '$Path' must be a nonempty JSON array."
    }

    # Ordinal comparison preserves distinct case-sensitive paths without assuming filesystem rules.
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($file in $files) {
        $filePath = Get-JsonProperty -Node $file -Name 'path'
        $size = Get-JsonProperty -Node $file -Name 'sizeBytes'
        $change = Get-JsonProperty -Node $file -Name 'change'
        if ($filePath -isnot [string] -or [string]::IsNullOrWhiteSpace($filePath) -or
            -not [IO.Path]::IsPathFullyQualified($filePath)) {
            throw "Each file in '$Path' requires an absolute 'path'."
        }
        if (-not $seen.Add([IO.Path]::GetFullPath($filePath))) {
            throw "The file inventory '$Path' repeats '$filePath'."
        }
        if ($size -isnot [long] -or $size -lt 0) {
            throw "File '$filePath' requires a nonnegative integer 'sizeBytes'."
        }
        if (@('new', 'modified', 'deleted') -cnotcontains $change) {
            throw "File '$filePath' requires 'change' to be new, modified, or deleted."
        }
    }

    Assert-WorkflowData -Kind inventory -Value $files
    , $files
}

function Read-ReviewFileGroups {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [object[]] $FileInventory,
        [string] $Json
    )

    if (-not $PSBoundParameters.ContainsKey('Json')) { $Json = Read-PullRequestFileText -Path $Path }
    $groups = ConvertFrom-JsonVerbatim -Text $Json
    if ($groups -isnot [object[]] -or $groups.Count -eq 0) {
        throw "The grouped inventory '$Path' must be a nonempty JSON array."
    }

    $inventory = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($file in $FileInventory) { $inventory.Add($file['path'], $file) }
    $seenFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $seenGroups = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($group in $groups) {
        $id = Get-JsonProperty -Node $group -Name 'groupId'
        $files = Get-JsonProperty -Node $group -Name 'files'
        if ($id -isnot [string] -or $id -cnotmatch '^[A-Za-z0-9_-]+$' -or -not $seenGroups.Add($id)) {
            throw "The grouped inventory '$Path' requires unique 'groupId' identifiers using letters, digits, dashes, and underscores."
        }
        if ($files -isnot [object[]] -or $files.Count -eq 0) {
            throw "Group '$id' requires a nonempty 'files' array."
        }
        # Ref: docs/design.md, "Review coverage"; bounds leave room for guidance between compactions.
        if ($files.Count -gt 20) {
            throw "Group '$id' exceeds the file-count limit."
        }
        $bytes = [decimal] 0
        foreach ($file in $files) {
            $filePath = Get-JsonProperty -Node $file -Name 'path'
            if ($filePath -isnot [string] -or -not $inventory.ContainsKey($filePath)) {
                throw "Group '$id' names a file outside the inventory: '$filePath'."
            }
            if (-not $seenFiles.Add($filePath)) {
                throw "The grouped inventory '$Path' repeats '$filePath'."
            }
            $original = $inventory[$filePath]
            $size = Get-JsonProperty -Node $file -Name 'sizeBytes'
            $change = Get-JsonProperty -Node $file -Name 'change'
            if ($size -isnot [long] -or $size -ne $original['sizeBytes'] -or $change -cne $original['change']) {
                throw "Group '$id' changes inventory metadata for '$filePath'."
            }
            $bytes += $size
        }
        # A file larger than the group budget must remain reviewable as a singleton.
        # Ref: docs/design.md, "Review coverage".
        if ($files.Count -gt 1 -and $bytes -gt 100000) {
            throw "Group '$id' exceeds the byte limit; oversized files require singleton groups."
        }
    }
    if ($seenFiles.Count -ne $inventory.Count) {
        $missing = @($inventory.Keys | Where-Object { -not $seenFiles.Contains($_) })
        throw "The grouped inventory '$Path' omits files: $($missing -join ', ')."
    }

    Assert-WorkflowData -Kind fileGroups -Value $groups
    , $groups
}

# Binds validation to exactly the text read, rather than hashing a later read of mutable paths.
function Get-ReviewTextHash {
    param([Parameter(Mandatory)][string] $Text)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Text)))
}

# Each transaction reads and validates its immutable inputs once; journal state is read under lock.
function Read-ReviewWorkInputs {
    param([Parameter(Mandatory)][string] $InventoryPath, [Parameter(Mandatory)][string] $FileGroupsPath)
    $inventoryText = Read-PullRequestFileText -Path $InventoryPath
    $groupsText = Read-PullRequestFileText -Path $FileGroupsPath
    $files = Read-ReviewFileInventory -Path $InventoryPath -Json $inventoryText
    $groups = Read-ReviewFileGroups -Path $FileGroupsPath -FileInventory $files -Json $groupsText
    @{
        files = $files
        groups = $groups
        snapshot = [ordered]@{
            inventorySha256 = Get-ReviewTextHash -Text $inventoryText
            fileGroupsSha256 = Get-ReviewTextHash -Text $groupsText
        }
    }
}

# Missing bindings are supported only by the legacy per-file repair path, never by batch completion.
function Assert-ReviewSnapshot {
    param([AllowNull()] $Expected, [Parameter(Mandatory)] $Actual)
    if ($null -eq $Expected -or $Expected['inventorySha256'] -cne $Actual['inventorySha256'] -or
        $Expected['fileGroupsSha256'] -cne $Actual['fileGroupsSha256']) {
        throw 'Stale or missing review snapshot identity. Reuse the assigned immutable inputs; do not reset the journal.'
    }
}

function New-ReviewQueueDocument {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('focusAreas', 'fileGroups')]
        [string] $Kind,

        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [object[]] $Item
    )

    if ($Kind -ceq 'fileGroups') {
        # Claims and completion records share one atomic write with the pending queue.
        # Ref: docs/implementation.md, "Review work queues".
        return [ordered]@{
            schemaVersion = 3
            kind = $Kind
            items = $Item
            inProgress = $null
            completedGroupIds = @()
            assessments = @()
            snapshot = $null
            failure = $null
            batchReceipts = @()
        }
    }
    [ordered]@{ schemaVersion = 1; kind = $Kind; items = $Item }
}

function Read-ReviewQueueDocument {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $queue = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $Path)
    $kind = Get-JsonProperty -Node $queue -Name 'kind'
    $version = if ($kind -ceq 'fileGroups') { 3 } else { 1 }
    if ((Get-JsonProperty -Node $queue -Name 'schemaVersion') -ne $version -or
        @('focusAreas', 'fileGroups') -cnotcontains $kind -or
        (Get-JsonProperty -Node $queue -Name 'items') -isnot [object[]]) {
        throw "Invalid review queue '$Path': expected supported schemaVersion, kind, and an items array. Do not reset existing review state."
    }
    foreach ($item in $queue['items']) {
        if ($item -isnot [Collections.IDictionary]) {
            throw "Invalid review queue '$Path': each item must be an object."
        }
    }
    if ($kind -ceq 'fileGroups') {
        Assert-WorkflowData -Kind fileGroupJournal -Value $queue
        if ((Get-JsonProperty -Node $queue -Name 'assessments') -isnot [object[]]) {
            throw "Invalid group journal '$Path': expected an assessments array."
        }
        $assessed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($assessment in $queue['assessments']) {
            if (-not [IO.Path]::IsPathFullyQualified($assessment['path']) -or
                -not $assessed.Add($assessment['path'])) {
                throw "Invalid or duplicate assessment path in '$Path'."
            }
        }
        if (-not $queue.Contains('inProgress') -or
            ($null -ne $queue['inProgress'] -and $queue['inProgress'] -isnot [Collections.IDictionary]) -or
            (Get-JsonProperty -Node $queue -Name 'completedGroupIds') -isnot [object[]]) {
            throw "Invalid group journal '$Path': expected inProgress and a completedGroupIds array."
        }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($id in $queue['completedGroupIds']) {
            if ($id -isnot [string] -or $id -cnotmatch '^[A-Za-z0-9_-]+$' -or -not $seen.Add($id)) {
                throw "Invalid group journal '$Path': invalid or repeated completed group identifier."
            }
        }
        $outstanding = @($queue['items'])
        if ($null -ne $queue['inProgress']) { $outstanding += $queue['inProgress'] }
        foreach ($group in $outstanding) {
            $id = Get-JsonProperty -Node $group -Name 'groupId'
            if ($id -isnot [string] -or $id -cnotmatch '^[A-Za-z0-9_-]+$' -or -not $seen.Add($id) -or
                (Get-JsonProperty -Node $group -Name 'files') -isnot [object[]] -or $group['files'].Count -eq 0) {
                throw "Invalid group journal '$Path': each pending or in-progress group must be unique and contain files."
            }
        }
        $receipts = Get-JsonProperty -Node $queue -Name 'batchReceipts'
        $receiptIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($receipt in $receipts) {
            if (-not $receiptIds.Add($receipt['groupId']) -or
                $queue['completedGroupIds'] -cnotcontains $receipt['groupId'] -or
                -not [IO.Path]::IsPathFullyQualified($receipt['artifactPath'])) {
                throw "Invalid batch completion receipt in '$Path'."
            }
        }
    }
    $queue
}

function Open-ReviewQueueLock {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    Open-PullRequestFileLock -Path $Path
}

function Get-NextReviewWorkItem {
    param(
        [Parameter(Mandatory)]
        [string] $QueuePath
    )

    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($null -ne (Get-JsonProperty -Node $queue -Name failure)) {
            throw 'Reviewer failure is recorded. Confirm the old worker stopped and resume its journal before claiming more work.'
        }
        if ($queue['kind'] -ceq 'fileGroups' -and $null -ne $queue['inProgress']) {
            return [ordered]@{ exhausted = $false; item = $queue['inProgress']; remainingCount = $queue['items'].Count; snapshot = Get-JsonProperty -Node $queue -Name snapshot }
        }
        $items = $queue['items']
        if ($items.Count -eq 0) {
            return [ordered]@{ exhausted = $true; item = $null; remainingCount = 0 }
        }

        $item = $items[0]
        $queue['items'] = @($items | Select-Object -Skip 1)
        if ($queue['kind'] -ceq 'fileGroups') { $queue['inProgress'] = $item }
        Write-PullRequestOutputFileAtomically -Path $QueuePath -Content (ConvertTo-JsonText -Value $queue)
        [ordered]@{ exhausted = $false; item = $item; remainingCount = $queue['items'].Count; snapshot = Get-JsonProperty -Node $queue -Name snapshot }
    } finally {
        $lock.Dispose()
    }
}

function Complete-ReviewFileGroup {
    param(
        [Parameter(Mandatory)]
        [string] $QueuePath,

        [Parameter(Mandatory)]
        [string] $GroupId
    )

    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($queue['kind'] -cne 'fileGroups') {
            throw "Queue '$QueuePath' is not a file-group journal."
        }
        if ($null -ne (Get-JsonProperty -Node $queue -Name failure)) { throw 'Cannot complete failed work before explicit resume.' }
        $alreadyComplete = $queue['completedGroupIds'] -ccontains $GroupId
        if (-not $alreadyComplete) {
            if ((Get-JsonProperty -Node $queue['inProgress'] -Name 'groupId') -cne $GroupId) {
                throw "Group '$GroupId' is not the current in-progress group in '$QueuePath'."
            }
            Assert-ReviewFileAssessments -Queue $queue -Group $queue['inProgress']
            $queue['completedGroupIds'] = @($queue['completedGroupIds']) + $GroupId
            $queue['inProgress'] = $null
            Write-PullRequestOutputFileAtomically -Path $QueuePath -Content (ConvertTo-JsonText -Value $queue)
        }
        [ordered]@{
            groupId = $GroupId
            alreadyComplete = $alreadyComplete
            completedGroupCount = $queue['completedGroupIds'].Count
            remainingCount = $queue['items'].Count
        }
    } finally {
        $lock.Dispose()
    }
}

# Recovery uses the immutable group inventory to detect lost or altered queue entries.
function Assert-ReviewGroupJournalCoverage {
    param(
        [Parameter(Mandatory)]
        [Collections.IDictionary] $Queue,

        [Parameter(Mandatory)]
        [object[]] $Group,

        # Targeted repair may rebuild missing assessments one file at a time, but resume and
        # final coverage validation must still require the whole completed group.
        [switch] $AllowIncompleteAssessments
    )

    if ($Queue['kind'] -cne 'fileGroups') { throw 'Expected a file-group journal.' }
    $expected = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Group) { $expected.Add($entry['groupId'], $entry) }
    $ids = @($Queue['completedGroupIds'])
    $outstanding = @($Queue['items'])
    if ($null -ne $Queue['inProgress']) { $outstanding += $Queue['inProgress'] }
    foreach ($entry in $outstanding) {
        $id = $entry['groupId']
        $ids += $id
        if (-not $expected.ContainsKey($id)) { throw "Group journal contains unknown group '$id'." }
        $files = $entry['files']
        $expectedFiles = $expected[$id]['files']
        if ($files.Count -ne $expectedFiles.Count) { throw "Group journal changed files for '$id'." }
        for ($index = 0; $index -lt $files.Count; $index++) {
            foreach ($field in @('path', 'sizeBytes', 'change')) {
                if ((Get-JsonProperty -Node $files[$index] -Name $field) -cne $expectedFiles[$index][$field]) {
                    throw "Group journal changed '$field' for '$id'."
                }
            }
        }
    }
    if ($ids.Count -ne $expected.Count) { throw 'Group journal does not cover the complete grouped inventory.' }
    foreach ($id in $ids) {
        if (-not $expected.ContainsKey($id)) { throw "Group journal contains unknown group '$id'." }
    }
    foreach ($assessment in $Queue['assessments']) {
        $id = $assessment['groupId']
        if (-not $expected.ContainsKey($id) -or
            @($expected[$id]['files'].path) -cnotcontains $assessment['path']) {
            throw "Assessment names a file outside group '$id'."
        }
    }
    if (-not $AllowIncompleteAssessments) {
        foreach ($id in $Queue['completedGroupIds']) {
            Assert-ReviewFileAssessments -Queue $Queue -Group $expected[$id]
        }
    }
}

# Each completion declares a disposition for every file; text judgments still require human/agent
# examination. Keeping these records in the journal makes resume and completion share one authority.
function Assert-ReviewFileAssessments {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Queue,
        [Parameter(Mandatory)][Collections.IDictionary] $Group
    )
    $paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($assessment in $Queue['assessments']) {
        if ($assessment['groupId'] -ceq $Group['groupId']) { [void] $paths.Add($assessment['path']) }
    }
    foreach ($file in $Group['files']) {
        if (-not $paths.Contains($file['path'])) { throw "File '$($file['path'])' requires one persisted assessment before group completion." }
    }
}

# Records or repairs one file's assessment without replacing other files or the completion journal.
function Set-ReviewFileAssessment {
    param(
        [Parameter(Mandatory)][string] $QueuePath,
        [Parameter(Mandatory)][string] $InventoryPath,
        [Parameter(Mandatory)][string] $FileGroupsPath,
        [Parameter(Mandatory)][Collections.IDictionary] $Assessment
    )
    Assert-WorkflowData -Kind assessment -Value $Assessment
    $inputs = Read-ReviewWorkInputs -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath
    $groups = $inputs.groups
    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($null -ne (Get-JsonProperty -Node $queue -Name snapshot)) {
            Assert-ReviewSnapshot -Expected $queue['snapshot'] -Actual $inputs.snapshot
        }
        $group = @($groups | Where-Object { $_['groupId'] -ceq $Assessment['groupId'] })
        if ($group.Count -ne 1 -or @($group[0]['files'].path) -cnotcontains $Assessment['path']) {
            throw 'Assessment must name a file in its assigned group.'
        }
        if ($queue['completedGroupIds'] -cnotcontains $Assessment['groupId'] -and
            (Get-JsonProperty -Node $queue['inProgress'] -Name 'groupId') -cne $Assessment['groupId']) {
            throw 'Assessments require a claimed group or a completed group assigned for repair.'
        }
        $queue['assessments'] = @($queue['assessments'] | Where-Object { $_['path'] -cne $Assessment['path'] }) + @($Assessment)
        Assert-ReviewGroupJournalCoverage -Queue $queue -Group $groups -AllowIncompleteAssessments
        Write-PullRequestOutputFileAtomically -Path $QueuePath -Content (ConvertTo-JsonText -Value $queue)
    } finally { $lock.Dispose() }
}

# A receipt binds completion to durable finding identifiers, including an explicitly empty set.
function Assert-ReviewBatchFindings {
    param([Parameter(Mandatory)][string] $ArtifactPath, [Parameter(Mandatory)][AllowEmptyCollection()][string[]] $FindingId)
    $artifact = Read-FindingArtifact -Path $ArtifactPath
    $problems = Get-FindingArtifactProblem -Document $artifact -Path $ArtifactPath
    if ($problems.Count -gt 0) { throw ($problems -join "`n") }
    if ($artifact['kind'] -cne 'reviewer' -or
        (Get-JsonProperty -Node $artifact -Name findings) -isnot [object[]]) {
        throw 'Batch completion requires a reviewer finding artifact.'
    }
    $ids = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    # Receipts retain withdrawn findings as evidence of completed work, not as active findings.
    foreach ($id in (Get-FindingIdentifierList -Document $artifact)) {
        if (-not $ids.Add($id)) { throw 'Duplicate persisted finding identifier.' }
    }
    foreach ($id in $FindingId) {
        if (-not $ids.Contains($id)) { throw "Required finding '$id' is not persisted in '$ArtifactPath'." }
    }
}

# Canonical field and file order makes semantically identical retries independent of JSON formatting.
function Get-ReviewBatchHash {
    param([Parameter(Mandatory)] $Group, [Parameter(Mandatory)] $Assessment, [Parameter(Mandatory)] $Receipt)
    $records = @(foreach ($file in $Group['files']) {
        $record = $Assessment[$file['path']]
        [ordered]@{ groupId = $record['groupId']; path = $record['path']; disposition = $record['disposition']; scope = $record['scope']; reason = $record['reason'] }
    })
    [string[]] $findingIds = @($Receipt['findingIds'])
    [Array]::Sort($findingIds, [StringComparer]::Ordinal)
    Get-ReviewTextHash -Text (ConvertTo-JsonText -Value ([ordered]@{
        assessments = $records
        artifactPath = $Receipt['artifactPath']
        findingIds = @($findingIds)
    }))
}

# Commits all assessments and completion together; failures leave the current claim replayable.
function Set-ReviewAssessmentBatch {
    param(
        [Parameter(Mandatory)][string] $QueuePath,
        [Parameter(Mandatory)][string] $InventoryPath,
        [Parameter(Mandatory)][string] $FileGroupsPath,
        [Parameter(Mandatory)][string] $ArtifactPath,
        [Parameter(Mandatory)][Collections.IDictionary] $Batch
    )
    Assert-WorkflowData -Kind assessmentBatch -Value $Batch
    $inputs = Read-ReviewWorkInputs -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath
    Assert-ReviewSnapshot -Expected $Batch['snapshot'] -Actual $inputs.snapshot
    $group = @($inputs.groups | Where-Object { $_['groupId'] -ceq $Batch['groupId'] })
    if ($group.Count -ne 1) { throw 'Batch must name an assigned group.' }
    $group = $group[0]
    $receipt = [ordered]@{
        groupId = $Batch['groupId']; sha256 = ''
        artifactPath = Resolve-PullRequestPath -Path $ArtifactPath; findingIds = $Batch['findingIds']
    }
    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($null -ne (Get-JsonProperty -Node $queue -Name failure)) { throw 'Cannot complete failed work before explicit resume.' }
        Assert-ReviewSnapshot -Expected (Get-JsonProperty -Node $queue -Name snapshot) -Actual $inputs.snapshot
        Assert-ReviewGroupJournalCoverage -Queue $queue -Group $inputs.groups
        $alreadyComplete = $queue['completedGroupIds'] -ccontains $Batch['groupId']
        if (-not $alreadyComplete -and (Get-JsonProperty -Node $queue['inProgress'] -Name groupId) -cne $Batch['groupId']) {
            throw 'Batch completion requires the current claimed group.'
        }
        $records = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
        foreach ($assessment in $queue['assessments']) {
            if ($assessment['groupId'] -ceq $Batch['groupId']) { $records.Add($assessment['path'], $assessment) }
        }
        $previousHash = if ($alreadyComplete) { Get-ReviewBatchHash -Group $group -Assessment $records -Receipt $receipt } else { $null }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($assessment in $Batch['assessments']) {
            if ($assessment['groupId'] -cne $Batch['groupId'] -or
                @($group['files'].path) -cnotcontains $assessment['path'] -or -not $seen.Add($assessment['path'])) {
                throw 'Batch assessments must name distinct files in the claimed group.'
            }
            if (-not $alreadyComplete -and $records.ContainsKey($assessment['path'])) {
                throw "File '$($assessment['path'])' already has a persisted assessment. Omit it from the initial batch; use explicit per-file repair to change it."
            }
            $records[$assessment['path']] = $assessment
        }
        foreach ($file in $group['files']) {
            if (-not $records.ContainsKey($file['path'])) { throw "Partial batch: no assessment for '$($file['path'])'." }
        }
        $receipt['sha256'] = Get-ReviewBatchHash -Group $group -Assessment $records -Receipt $receipt
        Assert-ReviewBatchFindings -ArtifactPath $ArtifactPath -FindingId $Batch['findingIds']
        if ($alreadyComplete) {
            $existing = @($queue['batchReceipts'] | Where-Object { $_['groupId'] -ceq $Batch['groupId'] })
            if ($existing.Count -ne 1 -or $existing[0]['sha256'] -cne $receipt['sha256'] -or $previousHash -cne $receipt['sha256']) {
                throw 'Conflicting batch retry. Use explicit per-file repair, not a completion replay.'
            }
        } else {
            $queue['assessments'] = @($queue['assessments'] | Where-Object { $_['groupId'] -cne $Batch['groupId'] }) +
                @($group['files'] | ForEach-Object { $records[$_['path']] })
            $queue['completedGroupIds'] = @($queue['completedGroupIds']) + $Batch['groupId']
            $queue['inProgress'] = $null
            $queue['batchReceipts'] = @($queue['batchReceipts']) + @($receipt)
            Assert-WorkflowData -Kind fileGroupJournal -Value $queue
            Write-PullRequestOutputFileAtomically -Path $QueuePath -Content (ConvertTo-JsonText -Value $queue)
        }
        [ordered]@{ groupId = $Batch['groupId']; alreadyComplete = $alreadyComplete; assessmentCount = $records.Count; remainingCount = $queue['items'].Count }
    } finally { $lock.Dispose() }
}

# The orchestrator records a stopped worker's failure without changing its findings or coverage.
# Explicit resume clears the failure only after validating the preserved journal and snapshots.
function Set-ReviewWorkerFailure {
    param([Parameter(Mandatory)][string] $QueuePath, [Parameter(Mandatory)][string] $Reason)
    if ([string]::IsNullOrWhiteSpace($Reason)) { throw 'Worker failure requires a nonblank reason.' }
    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($queue['kind'] -cne 'fileGroups') { throw 'Worker failure requires a private file-group journal.' }
        if ((Get-JsonProperty -Node $queue -Name failure) -cne $Reason) {
            $queue['failure'] = $Reason
            Assert-WorkflowData -Kind fileGroupJournal -Value $queue
            Write-PullRequestOutputFileAtomically -Path $QueuePath -Content (ConvertTo-JsonText -Value $queue)
        }
        [ordered]@{ status = 'Failed'; queuePath = Resolve-PullRequestPath -Path $QueuePath; reason = $Reason }
    } finally { $lock.Dispose() }
}

# Artifact creation precedes group claims. Missing state after that point is corruption, not a
# fresh start. Ref: docs/implementation.md, "Review work queues".
function Assert-ReviewWorkerArtifactState {
    param(
        [Parameter(Mandatory)]
        [string] $ArtifactPath,

        [Parameter(Mandatory)]
        [string] $QueuePath
    )

    $artifactExists = Test-Path -LiteralPath $ArtifactPath -PathType Leaf
    if (-not (Test-Path -LiteralPath $QueuePath -PathType Leaf)) {
        if ($artifactExists) {
            $artifact = Read-FindingArtifact -Path $ArtifactPath
            if ((Get-JsonArrayProperty -Node $artifact -Name 'findings').Count -gt 0) {
                throw "The group journal '$QueuePath' is missing despite recorded findings. Investigate without resetting review state."
            }
        }
        return
    }

    $lock = Open-ReviewQueueLock -Path $QueuePath
    try {
        $queue = Read-ReviewQueueDocument -Path $QueuePath
        if ($queue['kind'] -cne 'fileGroups') { throw "Expected a file-group journal at '$QueuePath'." }
        if (-not $artifactExists -and ($null -ne $queue['inProgress'] -or $queue['completedGroupIds'].Count -gt 0)) {
            throw "The finding artifact '$ArtifactPath' is missing despite journaled work. Investigate without replacing lost findings."
        }
    } finally {
        $lock.Dispose()
    }
}

function New-ReviewFileGroupQueue {
    param(
        [Parameter(Mandatory)]
        [string] $InventoryPath,

        [Parameter(Mandatory)]
        [string] $FileGroupsPath,

        [Parameter(Mandatory)]
        [string] $QueuePath,

        [switch] $Resume
    )

    $inputs = Read-ReviewWorkInputs -InventoryPath $InventoryPath -FileGroupsPath $FileGroupsPath
    $files = $inputs.files
    $groups = $inputs.groups
    $fullPath = Resolve-PullRequestPath -Path $QueuePath
    [IO.Directory]::CreateDirectory((Split-Path -Parent $fullPath)) | Out-Null
    $lock = Open-ReviewQueueLock -Path $fullPath
    try {
        if ([IO.File]::Exists($fullPath)) {
            if (-not $Resume) {
                throw "The file-group queue '$fullPath' already exists. Use -Resume to preserve its journal and remaining work."
            }
            $queue = Read-ReviewQueueDocument -Path $fullPath
            if ($null -ne (Get-JsonProperty -Node $queue -Name snapshot)) {
                Assert-ReviewSnapshot -Expected $queue['snapshot'] -Actual $inputs.snapshot
            }
            Assert-ReviewGroupJournalCoverage -Queue $queue -Group $groups
            if ($null -ne (Get-JsonProperty -Node $queue -Name failure)) {
                $queue['failure'] = $null
                Write-PullRequestOutputFileAtomically -Path $fullPath -Content (ConvertTo-JsonText -Value $queue)
            }
        } else {
            $queue = New-ReviewQueueDocument -Kind fileGroups -Item $groups
            $queue['snapshot'] = $inputs.snapshot
            Write-PullRequestOutputFileAtomically -Path $fullPath -Content (ConvertTo-JsonText -Value $queue)
        }
        [ordered]@{
            queuePath = $fullPath
            groupCount = $groups.Count
            fileCount = $files.Count
            completedGroupCount = $queue['completedGroupIds'].Count
            inProgressGroupId = Get-JsonProperty -Node $queue['inProgress'] -Name 'groupId'
            remainingCount = $queue['items'].Count
            snapshot = $inputs.snapshot
        }
    } finally {
        $lock.Dispose()
    }
}

function Assert-ReviewFileGroupQueueComplete {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [object[]] $Group = @()
    )

    $lock = Open-ReviewQueueLock -Path $Path
    try {
        $queue = Read-ReviewQueueDocument -Path $Path
        if ($null -ne (Get-JsonProperty -Node $queue -Name failure)) {
            throw "The reviewer's group journal '$Path' records failed work requiring explicit resume."
        }
        if ($queue['kind'] -cne 'fileGroups' -or $queue['items'].Count -ne 0 -or $null -ne $queue['inProgress']) {
            throw "The reviewer's group journal '$Path' must have no remaining items or in-progress group before completion."
        }
        if ($Group.Count -gt 0) { Assert-ReviewGroupJournalCoverage -Queue $queue -Group $Group }
        foreach ($receipt in (Get-JsonProperty -Node $queue -Name batchReceipts)) {
            Assert-ReviewBatchFindings -ArtifactPath $receipt['artifactPath'] -FindingId $receipt['findingIds']
        }
    } finally {
        $lock.Dispose()
    }
}

# Reads the assembler's complete section map, including cached copies, without interpreting prose.
# Unmarked guidance requires review; a damaged generated record is an error, never an exclusion.
function Read-ReviewApplicability {
    param([Parameter(Mandatory)][string] $Path, [string] $Text)
    if (-not $PSBoundParameters.ContainsKey('Text')) { $Text = Read-PullRequestFileText -Path $Path }
    $text = $Text.Replace("`r`n", "`n")
    if (-not $text.StartsWith('<!-- ss-review applicability')) { return $null }
    $match = [regex]::Match($text, '\A<!-- ss-review applicability ([A-Za-z0-9+/=]+) -->\n')
    if (-not $match.Success) { throw "Malformed applicability metadata in '$Path'." }
    $metadata = ConvertFrom-JsonVerbatim -Text ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($match.Groups[1].Value)))
    if ($metadata -isnot [Collections.IDictionary] -or $metadata.Count -ne 3 -or
        (Get-JsonProperty -Node $metadata -Name schemaVersion) -ne 1 -or
        (Get-JsonProperty -Node $metadata -Name contentSha256) -cne (Get-ReviewTextHash -Text $text.Substring($match.Length))) {
        throw "Stale applicability metadata in '$Path'. Regenerate guidance instead of guessing coverage."
    }
    $map = Get-JsonProperty -Node $metadata -Name map
    if (-not (Test-Json -Json (ConvertTo-JsonText -Value $map) -SchemaFile (Join-Path $PSScriptRoot '..\..\schemas\focus-area-map.schema.json'))) {
        throw "Invalid applicability map in '$Path'."
    }
    if ($map['categories'].Count -ne 1) { throw "Applicability metadata must describe one complete focus area in '$Path'." }
    $ids = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($chapter in $map['chapters']) {
        if (-not $ids.Add($chapter['id']) -or $chapter['categoryId'] -cne $map['categories'][0]['id']) {
            throw "Invalid applicability chapter in '$Path'."
        }
    }
    $map
}

# Supports a deliberately small glob language. IgnoreCase can only retain extra work and avoids
# assuming filesystem case rules. Unknown syntax is an error; preparation represents it as null.
function Test-ReviewPathScope {
    param([Parameter(Mandatory)][Collections.IDictionary] $Scope, [Parameter(Mandatory)][string] $RelativePath)
    $base = $Scope['base']
    foreach ($pattern in $Scope['include']) {
        $fullPattern = if ($base -ceq '.') { $pattern } else { "$base/$pattern" }
        $parts = [Collections.Generic.List[string]]::new()
        $segments = $fullPattern.Split('/')
        for ($index = 0; $index -lt $segments.Count; $index++) {
            $segment = $segments[$index]
            if ($segment -in @('', '.', '..') -or ($segment.Contains('**') -and $segment -cne '**')) {
                throw "Unsupported path-scope pattern '$fullPattern'."
            }
            if ($segment -ceq '**') {
                $parts.Add($(if ($index -eq $segments.Count - 1) { '.*' } else { '(?:[^/]+/)*' }))
            } else {
                $parts.Add([regex]::Escape($segment).Replace('\*', '[^/]*').Replace('\?', '[^/]'))
                if ($index -lt $segments.Count - 1) { $parts.Add('/') }
            }
        }
        if ([regex]::IsMatch($RelativePath, '\A' + ($parts -join '') + '\z', [Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [Text.RegularExpressions.RegexOptions]::CultureInvariant)) {
            return $true
        }
    }
    $false
}

# Expands a shared proof into ordinary per-file records, preserving deleted and rename-side entries.
function Get-ReviewScopeExclusions {
    param(
        [AllowNull()] $Map,
        [Parameter(Mandatory)][string] $RepositoryRoot,
        [Parameter(Mandatory)][object[]] $Group
    )
    if ($null -eq $Map) { return }
    $contexts = @($Map['chapters'] | ForEach-Object { $_['applicability'] })
    foreach ($context in $contexts) {
        if ($null -eq (Get-JsonProperty -Node $context -Name pathScope) -or
            $context['uncertainties'].Count -gt 0 -or $context['declarations'].Count -eq 0) { return }
    }
    $scopeText = ($contexts | ForEach-Object { $_['effectiveScope'] }) -join "`n"
    $rootPrefix = [IO.Path]::GetFullPath($RepositoryRoot).TrimEnd([char[]] @('\', '/')) + [IO.Path]::DirectorySeparatorChar
    foreach ($entry in $Group) {
        foreach ($file in $entry['files']) {
            $fullPath = [IO.Path]::GetFullPath($file['path'])
            # Ordinal containment only: uncertain root spelling or external files retain review,
            # regardless of the host's default path-case rules.
            if (-not $fullPath.StartsWith($rootPrefix, [StringComparison]::Ordinal)) { continue }
            $relative = $fullPath.Substring($rootPrefix.Length).Replace('\', '/')
            $included = $false
            foreach ($context in $contexts) {
                if (Test-ReviewPathScope -Scope $context['pathScope'] -RelativePath $relative) { $included = $true; break }
            }
            if (-not $included) {
                [ordered]@{
                    groupId = $entry['groupId']; path = $file['path']; disposition = 'excluded'
                    scope = $scopeText
                    reason = "Repository-relative path '$relative' matches no path alternative in any section of the validated focus-area metadata."
                }
            }
        }
    }
}

function New-ReviewWork {
    param(
        [Parameter(Mandatory)]
        [string] $InventoryPath,

        [Parameter(Mandatory)]
        [string] $FileGroupsPath,

        [Parameter(Mandatory)]
        [string] $FocusAreaDirectory,

        [Parameter(Mandatory)]
        [string] $OutputDirectory,

        [string] $RepositoryRoot,
        [string] $ArtifactDirectory
    )

    $transactionLock = Open-PullRequestFileLock -Path $FocusAreaDirectory
    try {
        $files = Read-ReviewFileInventory -Path $InventoryPath
        $groups = Read-ReviewFileGroups -Path $FileGroupsPath -FileInventory $files
        $focusAreas = @(Get-ChildItem -LiteralPath $FocusAreaDirectory -Filter '*.md' -File -Force | Sort-Object -Property Name -CaseSensitive)
        if ($focusAreas.Count -eq 0) {
            throw "No focus areas were found in '$FocusAreaDirectory'."
        }
        foreach ($area in $focusAreas) {
            if ([string]::IsNullOrWhiteSpace((Read-PullRequestFileText -Path $area.FullName))) {
                throw "Focus area '$($area.FullName)' is empty."
            }
        }
        $scopeMaps = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
        $scopeTexts = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
        if ($RepositoryRoot -or $ArtifactDirectory) {
            if (-not $RepositoryRoot -or -not $ArtifactDirectory) { throw 'Applicability preparation requires both RepositoryRoot and ArtifactDirectory.' }
            $repository = Get-Item -LiteralPath $RepositoryRoot
            if (-not $repository.PSIsContainer) { throw 'RepositoryRoot must name a directory.' }
            $RepositoryRoot = $repository.FullName
            foreach ($area in $focusAreas) {
                $scopeTexts.Add($area.FullName, (Read-PullRequestFileText -Path $area.FullName))
                $scopeMaps.Add($area.FullName, (Read-ReviewApplicability -Path $area.FullName -Text $scopeTexts[$area.FullName]))
            }
        }

        $root = Resolve-PullRequestPath -Path $OutputDirectory
        # Refuse reuse rather than silently resetting consumed queues or partially initialized state.
        New-Item -ItemType Directory -Path $root -ErrorAction Stop | Out-Null
        $inventorySnapshot = Join-Path $root 'file-inventory.json'
        $groupsSnapshot = Join-Path $root 'file-inventory-grouped.json'
        $queuePath = Join-Path $root 'focus-areas.json'
        $manifestPath = Join-Path $root 'manifest.json'
        $reviewers = @(
            for ($index = 0; $index -lt $focusAreas.Count; $index++) {
                # Ordinals avoid filename restrictions and collisions in arbitrary focus-area titles.
                $reviewerId = 'focus-{0:D3}' -f ($index + 1)
                [ordered]@{
                    reviewerId = $reviewerId
                    focusArea = $focusAreas[$index].FullName
                    fileGroupQueuePath = Join-Path $root "file-groups\$reviewerId.json"
                }
            }
        )

        Write-PullRequestOutputFileAtomically -Path $inventorySnapshot -Content (ConvertTo-JsonText -Value $files)
        Write-PullRequestOutputFileAtomically -Path $groupsSnapshot -Content (ConvertTo-JsonText -Value $groups)
        $excludedReviewerCount = 0
        if ($RepositoryRoot) {
            $inputs = Read-ReviewWorkInputs -InventoryPath $inventorySnapshot -FileGroupsPath $groupsSnapshot
            foreach ($reviewer in $reviewers) {
                $excluded = @(Get-ReviewScopeExclusions -Map $scopeMaps[$reviewer['focusArea']] -RepositoryRoot $RepositoryRoot -Group $groups)
                $reviewer['dispatch'] = if ($excluded.Count -eq $files.Count) { 'excluded' } else { 'review' }
                $reviewer['focusAreaSha256'] = Get-ReviewTextHash -Text $scopeTexts[$reviewer['focusArea']]
                $reviewer['artifactPath'] = Join-Path (Resolve-PullRequestPath -Path $ArtifactDirectory) "$($reviewer['reviewerId']).json"
                if (Test-Path -LiteralPath $reviewer['artifactPath']) { throw "Finding artifact already exists: '$($reviewer['artifactPath'])'." }
                $artifact = New-FindingArtifactDocument -Kind reviewer -ReviewerId $reviewer['reviewerId'] -FocusArea $reviewer['focusArea']
                Write-FindingArtifact -Path $reviewer['artifactPath'] -Document $artifact
                $journal = New-ReviewQueueDocument -Kind fileGroups -Item $groups
                $journal['snapshot'] = $inputs.snapshot
                $journal['assessments'] = $excluded
                $excludedPaths = [Collections.Generic.HashSet[string]]::new([string[]] @($excluded | ForEach-Object { $_['path'] }), [StringComparer]::Ordinal)
                foreach ($group in $groups) {
                    if (@($group['files'] | Where-Object { -not $excludedPaths.Contains($_['path']) }).Count -eq 0) {
                        $journal['completedGroupIds'] += $group['groupId']
                    }
                }
                $journal['items'] = @($groups | Where-Object { $journal['completedGroupIds'] -cnotcontains $_['groupId'] })
                Assert-WorkflowData -Kind fileGroupJournal -Value $journal
                Assert-ReviewGroupJournalCoverage -Queue $journal -Group $groups
                Write-PullRequestOutputFileAtomically -Path $reviewer['fileGroupQueuePath'] -Content (ConvertTo-JsonText -Value $journal)
                if ($reviewer['dispatch'] -ceq 'excluded') {
                    Complete-FindingArtifactDocument -Document $artifact
                    Write-FindingArtifact -Path $reviewer['artifactPath'] -Document $artifact
                    $excludedReviewerCount++
                }
            }
        }
        $dispatch = @($reviewers | Where-Object { (Get-JsonProperty -Node $_ -Name dispatch) -cne 'excluded' })
        $queue = New-ReviewQueueDocument -Kind focusAreas -Item $dispatch
        Write-PullRequestOutputFileAtomically -Path $queuePath -Content (ConvertTo-JsonText -Value $queue)
        $manifest = [ordered]@{
            schemaVersion = 1
            inventoryPath = $inventorySnapshot
            fileGroupsPath = $groupsSnapshot
            focusAreaQueuePath = $queuePath
            reviewers = $reviewers
        }
        if ($RepositoryRoot) { $manifest['repositoryRoot'] = $RepositoryRoot; $manifest['snapshot'] = $inputs.snapshot }
        Assert-WorkflowData -Kind reviewWork -Value $manifest
        # Publish the coverage authority last; no workers may start without a complete manifest.
        Write-PullRequestOutputFileAtomically -Path $manifestPath -Content (ConvertTo-JsonText -Value $manifest)
        [ordered]@{
            manifestPath = $manifestPath
            inventoryPath = $inventorySnapshot
            fileGroupsPath = $groupsSnapshot
            focusAreaQueuePath = $queuePath
            reviewerCount = $reviewers.Count
            dispatchCount = $dispatch.Count
            excludedReviewerCount = $excludedReviewerCount
            fileCount = $files.Count
            groupCount = $groups.Count
        }
    } finally { $transactionLock.Dispose() }
}

function Read-ReviewWorkManifest {
    param(
        [Parameter(Mandatory)]
        [string] $Path
    )

    $manifest = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $Path)
    $reviewers = Get-JsonProperty -Node $manifest -Name 'reviewers'
    if ((Get-JsonProperty -Node $manifest -Name 'schemaVersion') -ne 1 -or
        $reviewers -isnot [object[]] -or $reviewers.Count -eq 0) {
        throw "Invalid review-work manifest '$Path': expected a versioned, nonempty reviewers array."
    }
    Assert-WorkflowData -Kind reviewWork -Value $manifest
    $ids = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $areas = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $queues = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($reviewer in $reviewers) {
        $id = Get-JsonProperty -Node $reviewer -Name 'reviewerId'
        if ($id -isnot [string] -or $id -cnotmatch '^[A-Za-z0-9_-]+$' -or -not $ids.Add($id)) {
            throw "Invalid review-work manifest '$Path': each reviewer requires a unique identifier."
        }
        foreach ($name in @('focusArea', 'fileGroupQueuePath')) {
            $value = Get-JsonProperty -Node $reviewer -Name $name
            if ($value -isnot [string] -or -not [IO.Path]::IsPathFullyQualified($value)) {
                throw "Invalid review-work manifest '$Path': reviewer '$id' requires an absolute '$name'."
            }
        }
        if (-not $areas.Add($reviewer['focusArea']) -or -not $queues.Add($reviewer['fileGroupQueuePath'])) {
            throw "Invalid review-work manifest '$Path': focus areas and group queues must belong to distinct reviewers."
        }
        if ($reviewer.Contains('dispatch')) {
            if (@('review', 'excluded') -cnotcontains $reviewer['dispatch'] -or
                (Get-JsonProperty -Node $reviewer -Name focusAreaSha256) -cnotmatch '^[0-9A-F]{64}$' -or
                -not [IO.Path]::IsPathFullyQualified((Get-JsonProperty -Node $reviewer -Name artifactPath))) {
                throw "Invalid review-work manifest '$Path': incomplete applicability assignment."
            }
        }
        if ($manifest.Contains('snapshot') -ne $reviewer.Contains('dispatch')) {
            throw "Invalid review-work manifest '$Path': applicability assignments require snapshot-bound repository state."
        }
    }
    foreach ($name in @('inventoryPath', 'fileGroupsPath', 'focusAreaQueuePath')) {
        $value = Get-JsonProperty -Node $manifest -Name $name
        if ($value -isnot [string] -or -not [IO.Path]::IsPathFullyQualified($value)) {
            throw "Invalid review-work manifest '$Path': expected an absolute '$name'."
        }
    }
    $manifest
}

function Get-ReviewWorkValidation {
    param(
        [Parameter(Mandatory)]
        [string] $ManifestPath,

        [Parameter(Mandatory)]
        [string] $ArtifactDirectory
    )

    $manifest = Read-ReviewWorkManifest -Path $ManifestPath
    $inputs = Read-ReviewWorkInputs -InventoryPath $manifest['inventoryPath'] -FileGroupsPath $manifest['fileGroupsPath']
    $groups = $inputs.groups
    if ($manifest.Contains('snapshot')) { Assert-ReviewSnapshot -Expected $manifest['snapshot'] -Actual $inputs.snapshot }
    $reviewers = $manifest['reviewers']
    $result = Get-FindingArtifactValidation -Path $ArtifactDirectory -ExpectedReviewerId @($reviewers.reviewerId)
    $errors = [Collections.Generic.List[string]]::new()
    $failedReviewers = [Collections.Generic.List[string]]::new()
    foreach ($errorText in $result['errors']) { $errors.Add($errorText) }

    $lock = Open-ReviewQueueLock -Path $manifest['focusAreaQueuePath']
    try {
        $queue = Read-ReviewQueueDocument -Path $manifest['focusAreaQueuePath']
        if ($queue['kind'] -cne 'focusAreas' -or $queue['items'].Count -ne 0) {
            $errors.Add('The focus-area dispatch queue has not been exhausted.')
        }
    } finally {
        $lock.Dispose()
    }

    foreach ($reviewer in $reviewers) {
        if ([IO.File]::Exists($reviewer['fileGroupQueuePath'])) {
            try {
                $lock = Open-ReviewQueueLock -Path $reviewer['fileGroupQueuePath']
                try {
                    $journal = Read-ReviewQueueDocument -Path $reviewer['fileGroupQueuePath']
                    $failure = Get-JsonProperty -Node $journal -Name failure
                    if ($null -ne $failure) {
                        $failedReviewers.Add($reviewer['reviewerId'])
                        $errors.Add("Reviewer '$($reviewer['reviewerId'])' failed: $failure")
                    }
                } finally { $lock.Dispose() }
            } catch { $errors.Add($_.Exception.Message) }
        }
        $scopeText = $null
        if ($reviewer.Contains('focusAreaSha256')) {
            $scopeText = Read-PullRequestFileText -Path $reviewer['focusArea']
            if ($reviewer['focusAreaSha256'] -cne (Get-ReviewTextHash -Text $scopeText)) {
                $errors.Add("Focus-area snapshot changed for '$($reviewer['reviewerId'])'.")
                continue
            }
        }
        foreach ($artifact in $result['artifacts']) {
            if ($artifact['reviewerId'] -cne $reviewer['reviewerId']) { continue }
            if ($artifact['focusArea'] -cne $reviewer['focusArea']) {
                $errors.Add("Reviewer '$($reviewer['reviewerId'])' recorded a different focus area from its manifest assignment.")
            }
            if ($reviewer.Contains('artifactPath') -and $reviewer['artifactPath'] -cne $artifact['path']) {
                $errors.Add("Reviewer '$($reviewer['reviewerId'])' changed its assigned artifact path.")
            }
            if ($artifact['status'] -cne 'complete') { continue }
            try {
                Assert-ReviewFileGroupQueueComplete -Path $reviewer['fileGroupQueuePath'] -Group $groups
                $journal = Read-ReviewQueueDocument -Path $reviewer['fileGroupQueuePath']
                if ($manifest.Contains('snapshot') -or $null -ne (Get-JsonProperty -Node $journal -Name snapshot)) {
                    Assert-ReviewSnapshot -Expected (Get-JsonProperty -Node $journal -Name snapshot) -Actual $inputs.snapshot
                }
                foreach ($receipt in (Get-JsonProperty -Node $journal -Name batchReceipts)) {
                    if ($receipt['artifactPath'] -cne $artifact['path']) { throw 'Batch receipt names another reviewer artifact.' }
                }
                if ((Get-JsonProperty -Node $reviewer -Name dispatch) -ceq 'excluded') {
                    $exclusions = @(Get-ReviewScopeExclusions -Map (Read-ReviewApplicability -Path $reviewer['focusArea'] -Text $scopeText) -RepositoryRoot $manifest['repositoryRoot'] -Group $groups)
                    if ($exclusions.Count -ne $inputs.files.Count -or $artifact['findingCount'] -ne 0 -or
                        @($journal['assessments'] | Where-Object { $_['disposition'] -cne 'excluded' }).Count -gt 0) {
                        throw "Invalid completed-empty exclusion result for '$($reviewer['reviewerId'])'."
                    }
                    foreach ($expected in $exclusions) {
                        $actual = @($journal['assessments'] | Where-Object { $_['path'] -ceq $expected['path'] })[0]
                        if ($actual['scope'] -cne $expected['scope'] -or $actual['reason'] -cne $expected['reason']) {
                            throw 'Completed-empty exclusion evidence differs from the validated scope metadata.'
                        }
                    }
                }
            } catch {
                $errors.Add($_.Exception.Message)
            }
        }
    }

    $result['errors'] = $errors.ToArray()
    $result['failedReviewers'] = $failedReviewers.ToArray()
    $result['isValid'] = $result['isValid'] -and $errors.Count -eq 0
    $result
}
