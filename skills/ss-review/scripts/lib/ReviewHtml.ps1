#Requires -Version 7.6

# Produces reading views, never finding records or approval state.
# Ref: docs/implementation.md, "HTML report rendering".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

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

# Keeps the overview publisher's object contract while native code owns the transaction.
function Write-ReviewHtml {
    param(
        [Parameter(Mandatory)][string] $Path,
        [Parameter(Mandatory)][string] $OutputPath,
        [Parameter(Mandatory)][string] $SkillRoot,
        # Retained for callers of the PowerShell renderer; native code discovers and runs tools.
        [System.Collections.IDictionary] $Tools,
        [scriptblock] $InvokeProcess,
        [string] $Title = 'Review report',
        [switch] $InstallationLocked
    )

    # A PowerShell-held lock cannot transfer to the native process, which acquires its own.
    if ($InstallationLocked) { throw 'Report rendering cannot reuse a PowerShell installation lock.' }
    # Resolve relative to the PowerShell location, as the native child uses that location too.
    $output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
    $directory = [IO.Path]::GetDirectoryName($output)
    [void] [IO.Directory]::CreateDirectory($directory)
    # Keep title prose out of process arguments without changing the renderer's working directory.
    $titlePath = Join-Path $directory ".ss-ai-title-$([guid]::NewGuid().ToString('N')).txt"
    $created = $false
    try {
        $options = [IO.FileStreamOptions]::new()
        $options.Mode = [IO.FileMode]::CreateNew
        $options.Access = [IO.FileAccess]::Write
        $options.Share = [IO.FileShare]::None
        if (-not $IsWindows) {
            $options.UnixCreateMode = [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite
        }
        $stream = [IO.FileStream]::new($titlePath, $options)
        $created = $true
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Title)
            $stream.Write($bytes, 0, $bytes.Length)
        } finally { $stream.Dispose() }
        $json = Invoke-NativeReviewWorkflow -Arguments @('render-review-html', "--path=$Path",
            "--output-path=$OutputPath", "--skill-root=$SkillRoot", "--tools-directory=$(Get-ReportToolsDirectory)",
            "--title-path=$titlePath")
        ConvertFrom-Json -InputObject $json -AsHashtable -NoEnumerate -ErrorAction Stop
    } finally {
        if ($created) { [IO.File]::Delete($titlePath) }
    }
}
