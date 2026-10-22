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
    . (Join-Path $script:LibraryRoot 'TrackerPublication.ps1')

    # Characters are built from their code points so the fixtures never depend on how this file is
    # encoded on disk, and so the assertions compare code points rather than rendered glyphs.
    $script:EmDash = [string] [char]0x2014
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)

    # Every transport built here reads this instant instead of a clock, so nothing in this file
    # depends on when it runs.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    $script:Target = [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = 'octo'
        project       = $null
        repository    = 'widget'
        webUrl        = 'https://github.com/octo/widget/issues'
    }

    # --- The stand-in service ---------------------------------------------------------------------

    function New-GhResponse {
        param(
            [string] $Body = '',
            [int] $ExitCode = 0,
            [string] $StandardError = ''
        )

        [ordered]@{
            exitCode            = $ExitCode
            standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($Body)
            standardErrorText   = $StandardError
        }
    }

    # Registers a canned reply for every invocation whose standard input contains the key. Later
    # registrations win, so a test overrides a default fixture by registering again. Supplying
    # several replies serves them in order and then repeats the last, which is what lets a retried
    # request be expressed without a bespoke fake per test.
    function Add-GhRoute {
        param(
            [Parameter(Mandatory)]
            [string] $Key,

            [Parameter(Mandatory)]
            [object[]] $Response
        )

        $script:GhRoutes.Add([ordered]@{
                key       = $Key
                responses = [System.Collections.Generic.List[object]]::new([object[]] $Response)
            })
    }

    function New-TestTransport {
        param([int] $MaxAttempts = 5)

        $routingInvoker = {
            param(
                [string[]] $ArgumentList,
                [string] $StandardInput
            )

            $script:GhCalls.Add([ordered]@{
                    arguments     = @($ArgumentList)
                    standardInput = [string] $StandardInput
                })

            $composite = ($ArgumentList -join ' ') + ' ' + [string] $StandardInput
            for ($index = $script:GhRoutes.Count - 1; $index -ge 0; $index--) {
                $route = $script:GhRoutes[$index]
                if (-not $composite.Contains([string] $route['key'])) { continue }
                $responses = $route['responses']
                if ($responses.Count -eq 0) {
                    throw "The fake GitHub CLI ran out of replies for '$($route['key'])'."
                }
                $response = $responses[0]
                if ($responses.Count -gt 1) { $responses.RemoveAt(0) }
                return $response
            }

            throw "The fake GitHub CLI has no reply for 'gh $($ArgumentList -join ' ')'."
        }

        $recordingWait = {
            param([int] $Milliseconds)

            $script:GhWaits.Add($Milliseconds)
        }

        $clock = { $script:FixedNowUtc }

        New-GitHubCliTransport `
            -InvokeGh $routingInvoker `
            -Wait $recordingWait `
            -GetUtcNow $clock `
            -MaxAttempts $MaxAttempts
    }

    function Reset-GhState {
        $script:GhRoutes = [System.Collections.Generic.List[object]]::new()
        $script:GhCalls = [System.Collections.Generic.List[object]]::new()
        $script:GhWaits = [System.Collections.Generic.List[int]]::new()
    }

    # Each GraphQL document carries a fragment no other document carries, which is what lets the
    # routing table stand in for a service without parsing the query.
    $script:RepositoryKey = 'repository(owner:'
    $script:CreateKey = 'createIssue(input:'
    $script:VerifyKey = 'node(id:$issueId)'

    function Get-GhOperationSequence {
        $sequence = [System.Collections.Generic.List[string]]::new()
        foreach ($call in $script:GhCalls) {
            $text = [string] $call['standardInput']
            $name = switch -Regex ($text) {
                'createIssue\(input:' { 'createIssue'; break }
                'node\(id:\$issueId\)' { 'readIssue'; break }
                'repository\(owner:' { 'readRepository'; break }
                default { 'other' }
            }
            $sequence.Add($name)
        }
        $sequence.ToArray()
    }

    # Hands back the variables of every call of one kind, in the order they were sent.
    function Get-GhCallVariable {
        param([Parameter(Mandatory)][string] $Operation)

        $found = [System.Collections.Generic.List[object]]::new()
        $index = 0
        foreach ($name in (Get-GhOperationSequence)) {
            if ($name -ceq $Operation) {
                $payload = ConvertFrom-JsonVerbatim -Text ([string] $script:GhCalls[$index]['standardInput'])
                $found.Add((Get-JsonProperty -Node $payload -Name 'variables'))
            }
            $index++
        }
        , $found.ToArray()
    }

    function New-RepositoryJson {
        param(
            [string] $Id = 'R_kgDOwidget',
            [string] $NameWithOwner = 'octo/widget',
            [bool] $HasIssuesEnabled = $true,
            [bool] $IsArchived = $false,
            [string] $ViewerPermission = 'WRITE'
        )

        ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                data = [ordered]@{
                    repository = [ordered]@{
                        id               = $Id
                        nameWithOwner    = $NameWithOwner
                        hasIssuesEnabled = $HasIssuesEnabled
                        isArchived       = $IsArchived
                        viewerPermission = $ViewerPermission
                    }
                }
            })
    }

    function New-IssueJson {
        param(
            [Parameter(Mandatory)]
            [string] $Root,

            [string] $NodeId = 'I_issue1',
            $Number = 101,
            [string] $Url = 'https://github.com/octo/widget/issues/101',
            [string] $Title = 'Terminate the retry loop',
            [string] $Body = 'the body'
        )

        $issue = [ordered]@{
            id     = $NodeId
            number = $Number
            url    = $Url
            title  = $Title
            body   = $Body
        }

        $data = if ($Root -ceq 'createIssue') {
            [ordered]@{ createIssue = [ordered]@{ issue = $issue } }
        } else {
            [ordered]@{ node = $issue }
        }

        ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{ data = $data })
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

    function New-TestState {
        param([string] $RepositoryId = 'R_kgDOwidget')

        [ordered]@{
            platform     = 'gitHub'
            repositoryId = $RepositoryId
            trackerName  = 'octo/widget'
        }
    }

    # Runs a whole publication against the stand-in service and hands back the journal and whatever
    # failure the run raised.
    function Invoke-TestTrackerRun {
        param([Parameter(Mandatory)][object[]] $Item)

        $transport = New-TestTransport
        $outputDirectory = Join-Path $TestDrive ([guid]::NewGuid().ToString('n'))
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
        $resultPath = Get-TrackerPublicationResultPath -OutputDirectory $outputDirectory
        $state = Get-GitHubIssueTrackerState -Transport $transport -Target $script:Target
        $result = New-TrackerPublicationResult -Target $script:Target -State $state -Item $Item -NowUtc $script:FixedNowUtc
        Write-TrackerPublicationResult -Result $result -Path $resultPath

        $failure = $null
        try {
            Invoke-TrackerPublication `
                -Transport $transport `
                -Target $script:Target `
                -State $state `
                -Item $Item `
                -Result $result `
                -ResultPath $resultPath `
                -Wait { param([int] $Milliseconds) $script:GhWaits.Add($Milliseconds) } | Out-Null
        } catch {
            $failure = $_
        }

        [ordered]@{ result = $result; resultPath = $resultPath; failure = $failure }
    }
}

Describe 'GitHub tracker preflight' {
    BeforeEach { Reset-GhState }

    It 'reports the identifier every creation addresses the repository by' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson))

        $state = Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target

        @($state.Keys) | Should -Be @('platform', 'repositoryId', 'trackerName')
        $state['platform'] | Should -BeExactly 'gitHub'
        $state['repositoryId'] | Should -BeExactly 'R_kgDOwidget'
        $state['trackerName'] | Should -BeExactly 'octo/widget'
    }

    It 'names the repository by the coordinates the target holds' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson))

        Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target | Out-Null

        $variables = (Get-GhCallVariable -Operation 'readRepository')[0]
        # The transport declares its variables as a hashtable, so the request preserves no key order
        # and the assertion is about which variables were sent rather than about their sequence.
        @($variables.Keys) | Sort-Object | Should -Be @('owner', 'repo')
        $variables['owner'] | Should -BeExactly 'octo'
        $variables['repo'] | Should -BeExactly 'widget'
    }

    It 'refuses a repository the service does not report' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body '{"data":{"repository":null}}')

        { Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage "*missing 'data.repository'*"
    }

    It 'refuses a repository reported without an identifier' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -Id ''))

        { Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage "*without an identifier*"
    }

    It 'refuses an archived repository' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -IsArchived $true))

        { Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage "*'octo/widget' is archived*"
    }

    It 'refuses a repository with issues disabled' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -HasIssuesEnabled $false))

        { Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage "*has issues disabled*"
    }

    It 'refuses a repository the account has no access to' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -ViewerPermission 'NONE'))

        { Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target } |
            Should -Throw -ExpectedMessage "*no access to 'octo/widget'*"
    }

    # An account that may open an issue in a public repository is reported as READ, so a permission
    # check stricter than the refused list would turn away a case the service accepts.
    It 'accepts a repository the account only reads' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -ViewerPermission 'READ'))

        $state = Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target

        $state['repositoryId'] | Should -BeExactly 'R_kgDOwidget'
    }

    It 'reads the repository once' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson))

        Get-GitHubIssueTrackerState -Transport (New-TestTransport) -Target $script:Target | Out-Null

        Get-GhOperationSequence | Should -Be @('readRepository')
    }
}

Describe 'GitHub issue creation' {
    BeforeEach { Reset-GhState }

    It 'sends the repository identifier, the title and the body, and nothing else' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body (New-IssueJson -Root 'createIssue'))
        $item = (New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-9' -Title 'Terminate the retry loop'))[0]

        New-GitHubIssueForFinding -Transport (New-TestTransport) -State (New-TestState) -Item $item | Out-Null

        $variables = (Get-GhCallVariable -Operation 'createIssue')[0]
        @($variables.Keys) | Sort-Object | Should -Be @('body', 'repositoryId', 'title')
        $variables['repositoryId'] | Should -BeExactly 'R_kgDOwidget'
        $variables['title'] | Should -BeExactly 'Terminate the retry loop'
        $variables['body'] | Should -BeExactly ([string] $item['body'])
    }

    # The mutation addresses the repository by node identifier, and the only place that identifier
    # comes from is the preflight read, so a run that skipped the read would have nothing to send.
    It 'addresses the repository the preflight read reported' {
        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson -Id 'R_other'))
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body (New-IssueJson -Root 'createIssue'))
        $transport = New-TestTransport

        $state = Get-GitHubIssueTrackerState -Transport $transport -Target $script:Target
        New-GitHubIssueForFinding `
            -Transport $transport `
            -State $state `
            -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0] | Out-Null

        ((Get-GhCallVariable -Operation 'createIssue')[0])['repositoryId'] | Should -BeExactly 'R_other'
    }

    It 'sends the rendered body verbatim' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body (New-IssueJson -Root 'createIssue'))
        $description = "A dash $($script:EmDash) and a robot $($script:Robot)."
        $item = (New-TrackerItemSet -Finding @(New-TestFinding -Description $description))[0]

        New-GitHubIssueForFinding -Transport (New-TestTransport) -State (New-TestState) -Item $item | Out-Null

        $sent = [string] ((Get-GhCallVariable -Operation 'createIssue')[0])['body']
        $sent | Should -BeExactly ([string] $item['body'])
        $sent.Contains($description, [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'reports the filed issue as the record both adapters return' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body (New-IssueJson `
                    -Root 'createIssue' `
                    -NodeId 'I_issue1' `
                    -Number 101 `
                    -Url 'https://github.com/octo/widget/issues/101' `
                    -Title 'Terminate the retry loop' `
                    -Body 'the body'))

        $record = New-GitHubIssueForFinding `
            -Transport (New-TestTransport) `
            -State (New-TestState) `
            -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]

        @($record.Keys) | Should -Be @('itemId', 'itemNodeId', 'url', 'title', 'body', 'descriptionFormat')
        $record['itemId'] | Should -BeExactly '101'
        $record['itemNodeId'] | Should -BeExactly 'I_issue1'
        $record['url'] | Should -BeExactly 'https://github.com/octo/widget/issues/101'
        $record['title'] | Should -BeExactly 'Terminate the retry loop'
        $record['body'] | Should -BeExactly 'the body'
        $record['descriptionFormat'] | Should -BeExactly 'Markdown'
    }

    It 'refuses an answer that names no issue number' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        data = [ordered]@{ createIssue = [ordered]@{ issue = [ordered]@{ id = 'I_1'; url = 'u'; title = 't'; body = 'b' } } }
                    })))

        {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } | Should -Throw -ExpectedMessage '*without reporting its number*'
    }
}

Describe 'GitHub issue creation failure' {
    BeforeEach { Reset-GhState }

    # A creation whose response never arrived may have filed the issue, so repeating it is how the
    # same finding gets filed twice. The transport is given a failure it would otherwise retry, and
    # the point of the assertion is that it did not.
    It 'attempts a creation whose response was lost exactly once' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -ExitCode 1 -StandardError 'connection reset by peer')

        $failure = $null
        try {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport -MaxAttempts 5) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } catch {
            $failure = $_
        }

        $failure | Should -Not -BeNullOrEmpty
        Get-GhOperationSequence | Should -Be @('createIssue')
        $script:GhWaits.Count | Should -Be 0
    }

    It 'leaves a creation whose response was lost unproven' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -ExitCode 1 -StandardError 'connection reset by peer')

        $failure = $null
        try {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } catch {
            $failure = $_
        }

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'ambiguous'
    }

    # Parsing, validation and coercion failures answer with errors and no data entry at all, which
    # proves no resolver ran and therefore that nothing was filed.
    It 'records a creation the service refused before execution as failed' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse `
                -ExitCode 1 `
                -Body '{"errors":[{"message":"Body is too long (maximum is 65536 characters)"}]}')

        $failure = $null
        try {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } catch {
            $failure = $_
        }

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'failed'
        $failure.Exception.Message | Should -BeLike '*refused by the service*'
    }

    # A response carrying a data entry proves execution began, and a mutation resolver may apply its
    # side effect before an execution error is reported, so the creation stays unproven.
    It 'leaves a creation that failed during execution unproven' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse `
                -Body '{"data":{"createIssue":null},"errors":[{"message":"Something went wrong while executing your query."}]}')

        $failure = $null
        try {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } catch {
            $failure = $_
        }

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'ambiguous'
        $failure.Exception.Message | Should -BeLike '*returned service errors*'
    }

    It 'refuses a creation the service answered with errors and no data at exit code zero' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -Body '{"errors":[{"message":"Resource not accessible"}]}')

        $failure = $null
        try {
            New-GitHubIssueForFinding `
                -Transport (New-TestTransport) `
                -State (New-TestState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding))[0]
        } catch {
            $failure = $_
        }

        Get-PublicationFailureOutcome -Exception $failure.Exception | Should -BeExactly 'failed'
    }
}

Describe 'GitHub issue read back' {
    BeforeEach { Reset-GhState }

    It 'reads the issue by the identifier the creation answered with' {
        Add-GhRoute -Key $script:VerifyKey -Response @(New-GhResponse -Body (New-IssueJson -Root 'node' -NodeId 'I_issue1'))

        Get-GitHubIssueRecord -Transport (New-TestTransport) -ItemNodeId 'I_issue1' | Out-Null

        $variables = (Get-GhCallVariable -Operation 'readIssue')[0]
        @($variables.Keys) | Should -Be @('issueId')
        $variables['issueId'] | Should -BeExactly 'I_issue1'
    }

    It 'reports what the service stored' {
        Add-GhRoute -Key $script:VerifyKey -Response @(New-GhResponse -Body (New-IssueJson `
                    -Root 'node' -NodeId 'I_issue1' -Number 101 -Title 'a title' -Body 'a body'))

        $record = Get-GitHubIssueRecord -Transport (New-TestTransport) -ItemNodeId 'I_issue1'

        $record['itemId'] | Should -BeExactly '101'
        $record['title'] | Should -BeExactly 'a title'
        $record['body'] | Should -BeExactly 'a body'
        $record['descriptionFormat'] | Should -BeExactly 'Markdown'
    }

    # A read that failed cannot have filed anything, so it keeps the retry budget the creation gives
    # up.
    It 'retries a read whose response was lost' {
        Add-GhRoute -Key $script:VerifyKey -Response @(
            (New-GhResponse -ExitCode 1 -StandardError 'connection reset by peer')
            (New-GhResponse -Body (New-IssueJson -Root 'node'))
        )

        Get-GitHubIssueRecord -Transport (New-TestTransport) -ItemNodeId 'I_issue1' | Out-Null

        Get-GhOperationSequence | Should -Be @('readIssue', 'readIssue')
        $script:GhWaits.Count | Should -Be 1
    }
}

Describe 'GitHub stored item comparison' {
    BeforeEach {
        Reset-GhState
        $script:Item = (New-TrackerItemSet -Finding @(New-TestFinding -Title 'Terminate the retry loop'))[0]
    }

    It 'accepts an issue holding exactly what was sent' {
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_issue1'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = [string] $script:Item['title']
            body              = [string] $script:Item['body']
            descriptionFormat = 'Markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $script:Item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'match'
    }

    It 'refuses an issue whose body is not the one the run sent' {
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_issue1'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = [string] $script:Item['title']
            body              = [string] $script:Item['body'] + ' and something nobody approved'
            descriptionFormat = 'Markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $script:Item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike "*Item 101, filed for finding 'F-1', holds a body the run did not send*"
    }

    It 'refuses an issue whose title is not the one the run sent' {
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_issue1'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = 'Terminate the Retry Loop'
            body              = [string] $script:Item['body']
            descriptionFormat = 'Markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $script:Item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike '*holds a title the run did not send*'
    }

    # GitHub renders every issue body as Markdown and offers no alternative, so the format the record
    # states is never a reason to refuse an issue.
    It 'settles a GitHub issue on its text alone' {
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_issue1'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = [string] $script:Item['title']
            body              = [string] $script:Item['body']
            descriptionFormat = ''
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $script:Item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'match'
    }
}

Describe 'GitHub tracker run' {
    BeforeEach {
        Reset-GhState
        $script:Items = New-TrackerItemSet -Finding @(
            (New-TestFinding -Id 'F-1' -Title 'First title')
            (New-TestFinding -Id 'F-2' -Title 'Second title')
        )

        Add-GhRoute -Key $script:RepositoryKey -Response @(New-GhResponse -Body (New-RepositoryJson))
        Add-GhRoute -Key $script:CreateKey -Response @(
            (New-GhResponse -Body (New-IssueJson -Root 'createIssue' -NodeId 'I_1' -Number 101 -Title 'First title' -Body ([string] $script:Items[0]['body'])))
            (New-GhResponse -Body (New-IssueJson -Root 'createIssue' -NodeId 'I_2' -Number 102 -Title 'Second title' -Body ([string] $script:Items[1]['body'])))
        )
        Add-GhRoute -Key $script:VerifyKey -Response @(
            (New-GhResponse -Body (New-IssueJson -Root 'node' -NodeId 'I_1' -Number 101 -Title 'First title' -Body ([string] $script:Items[0]['body'])))
            (New-GhResponse -Body (New-IssueJson -Root 'node' -NodeId 'I_2' -Number 102 -Title 'Second title' -Body ([string] $script:Items[1]['body'])))
            (New-GhResponse -Body (New-IssueJson -Root 'node' -NodeId 'I_1' -Number 101 -Title 'First title' -Body ([string] $script:Items[0]['body'])))
            (New-GhResponse -Body (New-IssueJson -Root 'node' -NodeId 'I_2' -Number 102 -Title 'Second title' -Body ([string] $script:Items[1]['body'])))
        )
    }

    It 'reads the repository, files each item and reads every one back' {
        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -BeNullOrEmpty
        Get-GhOperationSequence |
            Should -Be @('readRepository', 'createIssue', 'readIssue', 'createIssue', 'readIssue', 'readIssue', 'readIssue')
    }

    It 'files the items in the order the review presents them' {
        Invoke-TestTrackerRun -Item $script:Items | Out-Null

        $titles = foreach ($variables in (Get-GhCallVariable -Operation 'createIssue')) { [string] $variables['title'] }
        $titles | Should -Be @('First title', 'Second title')
    }

    It 'records what the service filed' {
        $run = Invoke-TestTrackerRun -Item $script:Items

        $operations = Get-JsonArrayProperty -Node $run['result'] -Name 'operations'
        $operations[0]['outcome'] | Should -BeExactly 'succeeded'
        $operations[0]['itemId'] | Should -BeExactly '101'
        $operations[0]['itemNodeId'] | Should -BeExactly 'I_1'
        $operations[0]['url'] | Should -BeExactly 'https://github.com/octo/widget/issues/101'
        $operations[1]['itemId'] | Should -BeExactly '102'
    }

    It 'records a creation whose response was lost as unproven and files nothing further' {
        Add-GhRoute -Key $script:CreateKey -Response @(New-GhResponse -ExitCode 1 -StandardError 'connection reset by peer')

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'] | Should -Not -BeNullOrEmpty
        $operations = Get-JsonArrayProperty -Node $run['result'] -Name 'operations'
        $operations[0]['outcome'] | Should -BeExactly 'ambiguous'
        $operations[1]['outcome'] | Should -BeExactly 'pending'
        Get-GhOperationSequence | Should -Be @('readRepository', 'createIssue')
    }

    It 'stops before the second item when the first is not stored as sent' {
        Add-GhRoute -Key $script:VerifyKey -Response @(New-GhResponse -Body (New-IssueJson `
                    -Root 'node' -NodeId 'I_1' -Number 101 -Title 'First title' -Body 'a body nobody approved'))

        $run = Invoke-TestTrackerRun -Item $script:Items

        $run['failure'].Exception.Message | Should -BeLike '*holds a body the run did not send*'
        Get-GhOperationSequence | Should -Be @('readRepository', 'createIssue', 'readIssue')
    }
}
