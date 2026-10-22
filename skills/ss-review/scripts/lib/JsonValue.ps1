#Requires -Version 7.0

# Canonical JSON value handling for pull request capture (docs/implementation.md, "Verbatim
# fidelity").
#
# Every other library in this skill reads platform payloads through here rather than through
# ConvertFrom-Json, because ConvertFrom-Json rewrites any date-shaped *string value* into a DateTime
# and then back into locale-shaped text. A review comment whose body is an ISO-8601 timestamp would
# be silently corrupted by that, which is precisely the class of loss this capture exists to
# eliminate. System.Text.Json returns strings exactly as the service sent them.
#
# The exactness promise covers strings, which is where the capture's fidelity requirement lies: the
# comment bodies, identifiers and timestamps a discussion is reconstructed from all arrive as
# strings. Numbers materialize as Int64 where the value fits and as Double otherwise, so a numeric
# token beyond Int64 range or Double precision is not reproduced digit for digit. Preserving those
# would mean carrying the raw token alongside the value, which no reader of this capture needs.
#
# Set-StrictMode also turns a missing key into a terminating error, and platform payloads omit
# properties freely rather than sending nulls, so reads go through an existence-checking accessor
# instead of direct member access.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Materializes one System.Text.Json element into ordered dictionaries, arrays and scalars.
#
# Ordered dictionaries preserve document order so that a captured document re-serializes stably and
# diffs between two captures show only genuine change.
function ConvertFrom-JsonElement {
    param(
        [Parameter(Mandatory)]
        [System.Text.Json.JsonElement] $Element
    )

    switch ($Element.ValueKind) {
        ([System.Text.Json.JsonValueKind]::Object) {
            $result = [ordered]@{}
            foreach ($property in $Element.EnumerateObject()) {
                $result[$property.Name] = ConvertFrom-JsonElement -Element $property.Value
            }
            return $result
        }
        ([System.Text.Json.JsonValueKind]::Array) {
            $items = [System.Collections.Generic.List[object]]::new()
            foreach ($item in $Element.EnumerateArray()) {
                $items.Add((ConvertFrom-JsonElement -Element $item))
            }
            # Comma-wrapped so that an empty or single-element array survives the pipeline as an
            # array rather than collapsing to $null or to a bare scalar.
            return , $items.ToArray()
        }
        ([System.Text.Json.JsonValueKind]::String) {
            return $Element.GetString()
        }
        ([System.Text.Json.JsonValueKind]::Number) {
            # Int64 is attempted first because every number the capture reads -- identifiers,
            # counts, line numbers, revision ordinals -- is an integer within that range, and Int64
            # holds those exactly. Anything else falls back to Double, the widest scalar reachable
            # without carrying the original token, which is why the fidelity promise is stated for
            # strings rather than for numbers.
            $integer = [long] 0
            if ($Element.TryGetInt64([ref] $integer)) {
                return $integer
            }
            return $Element.GetDouble()
        }
        ([System.Text.Json.JsonValueKind]::True) { return $true }
        ([System.Text.Json.JsonValueKind]::False) { return $false }
        default { return $null }
    }
}

# Parses JSON text, reproducing every string value exactly as it appears in the text.
#
# Numbers materialize as Int64 where the value fits and as Double otherwise; booleans and null
# materialize as their PowerShell equivalents.
function ConvertFrom-JsonVerbatim {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text
    )

    $document = [System.Text.Json.JsonDocument]::Parse($Text)
    try {
        $value = ConvertFrom-JsonElement -Element $document.RootElement
        if ($value -is [object[]]) {
            return , $value
        }
        return $value
    } finally {
        $document.Dispose()
    }
}

# Reports whether a materialized node carries a non-null value under the given name.
function Test-JsonProperty {
    param(
        $Node,

        [Parameter(Mandatory)]
        [string] $Name
    )

    # Coercion rather than a type test, because anything that has been through the pipeline arrives
    # wrapped in a PSObject, and indexing that wrapper by name binds to the integer indexer of
    # OrderedDictionary instead of the key indexer.
    $dictionary = $Node -as [System.Collections.IDictionary]
    if ($null -eq $dictionary) {
        return $false
    }
    if (-not $dictionary.Contains($Name)) {
        return $false
    }
    return $null -ne $dictionary[$Name]
}

# Reads a property, treating absent and JSON null identically.
#
# The platforms are inconsistent about which they send for the same missing concept -- Azure DevOps
# omits commentType on system comments while GitHub sends line as null on outdated threads -- and no
# caller has ever needed to tell the two apart.
function Get-JsonProperty {
    param(
        $Node,

        [Parameter(Mandatory)]
        [string] $Name,

        $Default = $null
    )

    if (-not (Test-JsonProperty -Node $Node -Name $Name)) {
        return $Default
    }

    $value = ($Node -as [System.Collections.IDictionary])[$Name]
    if ($value -is [object[]]) {
        return , $value
    }
    return $value
}

# Reads a property that is expected to hold a list, always yielding an array.
#
# Assign the result to a variable before using it. A command that emits a single array emits it as one
# object, so wrapping the call in @( ) produces an array containing the array, and piping it directly
# hands the whole array to the downstream cmdlet as one item.
function Get-JsonArrayProperty {
    param(
        $Node,

        [Parameter(Mandatory)]
        [string] $Name
    )

    $value = Get-JsonProperty -Node $Node -Name $Name
    if ($null -eq $value) {
        return , @()
    }
    if ($value -is [object[]]) {
        return , $value
    }
    return , @($value)
}

# Assigns a property on a materialized node.
#
# The coercion is what makes this safe: OrderedDictionary exposes both an integer indexer and a key
# indexer, and assigning through anything but an IDictionary reference binds to the integer one, so a
# name-keyed write fails as a type conversion rather than storing the value.
function Set-JsonProperty {
    param(
        [Parameter(Mandatory)]
        $Node,

        [Parameter(Mandatory)]
        [string] $Name,

        [AllowNull()]
        $Value
    )

    $dictionary = $Node -as [System.Collections.IDictionary]
    if ($null -eq $dictionary) {
        $typeName = if ($null -eq $Node) { 'null' } else { $Node.GetType().FullName }
        throw "Cannot assign '$Name' because the target is $typeName rather than a dictionary."
    }
    $dictionary[$Name] = $Value
}

# Serializes a canonical document to text.
#
# The depth is stated explicitly because ConvertTo-Json defaults to 2 and truncates deeper structures
# to the string "System.Collections.Hashtable" with only a warning; a captured pull request nests
# well past that. Line endings are normalized so a capture taken on any host produces the same bytes.
function ConvertTo-JsonText {
    param(
        [Parameter(Mandatory)]
        [AllowNull()]
        $Value,

        [ValidateRange(1, 100)]
        [int] $Depth = 32
    )

    $text = ConvertTo-Json -InputObject $Value -Depth $Depth
    $text.Replace("`r`n", "`n")
}

# Renders an instant in round-trip UTC form.
#
# Timestamps arrive in mixed forms and offsets across the two platforms and would otherwise sort and
# compare inconsistently. Round-trip UTC keeps sub-second precision, so this normalizes presentation
# without discarding information.
function ConvertTo-CanonicalTimestamp {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Value
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $null
    }

    $parsed = [DateTimeOffset]::MinValue

    # A timestamp that carries no offset is read as UTC rather than as host local time. Without
    # this, the same payload canonicalizes differently on two machines, and since these values are
    # the sort keys for the whole discussion, the capture would not be reproducible across hosts.
    # An explicit offset in the input still takes precedence.
    $styles = [System.Globalization.DateTimeStyles]::RoundtripKind -bor [System.Globalization.DateTimeStyles]::AssumeUniversal
    if (-not [DateTimeOffset]::TryParse($Value, [cultureinfo]::InvariantCulture, $styles, [ref] $parsed)) {
        throw "Cannot interpret '$Value' as a timestamp."
    }

    $parsed.UtcDateTime.ToString('o', [cultureinfo]::InvariantCulture)
}
