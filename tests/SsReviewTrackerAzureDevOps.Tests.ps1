BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts\lib'

    # Libraries never dot-source one another, so every dependency is loaded here in the order the
    # composition root uses.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'TrackerTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'AdoWorkItemApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubIssueApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')
    . (Join-Path $script:LibraryRoot 'FindingMarkdown.ps1')
    . (Join-Path $script:LibraryRoot 'TrackerItem.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTransportHost.ps1')
    . (Join-Path $script:LibraryRoot 'TrackerPublication.ps1')

    # The suite is entirely offline. Every request, every wait and every token acquisition is
    # injected, so nothing here depends on a service being reachable or on wall-clock time passing.
    # Azure DevOps is unreachable from the machines this repository is developed on, so this suite is
    # the whole of what stands behind the adapter.

    # Characters are built from their code points so the fixtures never depend on how this file is
    # encoded on disk, and so the assertions compare code points rather than rendered glyphs.
    $script:EmDash = [string] [char]0x2014
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)

    # Every record that carries an instant is given this one, so a run at any moment produces the
    # same document.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    # The project name carries a space, so every assertion about a request path is also an assertion
    # that the path was escaped.
    $script:Target = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = 'https://dev.azure.com/contoso'
        organization  = 'contoso'
        project       = 'Example Project'
        repository    = $null
        webUrl        = 'https://dev.azure.com/contoso/Example%20Project/_workitems'
    }

    $script:AreaPath = 'Example Project\Widgets\Core'

    # --- The stand-in service ---------------------------------------------------------------------

    function New-AdoResponse {
        param(
            [AllowEmptyString()]
            [string] $Body = '{}',

            [int] $StatusCode = 200,

            [hashtable] $Headers = @{}
        )

        [ordered]@{
            statusCode   = $StatusCode
            headers      = $Headers
            contentType  = 'application/json; charset=utf-8'
            contentBytes = [System.Text.Encoding]::UTF8.GetBytes($Body)
        }
    }

    # The in-memory stand-in for a project's work item store.
    #
    # A created work item is stored and served back, so the read back reports what the creation was
    # sent rather than a fixture a test had to keep in step with the request.
    function Reset-AdoState {
        $script:AdoCalls = [System.Collections.Generic.List[object]]::new()
        $script:AdoWaits = [System.Collections.Generic.List[int]]::new()
        $script:AdoFaults = [System.Collections.Generic.List[object]]::new()
        $script:AdoWorkItems = [System.Collections.Generic.List[object]]::new()
        $script:AdoNextItemId = 4001

        $script:AdoTypeName = @('Bug', 'Task', 'User Story')
        $script:AdoTypeField = @{
            'Bug'        = @('System.Title', 'System.Description', 'System.AreaPath', 'Microsoft.VSTS.TCM.ReproSteps')
            'Task'       = @('System.Title', 'System.Description', 'System.AreaPath')
            'User Story' = @('System.Title', 'System.Description', 'System.AreaPath')
        }

        # Keyed by the segments below the project root, joined as the request spells them.
        $script:AdoAreaNode = @{
            ''             = 'Example Project'
            'Widgets'      = 'Widgets'
            'Widgets/Core' = 'Core'
        }

        $script:AdoParentItem = @{
            '900' = [ordered]@{
                project = 'Example Project'
                title   = 'Review debt'
                url     = 'https://dev.azure.com/contoso/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/900'
            }
        }

        # What the read back reports as the stored format. A test that is about the format states it
        # here rather than intercepting the response.
        $script:AdoStoredFormat = 'markdown'

        # Whether the read back reports a format at all.
        $script:AdoReportsFormat = $true

        # Descriptions the service stores in place of the one a creation sent, keyed by the creation's
        # one-based ordinal. A test that is about a Markdown rewrite states the stored form here rather
        # than intercepting the response, mirroring the GitHub harness's Add-TrackerBodyRewrite.
        $script:AdoBodyRewrites = [System.Collections.Generic.List[object]]::new()

        # When set to a result path, the send fake reads that journal's status the moment a creation is
        # sent and records it, so a test can prove the status the run persisted before it filed.
        $script:AdoStatusProbePath = $null
        $script:AdoStatusAtCreation = [System.Collections.Generic.List[string]]::new()
    }

    # Registers a canned answer for the requests whose "METHOD URL" matches the pattern. A fault
    # takes precedence over the fake's own behaviour, so a test states the request it wants to fail
    # without restating the ones it wants to succeed.
    function Add-AdoFault {
        param(
            [Parameter(Mandatory)]
            [string] $Pattern,

            [int] $StatusCode = 500,

            [string] $Body = '{"message":"the service refused"}',

            # Raised instead of answered, which is how a response that never arrived reaches the
            # caller: the request may or may not have been applied.
            [switch] $Lost,

            [int] $Skip = 0
        )

        $script:AdoFaults.Add([ordered]@{
                pattern    = $Pattern
                statusCode = $StatusCode
                body       = $Body
                lost       = [bool] $Lost
                skip       = $Skip
            })
    }

    function Get-AdoFaultResponse {
        param([Parameter(Mandatory)][string] $Composite)

        foreach ($fault in $script:AdoFaults) {
            if ($Composite -notmatch [string] $fault['pattern']) { continue }
            if ([int] $fault['skip'] -gt 0) {
                $fault['skip'] = [int] $fault['skip'] - 1
                continue
            }
            return $fault
        }
        return $null
    }

    # Changes what the stand-in service stores for one creation's description, modelling a successful
    # write whose read back does not match the requested body.
    function Add-AdoBodyRewrite {
        param(
            [Parameter(Mandatory)]
            [ValidateRange(1, [int]::MaxValue)]
            [int] $AtCreation,

            [Parameter(Mandatory)]
            [string] $Body
        )

        $script:AdoBodyRewrites.Add([ordered]@{
                atCreation = $AtCreation
                body       = $Body
            })
    }

    function Get-AdoBodyRewrite {
        param([Parameter(Mandatory)][int] $Ordinal)

        foreach ($rewrite in $script:AdoBodyRewrites) {
            if ([int] $rewrite['atCreation'] -eq $Ordinal) { return [string] $rewrite['body'] }
        }
        return $null
    }

    function Find-AdoStoredWorkItem {
        param([Parameter(Mandatory)][string] $ItemId)

        foreach ($item in $script:AdoWorkItems) {
            if ([string] $item['id'] -ceq $ItemId) { return $item }
        }
        return $null
    }

    # Applies a JSON Patch document the way the service does: each add writes a field, a format or a
    # relation.
    function New-AdoStoredWorkItem {
        param([Parameter(Mandatory)][object[]] $Operation)

        $item = [ordered]@{
            id                    = $script:AdoNextItemId
            fields                = [ordered]@{}
            multilineFieldsFormat = [ordered]@{}
            relations             = [System.Collections.Generic.List[object]]::new()
        }
        $script:AdoNextItemId++

        # The loop variable may not be named for the parameter: PowerShell variable names are
        # case-insensitive, so assigning each element to a name the [object[]] parameter already
        # holds would re-wrap every element in a single-element array.
        foreach ($entry in $Operation) {
            $path = [string] (Get-JsonProperty -Node $entry -Name 'path')
            $value = Get-JsonProperty -Node $entry -Name 'value'

            if ($path.StartsWith('/fields/', [StringComparison]::Ordinal)) {
                Set-JsonProperty -Node $item['fields'] -Name $path.Substring('/fields/'.Length) -Value ([string] $value)
                continue
            }
            if ($path.StartsWith('/multilineFieldsFormat/', [StringComparison]::Ordinal)) {
                # The service answers with its own casing rather than the casing the request stated.
                Set-JsonProperty -Node $item['multilineFieldsFormat'] -Name $path.Substring('/multilineFieldsFormat/'.Length) -Value $script:AdoStoredFormat
                continue
            }
            $item['relations'].Add($value)
        }

        # A registered rewrite replaces the description the creation sent, targeting the field the
        # request marked as multiline (the description), so the read back reports a Markdown
        # normalization the run did not send. The first stored item is 4001, so its id less 4000 is
        # the creation's one-based ordinal.
        $rewrite = Get-AdoBodyRewrite -Ordinal ([int] $item['id'] - 4000)
        if ($null -ne $rewrite) {
            foreach ($field in @($item['multilineFieldsFormat'].Keys)) {
                Set-JsonProperty -Node $item['fields'] -Name $field -Value $rewrite
            }
        }

        $script:AdoWorkItems.Add($item)
        return $item
    }

    function ConvertTo-AdoWorkItemJson {
        param([Parameter(Mandatory)][System.Collections.IDictionary] $Item)

        $payload = [ordered]@{
            id     = $Item['id']
            fields = $Item['fields']
        }
        if ($script:AdoReportsFormat) { $payload['multilineFieldsFormat'] = $Item['multilineFieldsFormat'] }
        $payload['relations'] = @($Item['relations'])

        ConvertTo-Json -Depth 12 -Compress -InputObject $payload
    }

    function New-AdoListJson {
        param([Parameter(Mandatory)][AllowEmptyCollection()][object[]] $Value)

        ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{ count = $Value.Count; value = @($Value) })
    }

    # Answers one request the way the service would, or the way the test asked for it to fail.
    function Get-AdoFakeResponse {
        param([Parameter(Mandatory)] $Request)

        $composite = "$($Request.Method) $($Request.Url)"

        $fault = Get-AdoFaultResponse -Composite $composite
        if ($null -ne $fault) {
            if ([bool] $fault['lost']) { throw 'the connection was closed before a response arrived' }
            return New-AdoResponse -StatusCode ([int] $fault['statusCode']) -Body ([string] $fault['body'])
        }

        switch -Regex ($composite) {
            '^POST .*/_apis/wit/workitems/\$(?<type>[^?]+)\?' {
                # ConvertFrom-JsonVerbatim hands an array back as one object, so it is bound before
                # it is used rather than collected, which would wrap it a second time.
                $document = ConvertFrom-JsonVerbatim -Text ([string] $Request.Body)
                return New-AdoResponse -Body (ConvertTo-AdoWorkItemJson -Item (New-AdoStoredWorkItem -Operation ([object[]] $document)))
            }

            # The parent read names the fields it wants and reaches the collection root; the read
            # back names none and reaches the project.
            '^GET .*/_apis/wit/workitems/(?<id>\d+)\?fields=' {
                $parent = $script:AdoParentItem[$Matches['id']]
                if ($null -eq $parent) {
                    return New-AdoResponse -StatusCode 404 -Body '{"message":"TF401232: Work item does not exist, or you do not have permissions to read it."}'
                }
                $payload = [ordered]@{
                    id     = [int] $Matches['id']
                    fields = [ordered]@{
                        'System.TeamProject' = [string] $parent['project']
                        'System.Title'       = [string] $parent['title']
                    }
                }
                if ($null -ne $parent['url']) { $payload['url'] = [string] $parent['url'] }
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject $payload)
            }

            '^GET .*/_apis/wit/workitems/(?<id>\d+)\?' {
                $item = Find-AdoStoredWorkItem -ItemId $Matches['id']
                if ($null -eq $item) { throw "the fake holds no work item $($Matches['id'])" }
                return New-AdoResponse -Body (ConvertTo-AdoWorkItemJson -Item $item)
            }

            '^GET .*/_apis/wit/workitemtypes/(?<type>[^?]+)\?' {
                $type = [uri]::UnescapeDataString($Matches['type'])
                $fields = $script:AdoTypeField[$type]
                if ($null -eq $fields) { throw "the fake holds no work item type '$type'" }
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            name   = $type
                            fields = @(foreach ($field in $fields) { [ordered]@{ referenceName = $field; name = $field } })
                        }))
            }

            '^GET .*/_apis/wit/workitemtypes\?' {
                return New-AdoResponse -Body (New-AdoListJson -Value @(
                        foreach ($name in $script:AdoTypeName) { [ordered]@{ name = $name; referenceName = "Microsoft.VSTS.WorkItemTypes.$($name.Replace(' ', ''))" } }
                    ))
            }

            '^GET .*/_apis/wit/classificationnodes/areas(?<below>[^?]*)\?' {
                $below = [string] $Matches['below']
                $key = ($below.TrimStart('/').Split('/') | ForEach-Object { [uri]::UnescapeDataString($_) }) -join '/'
                $name = $script:AdoAreaNode[$key]
                if ($null -eq $name) {
                    return New-AdoResponse -StatusCode 404 -Body '{"message":"VS402485: The classification node does not exist."}'
                }
                return New-AdoResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                            id            = 42
                            name          = $name
                            structureType = 'area'
                            path          = "\\$($script:Target['project'])\Area$(if ($key) { '\' + $key.Replace('/', '\') })"
                        }))
            }

            default { throw "the fake has no answer for '$composite'" }
        }
    }

    function New-TestTransport {
        param([int] $MaxAttempts = 5)

        # The recorded state lives at script scope rather than inside a closure, because a closure is
        # bound to its own module and would no longer resolve the helper functions this file defines.
        $send = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            $script:AdoCalls.Add([ordered]@{
                    method  = $Method
                    url     = $Url
                    headers = $Headers
                    body    = [string] $Body
                })

            if ($null -ne $script:AdoStatusProbePath -and $Method -ceq 'POST' -and $Url -match '/_apis/wit/workitems/\$') {
                $probed = ConvertFrom-JsonVerbatim -Text (Get-Content -LiteralPath $script:AdoStatusProbePath -Raw)
                $script:AdoStatusAtCreation.Add([string] (Get-JsonProperty -Node $probed -Name 'status'))
            }

            Get-AdoFakeResponse -Request ([pscustomobject]@{ Method = $Method; Url = $Url; Body = $Body })
        }

        $wait = {
            param([int] $Milliseconds)

            $script:AdoWaits.Add($Milliseconds)
        }

        New-AdoRestTransport `
            -CollectionUrl ([string] $script:Target['collectionUrl']) `
            -SendRequest $send `
            -GetAccessToken { 'token' } `
            -Wait $wait `
            -MaxAttempts $MaxAttempts
    }

    # Names each recorded request by what it does, which is what makes an ordering assertion legible.
    function Get-AdoOperationSequence {
        $sequence = [System.Collections.Generic.List[string]]::new()
        foreach ($call in $script:AdoCalls) {
            $composite = "$($call['method']) $($call['url'])"

            $name = switch -Regex ($composite) {
                '^POST .*/_apis/wit/workitems/\$' { 'createItem'; break }
                '^GET .*/_apis/wit/workitems/\d+\?fields=' { 'readParent'; break }
                '^GET .*/_apis/wit/workitems/\d+\?' { 'readItem'; break }
                '^GET .*/_apis/wit/workitemtypes/' { 'readType'; break }
                '^GET .*/_apis/wit/workitemtypes\?' { 'readTypeList'; break }
                '^GET .*/_apis/wit/classificationnodes/areas' { 'readArea'; break }
                default { 'other' }
            }
            $sequence.Add($name)
        }
        $sequence.ToArray()
    }

    function Get-AdoCall {
        param([Parameter(Mandatory)][string] $Operation)

        $found = [System.Collections.Generic.List[object]]::new()
        $index = 0
        foreach ($name in (Get-AdoOperationSequence)) {
            if ($name -ceq $Operation) { $found.Add($script:AdoCalls[$index]) }
            $index++
        }
        , $found.ToArray()
    }

    # Hands back the patch document of one creation as the parsed operations it carried.
    function Get-AdoPatchOperation {
        param([int] $Ordinal = 0)

        $call = (Get-AdoCall -Operation 'createItem')[$Ordinal]
        $document = ConvertFrom-JsonVerbatim -Text ([string] $call['body'])
        , ([object[]] $document)
    }

    # The record a read back would produce, so a test about the comparison states only what differs.
    function New-StoredRecord {
        param([AllowNull()][AllowEmptyString()][string] $Format = 'markdown')

        [ordered]@{
            itemId            = '4001'
            itemNodeId        = '4001'
            url               = 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/4001'
            title             = 'Terminate the retry loop'
            body              = 'the body'
            descriptionFormat = $Format
        }
    }

    # --- Findings ---------------------------------------------------------------------------------

    function New-TestFinding {
        param(
            [string] $Id = 'F-1',
            [string] $Origin = 'reviewer',
            [string] $Title = '',
            [string] $Description = ''
        )

        if (-not $Title) { $Title = "Title of $Id" }
        if (-not $Description) { $Description = "Description of $Id" }

        [ordered]@{
            schemaVersion     = 1
            id                = $Id
            sourceFindingIds  = @()
            origin            = $Origin
            title             = $Title
            description       = $Description
            reasoning         = "Reasoning of $Id"
            consequence       = "Consequence of $Id"
            recommendedAction = "Recommended action of $Id"
            references        = @()
            impactedLocations = @()
            anchor            = [ordered]@{ kind = 'line'; path = 'src/app.ps1'; startLine = 3; endLine = 3; side = 'right' }
        }
    }

    # Reads the tracker state the way the composition root does, so every test that files a work item
    # files it against the state the preflight produced rather than one restated by hand.
    #
    # The field is named rather than defaulted, because the default type here carries the repro steps
    # field and the preflight refuses the default on such a type.
    function Get-TestState {
        param(
            [System.Collections.IDictionary] $Transport,
            [string] $WorkItemType = 'Bug',
            [string] $DescriptionField = 'System.Description',
            [int] $ParentWorkItemId = 0
        )

        Get-AdoWorkItemTrackerState `
            -Transport $Transport `
            -Target $script:Target `
            -WorkItemType $WorkItemType `
            -AreaPath $script:AreaPath `
            -DescriptionField $DescriptionField `
            -ParentWorkItemId $ParentWorkItemId
    }

    # Runs a whole publication against the stand-in service and hands back the journal and whatever
    # failure the run raised. A run given an existing journal resumes it rather than starting one, so a
    # test can drive a pause and then a continuation across two passes against the one work item store.
    function Invoke-TestTrackerRun {
        param(
            [Parameter(Mandatory)]
            [object[]] $Item,

            [System.Collections.IDictionary] $Transport = $null,

            [System.Collections.IDictionary] $State = $null,

            [System.Collections.IDictionary] $Result = $null,

            [string] $ResultPath = $null
        )

        if ($null -eq $Transport) { $Transport = New-TestTransport }
        if ($null -eq $State) { $State = Get-TestState -Transport $Transport }

        if ($null -eq $Result) {
            $outputDirectory = Join-Path $TestDrive ([guid]::NewGuid().ToString('n'))
            New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
            $ResultPath = Get-TrackerPublicationResultPath -OutputDirectory $outputDirectory
            $Result = New-TrackerPublicationResult -Target $script:Target -State $State -Item $Item -NowUtc $script:FixedNowUtc
            Write-TrackerPublicationResult -Result $Result -Path $ResultPath
        }

        $failure = $null
        try {
            Invoke-TrackerPublication `
                -Transport $Transport `
                -Target $script:Target `
                -State $State `
                -Item $Item `
                -Result $Result `
                -ResultPath $ResultPath `
                -Wait { param([int] $Milliseconds) $script:AdoWaits.Add($Milliseconds) } | Out-Null
        } catch {
            $failure = $_
        }

        [ordered]@{ result = $Result; resultPath = $ResultPath; transport = $Transport; state = $State; failure = $failure }
    }
}

Describe 'Azure DevOps tracker preflight' {
    BeforeEach { Reset-AdoState }

    It 'reports what every creation needs to know' {
        $state = Get-TestState -Transport (New-TestTransport)

        @($state.Keys) | Should -Be @('platform', 'trackerName', 'createPath', 'readPathPrefix', 'workItemType', 'areaPath', 'descriptionField', 'parent')
        $state['platform'] | Should -BeExactly 'azureDevOps'
        $state['trackerName'] | Should -BeExactly 'contoso/Example Project'
        $state['workItemType'] | Should -BeExactly 'Bug'
        $state['areaPath'] | Should -BeExactly 'Example Project\Widgets\Core'
        $state['descriptionField'] | Should -BeExactly 'System.Description'
        $state['parent'] | Should -BeNullOrEmpty
    }

    It 'reads the type list, the type and the area, in that order' {
        Get-TestState -Transport (New-TestTransport) | Out-Null

        Get-AdoOperationSequence | Should -Be @('readTypeList', 'readType', 'readArea')
    }

    # The composition root passes whatever the command line held, and an unsupplied string parameter
    # holds the empty string, which no parameter default would ever replace. A run that named no
    # description field therefore reaches this function with one that is empty.
    It 'writes the description to the field every process defines when the caller named none' {
        $state = Get-TestState -Transport (New-TestTransport) -WorkItemType 'Task' -DescriptionField ''

        $state['descriptionField'] | Should -BeExactly 'System.Description'
    }

    # A bug type under either standard process defines the description and shows the repro steps, and
    # the service says nothing about which its form shows, so accepting the default here would file a
    # whole review into a field nobody sees and report success.
    It 'refuses the default field on a type that carries the field a bug form shows' {
        { Get-TestState -Transport (New-TestTransport) -WorkItemType 'Bug' -DescriptionField '' } |
            Should -Throw -ExpectedMessage "*carries 'Microsoft.VSTS.TCM.ReproSteps'*Name the field with -DescriptionField*"
    }

    It 'accepts the same field on that type when the caller names it' {
        $state = Get-TestState -Transport (New-TestTransport) -WorkItemType 'Bug' -DescriptionField 'System.Description'

        $state['descriptionField'] | Should -BeExactly 'System.Description'
    }

    It 'writes the description to the field the caller named' {
        $state = Get-TestState -Transport (New-TestTransport) -DescriptionField 'Microsoft.VSTS.TCM.ReproSteps'

        $state['descriptionField'] | Should -BeExactly 'Microsoft.VSTS.TCM.ReproSteps'
    }

    # The service records a type in its own casing, so the journal reports what the project holds
    # rather than what the command line said.
    It 'adopts the spelling the project holds for the type' {
        $state = Get-TestState -Transport (New-TestTransport) -WorkItemType 'bug'

        $state['workItemType'] | Should -BeExactly 'Bug'
    }

    It 'refuses a type the project does not define, naming the ones it does' {
        { Get-TestState -Transport (New-TestTransport) -WorkItemType 'Defect' } |
            Should -Throw -ExpectedMessage "*defines no work item type named 'Defect'. It defines: Bug, Task, User Story.*"
    }

    It 'refuses a description field the type does not carry, naming the field a bug form uses' {
        { Get-TestState -Transport (New-TestTransport) -WorkItemType 'Task' -DescriptionField 'Microsoft.VSTS.TCM.ReproSteps' } |
            Should -Throw -ExpectedMessage "*carries no field 'Microsoft.VSTS.TCM.ReproSteps'*Microsoft.VSTS.TCM.ReproSteps*"
    }

    It 'refuses an area the project does not contain' {
        $transport = New-TestTransport

        {
            Get-AdoWorkItemTrackerState `
                -Transport $transport `
                -Target $script:Target `
                -WorkItemType 'Bug' `
                -AreaPath 'Example Project\Nowhere' `
                -DescriptionField 'System.Description' `
                -ParentWorkItemId 0
        } | Should -Throw -ExpectedMessage "*contains no area 'Example Project\Nowhere'*"
    }

    It 'refuses an area path rooted somewhere other than the project' {
        $transport = New-TestTransport

        {
            Get-AdoWorkItemTrackerState `
                -Transport $transport `
                -Target $script:Target `
                -WorkItemType 'Bug' `
                -AreaPath 'Other\Widgets' `
                -DescriptionField 'System.Description' `
                -ParentWorkItemId 0
        } | Should -Throw -ExpectedMessage "*rooted at 'Other', but the tracker names project 'Example Project'*"
    }

    # The area is asked about below the project root, so the request names the segments beneath it
    # and never the project itself.
    It 'asks about the area below the project root' {
        Get-TestState -Transport (New-TestTransport) | Out-Null

        $url = [string] (Get-AdoCall -Operation 'readArea')[0]['url']
        $url | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_apis/wit/classificationnodes/areas/Widgets/Core?api-version=7.1'
    }

    It 'asks about the project root itself for an area path that names no area below it' {
        $transport = New-TestTransport

        $state = Get-AdoWorkItemTrackerState `
            -Transport $transport `
            -Target $script:Target `
            -WorkItemType 'Bug' `
            -AreaPath 'Example Project' `
            -DescriptionField 'System.Description' `
            -ParentWorkItemId 0

        $state['areaPath'] | Should -BeExactly 'Example Project'
        [string] (Get-AdoCall -Operation 'readArea')[0]['url'] |
            Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_apis/wit/classificationnodes/areas?api-version=7.1'
    }

    # The service records an area in its own casing, and the rebuilt path adopts it for the leaf so
    # the field is written the way the project spells it.
    It 'adopts the spelling the project holds for the area leaf' {
        $script:AdoAreaNode['Widgets/core'] = 'Core'
        $transport = New-TestTransport

        $state = Get-AdoWorkItemTrackerState `
            -Transport $transport `
            -Target $script:Target `
            -WorkItemType 'Bug' `
            -AreaPath 'Example Project\Widgets\core' `
            -DescriptionField 'System.Description' `
            -ParentWorkItemId 0

        $state['areaPath'] | Should -BeExactly 'Example Project\Widgets\Core'
    }
}

Describe 'Azure DevOps parent resolution' {
    BeforeEach { Reset-AdoState }

    It 'reads the parent from the collection root and names only the fields it needs' {
        Get-TestState -Transport (New-TestTransport) -ParentWorkItemId 900 | Out-Null

        [string] (Get-AdoCall -Operation 'readParent')[0]['url'] |
            Should -BeExactly 'https://dev.azure.com/contoso/_apis/wit/workitems/900?fields=System.TeamProject%2CSystem.Title&api-version=7.1'
    }

    It 'carries the service URL the read answered with' {
        $state = Get-TestState -Transport (New-TestTransport) -ParentWorkItemId 900

        @($state['parent'].Keys) | Should -Be @('itemId', 'title', 'url', 'webUrl')
        $state['parent']['itemId'] | Should -BeExactly '900'
        $state['parent']['title'] | Should -BeExactly 'Review debt'
        $state['parent']['url'] | Should -BeExactly 'https://dev.azure.com/contoso/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/900'
        $state['parent']['webUrl'] | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/900'
    }

    It 'refuses a parent that does not exist' {
        { Get-TestState -Transport (New-TestTransport) -ParentWorkItemId 901 } |
            Should -Throw -ExpectedMessage '*Work item 901, named as the parent, could not be read*'
    }

    # Azure DevOps permits a cross-project parent link, and a review filed under a parent nobody
    # working on the project can see is a review nobody reads.
    It 'refuses a parent in another project' {
        $script:AdoParentItem['902'] = [ordered]@{
            project = 'Other Project'
            title   = 'Elsewhere'
            url     = 'https://dev.azure.com/contoso/_apis/wit/workItems/902'
        }

        { Get-TestState -Transport (New-TestTransport) -ParentWorkItemId 902 } |
            Should -Throw -ExpectedMessage "*belongs to project 'Other Project' rather than 'Example Project'*"
    }

    It 'refuses a parent the service reported without its own URL' {
        $script:AdoParentItem['903'] = [ordered]@{ project = 'Example Project'; title = 'No URL'; url = $null }

        { Get-TestState -Transport (New-TestTransport) -ParentWorkItemId 903 } |
            Should -Throw -ExpectedMessage '*without its own URL, so it cannot be linked as a parent*'
    }

    It 'reads the parent only when one was named' {
        Get-TestState -Transport (New-TestTransport) | Out-Null

        Get-AdoOperationSequence | Should -Not -Contain 'readParent'
    }
}

Describe 'Azure DevOps work item creation request' {
    BeforeEach {
        Reset-AdoState
        $script:Items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1' -Title 'Terminate the retry loop')
    }

    It 'sends the title, the description, the area and the format, in that order' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        $operations = Get-AdoPatchOperation
        $operations.Count | Should -Be 4
        foreach ($operation in $operations) { $operation['op'] | Should -BeExactly 'add' }
        $operations[0]['path'] | Should -BeExactly '/fields/System.Title'
        $operations[0]['value'] | Should -BeExactly 'Terminate the retry loop'
        $operations[1]['path'] | Should -BeExactly '/fields/System.Description'
        $operations[1]['value'] | Should -BeExactly ([string] $script:Items[0]['body'])
        $operations[2]['path'] | Should -BeExactly '/fields/System.AreaPath'
        $operations[2]['value'] | Should -BeExactly 'Example Project\Widgets\Core'
        $operations[3]['path'] | Should -BeExactly '/multilineFieldsFormat/System.Description'
        $operations[3]['value'] | Should -BeExactly 'Markdown'
    }

    It 'writes the description and its format to the field the caller named' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport -DescriptionField 'Microsoft.VSTS.TCM.ReproSteps'
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        $operations = Get-AdoPatchOperation
        $operations[1]['path'] | Should -BeExactly '/fields/Microsoft.VSTS.TCM.ReproSteps'
        $operations[3]['path'] | Should -BeExactly '/multilineFieldsFormat/Microsoft.VSTS.TCM.ReproSteps'
    }

    # A body stored as HTML cannot be converted in place, so the opt-in is stated for every item
    # rather than only for a body that happens to carry a construct that renders differently.
    It 'asks for Markdown even for a body that holds no Markdown construct' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        $plain = (New-TrackerItemSet -Finding @(New-TestFinding -Description 'Nothing but words.'))[0]

        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $plain | Out-Null

        (Get-AdoPatchOperation)[3]['value'] | Should -BeExactly 'Markdown'
    }

    # Azure DevOps Markdown recognizes raw HTML, so an angle-delimited placeholder can disappear as
    # an empty element. Encoding only the brackets follows the platform's own MCP implementation and
    # avoids double-encoding entities the approved prose already contains.
    It 'encodes literal angle brackets without double-encoding existing entities' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        $description = 'Use <widget> with Vec<T>; keep &lt;escaped&gt; literal.'
        $item = (New-TrackerItemSet -Finding @(New-TestFinding -Description $description))[0]

        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $item | Out-Null

        $sent = [string] (Get-AdoPatchOperation)[1]['value']
        $sent | Should -BeLike '*Use &lt;widget&gt; with Vec&lt;T&gt;; keep &lt;escaped&gt; literal.*'
        $sent | Should -Not -BeLike '*&amp;lt;escaped&amp;gt;*'
        $sent | Should -Not -Match '[<>]'
    }

    It 'adds the parent relation as a reverse hierarchy link to the URL the parent read reported' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport -ParentWorkItemId 900
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        $operations = Get-AdoPatchOperation
        $operations.Count | Should -Be 5
        $operations[4]['op'] | Should -BeExactly 'add'
        $operations[4]['path'] | Should -BeExactly '/relations/-'
        $relation = $operations[4]['value']
        @($relation.Keys) | Should -Be @('rel', 'url')
        $relation['rel'] | Should -BeExactly 'System.LinkTypes.Hierarchy-Reverse'
        $relation['url'] | Should -BeExactly 'https://dev.azure.com/contoso/1f9c8c6e-0000-0000-0000-000000000000/_apis/wit/workItems/900'
    }

    It 'adds no relation when no parent was named' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        $operations = Get-AdoPatchOperation
        @($operations | Where-Object { [string] $_['path'] -ceq '/relations/-' }).Count | Should -Be 0
    }

    # The service reads the same bytes as a replacement or as a list of operations depending on the
    # media type, and a patch sent as plain JSON is refused.
    It 'sends the patch under the JSON Patch media type' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        $headers = (Get-AdoCall -Operation 'createItem')[0]['headers']
        $headers['Content-Type'] | Should -BeExactly 'application/json-patch+json'

        # The headers a transport hands its sender are only half the journey, because .NET holds the
        # media type on the content rather than on the request. What the service receives is decided
        # by the message the host builds out of them, so that message is built here out of the very
        # headers this run recorded.
        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/wit/workitems' -Headers $headers -Body '[]'
        try {
            $request.Content.Headers.ContentType.MediaType | Should -BeExactly 'application/json-patch+json'
        } finally {
            $request.Dispose()
        }
    }

    It 'reads under the media type every other request uses' {
        Get-TestState -Transport (New-TestTransport) | Out-Null

        (Get-AdoCall -Operation 'readTypeList')[0]['headers'].ContainsKey('Content-Type') | Should -BeFalse
    }

    # The type reaches the path after a literal dollar sign, which is what tells the service the
    # segment names a type rather than an identifier.
    It 'names the type in the path after a literal dollar sign' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        [string] (Get-AdoCall -Operation 'createItem')[0]['url'] |
            Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_apis/wit/workitems/$Bug?api-version=7.2-preview.3'
    }

    It 'escapes a type whose name holds a space' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport -WorkItemType 'User Story'
        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] | Out-Null

        [string] (Get-AdoCall -Operation 'createItem')[0]['url'] |
            Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_apis/wit/workitems/$User%20Story?api-version=7.2-preview.3'
    }

    # The format opt-in exists only in this preview, so the version is stated by the adapter rather
    # than taken from the version the rest of the skill reads pull requests with.
    It 'creates and reads back under the API version that accepts the format opt-in' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        $record = New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0]
        Get-AdoWorkItemRecord -Transport $transport -Target $script:Target -State $state -ItemId ([string] $record['itemId']) | Out-Null

        [string] (Get-AdoCall -Operation 'createItem')[0]['url'] | Should -BeLike '*?api-version=7.2-preview.3'
        [string] (Get-AdoCall -Operation 'readItem')[0]['url'] |
            Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_apis/wit/workitems/4001?api-version=7.2-preview.3'
    }

    It 'sends the rendered body verbatim' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        $description = "A dash $($script:EmDash) and a robot $($script:Robot)."
        $item = (New-TrackerItemSet -Finding @(New-TestFinding -Description $description))[0]

        New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $item | Out-Null

        $sent = [string] (Get-AdoPatchOperation)[1]['value']
        $sent | Should -BeExactly ([string] $item['body'])
        $sent.Contains($description, [StringComparison]::Ordinal) | Should -BeTrue
    }
}

Describe 'Azure DevOps work item record' {
    BeforeEach {
        Reset-AdoState
        $script:Items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1' -Title 'Terminate the retry loop')
    }

    It 'reports the filed work item as the record both adapters return' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport

        $record = New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0]

        @($record.Keys) | Should -Be @('itemId', 'itemNodeId', 'url', 'title', 'body', 'descriptionFormat')
        $record['itemId'] | Should -BeExactly '4001'
        $record['itemNodeId'] | Should -BeExactly '4001'
        $record['url'] | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/4001'
        $record['title'] | Should -BeExactly 'Terminate the retry loop'
        $record['body'] | Should -BeExactly ([string] $script:Items[0]['body'])
        $record['descriptionFormat'] | Should -BeExactly 'markdown'
    }

    It 'refuses an answer that names no work item' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        Add-AdoFault -Pattern '^POST .*/_apis/wit/workitems/\$' -StatusCode 200 -Body '{"fields":{"System.Title":"a title"}}'

        { New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0] } |
            Should -Throw -ExpectedMessage '*without reporting its identifier*'
    }

    # The whole reason the format is read is that the service may have stored HTML, so an absent
    # report is answered with nothing rather than with the format the request asked for.
    It 'reports no format for an answer that carries none' {
        $transport = New-TestTransport
        $state = Get-TestState -Transport $transport
        $script:AdoReportsFormat = $false

        $record = New-AdoWorkItemForFinding -Transport $transport -Target $script:Target -State $state -Item $script:Items[0]

        $record['descriptionFormat'] | Should -BeExactly ''
    }

    It 'reports no format for an answer that names other fields only' {
        $record = ConvertTo-AdoWorkItemRecord `
            -Target $script:Target `
            -State ([ordered]@{ descriptionField = 'System.Description' }) `
            -WorkItem (ConvertFrom-JsonVerbatim -Text '{"id":7,"fields":{"System.Title":"t","System.Description":"b"},"multilineFieldsFormat":{"Microsoft.VSTS.TCM.ReproSteps":"Markdown"}}')

        $record['descriptionFormat'] | Should -BeExactly ''
    }
}

Describe 'Azure DevOps stored item comparison' {
    BeforeEach {
        Reset-AdoState
        $script:Items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1' -Title 'Terminate the retry loop')
    }

    # The request states the casing the service documents and the service answers in its own, so the
    # comparison ignores case rather than refusing an item that is stored exactly as asked.
    It 'accepts the format the service echoes in its own casing' {
        Test-AdoWorkItemMarkdownFormat -Format 'markdown' | Should -BeTrue
        Test-AdoWorkItemMarkdownFormat -Format 'Markdown' | Should -BeTrue
        Test-AdoWorkItemMarkdownFormat -Format 'MARKDOWN' | Should -BeTrue
    }

    It 'refuses any other format' {
        Test-AdoWorkItemMarkdownFormat -Format 'Html' | Should -BeFalse
        Test-AdoWorkItemMarkdownFormat -Format '' | Should -BeFalse
        Test-AdoWorkItemMarkdownFormat -Format $null | Should -BeFalse
    }

    It 'accepts a work item stored as asked' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'the body' }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record (New-StoredRecord) -Platform 'azureDevOps'
        $verdict['verdict'] | Should -BeExactly 'match'
    }

    # A body that itself holds a literal entity, stored with only its line ending changed, is settled by
    # the line-ending comparison before the entity decode, so the decode does not turn the entity into
    # the character it names and make an acceptable reformatting look like a change.
    It 'accepts a line-ending change in a body that holds a literal entity' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = "Escape &lt; in the shell`nand keep going" }
        $record = [ordered]@{
            itemId            = '4001'
            itemNodeId        = '4001'
            url               = 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/4001'
            title             = 'Terminate the retry loop'
            body              = "Escape &lt; in the shell`r`nand keep going"
            descriptionFormat = 'markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record $record -Platform 'azureDevOps'
        $verdict['verdict'] | Should -BeExactly 'match'
    }

    It 'refuses a work item whose description the service stored as HTML' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'the body' }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record (New-StoredRecord -Format 'Html') -Platform 'azureDevOps'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike "*Work item 4001, filed for finding 'F-1', reports the format 'Html' for its description rather than Markdown*"
    }

    It 'refuses a work item that reports no format at all' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'the body' }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record (New-StoredRecord -Format '') -Platform 'azureDevOps'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike '*reports no format at all for its description rather than Markdown*'
    }

    It 'refuses a work item whose description is not the one the run sent' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'a body nobody approved' }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record (New-StoredRecord) -Platform 'azureDevOps'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike '*too far apart to be a reformatting of it*'
    }
}

Describe 'Azure DevOps tracker run' {
    BeforeEach {
        Reset-AdoState
        $script:Items = New-TrackerItemSet -Finding @(
            (New-TestFinding -Id 'F-1' -Title 'First title')
            (New-TestFinding -Id 'F-2' -Title 'Second title')
            (New-TestFinding -Id 'F-3' -Title 'Third title')
        )
    }

    # A filed work item cannot be withdrawn by this run, so each read-back must succeed before the
    # next creation is attempted.
    It 'reads every item back before creating the next' {
        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        $sequence = @(Get-AdoOperationSequence | Where-Object { $_ -cin @('createItem', 'readItem') })
        $sequence |
            Should -Be @('createItem', 'readItem', 'createItem', 'readItem', 'createItem', 'readItem', 'readItem', 'readItem', 'readItem')
    }

    It 'creates nothing further when the first item is not stored as Markdown' {
        $script:AdoStoredFormat = 'Html'

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'].Exception.Message | Should -BeLike "*reports the format 'Html' for its description rather than Markdown*"
        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
    }

    It 'creates nothing further when the first read back reports no format at all' {
        $script:AdoReportsFormat = $false

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'].Exception.Message | Should -BeLike '*reports no format at all for its description rather than Markdown*'
        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
    }

    It 'accepts the format the service echoes in its own casing' {
        $script:AdoStoredFormat = 'markdown'

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        [string] $run['result']['status'] | Should -BeExactly 'succeeded'
    }

    It 'records what the service filed' {
        $run = Invoke-TestTrackerRun -Item $script:Items

        $operations = Get-JsonArrayProperty -Node $run['result'] -Name 'operations'
        $operations[0]['itemId'] | Should -BeExactly '4001'
        $operations[0]['url'] | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/4001'
        $operations[2]['itemId'] | Should -BeExactly '4003'
    }

    # A 4xx is the service stating that it declined the request, so nothing was filed and the record
    # says so.
    It 'records a creation the service refused as failed' {
        Add-AdoFault -Pattern '^POST .*/_apis/wit/workitems/\$' -StatusCode 400 -Body '{"message":"TF401320: Rule Error"}'

        $run = Invoke-TestTrackerRun -Item $script:Items

        $operations = Get-JsonArrayProperty -Node $run['result'] -Name 'operations'
        $operations[0]['outcome'] | Should -BeExactly 'failed'
        $operations[1]['outcome'] | Should -BeExactly 'pending'
    }

    # A 5xx may have been raised after the item was written, so the creation stays unproven.
    It 'records a creation the service faulted on as unproven' {
        Add-AdoFault -Pattern '^POST .*/_apis/wit/workitems/\$' -StatusCode 500

        $run = Invoke-TestTrackerRun -Item $script:Items

        (Get-JsonArrayProperty -Node $run['result'] -Name 'operations')[0]['outcome'] | Should -BeExactly 'ambiguous'
    }

    It 'records a creation whose response was lost as unproven' {
        Add-AdoFault -Pattern '^POST .*/_apis/wit/workitems/\$' -Lost

        $run = Invoke-TestTrackerRun -Item $script:Items

        (Get-JsonArrayProperty -Node $run['result'] -Name 'operations')[0]['outcome'] | Should -BeExactly 'ambiguous'
    }

    # A creation whose response never arrived may have filed the work item, so repeating it is how
    # the same finding gets filed twice.
    It 'attempts a creation exactly once whatever the service answers' {
        Add-AdoFault -Pattern '^POST .*/_apis/wit/workitems/\$' -StatusCode 503

        Invoke-TestTrackerRun -Item $script:Items | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
        $script:AdoWaits.Count | Should -Be 0
    }
}

# Azure DevOps stores a description through a Markdown pipeline that is entitled to reformat it, so a
# stored body that differs from the approved text is judged rather than rejected outright: an
# equivalent body after canonicalization is accepted, a body too far apart is refused, and anything in
# between pauses for a person to accept or reject and then resume.
Describe 'Azure DevOps tracker body tolerance' {
    BeforeEach {
        Reset-AdoState
        $script:Items = New-TrackerItemSet -Finding @(
            (New-TestFinding -Id 'F-1' -Title 'First title' -Description 'Wrap the state in Arc<Mutex<T>> & document why it is shared.')
            (New-TestFinding -Id 'F-2' -Title 'Second title' -Description 'Nothing but words.')
        )
    }

    It 'round-trips encoded brackets alongside an existing entity' {
        $description = 'Use <widget> with Vec<T>; keep &lt;escaped&gt; literal.'
        $items = New-TrackerItemSet -Finding @(
            New-TestFinding -Id 'F-1' -Title 'Preserve the placeholder' -Description $description
        )

        $run = Invoke-TestTrackerRun -Item $items

        $run['failure'] | Should -BeNullOrEmpty
        [string] $run['result']['status'] | Should -BeExactly 'succeeded'
        $stored = Find-AdoStoredWorkItem -ItemId '4001'
        [string] $stored['fields']['System.Description'] |
            Should -BeLike '*Use &lt;widget&gt; with Vec&lt;T&gt;; keep &lt;escaped&gt; literal.*'
    }

    It 'does not accept raw angle brackets as an equivalent stored representation' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Preserve the placeholder'; body = 'Use <widget> here.' }
        $record = New-StoredRecord
        $record['title'] = 'Preserve the placeholder'
        $record['body'] = 'Use <widget> here.'

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record $record -Platform 'azureDevOps'

        $verdict['verdict'] | Should -Not -BeExactly 'match'
    }

    It 'accepts a description the pipeline stored as encoded markup' {
        $sent = [string] $script:Items[0]['body']
        $encoded = [System.Net.WebUtility]::HtmlEncode($sent)
        $encoded | Should -Not -BeExactly $sent
        Add-AdoBodyRewrite -AtCreation 1 -Body $encoded

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        [string] $run['result']['status'] | Should -BeExactly 'succeeded'
    }

    It 'accepts a description the pipeline stored with other line endings' {
        $sent = ConvertTo-AdoWorkItemMarkdownText -Text ([string] $script:Items[0]['body'])
        $reended = ($sent -replace "`r`n", "`n") -replace "`n", "`r`n"
        $reended | Should -Not -BeExactly $sent
        Add-AdoBodyRewrite -AtCreation 1 -Body $reended

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        [string] $run['result']['status'] | Should -BeExactly 'succeeded'
    }

    # A body neither equal nor a recognizable reformatting, but close enough that it might be one, is
    # not the run's to decide. It pauses and records what a person needs to accept or reject it.
    It 'pauses for a decision on a description that only nearly matches' {
        $sent = [string] $script:Items[0]['body']
        $middle = [int] ($sent.Length / 2)
        $nearly = $sent.Substring(0, $middle) + '~' + $sent.Substring($middle + 1)
        Add-AdoBodyRewrite -AtCreation 1 -Body $nearly

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        [string] $run['result']['status'] | Should -BeExactly 'needsAdjudication'

        $adjudication = $run['result']['adjudication']
        $adjudication | Should -Not -BeNullOrEmpty
        [string] $adjudication['itemId'] | Should -BeExactly '4001'
        [string] $adjudication['findingId'] | Should -BeExactly 'F-1'
        [double] $adjudication['similarity'] | Should -BeGreaterThan 0.90
        [string] $adjudication['expectedBodyHash'] | Should -Not -BeNullOrEmpty
        [string] $adjudication['storedBodyHash'] | Should -Not -BeNullOrEmpty
    }

    It 'files nothing further while a decision is pending' {
        $sent = [string] $script:Items[0]['body']
        $middle = [int] ($sent.Length / 2)
        Add-AdoBodyRewrite -AtCreation 1 -Body ($sent.Substring(0, $middle) + '~' + $sent.Substring($middle + 1))

        Invoke-TestTrackerRun -Item $script:Items | Out-Null

        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
    }

    It 'refuses a description too far from the approved text to be a reformatting' {
        Add-AdoBodyRewrite -AtCreation 1 -Body 'an entirely different body nobody approved'

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'].Exception.Message | Should -BeLike '*too far apart to be a reformatting of it*'
        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
    }

    # Accepting the rewrite and continuing files the rest of the review without filing the accepted
    # item a second time, and leaves the run reporting success.
    It 'resumes a paused run once the rewrite is accepted' {
        $sent = [string] $script:Items[0]['body']
        $middle = [int] ($sent.Length / 2)
        Add-AdoBodyRewrite -AtCreation 1 -Body ($sent.Substring(0, $middle) + '~' + $sent.Substring($middle + 1))

        $paused = Invoke-TestTrackerRun -Item $script:Items
        [string] $paused['result']['status'] | Should -BeExactly 'needsAdjudication'
        $adjudication = $paused['result']['adjudication']

        Assert-TrackerContinuationPlan -Result $paused['result'] -Item $script:Items -State $paused['state']
        Set-TrackerPublicationAcceptedRewrite `
            -Result $paused['result'] `
            -ItemId ([string] $adjudication['itemId']) `
            -ExpectedBodyHash ([string] $adjudication['expectedBodyHash']) `
            -StoredBodyHash ([string] $adjudication['storedBodyHash'])
        [string] $paused['result']['status'] | Should -BeExactly 'resumeReady'

        $resumed = Invoke-TestTrackerRun `
            -Item $script:Items `
            -Transport $paused['transport'] `
            -State $paused['state'] `
            -Result $paused['result'] `
            -ResultPath ([string] $paused['resultPath'])

        $resumed['failure'] | Should -BeNullOrEmpty
        [string] $resumed['result']['status'] | Should -BeExactly 'succeeded'

        # F-1 was filed by the first pass and F-2 by the second; the accepted item is not filed again.
        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 2
        $operations = Get-JsonArrayProperty -Node $resumed['result'] -Name 'operations'
        $operations[0]['itemId'] | Should -BeExactly '4001'
        $operations[1]['itemId'] | Should -BeExactly '4002'
    }

    # A resumed run leaves its resumable state and persists that before it files, so a process killed
    # mid-creation is read back as one whose outcome is unknown - which the rerun guard refuses -
    # rather than one still resumable that a rerun would file a second time.
    It 'persists that it is no longer resumable before filing the next item' {
        $sent = [string] $script:Items[0]['body']
        $middle = [int] ($sent.Length / 2)
        Add-AdoBodyRewrite -AtCreation 1 -Body ($sent.Substring(0, $middle) + '~' + $sent.Substring($middle + 1))

        $paused = Invoke-TestTrackerRun -Item $script:Items
        $adjudication = $paused['result']['adjudication']
        Set-TrackerPublicationAcceptedRewrite `
            -Result $paused['result'] `
            -ItemId ([string] $adjudication['itemId']) `
            -ExpectedBodyHash ([string] $adjudication['expectedBodyHash']) `
            -StoredBodyHash ([string] $adjudication['storedBodyHash'])
        [string] $paused['result']['status'] | Should -BeExactly 'resumeReady'
        # The composition root persists the acceptance before resuming; mirror that so the journal on
        # disk is resumable when the resume begins.
        Write-TrackerPublicationResult -Result $paused['result'] -Path ([string] $paused['resultPath'])

        $script:AdoStatusProbePath = [string] $paused['resultPath']
        Invoke-TestTrackerRun `
            -Item $script:Items `
            -Transport $paused['transport'] `
            -State $paused['state'] `
            -Result $paused['result'] `
            -ResultPath ([string] $paused['resultPath']) | Out-Null

        # The one creation the resume made is F-2, and the journal on disk had already left resumeReady
        # when it was sent.
        @($script:AdoStatusAtCreation) | Should -Be @('inProgress')
    }

    # An accepted rewrite is bound to the exact bytes it was accepted against, so a description that
    # changed again after acceptance is caught on resume rather than filed against a stale decision.
    It 'refuses to resume when the accepted description has since changed' {
        $sent = [string] $script:Items[0]['body']
        $middle = [int] ($sent.Length / 2)
        Add-AdoBodyRewrite -AtCreation 1 -Body ($sent.Substring(0, $middle) + '~' + $sent.Substring($middle + 1))

        $paused = Invoke-TestTrackerRun -Item $script:Items
        $adjudication = $paused['result']['adjudication']
        Set-TrackerPublicationAcceptedRewrite `
            -Result $paused['result'] `
            -ItemId ([string] $adjudication['itemId']) `
            -ExpectedBodyHash ([string] $adjudication['expectedBodyHash']) `
            -StoredBodyHash ([string] $adjudication['storedBodyHash'])

        # The service reformats the stored description a second time, after the decision was recorded.
        $storedItem = Find-AdoStoredWorkItem -ItemId '4001'
        foreach ($field in @($storedItem['multilineFieldsFormat'].Keys)) {
            Set-JsonProperty -Node $storedItem['fields'] -Name $field -Value 'a description changed after the rewrite was accepted'
        }

        $resumed = Invoke-TestTrackerRun `
            -Item $script:Items `
            -Transport $paused['transport'] `
            -State $paused['state'] `
            -Result $paused['result'] `
            -ResultPath ([string] $paused['resultPath'])

        $resumed['failure'].Exception.Message | Should -BeLike '*no longer holds the body the accepted rewrite was recorded against*'
        # Only the first pass filed anything; the changed item stops the resume before F-2.
        @(Get-AdoOperationSequence | Where-Object { $_ -ceq 'createItem' }).Count | Should -Be 1
    }
}

# The transport hands its sender a method, a URL, a header table and a body, and the host turns
# those into the message a service actually receives. Nothing else in the suite crosses that
# boundary, because every other test injects a sender of its own; the media type in particular is
# held on the content rather than on the request, so it cannot be carried across with the headers
# and a mistake there is invisible until a service refuses the write.
Describe 'Azure DevOps request message' {
    It 'sends a JSON Patch body under the media type the transport named' {
        $headers = Get-AdoRestRequestHeader -Token 'token' -ContentType 'application/json-patch+json' -HasBody

        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/wit/workitems' -Headers $headers -Body '[]'
        try {
            $request.Content.Headers.ContentType.MediaType | Should -BeExactly 'application/json-patch+json'
        } finally {
            $request.Dispose()
        }
    }

    It 'sends every other body under plain JSON' {
        $headers = Get-AdoRestRequestHeader -Token 'token' -HasBody

        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/wit/wiql' -Headers $headers -Body '{}'
        try {
            $request.Content.Headers.ContentType.MediaType | Should -BeExactly 'application/json'
        } finally {
            $request.Dispose()
        }
    }

    It 'sends a body under plain JSON when the caller named no media type' {
        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/x' -Headers @{ 'Accept' = 'application/json' } -Body '{}'
        try {
            $request.Content.Headers.ContentType.MediaType | Should -BeExactly 'application/json'
        } finally {
            $request.Dispose()
        }
    }

    # The bytes are UTF-8 whatever the caller declared, so the declaration is taken from the encoding
    # rather than from the caller and the two cannot disagree.
    It 'declares the encoding the body was written in' {
        $request = New-HttpRequestMessage `
            -Method 'POST' `
            -Url 'https://dev.azure.com/contoso/_apis/x' `
            -Headers (Get-AdoRestRequestHeader -Token 'token' -ContentType 'application/json-patch+json' -HasBody) `
            -Body '[]'
        try {
            $request.Content.Headers.ContentType.CharSet | Should -BeExactly 'utf-8'
        } finally {
            $request.Dispose()
        }
    }

    It 'writes the body as UTF-8 without a byte order mark' {
        $body = "[{`"value`":`"$($script:EmDash)$($script:Robot)`"}]"

        $request = New-HttpRequestMessage `
            -Method 'POST' `
            -Url 'https://dev.azure.com/contoso/_apis/x' `
            -Headers (Get-AdoRestRequestHeader -Token 'token' -ContentType 'application/json-patch+json' -HasBody) `
            -Body $body
        try {
            $bytes = $request.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
            $expected = [System.Text.UTF8Encoding]::new($false).GetBytes($body)
            ($bytes -join ',') | Should -BeExactly ($expected -join ',')
        } finally {
            $request.Dispose()
        }
    }

    It 'carries the credential and the accepted type onto the request' {
        $headers = Get-AdoRestRequestHeader -Token 'token' -ContentType 'application/json-patch+json' -HasBody

        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/x' -Headers $headers -Body '[]'
        try {
            ($request.Headers.GetValues('Authorization') -join '') | Should -BeExactly ([string] $headers['Authorization'])
            ($request.Headers.GetValues('Accept') -join '') | Should -BeExactly 'application/json'
        } finally {
            $request.Dispose()
        }
    }

    # A content header is refused by the request's own collection, so copying it there would either
    # throw or be dropped, and the media type would be lost with it.
    It 'keeps a content header off the request' {
        $headers = Get-AdoRestRequestHeader -Token 'token' -ContentType 'application/json-patch+json' -HasBody

        $request = New-HttpRequestMessage -Method 'POST' -Url 'https://dev.azure.com/contoso/_apis/x' -Headers $headers -Body '[]'
        try {
            @($request.Headers | ForEach-Object { [string] $_.Key }) | Should -Not -Contain 'Content-Type'
        } finally {
            $request.Dispose()
        }
    }

    # A read names no body, and a body it never asked for is what a service sees when the check for
    # one is written against $null: PowerShell hands a [string] parameter the empty string instead.
    It 'sends no content for a request that carries no body' {
        $headers = Get-AdoRestRequestHeader -Token 'token'

        $request = New-HttpRequestMessage -Method 'GET' -Url 'https://dev.azure.com/contoso/_apis/x' -Headers $headers -Body $null
        try {
            $request.Content | Should -BeNullOrEmpty
            ($request.Headers.GetValues('Authorization') -join '') | Should -BeExactly ([string] $headers['Authorization'])
        } finally {
            $request.Dispose()
        }
    }

    It 'names the method and the URL it was given' {
        $request = New-HttpRequestMessage -Method 'PATCH' -Url 'https://dev.azure.com/contoso/_apis/x?api-version=7.1' -Headers $null -Body $null
        try {
            $request.Method.Method | Should -BeExactly 'PATCH'
            $request.RequestUri.AbsoluteUri | Should -BeExactly 'https://dev.azure.com/contoso/_apis/x?api-version=7.1'
        } finally {
            $request.Dispose()
        }
    }
}
