#Requires -Version 7.0

# Azure DevOps work item adapter for tracker publication (docs/implementation.md, "Tracker
# adapters").
#
# Filing a work item is a single write, and this file is the whole of what Azure DevOps needs for it:
# the reads that settle whether the type, the area and the parent the caller named exist, the
# creation itself, and the read back that proves what was stored.
#
# A work item is created by a JSON Patch document rather than by a JSON object, which is why the
# transport carries a content type at all: the service reads the same bytes as a replacement or as a
# list of operations depending on the media type, and sending a patch as plain JSON is refused.
#
# The description is written as Markdown, which the service treats as a per-field opt-in that cannot
# be reversed once taken. Literal angle brackets are encoded at this adapter boundary because the
# Markdown renderer treats them as raw HTML and can hide the text they delimit. That representation
# and the format opt-in are why publication verifies the first item before creating any others: a run
# that guessed wrong would otherwise leave a whole review of malformed work items.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls Get-JsonProperty and Get-JsonArrayProperty from
# JsonValue.ps1, Invoke-AdoRestRequest and Invoke-AdoRestListRequest from AdoRestTransport.ps1, and
# Assert-TrackerAreaPath and Get-TrackerItemWebUrl from TrackerTarget.ps1, which the composition root
# loads alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The API version that accepts the Markdown format opt-in. It is stated here rather than taken from
# the transport default, because the default is the version the rest of the skill reads pull requests
# with and the format opt-in exists only in this preview.
$script:AdoWorkItemApiVersion = '7.2-preview.3'

# The field the finding's body is written to unless the caller names another. Azure DevOps processes
# disagree about which large text field a work item form shows - the Agile and Scrum bug forms show
# the repro steps field and never the description - so the field is a parameter with a default rather
# than a constant, and the default is the field every process defines.
$script:AdoWorkItemDefaultDescriptionField = 'System.Description'

# The field the Agile and Scrum bug forms show in place of the description. A type carrying it is a
# type whose form may hide the default, which is why its presence is what the preflight asks about.
$script:AdoWorkItemReproStepsField = 'Microsoft.VSTS.TCM.ReproSteps'

$script:AdoWorkItemTitleField = 'System.Title'
$script:AdoWorkItemAreaPathField = 'System.AreaPath'
$script:AdoWorkItemProjectField = 'System.TeamProject'

# The JSON Patch path prefixes. The format prefix is a sibling of the field prefix rather than a
# field of its own, because the format describes how the service stores a field rather than being
# something the work item holds.
$script:AdoWorkItemFieldPathPrefix = '/fields/'
$script:AdoWorkItemFormatPathPrefix = '/multilineFieldsFormat/'
$script:AdoWorkItemRelationPath = '/relations/-'

# The format the description is stored in. The service answers with its own casing, so the answer is
# compared case-insensitively while the request states the casing the service documents.
$script:AdoWorkItemMarkdownFormat = 'Markdown'

# The link type that makes the created item a child of the named work item. Azure DevOps names a link
# from the perspective of the item carrying it, so the reverse hierarchy link is the one a child adds
# to point at its parent.
$script:AdoWorkItemParentLinkType = 'System.LinkTypes.Hierarchy-Reverse'

# The property carrying the per-field storage format in a work item payload.
$script:AdoWorkItemFormatProperty = 'multilineFieldsFormat'

# Reads what the tracker's project says about filing work items into it.
#
# Every refusal reachable here is one the service would otherwise repeat for every finding in the
# review. They are read once and reported once, before the journal exists, so a run that cannot file
# anything ends having written nothing.
function Get-AdoWorkItemTrackerState {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $WorkItemType,

        [Parameter(Mandatory)]
        [string] $AreaPath,

        # Resolved to the default inside rather than through a parameter default, because the
        # composition root passes whatever the command line held and an unsupplied string parameter
        # holds the empty string rather than nothing, which a parameter default never replaces.
        [AllowEmptyString()]
        [string] $DescriptionField,

        [int] $ParentWorkItemId
    )

    $project = [string] $Target['project']
    $projectSegment = [uri]::EscapeDataString($project)

    $resolvedDescriptionField = if ([string]::IsNullOrWhiteSpace($DescriptionField)) {
        $script:AdoWorkItemDefaultDescriptionField
    } else {
        $DescriptionField
    }

    $resolvedType = Resolve-AdoWorkItemType -Transport $Transport -Project $project -WorkItemType $WorkItemType
    Assert-AdoWorkItemDescriptionField `
        -Transport $Transport `
        -Project $project `
        -WorkItemType $resolvedType `
        -DescriptionField $resolvedDescriptionField `
        -IsExplicit:(-not [string]::IsNullOrWhiteSpace($DescriptionField))
    $resolvedAreaPath = Resolve-AdoWorkItemAreaPath -Transport $Transport -Target $Target -AreaPath $AreaPath

    $parent = $null
    if ($ParentWorkItemId -gt 0) {
        $parent = Resolve-AdoWorkItemParent -Transport $Transport -Target $Target -ParentWorkItemId $ParentWorkItemId
    }

    [ordered]@{
        platform         = 'azureDevOps'
        trackerName      = "$($Target['organization'])/$project"
        createPath       = "$projectSegment/_apis/wit/workitems/`$$([uri]::EscapeDataString($resolvedType))"
        readPathPrefix   = "$projectSegment/_apis/wit/workitems/"
        workItemType     = $resolvedType
        areaPath         = $resolvedAreaPath
        descriptionField = $resolvedDescriptionField
        parent           = $parent
    }
}

# Confirms the project defines the named work item type, and answers with the name the project spells
# it by.
#
# The service matches a type name case-insensitively when creating, but records it in its own casing,
# so adopting the project's spelling keeps the journal reporting what the project holds rather than
# what the command line said.
function Resolve-AdoWorkItemType {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Project,

        [Parameter(Mandatory)]
        [string] $WorkItemType
    )

    $types = Invoke-AdoRestListRequest `
        -Transport $Transport `
        -Path "$([uri]::EscapeDataString($Project))/_apis/wit/workitemtypes"

    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($type in $types) {
        $name = [string] (Get-JsonProperty -Node $type -Name 'name')
        if (-not $name) { continue }
        if ([string]::Equals($name, $WorkItemType, [StringComparison]::OrdinalIgnoreCase)) { return $name }
        $names.Add($name)
    }

    throw "The project '$Project' defines no work item type named '$WorkItemType'. It defines: $(($names | Sort-Object) -join ', ')."
}

# Confirms the work item type carries the field the description is written to.
#
# A field the type does not carry is refused by the service on creation, which would be a refusal per
# finding rather than one before the run. Naming the type's large text fields in the refusal is what
# lets a caller filing bugs discover the repro steps field without leaving the terminal.
#
# A type carrying the repro steps field is refused unless the caller named a field explicitly, even
# though the default field exists on it. In the Agile and Scrum processes a bug's form shows the
# repro steps and never the description, so the default would file every body into a field the form
# does not display and the run would report success over a review nobody can read. The service
# reports what a type defines but not what its form shows, so this cannot be settled here: the caller
# is asked to state which field they mean.
function Assert-AdoWorkItemDescriptionField {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Project,

        [Parameter(Mandatory)]
        [string] $WorkItemType,

        [Parameter(Mandatory)]
        [string] $DescriptionField,

        # Whether the caller named the field, as opposed to accepting the default.
        [switch] $IsExplicit
    )

    $payload = Invoke-AdoRestRequest `
        -Transport $Transport `
        -Path "$([uri]::EscapeDataString($Project))/_apis/wit/workitemtypes/$([uri]::EscapeDataString($WorkItemType))"

    $fields = Get-JsonArrayProperty -Node $payload -Name 'fields'
    $carriesField = $false
    $carriesReproSteps = $false
    foreach ($field in $fields) {
        $referenceName = [string] (Get-JsonProperty -Node $field -Name 'referenceName')
        if ([string]::Equals($referenceName, $DescriptionField, [StringComparison]::OrdinalIgnoreCase)) { $carriesField = $true }
        if ([string]::Equals($referenceName, $script:AdoWorkItemReproStepsField, [StringComparison]::OrdinalIgnoreCase)) { $carriesReproSteps = $true }
    }

    if (-not $carriesField) {
        throw "The work item type '$WorkItemType' in project '$Project' carries no field '$DescriptionField', so the finding has nowhere to be written. Name the type's own large text field with -DescriptionField; the Agile and Scrum bug forms use '$($script:AdoWorkItemReproStepsField)'."
    }

    if ($carriesReproSteps -and -not $IsExplicit) {
        throw "The work item type '$WorkItemType' in project '$Project' carries '$($script:AdoWorkItemReproStepsField)', which is where the Agile and Scrum bug forms show a bug's text; those forms never show '$($script:AdoWorkItemDefaultDescriptionField)', so filing into it would produce work items whose body nobody sees. Name the field with -DescriptionField to state which one this project's form uses."
    }
}

# Confirms the project contains the named area, and answers with the path the project spells it by.
#
# The area is read as a classification node rather than searched for in a tree walk, because the
# service addresses one directly and answers about the exact path the caller named. A path the
# project does not contain is refused by the service on creation, and a refusal per finding is a
# refusal after the journal exists.
function Resolve-AdoWorkItemAreaPath {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $AreaPath
    )

    # Restates the rule the command line already applied, because this function derives the request
    # path from the assumption that the first segment is the project and a path rooted elsewhere would
    # otherwise be asked about under the wrong root.
    Assert-TrackerAreaPath -Target $Target -AreaPath $AreaPath

    $project = [string] $Target['project']
    $segments = @($AreaPath.Split('\', [StringSplitOptions]::RemoveEmptyEntries))
    $belowRoot = @($segments | Select-Object -Skip 1)

    $path = "$([uri]::EscapeDataString($project))/_apis/wit/classificationnodes/areas"
    if ($belowRoot.Count -gt 0) {
        $path = "$path/$(($belowRoot | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/')"
    }

    $node = $null
    try {
        $node = Invoke-AdoRestRequest -Transport $Transport -Path $path
    } catch {
        throw "The project '$project' contains no area '$AreaPath'. Azure DevOps answered: $($_.Exception.Message)"
    }

    # The service reports the path with a classification segment the field does not use, so the answer
    # is rebuilt from the project and the segments below it rather than quoted as it arrives.
    $name = [string] (Get-JsonProperty -Node $node -Name 'name')
    if (-not $name) {
        throw "Azure DevOps answered about the area '$AreaPath' in project '$project' without naming it, so the run cannot tell which area it would file into."
    }

    if ($belowRoot.Count -eq 0) { return $project }

    $spelled = [System.Collections.Generic.List[string]]::new()
    $spelled.Add($project)
    $spelled.AddRange([string[]] $belowRoot)
    $spelled[$spelled.Count - 1] = $name
    $spelled -join '\'
}

# Confirms the named parent exists and belongs to the tracker's project.
#
# A parent in another project is refused rather than linked: Azure DevOps permits the link, and a
# review filed under a parent nobody working on this project can see is a review nobody reads.
function Resolve-AdoWorkItemParent {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [int] $ParentWorkItemId
    )

    $payload = $null
    try {
        $payload = Invoke-AdoRestRequest `
            -Transport $Transport `
            -Path "_apis/wit/workitems/$ParentWorkItemId" `
            -Query @{ fields = "$script:AdoWorkItemProjectField,$script:AdoWorkItemTitleField" }
    } catch {
        throw "Work item $ParentWorkItemId, named as the parent, could not be read. Azure DevOps answered: $($_.Exception.Message)"
    }

    $fields = Get-JsonProperty -Node $payload -Name 'fields'
    $parentProject = [string] (Get-JsonProperty -Node $fields -Name $script:AdoWorkItemProjectField)
    $project = [string] $Target['project']
    if (-not [string]::Equals($parentProject, $project, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Work item $ParentWorkItemId, named as the parent, belongs to project '$parentProject' rather than '$project'. Name a parent in the project the review is filed into."
    }

    # A relation is addressed by the service's own URL for the work item rather than by the browser
    # one, so the API URL the read answered with is carried through unchanged. Rebuilding it here
    # would be guessing at a route the service has already stated.
    $apiUrl = [string] (Get-JsonProperty -Node $payload -Name 'url')
    if (-not $apiUrl) {
        throw "Azure DevOps reported work item $ParentWorkItemId without its own URL, so it cannot be linked as a parent."
    }

    [ordered]@{
        itemId = [string] $ParentWorkItemId
        title  = [string] (Get-JsonProperty -Node $fields -Name $script:AdoWorkItemTitleField)
        url    = $apiUrl
        webUrl = Get-TrackerItemWebUrl -Target $Target -ItemId ([string] $ParentWorkItemId)
    }
}

# Files one finding as a work item.
#
# The title and the body arrive already rendered, so nothing here decides how a finding reads.
function New-AdoWorkItemForFinding {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item
    )

    $response = Invoke-AdoRestRequest `
        -Transport $Transport `
        -Method 'POST' `
        -Path ([string] $State['createPath']) `
        -ApiVersion $script:AdoWorkItemApiVersion `
        -ContentType $script:AdoRestJsonPatchContentType `
        -Body (New-AdoWorkItemPatchDocument -State $State -Item $Item) `
        -NoRetry

    ConvertTo-AdoWorkItemRecord -Target $Target -State $State -WorkItem $response
}

# Encodes literal angle brackets for an Azure DevOps Markdown field.
#
# Findings contain Markdown rather than intentional raw HTML. Azure DevOps otherwise interprets
# angle-delimited prose as an HTML element and omits it from the rendered work item. Encoding only
# the brackets follows docs/implementation.md, "Body verification", and preserves existing entities
# instead of double-encoding their ampersands.
function ConvertTo-AdoWorkItemMarkdownText {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyString()]
        [string] $Text
    )

    $Text.Replace('<', '&lt;', [StringComparison]::Ordinal).
        Replace('>', '&gt;', [StringComparison]::Ordinal)
}

# Builds the JSON Patch document one creation sends.
#
# The format operation is emitted unconditionally rather than only for a body that looks like
# Markdown, because the finding's body is Markdown whether or not any given finding happens to use a
# construct that renders differently, and a body stored as HTML is not correctable in place.
function New-AdoWorkItemPatchDocument {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Item
    )

    $descriptionField = [string] $State['descriptionField']

    $operations = [System.Collections.Generic.List[object]]::new()
    $operations.Add([ordered]@{ op = 'add'; path = "$script:AdoWorkItemFieldPathPrefix$script:AdoWorkItemTitleField"; value = [string] $Item['title'] })
    $operations.Add([ordered]@{
            op    = 'add'
            path  = "$script:AdoWorkItemFieldPathPrefix$descriptionField"
            value = ConvertTo-AdoWorkItemMarkdownText -Text ([string] $Item['body'])
        })
    $operations.Add([ordered]@{ op = 'add'; path = "$script:AdoWorkItemFieldPathPrefix$script:AdoWorkItemAreaPathField"; value = [string] $State['areaPath'] })
    $operations.Add([ordered]@{ op = 'add'; path = "$script:AdoWorkItemFormatPathPrefix$descriptionField"; value = $script:AdoWorkItemMarkdownFormat })

    $parent = Get-JsonProperty -Node $State -Name 'parent'
    if ($null -ne $parent) {
        $operations.Add([ordered]@{
                op    = 'add'
                path  = $script:AdoWorkItemRelationPath
                value = [ordered]@{
                    rel = $script:AdoWorkItemParentLinkType
                    url = [string] $parent['url']
                }
            })
    }

    return , $operations.ToArray()
}

# Reads one filed work item back, so what the service stored can be compared with what was sent.
function Get-AdoWorkItemRecord {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [string] $ItemId
    )

    $payload = Invoke-AdoRestRequest `
        -Transport $Transport `
        -Path "$($State['readPathPrefix'])$ItemId" `
        -ApiVersion $script:AdoWorkItemApiVersion

    ConvertTo-AdoWorkItemRecord -Target $Target -State $State -WorkItem $payload
}

# Normalizes one work item into the record both adapters return.
#
# `itemId` is the number a reader quotes and `itemNodeId` the identifier the service is addressed by.
# Azure DevOps addresses a work item by the same number a reader quotes, so both carry it; the key
# exists because GitHub needs the two to differ.
function ConvertTo-AdoWorkItemRecord {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $State,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $WorkItem
    )

    $id = Get-JsonProperty -Node $WorkItem -Name 'id'
    if ($null -eq $id) {
        throw 'Azure DevOps answered about a work item without reporting its identifier, so the run cannot tell which work item it holds.'
    }

    $descriptionField = [string] $State['descriptionField']
    $fields = Get-JsonProperty -Node $WorkItem -Name 'fields'

    [ordered]@{
        itemId            = [string] $id
        itemNodeId        = [string] $id
        url               = Get-TrackerItemWebUrl -Target $Target -ItemId ([string] $id)
        title             = [string] (Get-JsonProperty -Node $fields -Name $script:AdoWorkItemTitleField)
        body              = [string] (Get-JsonProperty -Node $fields -Name $descriptionField)
        descriptionFormat = Get-AdoWorkItemDescriptionFormat -WorkItem $WorkItem -DescriptionField $descriptionField
    }
}

# Reports the format the service stores the description in.
#
# An absent report is answered with the empty string rather than with the Markdown the request asked
# for. The whole reason the format is read is that the service may have stored HTML, and defaulting
# the answer to what was requested would confirm the request rather than the outcome.
function Get-AdoWorkItemDescriptionFormat {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $WorkItem,

        [Parameter(Mandatory)]
        [string] $DescriptionField
    )

    $formats = Get-JsonProperty -Node $WorkItem -Name $script:AdoWorkItemFormatProperty
    if ($null -eq $formats) { return '' }

    [string] (Get-JsonProperty -Node $formats -Name $DescriptionField -Default '')
}

# Reports whether a stored format is the Markdown one the creation asked for.
#
# The service answers with its own casing, which is not the casing the request states, so the
# comparison ignores case. It lives here rather than at the verification, because what counts as
# Markdown is this adapter's knowledge of the service and not something publication should restate.
function Test-AdoWorkItemMarkdownFormat {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Format
    )

    [string]::Equals($Format, $script:AdoWorkItemMarkdownFormat, [StringComparison]::OrdinalIgnoreCase)
}
