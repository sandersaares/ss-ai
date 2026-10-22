#Requires -Version 7.6

# Owns the reference graph and derives inheritance and expansion completeness mechanically.
# Callers load WorkflowData.ps1 and FocusAreaPreparation.ps1 plus their dependencies.
# Ref: docs/implementation.md, "Machine-readable handoffs".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Resolves every chain from a direct referrer. Path-local cycle detection makes cyclic input fail
# deterministically, while shared descendants still retain all distinct applicability alternatives.
function Get-FocusAreaReferenceContexts {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Document,
        [Parameter(Mandatory)][string] $ReferenceId,
        [string[]] $Ancestors = @()
    )
    if ($Ancestors -ccontains $ReferenceId) { throw "Reference cycle reaches '$ReferenceId'." }
    $matches = @($Document['references'] | Where-Object { $_['id'] -ceq $ReferenceId })
    if ($matches.Count -ne 1) { throw "Unknown or repeated reference '$ReferenceId'." }
    $reference = $matches[0]
    foreach ($referrer in $reference['referrers']) {
        $local = [ordered]@{
            referenceId = $ReferenceId; uri = $reference['uri']; source = $referrer['source']
            scope = $referrer['scope']; declarations = $referrer['declarations']
        }
        if ($null -eq $referrer['parentId']) {
            [ordered]@{ chain = @($local) }
        } else {
            $parents = @(Get-FocusAreaReferenceContexts -Document $Document -ReferenceId $referrer['parentId'] -Ancestors (@($Ancestors) + $ReferenceId))
            foreach ($parent in $parents) { [ordered]@{ chain = @($parent['chain']) + @($local) } }
        }
    }
}

# Rejects broken source references, shared output writers and inconsistent terminal states.
function Assert-FocusAreaExpansion {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Document,
        [Parameter(Mandatory)][string] $SessionStateDirectory,
        [switch] $RequireFinal
    )
    Assert-WorkflowData -Kind expansion -Value $Document
    $files = Get-FocusAreaSourceIndex -SessionStateDirectory $SessionStateDirectory
    $ids = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $uris = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $outputs = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($reference in $Document['references']) {
        if (-not $ids.Add($reference['id']) -or -not $uris.Add($reference['uri'])) {
            throw 'Reference IDs and canonical URIs must be unique; add referrers to the existing target.'
        }
        foreach ($referrer in $reference['referrers']) {
            $null = Get-FocusAreaSpan -Span $referrer['source'] -Files $files
            foreach ($span in $referrer['declarations']) { $null = Get-FocusAreaSpan -Span $span -Files $files }
            if ($null -ne $referrer['parentId']) {
                $parent = @($Document['references'] | Where-Object { $_['id'] -ceq $referrer['parentId'] })
                if ($parent.Count -ne 1 -or $parent[0]['state'] -cne 'Complete') {
                    throw 'A nested reference requires a successfully expanded parent.'
                }
                $parentOutputs = @($parent[0]['outputs'] | ForEach-Object { $_.Replace('/', '\') })
                if ($referrer['source']['root'] -cne 'instruction-set-raw' -or
                    $parentOutputs -cnotcontains $referrer['source']['path'].Replace('/', '\')) {
                    throw 'A nested referrer source must belong to its parent reference outputs.'
                }
            }
        }
        foreach ($path in $reference['outputs']) {
            $span = [ordered]@{ root = 'instruction-set-raw'; path = $path; startLine = 1; endLine = 1 }
            # Validate paths through the same source index, including files whose opening line is blank.
            $key = "instruction-set-raw\$($path.Replace('/', '\'))"
            if (-not $files.ContainsKey($key)) { throw "Expanded output does not exist: '$path'." }
            $lines = [IO.File]::ReadAllLines($files[$key].Path)
            $span['endLine'] = $lines.Count
            if ($lines.Count -eq 0) { throw "Expanded output is empty: '$path'." }
            $null = Get-FocusAreaSpan -Span $span -Files $files
            if (-not $outputs.Add($key)) { throw "Multiple references claim output '$path'." }
        }
        $null = @(Get-FocusAreaReferenceContexts -Document $Document -ReferenceId $reference['id'])
    }
    $pending = @($Document['references'] | Where-Object { $_['state'] -ceq 'Pending' }).Count
    $failed = @($Document['references'] | Where-Object { $_['state'] -ceq 'Failed' }).Count
    $expected = if ($failed) { 'Degraded' } else { 'Complete' }
    if ($Document['state'] -cne 'Unknown' -and ($pending -gt 0 -or $Document['state'] -cne $expected)) {
        throw 'Expansion state disagrees with the reference outcomes.'
    }
    if ($RequireFinal -and ($pending -gt 0 -or $Document['state'] -ceq 'Unknown')) {
        throw 'Expansion must be finalized with no pending references before categorization or caching.'
    }
}

# One writer owns the manifest. Content fetching remains agent work; no agent mutates the graph.
function Update-FocusAreaExpansion {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $SessionStateDirectory,
        [Parameter(Mandatory)][ValidateSet('Initialize', 'Register', 'Complete', 'Fail', 'Finalize', 'Read')][string] $Operation,
        [Collections.IDictionary] $InputRecord,
        [string] $ReferenceId,
        [string[]] $OutputPath = @(),
        [string] $Failure
    )
    $lock = Open-WorkflowDataLock -Path $Path
    try {
        if ($Operation -ceq 'Initialize') {
            if (Test-Path -LiteralPath $Path) { throw "Expansion manifest already exists: '$Path'." }
            $document = [ordered]@{ schemaVersion = 1; state = 'Unknown'; references = @() }
        } else {
            $document = Read-WorkflowData -Kind expansion -Path $Path
            Assert-FocusAreaExpansion -Document $document -SessionStateDirectory $SessionStateDirectory
            if ($Operation -notin @('Read', 'Finalize') -and $document['state'] -cne 'Unknown') {
                throw 'Finalized expansion inputs are immutable; start a new preparation pass.'
            }
            switch ($Operation) {
                'Register' {
                    Assert-WorkflowData -Kind referenceInput -Value $InputRecord
                    $existing = @($document['references'] | Where-Object { $_['id'] -ceq $InputRecord['id'] })
                    if ($existing.Count) {
                        if ($existing[0]['uri'] -cne $InputRecord['uri']) { throw 'A reference ID cannot change its URI.' }
                        foreach ($referrer in $InputRecord['referrers']) {
                            $serialized = ConvertTo-JsonText -Value $referrer
                            if (@($existing[0]['referrers'] | Where-Object { (ConvertTo-JsonText -Value $_) -ceq $serialized }).Count -eq 0) {
                                $existing[0]['referrers'] = @($existing[0]['referrers']) + @($referrer)
                            }
                        }
                    } else {
                        $document['references'] = @($document['references']) + @([ordered]@{
                            id = $InputRecord['id']; uri = $InputRecord['uri']; referrers = $InputRecord['referrers']
                            state = 'Pending'; outputs = @(); error = $null
                        })
                    }
                }
                { $_ -in @('Complete', 'Fail') } {
                    $matches = @($document['references'] | Where-Object { $_['id'] -ceq $ReferenceId })
                    if ($matches.Count -ne 1) { throw "Unknown reference '$ReferenceId'." }
                    $reference = $matches[0]
                    if ($Operation -ceq 'Complete') {
                        $reference['state'] = 'Complete'; $reference['outputs'] = @($OutputPath); $reference['error'] = $null
                    } else {
                        $reference['state'] = 'Failed'; $reference['outputs'] = @(); $reference['error'] = $Failure
                    }
                }
                'Finalize' {
                    if (@($document['references'] | Where-Object { $_['state'] -ceq 'Pending' }).Count) {
                        throw 'Cannot finalize expansion with pending references.'
                    }
                    $document['state'] = if (@($document['references'] | Where-Object { $_['state'] -ceq 'Failed' }).Count) { 'Degraded' } else { 'Complete' }
                }
            }
        }
        Assert-FocusAreaExpansion -Document $document -SessionStateDirectory $SessionStateDirectory
        if ($Operation -cne 'Read') {
            Write-PullRequestOutputFileAtomically -Path $Path -Content (ConvertTo-JsonText -Value $document)
        }
        $contexts = @(if ($ReferenceId) { Get-FocusAreaReferenceContexts -Document $document -ReferenceId $ReferenceId })
        [ordered]@{
            state = $document['state']
            referenceCount = $document['references'].Count
            pendingReferenceIds = @($document['references'] | Where-Object { $_['state'] -ceq 'Pending' } | ForEach-Object { $_['id'] })
            failedReferences = @($document['references'] | Where-Object { $_['state'] -ceq 'Failed' } | ForEach-Object {
                [ordered]@{ referenceId = $_['id']; uri = $_['uri']; error = $_['error'] }
            })
            contexts = @($contexts)
        }
    } finally { $lock.Dispose() }
}
