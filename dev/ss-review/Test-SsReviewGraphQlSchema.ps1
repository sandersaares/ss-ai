#Requires -Version 7.6

<#
.SYNOPSIS
    Validates the ss-review GitHub GraphQL documents against GitHub's live schema.

.DESCRIPTION
    Extracts every GraphQL query and mutation here-string from the ss-review
    GitHub adapters, supplies dummy values for required variables, and executes
    the document through `gh api graphql`. The dummy identifiers are deliberately
    invalid for execution, but GitHub validates the document against the schema
    before resolving them, so schema errors are reported without writing pull
    request or issue state.

    This is a manual live check rather than part of the offline Pester suite:
    it needs network access and an authenticated GitHub CLI session.

.PARAMETER AdapterPath
    Paths of the GitHub adapters to validate. Defaults to every ss-review
    GitHub adapter and the shared utility's GraphQL documents in this repository.

.EXAMPLE
    .\dev\ss-review\Test-SsReviewGraphQlSchema.ps1

    Validate the ss-review GitHub adapters' GraphQL documents.
#>

[CmdletBinding(PositionalBinding = $false)]
param(
    [string[]] $AdapterPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if (-not $AdapterPath) {
    $repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $adapterRoot = Join-Path $repoRoot 'skills\ss-review\scripts\lib'
    $AdapterPath = @(
        (Join-Path $adapterRoot 'GitHubPullRequestApi.ps1'),
        (Join-Path $adapterRoot 'GitHubIssueApi.ps1'),
        (Join-Path $repoRoot 'packages\ss-skills-util\src\commits.graphql')
    )
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw 'GitHub CLI (`gh`) is required to validate the live GraphQL schema.'
}

foreach ($path in $AdapterPath) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "GitHub adapter not found: $path"
    }
}

function Get-GraphQlDocument {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)] [string] $Path
    )

    $content = Get-Content -LiteralPath $Path -Raw
    # Native adapters include the same standalone document this check validates.
    if ([IO.Path]::GetExtension($Path) -eq '.graphql') {
        if ($content -notmatch '^\s*(query|mutation)\b') { throw "Invalid GraphQL document: $Path" }
        [pscustomobject]@{ Name = [IO.Path]::GetFileNameWithoutExtension($Path); Document = $content }
        return
    }
    foreach ($match in [regex]::Matches($content, "(?s)\`$script:(?<name>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*@'\r?\n(?<document>.*?)\r?\n'@")) {
        $document = $match.Groups['document'].Value
        if ($document -notmatch '^\s*(query|mutation)\b') { continue }
        [pscustomobject]@{
            Name     = $match.Groups['name'].Value
            Document = $document
        }
    }
}

function Get-GraphQlVariableArgument {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)] [string] $DocumentName,
        [Parameter(Mandatory)] [string] $Document
    )

    # Values are syntactically valid for their GraphQL scalar or enum type but deliberately do not
    # identify real pull request objects, so validation reaches GitHub's schema without performing
    # a meaningful mutation.
    # TODO: Support PullRequestReviewEvent, required by GitHubSubmitReviewMutation.
    $typeValues = @{
        String      = @{ Switch = '-f'; Value = 'x' }
        ID          = @{ Switch = '-f'; Value = 'PRR_kwDOAAAAAAAAAAA' }
        GitObjectID = @{ Switch = '-f'; Value = '0000000000000000000000000000000000000000' }
        DiffSide    = @{ Switch = '-f'; Value = 'RIGHT' }
        Int         = @{ Switch = '-F'; Value = '1' }
        Boolean     = @{ Switch = '-F'; Value = 'false' }
    }

    $signature = [regex]::Match($Document, '^\s*(?:query|mutation)(?:\s+[A-Za-z_][A-Za-z0-9_]*)?\s*\((?<variables>[^)]*)\)')
    if (-not $signature.Success) { return @() }

    $arguments = @()
    foreach ($declaration in ($signature.Groups['variables'].Value -split ',')) {
        $declaration = $declaration.Trim()
        if (-not $declaration) { continue }

        $variable = [regex]::Match($declaration, '^\$(?<name>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?<type>[^=]+?)(?:\s*=.*)?$')
        if (-not $variable.Success) {
            throw "Could not parse variable declaration '$declaration' in $DocumentName."
        }

        $typeExpression = $variable.Groups['type'].Value.Trim()
        if (-not $typeExpression.EndsWith('!')) { continue }

        $bareType = $typeExpression.TrimEnd('!')
        if (-not $typeValues.ContainsKey($bareType)) {
            throw "Required variable '$($variable.Groups['name'].Value)' in $DocumentName uses unsupported GraphQL type '$bareType'."
        }

        $typeValue = $typeValues[$bareType]
        $arguments += @(
            $typeValue['Switch'],
            "$($variable.Groups['name'].Value)=$($typeValue['Value'])"
        )
    }

    $arguments
}

function Get-GraphQlResultDetail {
    [CmdletBinding(PositionalBinding = $false)]
    param(
        [Parameter(Mandatory)] [string] $Output
    )

    $jsonStart = $Output.IndexOf('{')
    if ($jsonStart -ge 0) {
        try {
            $response = $Output.Substring($jsonStart) | ConvertFrom-Json -Depth 100
            if ($response.PSObject.Properties.Name -contains 'errors') {
                $message = @($response.errors | ForEach-Object { $_.message } | Where-Object { $_ } | Select-Object -First 1)
                if ($message) { return $message[0] }
            }
            if ($response.PSObject.Properties.Name -contains 'data') { return 'executed' }
        } catch {
            # GitHub CLI may render errors as human-readable text rather than JSON; the first line
            # still gives the operator the reason the document failed or passed execution.
        }
    }

    foreach ($line in ($Output -split '\r?\n')) {
        $line = $line.Trim()
        if ($line) { return $line }
    }

    'No output.'
}

# These are schema validation diagnostics emitted by GitHub. Resolver errors are intentionally not
# listed, because a bogus identifier proves the document passed validation before execution failed.
$schemaValidationErrorPattern = "doesn't exist on type|is not defined by type|undefinedField|argumentNotAccepted|Parse error|Unknown argument|no argument named|wrong type|isn't accepted"
# TODO: Reject authentication, transport and unexpected execution failures rather than counting
# every response without a recognized schema diagnostic as a successful validation.

$documents = @($AdapterPath | ForEach-Object { Get-GraphQlDocument -Path $_ })
if ($documents.Count -eq 0) {
    throw "No GraphQL query or mutation documents were found in $($AdapterPath -join ', ')."
}

$results = @()
foreach ($document in $documents) {
    try {
        $arguments = @('api', 'graphql', '-f', "query=$($document.Document)")
        $arguments += @(Get-GraphQlVariableArgument -DocumentName $document.Name -Document $document.Document)
    } catch {
        $results += [pscustomobject]@{
            Document = $document.Name
            Status   = 'FAIL'
            Detail   = $_.Exception.Message
        }
        continue
    }

    $output = & gh @arguments 2>&1 | Out-String
    $detail = Get-GraphQlResultDetail -Output $output
    $status = if ($output -match $schemaValidationErrorPattern) { 'FAIL' } else { 'PASS' }

    $results += [pscustomobject]@{
        Document = $document.Name
        Status   = $status
        Detail   = $detail
    }
}

$results | Format-Table -AutoSize -Wrap | Out-Host

$failed = @($results | Where-Object { $_.Status -eq 'FAIL' })
if ($failed.Count -gt 0) {
    Write-Host ("GraphQL schema validation failed for {0}/{1} document(s)." -f $failed.Count, $results.Count) -ForegroundColor Red
    exit 1
}

Write-Host ("GraphQL schema validation passed for {0}/{1} document(s)." -f $results.Count, $results.Count) -ForegroundColor Green
exit 0
