#Requires -Version 7.6

# Shared strict schemas and atomic mutations for agent-authored workflow records.
# Callers load JsonValue.ps1 and PullRequestModel.ps1. Ref: docs/implementation.md,
# "Machine-readable handoffs".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$script:WorkflowSchemaPath = Join-Path $PSScriptRoot '..\..\schemas\workflow-data.schema.json'

# Selects a named schema without relying on external reference fetching.
function Assert-WorkflowData {
    param([Parameter(Mandatory)][string] $Kind, [Parameter(Mandatory)][AllowNull()] $Value)
    $schema = ConvertFrom-JsonVerbatim -Text ((Read-PullRequestFileText -Path ($script:WorkflowSchemaPath)))
    if (-not $schema['$defs'].Contains($Kind)) { throw "Unknown workflow schema '$Kind'." }
    $schema['$ref'] = "#/`$defs/$Kind"
    if (-not (Test-Json -Json (ConvertTo-JsonText -Value $Value) -Schema (ConvertTo-JsonText -Value $schema))) {
        throw "Invalid '$Kind' workflow data."
    }
}

# All consumer reads reject missing documents and alternate field shapes.
function Read-WorkflowData {
    param([Parameter(Mandatory)][string] $Kind, [Parameter(Mandatory)][string] $Path)
    $value = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $Path)
    Assert-WorkflowData -Kind $Kind -Value $value
    if ($value -is [object[]]) { return , $value }
    $value
}

# Uses a retained sibling lock, like review queues, so atomic replacement cannot split ownership.
function Open-WorkflowDataLock {
    param([Parameter(Mandatory)][string] $Path)
    Open-PullRequestFileLock -Path $Path
}

# Maps the small plan envelopes to their only list; prose item content remains agent-authored.
function Get-WorkflowListName {
    param([Parameter(Mandatory)][string] $Kind)
    switch -CaseSensitive ($Kind) {
        'mergeCandidates' { 'clusters' }
        'mergePlan' { 'groups' }
        'updates' { 'updates' }
        'removals' { 'removals' }
        'decisions' { 'decisions' }
        'auditFeedback' { 'feedback' }
        default { throw "Schema '$Kind' is not an appendable plan." }
    }
}

# Explicit initialization makes an empty result intentional and never overwrites prior work.
function New-WorkflowList {
    param([Parameter(Mandatory)][string] $Kind, [Parameter(Mandatory)][string] $Path)
    $field = Get-WorkflowListName -Kind $Kind
    $lock = Open-WorkflowDataLock -Path $Path
    try {
        if (Test-Path -LiteralPath $Path) { throw "Workflow data already exists: '$Path'." }
        $document = [ordered]@{ schemaVersion = 1; $field = @() }
        Assert-WorkflowData -Kind $Kind -Value $document
        Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $document)
    } finally { $lock.Dispose() }
}

# Appends one typed record. Exact retries are idempotent; conflicting repeated identifiers fail.
function Add-WorkflowListItem {
    param(
        [Parameter(Mandatory)][string] $Kind,
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)] $Item,
        [ValidateRange(-1, [int]::MaxValue)][int] $ReplaceIndex = -1,
        [switch] $Initialize
    )
    $field = Get-WorkflowListName -Kind $Kind
    $lock = Open-WorkflowDataLock -Path $Path
    try {
        $document = if ($Initialize -and -not [IO.File]::Exists((Resolve-PullRequestPath -Path $Path))) {
            [ordered]@{ schemaVersion = 1; $field = @() }
        } else { Read-WorkflowData -Kind $Kind -Path $Path }
        $candidate = [ordered]@{ schemaVersion = 1; $field = @($Item) }
        Assert-WorkflowData -Kind $Kind -Value $candidate
        if ($ReplaceIndex -ge 0) {
            if ($Kind -ceq 'auditFeedback') { throw 'Audit feedback is append-only.' }
            if ($ReplaceIndex -ge $document[$field].Count) { throw 'Replacement index is outside the plan.' }
        }
        $itemJson = ConvertTo-JsonText -Value $Item
        for ($index = 0; $index -lt $document[$field].Count; $index++) {
            $existing = $document[$field][$index]
            if ($index -eq $ReplaceIndex) { continue }
            if ($ReplaceIndex -lt 0 -and (ConvertTo-JsonText -Value $existing) -ceq $itemJson) { return }
            if ($Kind -ceq 'auditFeedback') {
                if ($existing['responseId'] -ceq $Item['responseId'] -and $existing['findingId'] -ceq $Item['findingId']) {
                    throw 'A feedback response is immutable; use a new responseId for a new user response.'
                }
            } elseif ($Kind -cin @('updates', 'removals', 'decisions')) {
                if ($existing['findingId'] -ceq $Item['findingId']) { throw "Repeated finding '$($Item['findingId'])' in $Kind." }
            } elseif ($Kind -ceq 'mergePlan') {
                foreach ($id in $Item['sourceFindingIds']) {
                    if ($existing['sourceFindingIds'] -ccontains $id) { throw "Finding '$id' appears in multiple merge groups." }
                }
            }
        }
        if ($ReplaceIndex -ge 0) { $document[$field][$ReplaceIndex] = $Item }
        else { $document[$field] = @($document[$field]) + @($Item) }
        Assert-WorkflowData -Kind $Kind -Value $document
        Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $document)
    } finally { $lock.Dispose() }
}
