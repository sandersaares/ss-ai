#Requires -Version 7.6

# Exposes the finding-model operations exercised by the predecessor Pester corpus.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string] $CommandName,
    [string] $OutputDirectory,
    [string] $ReviewerId,
    [string] $FocusArea,
    [string] $ArtifactPath,
    [string] $FindingPath,
    [string] $FindingId,
    [string] $FileGroupQueuePath,
    [string] $Path,
    [string] $ExpectedReviewerId,
    [string] $ReviewerArtifactDirectory,
    [string] $MergePlanPath,
    [string] $OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$library = Join-Path $PSScriptRoot '../../../skills/ss-review/scripts/lib'
. (Join-Path $library 'JsonValue.ps1')
. (Join-Path $library 'PullRequestModel.ps1')
. (Join-Path $library 'WorkflowData.ps1')
. (Join-Path $library 'FindingModel.ps1')
. (Join-Path $library 'ReviewWork.ps1')

switch -CaseSensitive ($CommandName) {
    'ValidateFinding' {
        $finding = Read-FindingContent -Path $FindingPath
        Assert-FindingContent -Finding $finding -SourceDescription "'$FindingPath'"
    }
    'New-FindingArtifact.ps1' {
        $artifact = Get-FindingArtifactPath -OutputDirectory $OutputDirectory -ReviewerId $ReviewerId
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId $ReviewerId -FocusArea $FocusArea
        Write-FindingArtifact -Path $artifact -Document $document
    }
    'Add-Finding.ps1' {
        $document = Read-FindingArtifact -Path $ArtifactPath
        $finding = Read-FindingContent -Path $FindingPath
        Add-FindingToArtifact -Document $document -Finding $finding -Origin 'reviewer' | Out-Null
        Write-FindingArtifact -Path $ArtifactPath -Document $document
    }
    'Update-Finding.ps1' {
        $document = Read-FindingArtifact -Path $ArtifactPath
        $finding = Read-FindingContent -Path $FindingPath
        Set-FindingInArtifact -Document $document -FindingId $FindingId -Finding $finding
        Write-FindingArtifact -Path $ArtifactPath -Document $document
    }
    'Complete-FindingArtifact.ps1' {
        $document = Read-FindingArtifact -Path $ArtifactPath
        Assert-ReviewFileGroupQueueComplete -Path $FileGroupQueuePath
        Complete-FindingArtifactDocument -Document $document
        Write-FindingArtifact -Path $ArtifactPath -Document $document
    }
    'Test-FindingArtifact.ps1' {
        $result = Get-FindingArtifactValidation -Path $Path -ExpectedReviewerId (ConvertTo-FindingIdentifierList -Value @($ExpectedReviewerId))
        if (-not $result['isValid']) { throw 'Artifact validation failed.' }
    }
    'GetArtifactValidation' {
        $result = Get-FindingArtifactValidation -Path $Path -ExpectedReviewerId (ConvertTo-FindingIdentifierList -Value @($ExpectedReviewerId))
        Write-PullRequestOutputFile -Path $OutputPath -Content (ConvertTo-JsonText -Value $result)
    }
    'Get-FindingIndex.ps1' {
        $artifacts = Read-FindingReviewerArtifactSet -Path $ReviewerArtifactDirectory
        $document = New-FindingIndexDocument -ReviewerArtifact $artifacts
        Write-PullRequestOutputFile -Path $OutputPath -Content (ConvertTo-JsonText -Value $document)
    }
    'New-ConsolidatedFindings.ps1' {
        $artifacts = Read-FindingReviewerArtifactSet -Path $ReviewerArtifactDirectory
        $groups = Read-FindingMergePlan -Path $MergePlanPath
        $document = New-ConsolidatedFindingDocument -ReviewerArtifact $artifacts -MergeGroup $groups
        Write-FindingArtifact -Path $OutputPath -Document $document
    }
    default { throw "Unknown predecessor operation '$CommandName'." }
}
