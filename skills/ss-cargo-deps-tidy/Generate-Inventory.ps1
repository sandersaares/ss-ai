#Requires -Version 7
<#
.SYNOPSIS
    Catalogs Cargo dependency declarations across a Cargo workspace.

.DESCRIPTION
    Reads a workspace-level Cargo.toml, discovers all member packages by expanding
    the [workspace] members / exclude declarations, scans every manifest for
    dependency declarations, and emits a JSON inventory describing each declaration
    along with its style, attributes, target cfg, and same-workspace status.

    The inventory is consumed by the ss-cargo-deps-tidy skill.

.PARAMETER WorkspaceManifest
    Path to the workspace-level Cargo.toml. Defaults to ./Cargo.toml in the
    current working directory.

.PARAMETER OutputPath
    Optional path to write the JSON output to. If omitted, JSON is written to
    the success stream.
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$WorkspaceManifest = './Cargo.toml',

    [Parameter()]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ============================================================================
# Design notes
# ============================================================================
#
# - Hand-rolled TOML scanner, no external library. PowerShell ships no native
#   TOML parser, and pulling in PSToml / Tomlyn would force every consumer to
#   install extra modules before running the skill. We only need the
#   dependency-declaration subset of TOML, so a focused line-based scanner
#   keeps the script dependency-free at the cost of a few hundred lines.
#
# - Preserve declaration style, attribute order, and source line numbers.
#   The inventory feeds a tidy/refactor workflow that rewrites manifests in
#   place. Downstream code needs the original style ('bare-string',
#   'inline-table', 'sub-table', 'dotted-key') and source line to make
#   minimal, targeted edits without reformatting the whole file.
#
# - Permissive parsing, matching Cargo's `toml_edit`. Cargo accepts
#   multi-line inline tables despite strict TOML 1.0 forbidding them, so the
#   scanner accepts them too. Strict TOML conformance would reject
#   real-world manifests we need to handle.
#
# - JSON output, UTF-8 without BOM. Stable cross-process contract; a BOM
#   trips up some downstream JSON consumers.
#
# - $null (not '') for absent fields. The JSON contract distinguishes
#   "field not present" from "field is empty", so optional string params on
#   record builders are typed [object] rather than [string] (which would
#   silently coerce $null to '').
#
# - Dot-source-safe. Helpers can be loaded without running the main flow via
#   a guarded main block at the bottom of the file. The Pester suite relies
#   on this to unit-test helpers directly.
#
# - Case-insensitive, normalized path keys for same-workspace resolution.
#   Windows file systems are case-insensitive and member globs can produce
#   paths with mixed casing or trailing separators. Raw string comparison
#   would miss matches; Get-PathKey normalizes before comparing.

# ============================================================================
# Path helpers
# ============================================================================

function ConvertTo-WorkspacePath {
    param(
        [Parameter(Mandatory)] [string]$AbsolutePath,
        [Parameter(Mandatory)] [string]$WorkspaceRoot
    )
    $rel = [System.IO.Path]::GetRelativePath($WorkspaceRoot, $AbsolutePath)
    return ($rel -replace '\\', '/')
}

function Get-PathKey {
    # Case-folding is filesystem-dependent. Windows and macOS default volumes
    # are case-insensitive; Linux is case-sensitive. Lowercasing on a
    # case-sensitive FS would collapse genuinely distinct directories.
    param([Parameter(Mandatory)] [string]$Path)
    $normalized = ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\', '/')
    if ($IsWindows -or $IsMacOS) {
        return $normalized.ToLowerInvariant()
    }
    return $normalized
}

# ============================================================================
# Low-level TOML helpers
# ============================================================================

function Remove-TomlLineComment {
    param([Parameter(Mandatory)] [AllowEmptyString()] [string]$Line)
    $inSingle = $false
    $inDouble = $false
    for ($i = 0; $i -lt $Line.Length; $i++) {
        $c = $Line[$i]
        if ($inSingle) {
            if ($c -eq "'") { $inSingle = $false }
        }
        elseif ($inDouble) {
            if ($c -eq '\') { $i++ }
            elseif ($c -eq '"') { $inDouble = $false }
        }
        else {
            if ($c -eq "'") { $inSingle = $true }
            elseif ($c -eq '"') { $inDouble = $true }
            elseif ($c -eq '#') { return $Line.Substring(0, $i).TrimEnd() }
        }
    }
    return $Line.TrimEnd()
}

function Remove-Quotes {
    param([Parameter(Mandatory)] [string]$Text)
    if ($Text.Length -ge 2) {
        $first = $Text[0]
        $last = $Text[$Text.Length - 1]
        if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
            return $Text.Substring(1, $Text.Length - 2)
        }
    }
    return $Text
}

function Split-TomlKeyPath {
    # Splits a dotted TOML key path like  workspace.dependencies.foo  or
    # target.'cfg(windows)'.dependencies  into its tokens, respecting quoted
    # segments so that dots inside quotes are preserved.
    param([Parameter(Mandatory)] [string]$Header)
    $tokens = New-Object System.Collections.Generic.List[string]
    $sb = New-Object System.Text.StringBuilder
    $inSingle = $false
    $inDouble = $false
    for ($i = 0; $i -lt $Header.Length; $i++) {
        $c = $Header[$i]
        if ($inSingle) {
            if ($c -eq "'") { $inSingle = $false } else { [void]$sb.Append($c) }
        }
        elseif ($inDouble) {
            if ($c -eq '\') {
                $i++
                if ($i -lt $Header.Length) { [void]$sb.Append($Header[$i]) }
            }
            elseif ($c -eq '"') { $inDouble = $false }
            else { [void]$sb.Append($c) }
        }
        else {
            if ($c -eq "'") { $inSingle = $true }
            elseif ($c -eq '"') { $inDouble = $true }
            elseif ($c -eq '.') { $tokens.Add($sb.ToString().Trim()); [void]$sb.Clear() }
            else { [void]$sb.Append($c) }
        }
    }
    $tokens.Add($sb.ToString().Trim())
    return , $tokens.ToArray()
}

function Get-SectionInfo {
    # Classifies a TOML section header inner text.
    param([Parameter(Mandatory)] [string]$HeaderText)
    $tokens = Split-TomlKeyPath -Header $HeaderText
    $n = $tokens.Length

    if ($n -eq 1) {
        switch ($tokens[0]) {
            'package'   { return @{ Kind = 'package' } }
            'workspace' { return @{ Kind = 'workspace' } }
            'dependencies'     { return @{ Kind = 'deps'; Variant = 'normal'; Target = $null; SubName = $null } }
            'dev-dependencies' { return @{ Kind = 'deps'; Variant = 'dev';    Target = $null; SubName = $null } }
            default { return @{ Kind = 'other' } }
        }
    }
    if ($n -eq 2 -and $tokens[0] -eq 'workspace' -and $tokens[1] -eq 'dependencies') {
        return @{ Kind = 'deps'; Variant = 'workspace'; Target = $null; SubName = $null }
    }
    if ($n -eq 2 -and $tokens[0] -in @('dependencies', 'dev-dependencies')) {
        $variant = if ($tokens[0] -eq 'dev-dependencies') { 'dev' } else { 'normal' }
        return @{ Kind = 'deps'; Variant = $variant; Target = $null; SubName = $tokens[1] }
    }
    if ($n -eq 3 -and $tokens[0] -eq 'workspace' -and $tokens[1] -eq 'dependencies') {
        return @{ Kind = 'deps'; Variant = 'workspace'; Target = $null; SubName = $tokens[2] }
    }
    if ($n -ge 3 -and $tokens[0] -eq 'target') {
        $target = $tokens[1]
        if ($n -eq 3 -and $tokens[2] -in @('dependencies', 'dev-dependencies')) {
            $variant = if ($tokens[2] -eq 'dev-dependencies') { 'dev' } else { 'normal' }
            return @{ Kind = 'deps'; Variant = $variant; Target = $target; SubName = $null }
        }
        if ($n -eq 4 -and $tokens[2] -in @('dependencies', 'dev-dependencies')) {
            $variant = if ($tokens[2] -eq 'dev-dependencies') { 'dev' } else { 'normal' }
            return @{ Kind = 'deps'; Variant = $variant; Target = $target; SubName = $tokens[3] }
        }
    }
    return @{ Kind = 'other' }
}

function Read-ValueSpan {
    # Reads a TOML value starting at CleanLines[$StartIndex][$StartCol], possibly
    # spanning multiple lines for inline tables and arrays. Returns the trimmed
    # text span and the line index where the value ends.
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]]$CleanLines,
        [Parameter(Mandatory)] [int]$StartIndex,
        [Parameter(Mandatory)] [int]$StartCol
    )

    $sb = New-Object System.Text.StringBuilder
    $depth = 0
    $inSingle = $false
    $inDouble = $false
    $sawNonWs = $false

    for ($i = $StartIndex; $i -lt $CleanLines.Length; $i++) {
        $line = $CleanLines[$i]
        $col = if ($i -eq $StartIndex) { $StartCol } else { 0 }
        if ($i -gt $StartIndex) { [void]$sb.Append("`n") }
        while ($col -lt $line.Length) {
            $c = $line[$col]
            [void]$sb.Append($c)
            if ($inSingle) {
                if ($c -eq "'") { $inSingle = $false }
            }
            elseif ($inDouble) {
                if ($c -eq '\') {
                    $col++
                    if ($col -lt $line.Length) { [void]$sb.Append($line[$col]) }
                }
                elseif ($c -eq '"') { $inDouble = $false }
            }
            else {
                if ($c -eq "'") { $inSingle = $true; $sawNonWs = $true }
                elseif ($c -eq '"') { $inDouble = $true; $sawNonWs = $true }
                elseif ($c -eq '{' -or $c -eq '[') { $depth++; $sawNonWs = $true }
                elseif ($c -eq '}' -or $c -eq ']') { $depth-- }
                elseif ($c -ne ' ' -and $c -ne "`t") { $sawNonWs = $true }
            }
            $col++
        }
        if ($sawNonWs -and $depth -le 0 -and -not $inSingle -and -not $inDouble) {
            return @{ Text = $sb.ToString().Trim(); EndIndex = $i }
        }
    }
    return @{ Text = $sb.ToString().Trim(); EndIndex = $CleanLines.Length - 1 }
}

function Split-TopLevelEntries {
    # Splits a comma-separated list at top-level depth, respecting strings,
    # arrays, and inline tables.
    param([Parameter(Mandatory)] [AllowEmptyString()] [string]$Text)
    $entries = New-Object System.Collections.Generic.List[string]
    $sb = New-Object System.Text.StringBuilder
    $depth = 0
    $inSingle = $false
    $inDouble = $false
    for ($i = 0; $i -lt $Text.Length; $i++) {
        $c = $Text[$i]
        if ($inSingle) {
            if ($c -eq "'") { $inSingle = $false }
            [void]$sb.Append($c)
        }
        elseif ($inDouble) {
            if ($c -eq '\') {
                [void]$sb.Append($c)
                $i++
                if ($i -lt $Text.Length) { [void]$sb.Append($Text[$i]) }
            }
            elseif ($c -eq '"') {
                $inDouble = $false
                [void]$sb.Append($c)
            }
            else { [void]$sb.Append($c) }
        }
        elseif ($c -eq "'") { $inSingle = $true; [void]$sb.Append($c) }
        elseif ($c -eq '"') { $inDouble = $true; [void]$sb.Append($c) }
        elseif ($c -eq '{' -or $c -eq '[') { $depth++; [void]$sb.Append($c) }
        elseif ($c -eq '}' -or $c -eq ']') { $depth--; [void]$sb.Append($c) }
        elseif ($c -eq ',' -and $depth -eq 0) {
            $entries.Add($sb.ToString())
            [void]$sb.Clear()
        }
        else { [void]$sb.Append($c) }
    }
    if ($sb.Length -gt 0) { $entries.Add($sb.ToString()) }
    return , $entries.ToArray()
}

function ConvertFrom-TomlValue {
    # Parses a TOML value text into a PowerShell scalar / array. We only support
    # the value forms that appear in dependency declarations: strings, booleans,
    # and arrays of strings.
    param([Parameter(Mandatory)] [AllowEmptyString()] [string]$Text)
    $t = $Text.Trim()
    if ([string]::IsNullOrEmpty($t)) { return $null }
    if ($t -eq 'true')  { return $true }
    if ($t -eq 'false') { return $false }
    if ($t.StartsWith('"') -and $t.EndsWith('"')) {
        $inner = $t.Substring(1, $t.Length - 2)
        return ($inner -replace '\\"', '"' -replace '\\\\', '\')
    }
    if ($t.StartsWith("'") -and $t.EndsWith("'")) {
        return $t.Substring(1, $t.Length - 2)
    }
    if ($t.StartsWith('[')) {
        $inner = $t.Substring(1)
        if ($inner.EndsWith(']')) { $inner = $inner.Substring(0, $inner.Length - 1) }
        $parts = Split-TopLevelEntries -Text $inner
        $arr = New-Object System.Collections.Generic.List[object]
        foreach ($p in $parts) {
            $pt = $p.Trim()
            if ($pt) { $arr.Add((ConvertFrom-TomlValue -Text $pt)) }
        }
        return , $arr.ToArray()
    }
    return $t
}

function ConvertFrom-InlineTable {
    # Parses an inline-table text  { k = v, k = v }  into a hashtable of
    # PowerShell values.
    param([Parameter(Mandatory)] [string]$Text)
    $t = $Text.Trim()
    if ($t.StartsWith('{')) { $t = $t.Substring(1) }
    if ($t.EndsWith('}'))   { $t = $t.Substring(0, $t.Length - 1) }
    $entries = Split-TopLevelEntries -Text $t
    $result = @{}
    foreach ($entry in $entries) {
        $e = $entry.Trim()
        if (-not $e) { continue }
        $eqIdx = -1
        $inSingle = $false; $inDouble = $false
        for ($i = 0; $i -lt $e.Length; $i++) {
            $c = $e[$i]
            if ($inSingle) { if ($c -eq "'") { $inSingle = $false } }
            elseif ($inDouble) {
                if ($c -eq '\') { $i++ }
                elseif ($c -eq '"') { $inDouble = $false }
            }
            else {
                if ($c -eq "'") { $inSingle = $true }
                elseif ($c -eq '"') { $inDouble = $true }
                elseif ($c -eq '=') { $eqIdx = $i; break }
            }
        }
        if ($eqIdx -lt 0) { continue }
        $key = (Remove-Quotes $e.Substring(0, $eqIdx).Trim())
        $val = $e.Substring($eqIdx + 1).Trim()
        $result[$key] = (ConvertFrom-TomlValue -Text $val)
    }
    return $result
}

# ============================================================================
# Manifest scanner
# ============================================================================

function New-DepRecord {
    param(
        [Parameter(Mandatory)] [string]$ManifestPath,
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [string]$Style,
        [Parameter(Mandatory)] [string]$Variant,
        [AllowNull()] [object]$Target,
        [Parameter(Mandatory)] [int]$Line,
        [AllowNull()] [hashtable]$Attrs,
        [Parameter(Mandatory)] [AllowEmptyString()] [string]$Raw
    )
    return [ordered]@{
        AbsoluteManifestPath = $ManifestPath
        Name    = $Name
        Style   = $Style
        Variant = $Variant
        Target  = $Target
        Line    = $Line
        Attrs   = if ($null -ne $Attrs) { $Attrs } else { @{} }
        Raw     = $Raw
    }
}

function Save-PendingDepRecord {
    param($Pending, $Target)
    if ($null -ne $Pending) {
        [void]$Target.Add($Pending)
    }
}

function Read-CargoManifest {
    param(
        [Parameter(Mandatory)] [string]$ManifestPath,
        [Parameter(Mandatory)] [string]$WorkspaceRoot
    )

    $rawText = [System.IO.File]::ReadAllText($ManifestPath)
    $rawLines = $rawText -split "`r?`n"
    $cleanLines = @($rawLines | ForEach-Object { Remove-TomlLineComment -Line $_ })

    $result = [ordered]@{
        AbsolutePath       = $ManifestPath
        RelativePath       = (ConvertTo-WorkspacePath -AbsolutePath $ManifestPath -WorkspaceRoot $WorkspaceRoot)
        Package            = $null
        HasWorkspaceTable  = $false
        HasPackageTable    = $false
        Members            = @()
        Exclude            = @()
        Dependencies       = New-Object System.Collections.Generic.List[object]
    }

    $section = @{ Kind = 'other' }
    $pendingEntry = $null   # Used for dotted-key merging and sub-table accumulation.

    $i = 0
    while ($i -lt $cleanLines.Count) {
        $line = $cleanLines[$i]
        $trim = $line.Trim()

        # Skip blank lines and pure comment lines (already stripped, so just blank).
        if ($trim -eq '') {
            $i++
            continue
        }

        # Section header? Treat [[array.of.tables]] as a section break we ignore.
        if ($trim.StartsWith('[')) {
            Save-PendingDepRecord -Pending $pendingEntry -Target $result.Dependencies
            $pendingEntry = $null

            if ($trim.StartsWith('[[')) {
                $section = @{ Kind = 'other' }
                $i++
                continue
            }

            $closeIdx = $trim.IndexOf(']')
            if ($closeIdx -lt 0) {
                $i++
                continue
            }
            $inner = $trim.Substring(1, $closeIdx - 1).Trim()
            $section = Get-SectionInfo -HeaderText $inner

            if ($section.Kind -eq 'workspace') { $result.HasWorkspaceTable = $true }
            if ($section.Kind -eq 'package')   { $result.HasPackageTable   = $true }

            # If this is a sub-table dep section, prime a pending sub-table record.
            if ($section.Kind -eq 'deps' -and $section.SubName) {
                $pendingEntry = New-DepRecord `
                    -ManifestPath $ManifestPath `
                    -Name $section.SubName `
                    -Style 'sub-table' `
                    -Variant $section.Variant `
                    -Target $section.Target `
                    -Line ($i + 1) `
                    -Attrs (@{}) `
                    -Raw "[$inner]"
            }

            $i++
            continue
        }

        switch ($section.Kind) {

            'package' {
                if ($trim -match '^\s*name\s*=\s*"(?<v>[^"]+)"\s*$') {
                    $result.Package = $Matches['v']
                }
                elseif ($trim -match "^\s*name\s*=\s*'(?<v>[^']+)'\s*$") {
                    $result.Package = $Matches['v']
                }
                $i++
            }

            'workspace' {
                if ($trim -match '^(?<key>members|exclude)\s*=\s*\[') {
                    $key = $Matches['key']
                    $eqIdx = $line.IndexOf('=')
                    $span = Read-ValueSpan -CleanLines $cleanLines -StartIndex $i -StartCol ($eqIdx + 1)
                    $arr = ConvertFrom-TomlValue -Text $span.Text
                    if ($null -eq $arr) { $arr = @() }
                    if ($key -eq 'members') { $result.Members = @($arr) }
                    else { $result.Exclude = @($arr) }
                    $i = $span.EndIndex + 1
                }
                else {
                    $i++
                }
            }

            'deps' {
                if ($section.SubName) {
                    # Inside a sub-table dep section: each line is "key = value"
                    # applied to the pending sub-table record.
                    if ($trim -match '^(?<key>[A-Za-z0-9_\-]+)\s*=\s*') {
                        $key = $Matches['key']
                        $eqIdx = $line.IndexOf('=')
                        $span = Read-ValueSpan -CleanLines $cleanLines -StartIndex $i -StartCol ($eqIdx + 1)
                        $val = ConvertFrom-TomlValue -Text $span.Text
                        $pendingEntry.Attrs[$key] = $val
                        # Extend raw to cover the full source span (key + value),
                        # including any continuation lines for multi-line values.
                        $spanLines = for ($r = $i; $r -le $span.EndIndex; $r++) { $cleanLines[$r] }
                        $pendingEntry.Raw = "$($pendingEntry.Raw)`n$(($spanLines -join [Environment]::NewLine).TrimEnd())"
                        $i = $span.EndIndex + 1
                    }
                    else {
                        $i++
                    }
                }
                else {
                    if ($trim -match '^(?<key>[A-Za-z0-9_\-]+|"[^"]+"|''[^'']+'')(?:\.(?<attr>[A-Za-z0-9_\-]+))?\s*=\s*(?<rest>.*)$') {
                        $rawKey = $Matches['key']
                        $depName = Remove-Quotes $rawKey
                        $attr = $Matches['attr']
                        $eqIdx = $line.IndexOf('=')
                        $valStartCol = $eqIdx + 1
                        $span = Read-ValueSpan -CleanLines $cleanLines -StartIndex $i -StartCol $valStartCol
                        $valueText = $span.Text

                        if ($attr) {
                            # Dotted-key entry. Merge with the pending dotted-key
                            # record if it's for the same dep; otherwise flush
                            # and start a new pending record.
                            $parsedVal = ConvertFrom-TomlValue -Text $valueText
                            if ($null -ne $pendingEntry -and
                                $pendingEntry.Style -eq 'dotted-key' -and
                                $pendingEntry.Name -eq $depName -and
                                $pendingEntry.Variant -eq $section.Variant -and
                                $pendingEntry.Target -eq $section.Target) {
                                $pendingEntry.Attrs[$attr] = $parsedVal
                                $pendingEntry.Raw = "$($pendingEntry.Raw)`n$($line.Trim())"
                            }
                            else {
                                Save-PendingDepRecord -Pending $pendingEntry -Target $result.Dependencies
                                $pendingEntry = New-DepRecord `
                                    -ManifestPath $ManifestPath `
                                    -Name $depName `
                                    -Style 'dotted-key' `
                                    -Variant $section.Variant `
                                    -Target $section.Target `
                                    -Line ($i + 1) `
                                    -Attrs (@{ $attr = $parsedVal }) `
                                    -Raw $line.Trim()
                            }
                        }
                        elseif ($valueText.StartsWith('{')) {
                            Save-PendingDepRecord -Pending $pendingEntry -Target $result.Dependencies
                            $pendingEntry = $null
                            $attrs = ConvertFrom-InlineTable -Text $valueText
                            $rec = New-DepRecord `
                                -ManifestPath $ManifestPath `
                                -Name $depName `
                                -Style 'inline-table' `
                                -Variant $section.Variant `
                                -Target $section.Target `
                                -Line ($i + 1) `
                                -Attrs $attrs `
                                -Raw $valueText
                            [void]$result.Dependencies.Add($rec)
                        }
                        else {
                            Save-PendingDepRecord -Pending $pendingEntry -Target $result.Dependencies
                            $pendingEntry = $null
                            $version = ConvertFrom-TomlValue -Text $valueText
                            $rec = New-DepRecord `
                                -ManifestPath $ManifestPath `
                                -Name $depName `
                                -Style 'bare-string' `
                                -Variant $section.Variant `
                                -Target $section.Target `
                                -Line ($i + 1) `
                                -Attrs (@{ version = $version }) `
                                -Raw $valueText
                            [void]$result.Dependencies.Add($rec)
                        }
                        $i = $span.EndIndex + 1
                    }
                    else {
                        $i++
                    }
                }
            }

            default {
                $i++
            }
        }
    }

    Save-PendingDepRecord -Pending $pendingEntry -Target $result.Dependencies
    $pendingEntry = $null

    return $result
}

# ============================================================================
# Member glob expansion
# ============================================================================

function Expand-MemberGlob {
    # Expands a Cargo workspace member glob pattern relative to $Root, returning
    # absolute directory paths. Supports  *  ?  and  ** .
    param(
        [Parameter(Mandatory)] [string]$Root,
        [Parameter(Mandatory)] [string]$Pattern
    )

    $segments = $Pattern -split '[/\\]' | Where-Object { $_ -ne '' }
    return , (Expand-GlobSegments -BaseDir $Root -Segments $segments -Index 0)
}

function Expand-GlobSegments {
    param(
        [Parameter(Mandatory)] [string]$BaseDir,
        [Parameter(Mandatory)] [AllowEmptyCollection()] [AllowEmptyString()] [string[]]$Segments,
        [Parameter(Mandatory)] [int]$Index
    )

    if ($Index -ge $Segments.Length) {
        if (Test-Path -LiteralPath $BaseDir -PathType Container) {
            return , @($BaseDir)
        }
        return , @()
    }

    $seg = $Segments[$Index]

    if ($seg -eq '**') {
        $results = New-Object System.Collections.Generic.List[string]
        # Match zero segments: skip the **.
        $rest = Expand-GlobSegments -BaseDir $BaseDir -Segments $Segments -Index ($Index + 1)
        foreach ($r in $rest) { $results.Add($r) }
        # Match one or more segments: recurse into each child directory, keeping **.
        try {
            $children = Get-ChildItem -LiteralPath $BaseDir -Directory -ErrorAction Stop
        }
        catch {
            return , @()
        }
        foreach ($child in $children) {
            $more = Expand-GlobSegments -BaseDir $child.FullName -Segments $Segments -Index $Index
            foreach ($m in $more) { $results.Add($m) }
        }
        return , $results.ToArray()
    }

    if ($seg -match '[*?]') {
        try {
            $children = Get-ChildItem -LiteralPath $BaseDir -Directory -ErrorAction Stop
        }
        catch {
            return , @()
        }
        $results = New-Object System.Collections.Generic.List[string]
        foreach ($child in $children) {
            if ($child.Name -like $seg) {
                $more = Expand-GlobSegments -BaseDir $child.FullName -Segments $Segments -Index ($Index + 1)
                foreach ($m in $more) { $results.Add($m) }
            }
        }
        return , $results.ToArray()
    }

    $next = Join-Path $BaseDir $seg
    if (Test-Path -LiteralPath $next -PathType Container) {
        return , (Expand-GlobSegments -BaseDir $next -Segments $Segments -Index ($Index + 1))
    }
    return , @()
}

# ============================================================================
# Main
# ============================================================================

# Allow tests to dot-source this file purely to obtain the helper functions,
# without executing the main scanning logic.
if ($MyInvocation.InvocationName -eq '.') { return }

if (-not (Test-Path -LiteralPath $WorkspaceManifest -PathType Leaf)) {
    throw "Workspace manifest not found: $WorkspaceManifest"
}

$absManifest = (Resolve-Path -LiteralPath $WorkspaceManifest).ProviderPath
$workspaceRoot = [System.IO.Path]::GetDirectoryName($absManifest)

$rootScan = Read-CargoManifest -ManifestPath $absManifest -WorkspaceRoot $workspaceRoot

# Build the member manifest set.
$memberSet = [ordered]@{}   # key: directory key, value: ordered hashtable describing the manifest

# Normalize exclude entries to absolute directory keys.
$excludeKeys = New-Object System.Collections.Generic.HashSet[string]
foreach ($ex in $rootScan.Exclude) {
    if (-not $ex) { continue }
    $exAbs = [System.IO.Path]::GetFullPath((Join-Path $workspaceRoot $ex))
    [void]$excludeKeys.Add((Get-PathKey -Path $exAbs))
}

# If the workspace root has its own [package], it is an implicit member.
if ($rootScan.HasPackageTable) {
    $rootDirKey = Get-PathKey -Path $workspaceRoot
    if (-not $excludeKeys.Contains($rootDirKey)) {
        $memberSet[$rootDirKey] = [ordered]@{
            AbsolutePath = $absManifest
            RelativePath = $rootScan.RelativePath
            Package      = $rootScan.Package
            Scan         = $rootScan
        }
    }
}

# Expand each member glob.
foreach ($glob in $rootScan.Members) {
    if (-not $glob) { continue }
    $dirs = Expand-MemberGlob -Root $workspaceRoot -Pattern $glob
    if (-not $dirs -or $dirs.Count -eq 0) {
        Write-Warning "Workspace member pattern '$glob' did not match any directory."
        continue
    }
    foreach ($dir in $dirs) {
        $dirKey = Get-PathKey -Path $dir
        if ($excludeKeys.Contains($dirKey)) { continue }
        if ($memberSet.Contains($dirKey)) { continue }

        $memberManifest = Join-Path $dir 'Cargo.toml'
        if (-not (Test-Path -LiteralPath $memberManifest -PathType Leaf)) {
            Write-Warning "Member directory '$dir' has no Cargo.toml; skipping."
            continue
        }

        $scan = Read-CargoManifest -ManifestPath $memberManifest -WorkspaceRoot $workspaceRoot
        $memberSet[$dirKey] = [ordered]@{
            AbsolutePath = $memberManifest
            RelativePath = $scan.RelativePath
            Package      = $scan.Package
            Scan         = $scan
        }
    }
}

# If this is a single-package manifest (no [workspace] section) and the root
# itself isn't already in the set (which happens when there's no [package]
# either - degenerate empty manifest), add it as a degenerate single member so
# the user still gets some output back.
if (-not $rootScan.HasWorkspaceTable -and $memberSet.Count -eq 0) {
    $rootDirKey = Get-PathKey -Path $workspaceRoot
    $memberSet[$rootDirKey] = [ordered]@{
        AbsolutePath = $absManifest
        RelativePath = $rootScan.RelativePath
        Package      = $rootScan.Package
        Scan         = $rootScan
    }
}

# Build manifest-lookup index for same-workspace resolution.
$manifestByDir = @{}
foreach ($entry in $memberSet.Values) {
    $dirKey = Get-PathKey -Path ([System.IO.Path]::GetDirectoryName($entry.AbsolutePath))
    $manifestByDir[$dirKey] = $entry.RelativePath
}
# The workspace root manifest is always discoverable, even if not a member.
$rootKey = Get-PathKey -Path $workspaceRoot
if (-not $manifestByDir.ContainsKey($rootKey)) {
    $manifestByDir[$rootKey] = $rootScan.RelativePath
}

# Aggregate all dependency records (from the workspace root + every member).
$allScans = New-Object System.Collections.Generic.List[object]
$allScans.Add($rootScan)
foreach ($entry in $memberSet.Values) {
    if ($entry.Scan.AbsolutePath -ne $rootScan.AbsolutePath) {
        $allScans.Add($entry.Scan)
    }
}

# Index member manifests by [package] name. Cargo resolves a dep whose name
# matches a workspace member's package name to that member (the local source
# wins over any same-named registry crate), so the inventory must mark such
# deps as same-workspace even when no explicit path is given.
$manifestByPackageName = @{}
foreach ($entry in $memberSet.Values) {
    if ($entry.Package) {
        $manifestByPackageName[$entry.Package] = $entry.RelativePath
    }
}

$outputDeps = New-Object System.Collections.Generic.List[object]
foreach ($scan in $allScans) {
    $manifestRel = $scan.RelativePath
    $manifestPkg = $scan.Package
    $manifestDir = [System.IO.Path]::GetDirectoryName($scan.AbsolutePath)
    foreach ($d in $scan.Dependencies) {
        $section = switch ($d.Variant) {
            'workspace' { 'workspace.dependencies' }
            'dev'       { 'dev-dependencies' }
            default     { 'dependencies' }
        }

        # Pull out the five tracked attributes (null if absent).
        $attrs = $d.Attrs
        $version         = if ($attrs.ContainsKey('version'))          { $attrs['version'] }          else { $null }
        $pathAttr        = if ($attrs.ContainsKey('path'))             { $attrs['path'] }             else { $null }
        $workspaceAttr   = if ($attrs.ContainsKey('workspace'))        { $attrs['workspace'] }        else { $null }
        $defaultFeatures = if ($attrs.ContainsKey('default-features')) { $attrs['default-features'] } else { $null }
        $features        = if ($attrs.ContainsKey('features'))         { $attrs['features'] }         else { $null }

        # Pass 1 of same-workspace resolution: explicit path attribute.
        # Two follow-up passes (name match, workspace inheritance) run after
        # all records have been collected.
        $sameWorkspace = $false
        $resolvedManifest = $null
        if ($pathAttr) {
            try {
                $resolved = [System.IO.Path]::GetFullPath((Join-Path $manifestDir $pathAttr))
                $resolvedKey = Get-PathKey -Path $resolved
                if ($manifestByDir.ContainsKey($resolvedKey)) {
                    $sameWorkspace = $true
                    $resolvedManifest = $manifestByDir[$resolvedKey]
                }
            }
            catch {
                # Bad path; leave sameWorkspace=false.
            }
        }

        $outputDeps.Add([ordered]@{
            manifest         = $manifestRel
            package          = $manifestPkg
            section          = $section
            target           = $d.Target
            name             = $d.Name
            style            = $d.Style
            version          = $version
            path             = $pathAttr
            workspace        = $workspaceAttr
            defaultFeatures  = $defaultFeatures
            features         = $features
            sameWorkspace    = $sameWorkspace
            resolvedManifest = $resolvedManifest
            raw              = $d.Raw
            line             = $d.Line
        })
    }
}

# Pass 2: name-match fallback. A dep whose name matches a workspace member's
# package name is same-workspace regardless of whether a path was given —
# Cargo's resolver picks the local member over any registry crate of the
# same name in path/inheritance contexts.
foreach ($rec in $outputDeps) {
    if ($rec.sameWorkspace) { continue }
    if ($manifestByPackageName.ContainsKey($rec.name)) {
        $rec.sameWorkspace = $true
        $rec.resolvedManifest = $manifestByPackageName[$rec.name]
    }
}

# Pass 3: workspace = true inheritance. A member-level dep with workspace=true
# inherits its resolution from the matching workspace.dependencies entry,
# which by now has been resolved via path or name match. Without this, a
# member that aliases a same-workspace dep through workspace inheritance
# (e.g. workspace.dependencies inside = { path = "packages/host" }, member
# dep inside = { workspace = true }) would be reported as out-of-workspace.
$workspaceDepsByName = @{}
foreach ($rec in $outputDeps) {
    if ($rec.section -eq 'workspace.dependencies') {
        $workspaceDepsByName[$rec.name] = $rec
    }
}
foreach ($rec in $outputDeps) {
    if ($rec.sameWorkspace) { continue }
    if ($rec.workspace -eq $true -and $workspaceDepsByName.ContainsKey($rec.name)) {
        $wsEntry = $workspaceDepsByName[$rec.name]
        if ($wsEntry.sameWorkspace) {
            $rec.sameWorkspace = $true
            $rec.resolvedManifest = $wsEntry.resolvedManifest
        }
    }
}

# Build the JSON envelope.
$memberManifestList = New-Object System.Collections.Generic.List[object]
foreach ($entry in $memberSet.Values) {
    $memberManifestList.Add([ordered]@{
        path    = $entry.RelativePath
        package = $entry.Package
    })
}

$envelope = [ordered]@{
    workspaceRoot     = ($workspaceRoot -replace '\\', '/')
    workspaceManifest = [ordered]@{
        path               = $rootScan.RelativePath
        package            = $rootScan.Package
        isVirtualWorkspace = (-not $rootScan.HasPackageTable) -and $rootScan.HasWorkspaceTable
    }
    memberManifests   = $memberManifestList
    scannedAt         = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ")
    dependencies      = $outputDeps
}

$json = $envelope | ConvertTo-Json -Depth 12

if ($OutputPath) {
    $outDir = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($OutputPath))
    if ($outDir -and -not (Test-Path -LiteralPath $outDir)) {
        [void](New-Item -ItemType Directory -Path $outDir -Force)
    }
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
}
else {
    Write-Output $json
}
