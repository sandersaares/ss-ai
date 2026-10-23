#Requires -Version 7.6

# Produces reading views, never finding records or approval state.
# Ref: docs/implementation.md, "HTML report rendering".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-ReviewReportHash {
    param([Parameter(Mandatory)][AllowEmptyCollection()][byte[]] $Bytes)
    [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function ConvertTo-ReviewDiagramHtml {
    param(
        [Parameter(Mandatory)][string] $Source,
        [Parameter(Mandatory)][string] $Svg
    )

    $settings = [Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [Xml.DtdProcessing]::Prohibit
    $reader = [Xml.XmlReader]::Create([IO.StringReader]::new($Svg), $settings)
    try {
        $document = [Xml.XmlDocument]::new()
        $document.XmlResolver = $null
        $document.Load($reader)
    } finally { $reader.Dispose() }
    if ($document.DocumentElement.LocalName -cne 'svg') { throw 'Mermaid did not produce an SVG document.' }
    foreach ($element in $document.SelectNodes('//*')) {
        if ($element.LocalName -in @('script', 'foreignObject', 'image', 'iframe')) {
            throw "The generated diagram contains unsupported '$($element.LocalName)' content."
        }
        foreach ($attribute in $element.Attributes) {
            if ($attribute.LocalName -match '^on' -or
                ($attribute.LocalName -in @('href', 'src') -and -not $attribute.Value.StartsWith('#'))) {
                throw 'The generated diagram contains active or externally referenced content.'
            }
        }
    }
    if ($Svg -match '(?i)@import') {
        throw 'The generated diagram references an external style resource.'
    }
    foreach ($match in [regex]::Matches($Svg, '(?i)url\(\s*(?:["''](?<quoted>[^"'']*)["'']|(?<bare>[^)\s]+))\s*\)')) {
        $url = if ($match.Groups['quoted'].Success) { $match.Groups['quoted'].Value } else { $match.Groups['bare'].Value }
        if (-not $url.StartsWith('#')) { throw 'The generated diagram references an external style resource.' }
    }
    $description = $document.SelectSingleNode('//*[local-name()="desc"]')
    $alt = if ($description) { $description.InnerText } else { 'Mermaid diagram' }
    $image = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Svg))
    $escapedSource = [Net.WebUtility]::HtmlEncode($Source)
    $escapedAlt = [Net.WebUtility]::HtmlEncode($alt)
    $size = ''
    $viewBox = $document.DocumentElement.GetAttribute('viewBox')
    if ($viewBox) {
        $coordinates = $viewBox -split '[,\s]+'
        if ($coordinates.Count -ne 4) { throw 'The generated diagram has an invalid viewBox.' }
        $width = [double]::Parse($coordinates[2], [cultureinfo]::InvariantCulture)
        $height = [double]::Parse($coordinates[3], [cultureinfo]::InvariantCulture)
        if (-not [double]::IsFinite($width) -or -not [double]::IsFinite($height) -or $width -le 0 -or $height -le 0) {
            throw 'The generated diagram has invalid dimensions.'
        }
        # SVG images otherwise fall back to browser intrinsic sizing and shrink labels. Preserve
        # the renderer's layout dimensions; wide diagrams scroll instead of becoming unreadable.
        $size = " width=`"$([Math]::Ceiling($width).ToString([cultureinfo]::InvariantCulture))`" height=`"$([Math]::Ceiling($height).ToString([cultureinfo]::InvariantCulture))`""
    }
    "<figure class=`"diagram`"><div class=`"diagram-scroll`"><img alt=`"$escapedAlt`"$size src=`"data:image/svg+xml;base64,$image`"></div><details><summary>Diagram source</summary><pre><code>$escapedSource</code></pre></details></figure>"
}

function Convert-ReviewReportNode {
    param(
        [AllowNull()] $Node,
        [Parameter(Mandatory)][scriptblock] $RenderDiagram
    )

    if ($Node -is [System.Collections.IDictionary]) {
        $kind = [string] (Get-JsonProperty -Node $Node -Name 't')
        if ($kind -eq 'CodeBlock' -and $Node['c'][0][1] -ccontains 'mermaid') {
            $source = [string] $Node['c'][1]
            # Directives, image nodes and HTML resources are outside the report diagram contract.
            if ($source -match '%%\{|(?m)\A\s*---|(?i)\b(?:img|icon)\s*:|(?:https?|file|data):|<\s*(?:img|iframe|script|link)\b') {
                throw 'Report diagrams must not contain configuration directives or external resources.'
            }
            $svg = & $RenderDiagram $source
            $Node['t'] = 'RawBlock'
            $Node['c'] = @('html', (ConvertTo-ReviewDiagramHtml -Source $source -Svg $svg))
            return
        }
        if ($kind -eq 'Image') {
            throw 'Embedded image references are not supported in review reports. Use an ordinary link or a Mermaid diagram.'
        }
        if ($kind -eq 'Link') {
            $url = [string] $Node['c'][2][0]
            $uri = $null
            if (-not $url.StartsWith('#') -and
                (-not [uri]::TryCreate($url, [UriKind]::Absolute, [ref] $uri) -or $uri.Scheme -notin @('https', 'http'))) {
                if ($null -ne $uri) { throw "Report links must not use the '$($uri.Scheme)' scheme: '$url'." }
                # Existing findings may cite repository-relative files. Keep the label and
                # destination visible without linking relative to the unrelated session folder.
                $label = @($Node['c'][1]) + @(
                    @{ t = 'Space' }
                    @{ t = 'Str'; c = '(' }
                    @{ t = 'Code'; c = @(@('', @(), @()), $url) }
                    @{ t = 'Str'; c = ')' }
                )
                $Node['t'] = 'Span'
                $Node['c'] = @(@('', @(), @()), $label)
            }
        }
        # Some GFM constructs still produce raw nodes with raw_html disabled. Show their source
        # literally; only the Mermaid branch above may introduce renderer-owned raw HTML.
        if ($kind -eq 'RawInline') {
            $Node['t'] = 'Str'
            $Node['c'] = [string] $Node['c'][1]
            return
        }
        if ($kind -eq 'RawBlock') {
            $Node['t'] = 'CodeBlock'
            $Node['c'] = @(@('', @(), @()), [string] $Node['c'][1])
            return
        }
        foreach ($key in @($Node.Keys)) { Convert-ReviewReportNode -Node $Node[$key] -RenderDiagram $RenderDiagram }
    } elseif ($Node -is [object[]]) {
        foreach ($item in $Node) { Convert-ReviewReportNode -Node $item -RenderDiagram $RenderDiagram }
    }
}

function Write-ReviewHtml {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $OutputPath,
        [Parameter(Mandatory)][string] $SkillRoot,
        [Parameter(Mandatory)][System.Collections.IDictionary] $Tools,
        [Parameter(Mandatory)][scriptblock] $InvokeProcess,
        [string] $Title = 'Review report',
        # Only the installer may reuse its retained exclusive lock during its readiness probe.
        [switch] $InstallationLocked
    )

    $toolsLock = if (-not $InstallationLocked) { Open-PullRequestFileLock -Path (Get-ReportToolsLockPath) -Shared }
    try {
        $transactionLock = Open-PullRequestFileLock -Path $OutputPath
        try {
            Assert-ReviewReportTools -Tools $Tools
            $sourcePath = Resolve-PullRequestPath -Path $Path
            $htmlPath = Resolve-PullRequestPath -Path $OutputPath
            if ($sourcePath -eq $htmlPath) { throw 'HTML output must not replace the canonical Markdown file.' }
            $markdown = (Read-PullRequestFileText -Path ($sourcePath) -Encoding ([Text.Encoding]::UTF8))
            if ([string]::IsNullOrWhiteSpace($markdown)) { throw 'The report Markdown is empty.' }
            $sourceHash = Get-ReviewReportHash -Bytes ([Text.Encoding]::UTF8.GetBytes($markdown))
            $scratch = Join-Path ([IO.Path]::GetDirectoryName($htmlPath)) ".report-$([guid]::NewGuid().ToString('N'))"
            [IO.Directory]::CreateDirectory($scratch) | Out-Null
            try {
                $astText = Invoke-ReportProcess -InvokeProcess $InvokeProcess -FilePath $Tools['pandoc'] `
                    -ArgumentList @('--from=gfm-raw_html', '--to=json') -StandardInput $markdown
                $ast = ConvertFrom-JsonVerbatim -Text $astText
                $ast['meta'] = [ordered]@{}
                $diagramState = @{ index = 0 }
                # The traversal is synchronous; keep helper lookup in the loaded script's scope.
                $renderDiagram = {
                    param([string] $source)
                    $diagramState.index++
                    $inputPath = Join-Path $scratch "diagram-$($diagramState.index).mmd"
                    $outputPath = Join-Path $scratch "diagram-$($diagramState.index).svg"
                    $configPath = Join-Path $scratch 'mermaid.json'
                    $config = @{
                        securityLevel = 'strict'
                        htmlLabels = $false
                        flowchart = @{ htmlLabels = $false }
                        fontFamily = 'Arial, sans-serif'
                        deterministicIds = $true
                        deterministicIDSeed = "$sourceHash-$($diagramState.index)"
                        theme = 'neutral'
                    }
                    Write-PullRequestOutputFile -Path $inputPath -Content $source
                    Write-PullRequestOutputFile -Path $configPath -Content (ConvertTo-JsonText -Value $config)
                    Invoke-ReportProcess -InvokeProcess $InvokeProcess -FilePath $Tools['node'] `
                        -ArgumentList @($Tools['mermaid'], '--input', $inputPath, '--output', $outputPath, '--configFile', $configPath, '--quiet') | Out-Null
                    (Read-PullRequestFileText -Path ($outputPath) -Encoding ([Text.Encoding]::UTF8))
                }
                Convert-ReviewReportNode -Node $ast -RenderDiagram $renderDiagram
                $template = Join-Path $SkillRoot 'templates\report.html'
                $html = Invoke-ReportProcess -InvokeProcess $InvokeProcess -FilePath $Tools['pandoc'] `
                    -ArgumentList @('--from=json', '--to=html5', '--standalone', '--toc', '--wrap=none', '--syntax-highlighting=pygments',
                        '--template', $template, '--metadata', "title=$Title") `
                    -StandardInput (ConvertTo-JsonText -Value $ast -Depth 100)
                if ($html -notmatch '<!DOCTYPE html>' -or $html -notmatch 'Content-Security-Policy' -or
                    $html -match '(?is)<script\b|<[a-z][^>]*\son[a-z]+\s*=|<[a-z][^>]*\s(?:src|href)\s*=\s*["''](?:javascript:|file:)') {
                    throw 'The HTML renderer did not produce a safe standalone report.'
                }
                Write-PullRequestOutputFileAtomically -Path $htmlPath -Content $html
                [ordered]@{
                    sourcePath = $sourcePath
                    sourceHash = $sourceHash
                    htmlPath = $htmlPath
                    htmlUri = ([uri] $htmlPath).AbsoluteUri
                    htmlHash = Get-ReviewReportHash -Bytes ((Read-PullRequestFileBytes -Path ($htmlPath)))
                    diagramCount = $diagramState.index
                }
            } finally {
                Remove-Item -LiteralPath $scratch -Recurse -Force
            }
        } finally { $transactionLock.Dispose() }
    } finally { if ($null -ne $toolsLock) { $toolsLock.Dispose() } }
}

# Exercise the production pipeline, including a browser launch, before claiming setup is ready.
# Ref: docs/implementation.md, "Report tools".
function Assert-ReviewReportRendering {
    param(
        [Parameter(Mandatory)][string] $SkillRoot,
        [Parameter(Mandatory)][System.Collections.IDictionary] $Tools,
        [scriptblock] $InvokeProcess = (Get-ReportProcessInvoker),
        [switch] $InstallationLocked
    )

    Assert-ReviewReportTools -Tools $Tools
    $scratch = Join-Path ([IO.Path]::GetTempPath()) "ss-review-report-check-$([guid]::NewGuid().ToString('N'))"
    [IO.Directory]::CreateDirectory($scratch) | Out-Null
    try {
        $source = Join-Path $scratch 'check.md'
        $output = Join-Path $scratch 'check.html'
        # A plain-text probe would never launch Chrome and could miss absent system libraries.
        Write-PullRequestOutputFile -Path $source -Content @'
# Renderer prerequisite check

```mermaid
flowchart LR
    A[Prerequisites] --> B[Report]
```
'@
        $result = Write-ReviewHtml -Path $source -OutputPath $output -SkillRoot $SkillRoot -Tools $Tools `
            -InvokeProcess $InvokeProcess -InstallationLocked:$InstallationLocked
        $html = Read-PullRequestFileText -Path $output -Encoding ([Text.Encoding]::UTF8)
        if ($result.diagramCount -ne 1 -or $html -notmatch '<img\b[^>]*src="data:image/svg\+xml;base64,') {
            throw 'The report readiness check did not produce the expected embedded Mermaid diagram.'
        }
    } finally {
        Remove-Item -LiteralPath $scratch -Recurse -Force
    }
}
