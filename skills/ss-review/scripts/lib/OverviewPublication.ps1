#Requires -Version 7.6

# Freezes the initial overview and journals its best-effort publication independently of findings.
# Ref: docs/implementation.md, "Initial overview publication".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# Shared preparation/publication gate; fenced examples are not document headings.
function Assert-ReviewOverviewStructure {
    param([Parameter(Mandatory)][string] $Text)
    $headings = [Collections.Generic.List[object]]::new()
    $fence = ''
    $body = [Text.StringBuilder]::new()
    foreach ($line in [regex]::Split($Text, '\r\n|\r|\n')) {
        $marker = [regex]::Match($line, '^\s{0,3}(`{3,}|~{3,})(.*)$')
        if ($marker.Success) {
            $run = $marker.Groups[1].Value
            if (-not $fence) { $fence = $run }
            elseif ($run[0] -eq $fence[0] -and $run.Length -ge $fence.Length -and -not $marker.Groups[2].Value.Trim()) { $fence = '' }
            $null = $body.AppendLine($line)
            continue
        }
        $heading = if (-not $fence) { [regex]::Match($line, '^## (.+?)\s*$') } else { $null }
        if ($null -ne $heading -and $heading.Success) {
            if ($headings.Count) { $headings[$headings.Count - 1]['body'] = $body.ToString() }
            $null = $body.Clear()
            $headings.Add(@{ heading = $heading.Groups[1].Value; body = '' })
        } else { $null = $body.AppendLine($line) }
    }
    if ($headings.Count) { $headings[$headings.Count - 1]['body'] = $body.ToString() }
    $position = -1
    # Matches the overview's required public structure in prepare-content-overview.md.
    foreach ($name in @('Mission', 'Substance', 'Design changes', 'Significant changes', 'Structure', 'Mechanism', 'Completeness assessment', 'Related history', 'Related work')) {
        $indices = @(for ($i = 0; $i -lt $headings.Count; $i++) { if ($headings[$i]['heading'] -ceq $name) { $i } })
        if ($indices.Count -ne 1 -or $indices[0] -le $position) { throw "The overview is missing the ordered '$name' section." }
        $position = $indices[0]
        if ([string]::IsNullOrWhiteSpace($headings[$position]['body'])) { throw "The overview has an empty '$name' section." }
    }
}

function New-OverviewSnapshot {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Context,
        [Parameter(Mandatory)][string] $OverviewPath,
        [Parameter(Mandatory)][string] $OutputDirectory,
        [Parameter(Mandatory)][string] $SkillRoot,
        [switch] $RefreshUnpublished,
        [DateTimeOffset] $NowUtc = [DateTimeOffset]::UtcNow
    )

    $snapshotPath = Join-Path $OutputDirectory 'overview-snapshot.json'
    if ([IO.File]::Exists($snapshotPath)) {
        $snapshot = ConvertFrom-JsonVerbatim -Text ((Read-PullRequestFileText -Path ($snapshotPath)))
        if ($snapshot['schemaVersion'] -ne 1 -or $snapshot['pullRequestUrl'] -cne $Context['pullRequestUrl']) {
            throw 'The frozen overview belongs to another PR or an unsupported snapshot format.'
        }
        if ((Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($snapshot['markdownPath'])))) -cne $snapshot['markdownHash']) {
            throw 'The frozen initial overview was modified. Reconcile its publication before retrying.'
        }
        $currentText = (Read-PullRequestFileText -Path ((Resolve-PullRequestPath -Path $OverviewPath)) -Encoding ([Text.Encoding]::UTF8))
        if (-not $RefreshUnpublished -or
            (Get-ReviewReportHash -Bytes ([Text.Encoding]::UTF8.GetBytes($currentText))) -ceq $snapshot['sourceHash']) {
            return $snapshot
        }
        # A corrected local report can replace an unpublished snapshot. The caller permits this
        # only while no write completed or may have reached the service.
        $htmlManifestPath = Join-Path $OutputDirectory 'overview-publication\html.json'
        if ([IO.File]::Exists($htmlManifestPath)) { [IO.File]::Delete($htmlManifestPath) }
    }
    $sourcePath = Resolve-PullRequestPath -Path $OverviewPath
    $overview = (Read-PullRequestFileText -Path ($sourcePath) -Encoding ([Text.Encoding]::UTF8))
    Assert-ReviewOverviewStructure -Text $overview
    foreach ($privatePath in @($OutputDirectory, $SkillRoot, $HOME)) {
        if ($privatePath -and ($overview.Contains($privatePath, [StringComparison]::OrdinalIgnoreCase) -or
                $overview.Contains($privatePath.Replace('\', '/'), [StringComparison]::OrdinalIgnoreCase))) {
            throw 'The overview contains a private session, skill, or home path. Correct the local overview before publication.'
        }
    }
    if ($overview -match '\{\{[A-Z_][A-Z_0-9]*\}\}') { throw 'The overview contains an unexpanded workflow placeholder.' }
    $pullRequest = $Context['pullRequest']
    $sourceCommit = [string] $pullRequest['reviewedSourceCommit']
    $baseCommit = [string] $pullRequest['reviewedMergeBaseCommit']
    if ($sourceCommit -notmatch '^[0-9a-fA-F]{40,64}$' -or $baseCommit -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw 'Overview publication requires the inspected source and merge-base commits.'
    }
    $report = @(
        $script:FindingMarkdownAuthorshipPrefix
        'Generated during preparation, before the review. Completeness questions are preliminary, not verified findings or a merge recommendation. This snapshot is not updated by later review work.'
        "Pull request: $($Context['pullRequestUrl'])"
        "Inspected source commit: $sourceCommit"
        "Inspected merge-base commit: $baseCommit"
        $overview.TrimEnd()
    ) -join "`n`n"
    $markdownPath = Join-Path $OutputDirectory 'overview-publication\report.md'
    Write-PullRequestOutputFileAtomically -Path $markdownPath -Content $report
    $snapshot = [ordered]@{
        schemaVersion = 1
        runId = [guid]::NewGuid().ToString('N')
        pullRequestUrl = $Context['pullRequestUrl']
        target = $Context['target']
        sourceCommit = $sourceCommit
        mergeBaseCommit = $baseCommit
        createdUtc = $NowUtc.ToString('o')
        sourceHash = Get-ReviewReportHash -Bytes ([Text.Encoding]::UTF8.GetBytes($overview))
        markdownPath = $markdownPath
        markdownHash = Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($markdownPath)))
    }
    Write-PullRequestOutputFileAtomically -Path $snapshotPath -Content (ConvertTo-JsonText -Value $snapshot)
    $snapshot
}

# Keeps attribution separate from the linked sentence; ref: docs/design.md, "Content overview".
function New-OverviewComment {
    param(
        [Parameter(Mandatory)][string] $Url,
        [string] $CommentPrefix = ''
    )
    $link = "[Pull request overview (auto-generated).]($Url)"
    if ([string]::IsNullOrEmpty($CommentPrefix)) { return $link }
    "$CommentPrefix`n`n$link"
}

function Write-OverviewArchive {
    param(
        [Parameter(Mandatory)][string] $HtmlPath,
        [Parameter(Mandatory)][string] $ArchivePath
    )
    $fullPath = Resolve-PullRequestPath -Path $ArchivePath
    $temporary = "$fullPath.$([guid]::NewGuid().ToString('N')).tmp"
    try {
        $file = [IO.File]::Create($temporary)
        try {
            $zip = [IO.Compression.ZipArchive]::new($file, [IO.Compression.ZipArchiveMode]::Create, $true)
            try {
                $entry = $zip.CreateEntry('report.html', [IO.Compression.CompressionLevel]::Optimal)
                # Use the ZIP format's earliest timestamp so archive identity does not depend on
                # the clock; provenance belongs in the frozen report, not archive metadata.
                $entry.LastWriteTime = [DateTimeOffset]::new(1980, 1, 1, 0, 0, 0, [TimeSpan]::Zero)
                $content = $entry.Open()
                $source = Open-PullRequestFileRead -Path $HtmlPath
                try { $source.CopyTo($content) } finally { $source.Dispose(); $content.Dispose() }
            } finally { $zip.Dispose() }
        } finally { $file.Dispose() }
        Publish-PullRequestStagingFile -TemporaryPath $temporary -Path $fullPath
    } finally {
        if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
    }
    [ordered]@{
        archivePath = $fullPath
        archiveHash = Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($fullPath)))
    }
}

# Keeps persisted artifact paths inside the publication directory, including
# retry directories, without following a link outside the publication tree.
function Resolve-OverviewArtifactPath {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $PublicationDirectory
    )

    $root = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath((Resolve-PullRequestPath -Path $PublicationDirectory)))
    $fullPath = [IO.Path]::GetFullPath((Resolve-PullRequestPath -Path $Path))
    $prefix = $root + [IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($prefix, [StringComparison]::Ordinal)) {
        throw 'The overview artifact path must be inside the publication directory.'
    }

    $relative = [IO.Path]::GetRelativePath($root, $fullPath)
    $current = $root
    $parts = $relative.Split([char[]] @([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar), [StringSplitOptions]::RemoveEmptyEntries)
    for ($index = -1; $index -lt $parts.Length; $index++) {
        if ($index -ge 0) { $current = Join-Path $current $parts[$index] }
        try {
            $attributes = [IO.File]::GetAttributes($current)
        } catch [IO.FileNotFoundException] {
            # A new archive may not exist yet, but its parent must already exist.
            if ($index -eq ($parts.Length - 1)) { break }
            throw
        }
        if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Overview artifact paths cannot traverse a symbolic link.'
        }
    }
    $fullPath
}

# Picks the matching legacy or run-specific manifest without rewriting either.
function Get-OverviewHtmlManifest {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Snapshot,
        [Parameter(Mandatory)][string] $PublicationDirectory
    )

    $legacy = Join-Path $PublicationDirectory 'html.json'
    $legacyHtml = $null
    if ([IO.File]::Exists($legacy)) {
        $legacy = Resolve-OverviewArtifactPath -Path $legacy -PublicationDirectory $PublicationDirectory
        $legacyHtml = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $legacy)
        if (-not (Test-JsonProperty -Node $legacyHtml -Name 'sourceHash') -or $legacyHtml['sourceHash'] -isnot [string]) {
            throw 'The overview HTML manifest has no source hash.'
        }
        if ($legacyHtml['sourceHash'] -ceq $Snapshot['markdownHash']) {
            return @{ path = $legacy; html = $legacyHtml }
        }
    }
    $runId = $Snapshot['runId']
    if ($runId -isnot [string] -or $runId -cnotmatch '^[0-9a-f]{32}$') {
        throw 'The overview snapshot has an invalid run identifier.'
    }
    $generation = Join-Path (Join-Path $PublicationDirectory $runId) 'html.json'
    if ([IO.File]::Exists($generation)) {
        $generation = Resolve-OverviewArtifactPath -Path $generation -PublicationDirectory $PublicationDirectory
        return @{ path = $generation; html = ConvertFrom-JsonVerbatim -Text (Read-PullRequestFileText -Path $generation) }
    }
    if ($null -ne $legacyHtml) {
        # A previous generation remains evidence; prepare this run separately.
        return @{ path = $generation; html = $null }
    }
    $null
}

function Invoke-OverviewWrite {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Result,
        [Parameter(Mandatory)][string] $ResultPath,
        [Parameter(Mandatory)][string] $Name,
        [Parameter(Mandatory)][scriptblock] $Action
    )

    $operation = [ordered]@{ name = $Name; status = 'pending'; response = $null }
    $Result['operations'] = @($Result['operations']) + @($operation)
    Write-PullRequestOutputFileAtomically -Path $ResultPath -Content (ConvertTo-JsonText -Value $Result)
    try {
        $response = & $Action
        $operation['response'] = $response
        $operation['status'] = 'completed'
    } catch {
        $operation['status'] = Get-PublicationFailureOutcome -Exception $_.Exception
        throw
    }
    Write-PullRequestOutputFileAtomically -Path $ResultPath -Content (ConvertTo-JsonText -Value $Result)
    $response
}

function Invoke-InitialOverviewPublication {
    param(
        [Parameter(Mandatory)][System.Collections.IDictionary] $Context,
        [Parameter(Mandatory)][string] $OverviewPath,
        [Parameter(Mandatory)][string] $OutputDirectory,
        [Parameter(Mandatory)][string] $SkillRoot,
        [Parameter(Mandatory)][scriptblock] $InvokeRemote,
        [Parameter(Mandatory)][scriptblock] $RenderHtml,
        [string] $CommentPrefix = ''
    )

    $transactionLock = Open-PullRequestFileLock -Path (Join-Path $OutputDirectory 'overview-publication-result.json')
    try {
        $resultPath = Join-Path $OutputDirectory 'overview-publication-result.json'
        $result = [ordered]@{
            schemaVersion = 1
            pullRequestUrl = $Context['pullRequestUrl']
            status = 'preparing'
            stage = 'preflight'
            resultPath = $resultPath
            snapshot = $null
            artifact = $null
            comment = $null
            operations = @()
            error = $null
            missingTools = @()
        }
        $mayWriteJournal = $true
        try {
            if ([IO.File]::Exists($resultPath)) {
                $mayWriteJournal = $false
                $previous = ConvertFrom-JsonVerbatim -Text ((Read-PullRequestFileText -Path ($resultPath)))
                if ($previous['schemaVersion'] -ne 1 -or $previous['pullRequestUrl'] -cne $Context['pullRequestUrl']) {
                    throw 'An overview publication record for a different PR or format already exists.'
                }
                if ($previous['status'] -eq 'succeeded') { return $previous }
                if (@($previous['operations'] | Where-Object { $_['status'] -ne 'failed' }).Count -gt 0) {
                    $previous['status'] = 'needsReconciliation'
                    $previous['error'] = 'A previous overview attempt wrote or may have written remote state. Investigate its recorded IDs before retrying; the review can continue.'
                    return $previous
                }
                $mayWriteJournal = $true
            }
            if (-not (& $InvokeRemote 'isOpen' $Context['target'] $null $null)) {
                $result['status'] = 'skipped'
                $result['error'] = 'Automatic initial overview publication is skipped for a closed pull request.'
            } else {
                $result['stage'] = 'freeze'
                $snapshot = New-OverviewSnapshot -Context $Context -OverviewPath $OverviewPath -OutputDirectory $OutputDirectory -SkillRoot $SkillRoot -RefreshUnpublished
                $result['snapshot'] = $snapshot
                if ($snapshot['target']['platform'] -eq 'azureDevOps') {
                    $result['stage'] = 'renderHtml'
                    $publicationDirectory = Join-Path $OutputDirectory 'overview-publication'
                    $htmlPath = Join-Path $publicationDirectory 'report.html'
                    $htmlManifestPath = Join-Path $publicationDirectory 'html.json'
                    $existingHtml = Get-OverviewHtmlManifest -Snapshot $snapshot -PublicationDirectory $publicationDirectory
                    if ($null -ne $existingHtml -and $null -ne $existingHtml['html']) {
                        $html = $existingHtml['html']
                        $htmlManifestPath = $existingHtml['path']
                        if (Test-JsonProperty -Node $html -Name 'htmlPath') {
                            if ($html['htmlPath'] -isnot [string] -or [string]::IsNullOrWhiteSpace($html['htmlPath'])) {
                                throw 'The overview HTML manifest has an invalid report path.'
                            }
                            $htmlPath = $html['htmlPath']
                        }
                        $htmlPath = Resolve-OverviewArtifactPath -Path $htmlPath -PublicationDirectory $publicationDirectory
                        if ($html['sourceHash'] -cne $snapshot['markdownHash'] -or
                            (Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($htmlPath)))) -cne $html['htmlHash']) {
                            throw 'The frozen HTML report no longer matches its snapshot.'
                        }
                    } else {
                        if ($null -ne $existingHtml) {
                            $htmlManifestPath = $existingHtml['path']
                            $generationDirectory = Split-Path -Parent $htmlManifestPath
                            $null = Resolve-OverviewArtifactPath -Path $generationDirectory -PublicationDirectory $publicationDirectory
                            [IO.Directory]::CreateDirectory($generationDirectory) | Out-Null
                            $htmlPath = Resolve-OverviewArtifactPath -Path (Join-Path $generationDirectory 'report.html') -PublicationDirectory $publicationDirectory
                            if ([IO.File]::Exists($htmlPath) -or [IO.File]::Exists((Join-Path $generationDirectory 'report.zip'))) {
                                throw 'Unrecorded overview artifacts already exist for this snapshot.'
                            }
                        }
                        $html = & $RenderHtml $snapshot['markdownPath'] $htmlPath
                        Write-PullRequestOutputFileAtomically -Path $htmlManifestPath -Content (ConvertTo-JsonText -Value $html)
                    }
                    $snapshot['htmlPath'] = $htmlPath
                    $snapshot['htmlHash'] = $html['htmlHash']
                    $archivePath = Join-Path (Split-Path -Parent $htmlPath) 'report.zip'
                    if (Test-JsonProperty -Node $html -Name 'archivePath') {
                        if ($html['archivePath'] -isnot [string] -or [string]::IsNullOrWhiteSpace($html['archivePath'])) {
                            throw 'The overview HTML manifest has an invalid archive path.'
                        }
                        $archivePath = $html['archivePath']
                    }
                    $archivePath = Resolve-OverviewArtifactPath -Path $archivePath -PublicationDirectory $publicationDirectory
                    if (Test-JsonProperty -Node $html -Name 'archiveHash') {
                        if ((Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($archivePath)))) -cne $html['archiveHash']) {
                            throw 'The frozen HTML archive no longer matches its snapshot.'
                        }
                    } else {
                        # An unrecorded archive may belong to another interrupted attempt.
                        if ([IO.File]::Exists($archivePath)) {
                            throw 'An unrecorded overview archive already exists.'
                        }
                        $archive = Write-OverviewArchive -HtmlPath $htmlPath -ArchivePath $archivePath
                        $html['archiveHash'] = $archive['archiveHash']
                        Write-PullRequestOutputFileAtomically -Path $htmlManifestPath -Content (ConvertTo-JsonText -Value $html)
                    }
                    $snapshot['archivePath'] = $archivePath
                    $snapshot['archiveHash'] = $html['archiveHash']
                }
                $result['stage'] = 'upload'
                $artifact = Invoke-OverviewWrite -Result $result -ResultPath $resultPath -Name 'upload' -Action {
                    & $InvokeRemote 'upload' $snapshot['target'] $snapshot $null
                }
                $result['artifact'] = $artifact
                $result['stage'] = 'verifyArtifact'
                $artifact = & $InvokeRemote 'verifyArtifact' $snapshot['target'] $snapshot $artifact
                $result['artifact'] = $artifact
                $commentBody = New-OverviewComment -Url $artifact['url'] -CommentPrefix $CommentPrefix
                $result['stage'] = 'comment'
                $comment = Invoke-OverviewWrite -Result $result -ResultPath $resultPath -Name 'comment' -Action {
                    & $InvokeRemote 'comment' $snapshot['target'] $snapshot @{ body = $commentBody }
                }
                $result['comment'] = $comment
                $result['stage'] = 'verifyComment'
                & $InvokeRemote 'verifyComment' $snapshot['target'] $snapshot @{ comment = $comment; body = $commentBody } | Out-Null
                $result['status'] = 'succeeded'
            }
        } catch {
            # This is the deliberately best-effort report boundary, not a catch around the review.
            $result['status'] = if (@($result['operations']).Count -gt 0) { 'incomplete' } else { 'failed' }
            $result['error'] = $_.Exception.Message
            if ($_.Exception.Data.Contains('missingTools')) { $result['missingTools'] = $_.Exception.Data['missingTools'] }
        }
        if ($mayWriteJournal) {
            try {
                Write-PullRequestOutputFileAtomically -Path $resultPath -Content (ConvertTo-JsonText -Value $result)
            } catch {
                $result['status'] = 'journalFailure'
                $result['error'] = "$($result['error']) Could not save publication diagnostics: $($_.Exception.Message)"
            }
        }
        $result
    } finally { $transactionLock.Dispose() }
}
