#Requires -Version 7.6

# Shared worker input preparation and measurements. Callers load WorkflowData.ps1.
# Ref: docs/implementation.md, "Worker input boundary".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Counts actual command attempts, including failures, without storing reviewed evidence.
function Add-ReviewInputDiagnostic {
    param(
        [Parameter(Mandatory)][string] $Path, [Parameter(Mandatory)][string] $Name,
        [ValidateRange(1, [int]::MaxValue)][int] $Count = 1
    )
    $lock = Open-WorkflowDataLock -Path $Path
    try {
        $data = if ([IO.File]::Exists($Path)) { Read-WorkflowData -Kind inputDiagnostics -Path $Path } else {
            [ordered]@{
                schemaVersion = 1
                counts = [ordered]@{
                    setupCalls = 0; repeatedSetupCalls = 0; documentReadCalls = 0
                    firstReadFailures = 0; malformedQueryFailures = 0; cursorRestarts = 0
                }
            }
        }
        if (-not $data['counts'].Contains($Name)) { throw "Unknown input diagnostic '$Name'." }
        $data['counts'][$Name] += $Count
        Assert-WorkflowData -Kind inputDiagnostics -Value $data
        Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $data)
    } finally { $lock.Dispose() }
}

# Hash the bytes used by the reader, not timestamps or filesystem-specific identity.
function Get-ReviewInputHash {
    param([Parameter(Mandatory)][byte[]] $Bytes)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes))
}

# Registers only explicitly selected shared documents, never a PR-context capture.
function New-ReviewSharedDocument {
    param([Parameter(Mandatory)][string] $Id, [Parameter(Mandatory)][string] $Path)
    $fullPath = Resolve-PullRequestPath -Path $Path
    if ([IO.Path]::GetExtension($fullPath) -cne '.md') { throw 'Shared reading accepts Markdown guidance only, never canonical captures.' }
    $bytes = Read-PullRequestFileBytes -Path $fullPath
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    if ([string]::IsNullOrWhiteSpace($text)) { throw "Shared document '$fullPath' is empty." }
    [ordered]@{
        id = $Id; path = $fullPath; sha256 = Get-ReviewInputHash -Bytes $bytes
        sizeBytes = $bytes.Length; lineCount = [regex]::Matches($text, '\r\n|\r|\n').Count + 1
    }
}

# Selects the canonical contract for the phase's structured drafts, not unrelated workflow records.
# Ref: docs/implementation.md, "Worker input boundary".
function Get-ReviewWorkerDataGuidePath {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('references', 'fileGroups', 'focusAreas', 'overview', 'review')]
        [string] $Phase
    )
    switch ($Phase) {
        'fileGroups' { 'data-guides\file-groups.md' }
        'focusAreas' { 'data-guides\focus-areas.md' }
        'review' { 'data-guides\review-coverage.md' }
        # Reference lists and overviews are prose, with no structured draft contract to load.
        default { $null }
    }
}

# Required roles prevent a partial manifest from being mistaken for a complete assignment.
function Assert-ReviewWorkerAssignment {
    param([Parameter(Mandatory)] $Assignment)
    Assert-WorkflowData -Kind workerAssignment -Value $Assignment
    foreach ($path in @($Assignment['diagnosticsPath'], $Assignment['contextPath'])) {
        if ($null -ne $path -and -not [IO.Path]::IsPathFullyQualified($path)) { throw 'Assignment paths must be absolute.' }
    }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($document in $Assignment['documents']) {
        if (-not [IO.Path]::IsPathFullyQualified($document['path']) -or
            [IO.Path]::GetExtension($document['path']) -cne '.md') { throw 'Expected an absolute shared Markdown path, never a capture.' }
        if (-not $seen.Add($document['id'])) { throw 'Shared document IDs must be unique.' }
    }
    $required = @('prompt', 'scope')
    if ($null -ne (Get-ReviewWorkerDataGuidePath -Phase $Assignment['phase'])) { $required += 'workflowData' }
    if ($Assignment['phase'] -in @('review', 'overview')) { $required += 'references' }
    if ($Assignment['phase'] -eq 'review') { $required += @('overview', 'focusArea') }
    elseif ($Assignment['resume']) { throw 'Only a reviewer assignment may resume.' }
    if ($null -ne $Assignment['contextPath']) { $required += 'queryRecipe' }
    if ($seen.Count -ne $required.Count -or @($required | Where-Object { -not $seen.Contains($_) }).Count) {
        throw 'Shared documents do not match the required phase inputs.'
    }
}

# Returns contiguous exact text slices, including very long lines, below the tool output ceiling.
function Read-ReviewSharedDocument {
    param(
        [Parameter(Mandatory)] $Assignment, [Parameter(Mandatory)][string] $DocumentId,
        [ValidateRange(0, [int]::MaxValue)][int] $Offset = 0
    )
    Assert-ReviewWorkerAssignment -Assignment $Assignment
    $documents = @($Assignment['documents'] | Where-Object { $_['id'] -ceq $DocumentId })
    if ($documents.Count -ne 1) { throw "Expected one shared document '$DocumentId'." }
    $document = $documents[0]
    if (-not [IO.Path]::IsPathFullyQualified($document['path']) -or
        [IO.Path]::GetExtension($document['path']) -cne '.md') { throw 'Expected an absolute shared Markdown path, never a capture.' }
    $bytes = Read-PullRequestFileBytes -Path $document['path']
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
    if ((Get-ReviewInputHash -Bytes $bytes) -cne $document['sha256'] -or $bytes.Length -ne $document['sizeBytes'] -or
        [regex]::Matches($text, '\r\n|\r|\n').Count + 1 -ne $document['lineCount']) {
        throw "Shared document '$DocumentId' changed. Rebuild the assignment explicitly."
    }
    if ($Offset -ge $text.Length -or [char]::IsLowSurrogate($text[$Offset])) { throw 'Invalid shared-document offset.' }
    # Use the PR reader's output ceiling while filling it by encoded size, not by line length.
    # This stays below ordinary viewer limits without multiplying calls for mostly ASCII guidance.
    $maxBytes = 16KB
    $low = 1
    $high = [Math]::Min($maxBytes, $text.Length - $Offset)
    $best = $null
    while ($low -le $high) {
        $candidate = $low + [int] [Math]::Floor(($high - $low) / 2)
        $length = $candidate
        if ([char]::IsHighSurrogate($text[$Offset + $length - 1])) { $length-- }
        $end = $Offset + $length
        $page = [ordered]@{
            documentId = $DocumentId; sha256 = $document['sha256']; contentOffset = $Offset
            content = $text.Substring($Offset, $length); contentLength = $text.Length
            nextOffset = if ($end -lt $text.Length) { $end } else { $null }
            complete = $end -eq $text.Length
        }
        $json = ConvertTo-Json -InputObject $page -Depth 100 -Compress
        if ([Text.Encoding]::UTF8.GetByteCount($json + [Environment]::NewLine) -le $maxBytes) {
            if ($length -gt 0) { $best = $page }
            $low = $candidate + 1
        } else { $high = $candidate - 1 }
    }
    if ($null -eq $best) { throw 'Shared document cannot make progress within the output budget.' }
    Assert-WorkflowData -Kind sharedReadPage -Value $best
    $best
}
