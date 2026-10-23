#Requires -Version 7.6

# Mechanical categorization handoff and rendering for focus-area preparation.
# Ref: docs/implementation.md, "Focus-area preparation". Callers load JsonValue.ps1.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$script:FocusAreaMapSchemaPath = Join-Path $PSScriptRoot '..\..\schemas\focus-area-map.schema.json'

# Indexes exact source spellings without assuming filesystem case sensitivity.
# Line contents are loaded only for referenced files or final coverage validation.
function Get-FocusAreaSourceIndex {
    param([Parameter(Mandatory)][string] $SessionStateDirectory)

    $files = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
        $directory = Get-Item -LiteralPath (Join-Path $SessionStateDirectory $root) -Force
        if (-not $directory.PSIsContainer -or $directory.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
            throw "Focus-area source root must be a directory, not a link: '$($directory.FullName)'."
        }
        foreach ($item in (Get-ChildItem -LiteralPath $directory.FullName -Recurse -Force)) {
            if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
                throw "Focus-area inputs must not contain links: '$($item.FullName)'."
            }
            if ($item.PSIsContainer) { continue }
            $relative = [IO.Path]::GetRelativePath($directory.FullName, $item.FullName).Replace('/', '\')
            $files.Add("$root\$relative", [pscustomobject]@{
                Path = $item.FullName
                Root = $root
                Lines = $null
                ContentLines = [Collections.Generic.HashSet[long]]::new()
                CoveredLines = [Collections.Generic.HashSet[long]]::new()
            })
        }
    }
    $files
}

# Resolves one schema-checked span and rejects drift in paths or line bounds before rendering.
function Get-FocusAreaSpan {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Span,
        [Parameter(Mandatory)][Collections.Generic.Dictionary[string, object]] $Files
    )

    $relative = $Span['path'].Replace('/', '\')
    if ($relative -match '(^\\|[:*?"<>|])' -or
        @($relative.Split('\') | Where-Object { $_ -in @('', '.', '..') }).Count -gt 0) {
        throw "Focus-area span requires a safe relative path: '$relative'."
    }
    $key = "$($Span['root'])\$relative"
    if (-not $Files.ContainsKey($key)) {
        throw "Focus-area source not found with exact spelling: '$key'."
    }
    $file = $Files[$key]
    if ($null -eq $file.Lines) { $file.Lines = [IO.File]::ReadAllLines($file.Path) }
    if ($Span['endLine'] -lt $Span['startLine'] -or $Span['endLine'] -gt $file.Lines.Count) {
        throw "Invalid line range for '$key': $($Span['startLine'])-$($Span['endLine']); file has $($file.Lines.Count) lines."
    }
    $lines = $file.Lines[($Span['startLine'] - 1)..($Span['endLine'] - 1)]
    $text = $lines -join "`n"
    if ([string]::IsNullOrWhiteSpace($text)) { throw "Empty focus-area span in '$key'." }
    [pscustomobject]@{
        File = $file
        Label = "${key}:$($Span['startLine'])-$($Span['endLine'])"
        Lines = @($lines)
        Text = $text
    }
}

# Records content and context coverage separately: only guideline content must not overlap.
function Add-FocusAreaSpanCoverage {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Span,
        [Parameter(Mandatory)][Collections.Generic.Dictionary[string, object]] $Files,
        [switch] $Content
    )

    $resolved = Get-FocusAreaSpan -Span $Span -Files $Files
    for ($line = $Span['startLine']; $line -le $Span['endLine']; $line++) {
        if ($Content -and -not $resolved.File.ContentLines.Add($line)) {
            throw "Overlapping chapter content at '$($resolved.File.Path):$line'."
        }
        [void] $resolved.File.CoveredLines.Add($line)
    }
}

# Validates both worker maps and complete assembly maps without modifying either.
# Semantic scope correctness remains the preparation agent's responsibility.
function Read-FocusAreaMap {
    param(
        [Parameter(Mandatory)][string] $MapPath,
        [Parameter(Mandatory)][string] $SessionStateDirectory,
        [switch] $RequireComplete
    )

    $json = Read-PullRequestFileText -Path $MapPath
    Test-FocusAreaMapValue -Json $json -SessionStateDirectory $SessionStateDirectory -RequireComplete:$RequireComplete
}

# In-memory validation lets prepared authoring tools reject a mutation before replacing the file.
function Test-FocusAreaMapValue {
    param(
        [Parameter(Mandatory)][string] $Json,
        [Parameter(Mandatory)][string] $SessionStateDirectory,
        [switch] $RequireComplete
    )
    if (-not (Test-Json -Json $json -SchemaFile $script:FocusAreaMapSchemaPath)) {
        throw 'Invalid focus-area map.'
    }
    $map = ConvertFrom-JsonVerbatim -Text $json
    $files = Get-FocusAreaSourceIndex -SessionStateDirectory $SessionStateDirectory
    $categories = [Collections.Generic.Dictionary[string, int]]::new([StringComparer]::Ordinal)
    foreach ($category in $map['categories']) {
        if ([string]::IsNullOrWhiteSpace($category['title'])) {
            throw "Category '$($category['id']) requires a nonblank title."
        }
        if (-not $categories.TryAdd($category['id'], 0)) {
            throw "Duplicate category ID '$($category['id'])'."
        }
    }
    $chapters = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($chapter in $map['chapters']) {
        if (-not $chapters.Add($chapter['id'])) { throw "Duplicate chapter ID '$($chapter['id'])'." }
        if (-not $categories.ContainsKey($chapter['categoryId'])) {
            throw "Chapter '$($chapter['id']) names unknown category '$($chapter['categoryId'])'."
        }
        $categories[$chapter['categoryId']]++
        if ($chapter['content']['root'] -cne 'instruction-set-raw') {
            throw "Chapter '$($chapter['id']) content must come from instruction-set-raw."
        }
        Add-FocusAreaSpanCoverage -Span $chapter['content'] -Files $files -Content
        foreach ($source in $chapter['sources']) { $null = Get-FocusAreaSpan -Span $source -Files $files }
        foreach ($context in $chapter['applicability']) {
            $pathScope = Get-JsonProperty -Node $context -Name pathScope
            if ($null -ne $pathScope) {
                if ($context['uncertainties'].Count -gt 0 -or $context['declarations'].Count -eq 0) {
                    throw 'Path-only scope requires declarations and no unresolved uncertainty.'
                }
                $paths = @($pathScope['include'])
                if ($pathScope['base'] -cne '.') { $paths += $pathScope['base'] }
                foreach ($path in $paths) {
                    foreach ($segment in $path.Split('/')) {
                        if ($segment -in @('', '.', '..') -or ($segment.Contains('**') -and $segment -cne '**')) {
                            throw "Unsupported path-scope pattern '$path'. Use null for non-path or unsupported scopes."
                        }
                    }
                }
            }
            foreach ($declaration in $context['declarations']) {
                Add-FocusAreaSpanCoverage -Span $declaration -Files $files
            }
            foreach ($uncertainty in $context['uncertainties']) {
                foreach ($evidence in $uncertainty['evidence']) {
                    Add-FocusAreaSpanCoverage -Span $evidence -Files $files
                }
            }
        }
    }
    foreach ($category in $categories.Keys) {
        if ($categories[$category] -eq 0) { throw "Category '$category' has no chapters." }
    }
    if ($RequireComplete) {
        # Ref: prepare-focus-areas.md, Stage 3; the bound limits the number of review workers.
        if ($categories.Count -gt 128) { throw 'Merge categories before assembly: the maximum is 128.' }
        foreach ($entry in $files.GetEnumerator()) {
            if ($entry.Value.Root -cne 'instruction-set-raw') { continue }
            $file = $entry.Value
            if ($null -eq $file.Lines) { $file.Lines = [IO.File]::ReadAllLines($file.Path) }
            for ($index = 0; $index -lt $file.Lines.Count; $index++) {
                if (-not [string]::IsNullOrWhiteSpace($file.Lines[$index]) -and
                    -not $file.CoveredLines.Contains($index + 1)) {
                    throw "Uncovered raw-input line '$($entry.Key):$($index + 1)'. Add its chapter or scope context."
                }
            }
        }
    }
    [pscustomobject]@{ Map = $map; Files = $files }
}

# Quotes source declarations without allowing embedded Markdown fences to end the quotation.
function Format-FocusAreaQuotation {
    param([Parameter(Mandatory)][string] $Text)

    $width = 3 # Markdown requires at least this many backticks for a fenced block.
    foreach ($match in [regex]::Matches($Text, '`+')) { $width = [Math]::Max($width, $match.Length + 1) }
    $fence = '`' * $width
    "$fence`n$Text`n$fence"
}

# Renders complete local metadata so a reviewer need not reconstruct inherited scope elsewhere.
function Format-FocusAreaMetadata {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Chapter,
        [Parameter(Mandatory)][Collections.Generic.Dictionary[string, object]] $Files
    )

    $parts = [Collections.Generic.List[string]]::new()
    $parts.Add('<!-- ss-review scope metadata -->')
    foreach ($source in $Chapter['sources']) {
        $span = Get-FocusAreaSpan -Span $source -Files $Files
        $parts.Add("Source: $($span.Label)")
        $sourceHeading = $span.Lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1
        $parts.Add((Format-FocusAreaQuotation -Text $sourceHeading))
    }
    $alternative = 0
    foreach ($context in $Chapter['applicability']) {
        $alternative++
        $parts.Add("Applicability alternative ${alternative}:")
        $parts.Add("Effective scope: $($context['effectiveScope'])")
        if ($context['declarations'].Count -eq 0) {
            $parts.Add('Scope declarations: None declared.')
        } else {
            $parts.Add('Scope declarations:')
            foreach ($declaration in $context['declarations']) {
                $span = Get-FocusAreaSpan -Span $declaration -Files $Files
                $parts.Add($span.Label)
                $parts.Add((Format-FocusAreaQuotation -Text $span.Text))
            }
        }
        if ($context['origins'].Count -eq 0) {
            $parts.Add('Reference origins: None (direct guidance).')
        } else {
            $parts.Add("Reference origins:`n$($context['origins'] -join "`n")")
        }
        if ($context['uncertainties'].Count -eq 0) {
            $parts.Add('Scope uncertainty: None recorded.')
        } else {
            foreach ($uncertainty in $context['uncertainties']) {
                $parts.Add("Scope uncertainty: $($uncertainty['reason'])")
                foreach ($evidence in $uncertainty['evidence']) {
                    $span = Get-FocusAreaSpan -Span $evidence -Files $Files
                    $parts.Add($span.Label)
                    $parts.Add((Format-FocusAreaQuotation -Text $span.Text))
                }
            }
        }
    }
    $parts.Add('<!-- end ss-review scope metadata -->')
    $parts -join "`n`n"
}

# Builds both reading views from the same validated model, preserving map and source-line order.
function ConvertTo-FocusAreaDocuments {
    param([Parameter(Mandatory)] $ValidatedMap)

    $catalog = [Collections.Generic.List[string]]::new()
    $catalog.Add('# Guideline categories')
    $documents = [ordered]@{}
    foreach ($category in $ValidatedMap.Map['categories']) {
        $heading = "# $($category['title'])"
        $catalog.Add("$heading`n`nCategory: $($category['id'])")
        $sections = [Collections.Generic.List[string]]::new()
        $sections.Add($heading)
        foreach ($chapter in $ValidatedMap.Map['chapters']) {
            if ($chapter['categoryId'] -cne $category['id']) { continue }
            $span = Get-FocusAreaSpan -Span $chapter['content'] -Files $ValidatedMap.Files
            $metadata = Format-FocusAreaMetadata -Chapter $chapter -Files $ValidatedMap.Files
            $catalog.Add("## $($chapter['id'])`n`nContent: $($span.Label)`n`n$metadata")

            # Only inspect the start of the selected range; repeated headings elsewhere are irrelevant.
            $headingLines = 0
            if ($span.Lines[0] -match '^#{1,6}[ \t]+') {
                $headingLines = 1
            } elseif ($span.Lines.Count -gt 1 -and $span.Lines[1] -match '^[ \t]*(=+|-+)[ \t]*$') {
                $headingLines = 2
            }
            if ($headingLines -eq 0) {
                $sections.Add("$metadata`n`n$($span.Text)")
            } else {
                $originalHeading = $span.Lines[0..($headingLines - 1)] -join "`n"
                $body = if ($span.Lines.Count -gt $headingLines) {
                    $span.Lines[$headingLines..($span.Lines.Count - 1)] -join "`n"
                } else { '' }
                $sections.Add("$originalHeading`n`n$metadata`n`n$body")
            }
        }
        # A fixed prefix keeps every valid category ID away from reserved device filenames.
        $documents["focus-areas\focus-$($category['id']).md"] = ($sections -join "`n`n") + "`n"
    }
    $documents['instruction-set-categories.md'] = ($catalog -join "`n`n") + "`n"
    foreach ($path in @($documents.Keys)) {
        $documents[$path] = $documents[$path].Replace("`r`n", "`n").Replace("`r", "`n")
    }
    foreach ($category in $ValidatedMap.Map['categories']) {
        $path = "focus-areas\focus-$($category['id']).md"
        # The complete map subset covers every rendered section. A digest rejects stale metadata;
        # base64 keeps arbitrary source prose from terminating the HTML comment.
        $metadata = [ordered]@{
            schemaVersion = 1
            contentSha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($documents[$path])))
            map = [ordered]@{
                schemaVersion = 1
                categories = @($category)
                chapters = @($ValidatedMap.Map['chapters'] | Where-Object { $_['categoryId'] -ceq $category['id'] })
            }
        }
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((ConvertTo-JsonText -Value $metadata)))
        $documents[$path] = "<!-- ss-review applicability $encoded -->`n" + $documents[$path]
    }
    $documents
}

# Installs the generated set only after validation and rendering have completed.
# Backups survive any failed rollback so an I/O failure cannot erase the previous set.
function Write-FocusAreaDocuments {
    param(
        [Parameter(Mandatory)][Collections.IDictionary] $Documents,
        [Parameter(Mandatory)][string] $SessionStateDirectory
    )

    $transactionLock = Open-PullRequestFileLock -Path (Join-Path $SessionStateDirectory 'focus-areas')
    try {
        $root = (Resolve-Path -LiteralPath $SessionStateDirectory).ProviderPath
        $staging = Join-Path $root ".focus-area-assembly-$([guid]::NewGuid().ToString('N'))"
        $artifacts = @('focus-areas', 'instruction-set-categories.md')
        foreach ($artifact in $artifacts) {
            $destination = Join-Path $root $artifact
            if (Test-Path -LiteralPath $destination) {
                $items = @(Get-Item -LiteralPath $destination -Force)
                if ($items[0].PSIsContainer) { $items += @(Get-ChildItem -LiteralPath $destination -Recurse -Force) }
                if (@($items | Where-Object { $_.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint) }).Count) {
                    throw "Assembly output must not contain links: '$destination'."
                }
            }
        }
        $installed = [Collections.Generic.List[string]]::new()
        $backups = [Collections.Generic.List[string]]::new()
        $removeStaging = $true
        New-Item -ItemType Directory -Path (Join-Path $staging 'focus-areas') -Force | Out-Null
        try {
            foreach ($path in $Documents.Keys) {
                [IO.File]::WriteAllText((Join-Path $staging $path), $Documents[$path], [Text.UTF8Encoding]::new($false))
            }
            try {
                foreach ($artifact in $artifacts) {
                    $destination = Join-Path $root $artifact
                    if (Test-Path -LiteralPath $destination) {
                        Move-Item -LiteralPath $destination -Destination (Join-Path $staging "backup-$artifact")
                        $backups.Add($artifact)
                    }
                    Move-Item -LiteralPath (Join-Path $staging $artifact) -Destination $destination
                    $installed.Add($artifact)
                }
            } catch {
                $installationError = $_
                try {
                    foreach ($artifact in $installed) {
                        Remove-Item -LiteralPath (Join-Path $root $artifact) -Recurse -Force
                    }
                    foreach ($artifact in $backups) {
                        Move-Item -LiteralPath (Join-Path $staging "backup-$artifact") -Destination (Join-Path $root $artifact)
                    }
                } catch {
                    $removeStaging = $false
                    throw "Focus-area installation failed: $installationError Rollback also failed: $_ Backups remain at '$staging'."
                }
                throw $installationError
            }
        } finally {
            if ($removeStaging) { Remove-Item -LiteralPath $staging -Recurse -Force }
        }
    } finally { $transactionLock.Dispose() }
}
