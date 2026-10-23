#Requires -Version 7.6

# Adds typed worker contributions, merges worker maps and regroups categories without bespoke JSON
# mutation. Each InputPath is a valid map, including a one-chapter map when appending one judgment.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $SessionStateDirectory,
    [Parameter(Mandatory)][string] $Path,
    [Parameter(Mandatory)][ValidateSet('Append', 'Replace', 'Regroup')][string] $Operation,
    [string] $InputPath,
    [string] $CategoryId,
    [string] $NewCategoryId,
    [string] $Title
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. "$PSScriptRoot\lib\JsonValue.ps1"
. "$PSScriptRoot\lib\PullRequestModel.ps1"
. "$PSScriptRoot\lib\WorkflowData.ps1"
. "$PSScriptRoot\lib\FocusAreaPreparation.ps1"
$lock = Open-WorkflowDataLock -Path $Path
try {
    $document = if (Test-Path -LiteralPath $Path) {
        (Read-FocusAreaMap -MapPath $Path -SessionStateDirectory $SessionStateDirectory).Map
    } else { [ordered]@{ schemaVersion = 1; categories = @(); chapters = @() } }
    if ($Operation -in @('Append', 'Replace')) {
        if (-not $InputPath) { throw "$Operation requires -InputPath." }
        $inputMap = (Read-FocusAreaMap -MapPath $InputPath -SessionStateDirectory $SessionStateDirectory).Map
        foreach ($category in $inputMap['categories']) {
            $existing = @($document['categories'] | Where-Object { $_['id'] -ceq $category['id'] })
            if ($existing.Count -and $existing[0]['title'] -cne $category['title']) { throw 'Conflicting category title; regroup explicitly before merging.' }
            if (-not $existing.Count) { $document['categories'] = @($document['categories']) + @($category) }
        }
        foreach ($chapter in $inputMap['chapters']) {
            $existing = @($document['chapters'] | Where-Object { $_['id'] -ceq $chapter['id'] })
            if ($existing.Count) {
                if ($Operation -ceq 'Replace') {
                    for ($index = 0; $index -lt $document['chapters'].Count; $index++) {
                        if ($document['chapters'][$index]['id'] -ceq $chapter['id']) { $document['chapters'][$index] = $chapter }
                    }
                } elseif ((ConvertTo-JsonText -Value $existing[0]) -cne (ConvertTo-JsonText -Value $chapter)) {
                    throw "Conflicting chapter '$($chapter['id'])'; correct its originating draft."
                }
            } elseif ($Operation -ceq 'Replace') { throw "Cannot replace unknown chapter '$($chapter['id'])'." }
            else { $document['chapters'] = @($document['chapters']) + @($chapter) }
        }
        $document['categories'] = @($document['categories'] | Where-Object { @($document['chapters'].categoryId) -ccontains $_['id'] })
    } else {
        if (-not $CategoryId -or -not $NewCategoryId -or -not $Title) { throw 'Regroup requires -CategoryId, -NewCategoryId and -Title.' }
        if (@($document['categories'] | Where-Object { $_['id'] -ceq $CategoryId }).Count -ne 1) { throw 'Unknown source category.' }
        foreach ($chapter in $document['chapters']) {
            if ($chapter['categoryId'] -ceq $CategoryId) { $chapter['categoryId'] = $NewCategoryId }
        }
        $document['categories'] = @($document['categories'] | Where-Object { $_['id'] -cne $CategoryId -and $_['id'] -cne $NewCategoryId }) +
            @([ordered]@{ id = $NewCategoryId; title = $Title })
    }
    $text = ConvertTo-JsonText -Value $document
    $null = Test-FocusAreaMapValue -Json $text -SessionStateDirectory $SessionStateDirectory
    Write-PullRequestOutputFileAtomically -Path $Path -Content $text
    @{ status = 'Recorded'; categoryCount = $document['categories'].Count; chapterCount = $document['chapters'].Count } | ConvertTo-Json -Compress
} finally { $lock.Dispose() }
