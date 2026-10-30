BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in the order the
    # composition root uses: value handling and failure signaling first, then the target
    # coordinates, each transport with the adapter that uses it, the shared models and the item
    # rendering, and finally the publication rules both destinations share and the tracker run
    # itself. PullRequestPublication.ps1 is loaded as well, because the artifact a tracker accepts
    # is exactly the one publication onto a pull request refuses.
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
    . (Join-Path $script:LibraryRoot 'NativeWorkflowBridge.ps1')
    . (Join-Path $script:LibraryRoot 'FindingMarkdown.ps1')
    . (Join-Path $script:LibraryRoot 'TrackerItem.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')
    . (Join-Path $script:LibraryRoot 'ReviewPublication.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPublication.ps1')
    . (Join-Path $script:LibraryRoot 'TrackerPublication.ps1')

    # The host running this suite stands in for the program the executable tests launch. Naming the
    # running executable rather than the command 'pwsh' keeps the child on the same build as the
    # parent and removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # Nothing in this file reads a clock. Every record that carries an instant is given this one, so
    # a run at any moment produces the same document.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 8, 4, 12, 0, 0, [TimeSpan]::Zero)

    $script:SourceCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    $script:MergeBaseCommit = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'

    $script:GitHubTracker = [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = 'octo'
        project       = $null
        repository    = 'widget'
        webUrl        = 'https://github.com/octo/widget/issues'
    }

    $script:AzureDevOpsTracker = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = 'https://dev.azure.com/contoso'
        organization  = 'contoso'
        project       = 'widgets'
        repository    = $null
        webUrl        = 'https://dev.azure.com/contoso/widgets/_workitems'
    }

    # --- Fixtures --------------------------------------------------------------------------------

    # Builds a stored finding record, as an approved artifact holds one.
    function New-TestFinding {
        param(
            [string] $Id = 'F-1',
            [string] $Origin = 'reviewer',
            [string] $Title = '',
            [string] $Description = '',
            [string] $FollowUpThreadId = '',
            [string] $FollowUpCommentId = ''
        )

        if (-not $Title) { $Title = "Title of $Id" }
        if (-not $Description) { $Description = "Description of $Id" }

        $finding = [ordered]@{
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

        if ($FollowUpThreadId) {
            $finding['followUp'] = [ordered]@{ threadId = $FollowUpThreadId; commentId = $FollowUpCommentId }
        }

        $finding
    }

    function New-TestApproved {
        param(
            [object[]] $Finding = @(),
            $Context = $null
        )

        if ($Finding.Count -eq 0) { $Finding = @(New-TestFinding) }

        [ordered]@{
            schemaVersion           = 2
            target                  = if ($null -eq $Context) { $null } else { $Context['target'] }
            reviewedSourceCommit    = if ($null -eq $Context) { $null } else { $Context['pullRequest']['reviewedSourceCommit'] }
            reviewedMergeBaseCommit = if ($null -eq $Context) { $null } else { $Context['pullRequest']['reviewedMergeBaseCommit'] }
            reviewedContextHash     = if ($null -eq $Context) { $null } else { Get-PullRequestEvidenceHash -Document $Context }
            findings                = @($Finding)
        }
    }

    # Builds one captured comment, carrying the browser link a tracker item points back at.
    function New-TestComment {
        param(
            [Parameter(Mandatory)]
            [string] $Id,

            [AllowEmptyString()]
            [AllowNull()]
            $Url = $null
        )

        if ($null -eq $Url) { $Url = "https://example.invalid/comment/$Id" }

        [ordered]@{
            id                 = $Id
            url                = $Url
            parentId           = $null
            author             = [ordered]@{ id = 'r1'; displayName = 'Reviewer'; uniqueName = 'r@example.invalid'; kind = 'person' }
            content            = 'An earlier remark.'
            publishedUtc       = '2026-08-01T09:00:00.0000000Z'
            lastEditedUtc      = $null
            isEdited           = $false
            isAuthoredBySelf   = $false
            authorship         = 'human'
            authorshipEvidence = @()
        }
    }

    function New-TestThread {
        param(
            [Parameter(Mandatory)]
            [string] $Id,

            [object[]] $Comment = @()
        )

        [ordered]@{
            id               = $Id
            url              = "https://example.invalid/thread/$Id"
            rawStatus        = 'active'
            resolution       = 'active'
            kind             = 'text'
            isOutdated       = $false
            isAuthoredBySelf = $false
            eventUtc         = $null
            lastUpdatedUtc   = $null
            context          = [ordered]@{ filePath = 'src/app.ps1'; startLine = 3; endLine = 3; side = 'right' }
            anchorKey        = 'src/app.ps1:3'
            properties       = [ordered]@{}
            openingCommentId = if ($Comment.Count -gt 0) { [string] $Comment[0]['id'] } else { $null }
            decision         = $null
            comments         = @($Comment)
        }
    }

    function New-TestContext {
        param(
            [object[]] $Thread = @(),
            [string] $PullRequestUrl = 'https://github.com/octo/widget/pull/7'
        )

        [ordered]@{
            schemaVersion         = 3
            fetchedUtc            = '2026-08-04T11:00:00.0000000Z'
            platform              = 'gitHub'
            repositoryUrl         = 'https://github.com/octo/widget'
            pullRequestUrl        = $PullRequestUrl
            target                = Resolve-PullRequestTarget -Url $PullRequestUrl
            authenticatedIdentity = [ordered]@{ uniqueName = 'runner' }
            pullRequest           = [ordered]@{
                title                   = 'Review fixture'
                description             = $null
                reviewedSourceCommit    = $script:SourceCommit
                reviewedMergeBaseCommit = $script:MergeBaseCommit
                isDraft                 = $false
                mergeState              = 'clean'
            }
            reviewers             = @()
            revisions             = @()
            threads               = @($Thread)
        }
    }

    # The tracker state a GitHub run carries. The adapter reads it from the service; a test about
    # the run rather than about the adapter states it directly.
    function New-TestGitHubState {
        [ordered]@{
            platform     = 'gitHub'
            repositoryId = 'R_repo'
            trackerName  = 'octo/widget'
        }
    }

    function New-ScratchDirectory {
        $path = Join-Path $TestDrive ([guid]::NewGuid().ToString('n'))
        New-Item -ItemType Directory -Path $path -Force | Out-Null
        $path
    }

    function Write-JsonFixture {
        param(
            [Parameter(Mandatory)]
            [string] $Path,

            [Parameter(Mandatory)]
            $Value
        )

        [System.IO.File]::WriteAllText($Path, (ConvertTo-Json -InputObject $Value -Depth 24), [System.Text.UTF8Encoding]::new($false))
        $Path
    }

    function Read-JsonFixture {
        param([Parameter(Mandatory)][string] $Path)

        ConvertFrom-JsonVerbatim -Text ([System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8))
    }

    # --- The stand-in issue service --------------------------------------------------------------
    #
    # The run under test files GitHub issues, because the run itself is platform-neutral and GitHub
    # is the shorter of the two adapters to stand in for. What the adapters do with the same run is
    # asserted in the suites named after them.

    function Reset-TrackerState {
        $script:TrackerCalls = [System.Collections.Generic.List[object]]::new()
        $script:TrackerWaits = [System.Collections.Generic.List[int]]::new()
        $script:TrackerIssues = [System.Collections.Generic.List[object]]::new()
        $script:TrackerFaults = [System.Collections.Generic.List[object]]::new()
        $script:TrackerBodyRewrites = [System.Collections.Generic.List[object]]::new()
        $script:TrackerJournalSnapshot = [System.Collections.Generic.List[object]]::new()
        $script:TrackerJournalPath = ''
        $script:TrackerNextNumber = 101
    }

    # Fails the creation whose one-based ordinal matches, so a test states which write goes wrong
    # without restating the ones that go right.
    function Add-TrackerFault {
        param(
            [Parameter(Mandatory)]
            [ValidateRange(1, [int]::MaxValue)]
            [int] $AtCreation,

            # A refusal the service itself stated: a GraphQL response carrying errors and no data
            # at all, which is the shape that proves the mutation never ran.
            [switch] $Refused,

            # The response never arrived, which is what leaves a creation unproven.
            [switch] $Lost
        )

        $script:TrackerFaults.Add([ordered]@{
                atCreation = $AtCreation
                refused    = [bool] $Refused
                lost       = [bool] $Lost
            })
    }

    # Changes what the stand-in service stores for one creation, modelling a successful write
    # whose read-back does not match the requested body.
    function Add-TrackerBodyRewrite {
        param(
            [Parameter(Mandatory)]
            [ValidateRange(1, [int]::MaxValue)]
            [int] $AtCreation,

            [Parameter(Mandatory)]
            [string] $Body
        )

        $script:TrackerBodyRewrites.Add([ordered]@{
                atCreation = $AtCreation
                body       = $Body
            })
    }

    function Find-TrackerIssue {
        param([Parameter(Mandatory)][string] $NodeId)

        foreach ($issue in $script:TrackerIssues) {
            if ([string] $issue['id'] -ceq $NodeId) { return $issue }
        }
        $null
    }

    function New-TrackerGhResponse {
        param([Parameter(Mandatory)][AllowEmptyString()][string] $Body)

        [ordered]@{
            exitCode            = 0
            standardOutputBytes = [System.Text.Encoding]::UTF8.GetBytes($Body)
            standardErrorText   = ''
        }
    }

    # Names one request by what it asks for. Each GraphQL document carries a fragment no other
    # document carries, which is what lets the stand-in service answer without parsing the query.
    function Get-TrackerOperationName {
        param([Parameter(Mandatory)][AllowEmptyString()][string] $Text)

        if ($Text.Contains('createIssue(input:')) { return 'createIssue' }
        if ($Text.Contains('node(id:$issueId)')) { return 'readIssue' }
        if ($Text.Contains('repository(owner:')) { return 'readRepository' }
        'other'
    }

    function Get-TrackerFakeResponse {
        param([Parameter(Mandatory)][string] $Text)

        $payload = ConvertFrom-JsonVerbatim -Text $Text
        $query = [string] (Get-JsonProperty -Node $payload -Name 'query')
        $variables = Get-JsonProperty -Node $payload -Name 'variables'

        if ($query.Contains('createIssue(input:')) {
            $ordinal = @($script:TrackerCalls | Where-Object { [string] $_['operation'] -ceq 'createIssue' }).Count
            foreach ($fault in $script:TrackerFaults) {
                if ([int] $fault['atCreation'] -ne $ordinal) { continue }
                if ([bool] $fault['lost']) { throw 'the connection was closed before a response arrived' }
                return New-TrackerGhResponse -Body '{"errors":[{"message":"Body is too long"}]}'
            }

            # The journal as it stands at the moment of this write, which is what a run that died
            # here would have left behind.
            if ($script:TrackerJournalPath -and (Test-Path -LiteralPath $script:TrackerJournalPath -PathType Leaf)) {
                $script:TrackerJournalSnapshot.Add((Read-JsonFixture -Path $script:TrackerJournalPath))
            } else {
                $script:TrackerJournalSnapshot.Add($null)
            }

            $number = $script:TrackerNextNumber
            $script:TrackerNextNumber++
            $issue = [ordered]@{
                id     = "I_$number"
                number = $number
                url    = "https://github.com/octo/widget/issues/$number"
                title  = [string] (Get-JsonProperty -Node $variables -Name 'title')
                body   = [string] (Get-JsonProperty -Node $variables -Name 'body')
            }
            foreach ($rewrite in $script:TrackerBodyRewrites) {
                if ([int] $rewrite['atCreation'] -eq $ordinal) {
                    $issue['body'] = [string] $rewrite['body']
                    break
                }
            }
            $script:TrackerIssues.Add($issue)

            return New-TrackerGhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        data = [ordered]@{ createIssue = [ordered]@{ issue = $issue } }
                    }))
        }

        if ($query.Contains('node(id:$issueId)')) {
            $issue = Find-TrackerIssue -NodeId ([string] (Get-JsonProperty -Node $variables -Name 'issueId'))
            if ($null -eq $issue) { throw "the fake holds no issue $([string] (Get-JsonProperty -Node $variables -Name 'issueId'))" }
            return New-TrackerGhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        data = [ordered]@{ node = $issue }
                    }))
        }

        if ($query.Contains('repository(owner:')) {
            return New-TrackerGhResponse -Body (ConvertTo-Json -Depth 12 -Compress -InputObject ([ordered]@{
                        data = [ordered]@{
                            repository = [ordered]@{
                                id               = 'R_repo'
                                nameWithOwner    = 'octo/widget'
                                hasIssuesEnabled = $true
                                isArchived       = $false
                                viewerPermission = 'WRITE'
                            }
                        }
                    }))
        }

        throw "the fake GitHub CLI has no answer for the query it was sent: $query"
    }

    function New-TestTransport {
        param([int] $MaxAttempts = 5)

        # The recorded state lives at script scope rather than inside a closure, because a closure
        # is bound to its own module and would no longer resolve the helper functions this file
        # defines.
        $routingInvoker = {
            param(
                [string[]] $ArgumentList,
                [string] $StandardInput
            )

            $text = [string] $StandardInput

            $script:TrackerCalls.Add([ordered]@{
                    operation     = Get-TrackerOperationName -Text $text
                    arguments     = @($ArgumentList)
                    standardInput = $text
                })

            Get-TrackerFakeResponse -Text $text
        }

        $recordingWait = {
            param([int] $Milliseconds)

            $script:TrackerWaits.Add($Milliseconds)
        }

        $clock = { $script:FixedNowUtc }

        New-GitHubCliTransport `
            -InvokeGh $routingInvoker `
            -Wait $recordingWait `
            -GetUtcNow $clock `
            -MaxAttempts $MaxAttempts
    }

    function Get-TrackerOperationSequence {
        $sequence = [System.Collections.Generic.List[string]]::new()
        foreach ($call in $script:TrackerCalls) { $sequence.Add([string] $call['operation']) }
        $sequence.ToArray()
    }

    # Runs a whole tracker publication against the stand-in service and hands back everything a test
    # asserts on: the journal in memory, the journal on disk, and the outcome the caller receives.
    function Invoke-TestTrackerRun {
        param(
            [object[]] $Item = @(),

            [System.Collections.IDictionary] $Target = $null,

            [System.Collections.IDictionary] $State = $null,

            [string] $OutputDirectory = ''
        )

        if ($null -eq $Target) { $Target = $script:GitHubTracker }
        if ($null -eq $State) { $State = New-TestGitHubState }
        if ($Item.Count -eq 0) { $Item = New-TrackerItemSet -Finding @(New-TestFinding) }
        if (-not $OutputDirectory) { $OutputDirectory = New-ScratchDirectory }

        $transport = New-TestTransport
        $resultPath = Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory
        $script:TrackerJournalPath = $resultPath

        $result = New-TrackerPublicationResult -Target $Target -State $State -Item $Item -NowUtc $script:FixedNowUtc

        # The journal reaches disk before the first creation, as the composition root writes it.
        Write-TrackerPublicationResult -Result $result -Path $resultPath

        $failure = $null
        try {
            Invoke-TrackerPublication `
                -Transport $transport `
                -Target $Target `
                -State $State `
                -Item $Item `
                -Result $result `
                -ResultPath $resultPath `
                -Wait { param([int] $Milliseconds) $script:TrackerWaits.Add($Milliseconds) } | Out-Null
        } catch {
            $failure = $_
        }

        [ordered]@{
            outputDirectory = $OutputDirectory
            resultPath      = $resultPath
            result          = $result
            stored          = Read-JsonFixture -Path $resultPath
            outcome         = New-TrackerPublicationOutcome -Result $result -ResultPath $resultPath -SummaryBody 'the summary'
            failure         = $failure
        }
    }

    # --- Journal fixtures -------------------------------------------------------------------------

    # Writes a journal an earlier run would have left, with each creation's outcome stated.
    function Write-TrackerJournal {
        param(
            [Parameter(Mandatory)]
            [string] $OutputDirectory,

            [Parameter(Mandatory)]
            [ValidateSet('inProgress', 'failed', 'succeeded', 'needsAdjudication', 'resumeReady')]
            [string] $Status,

            [Parameter(Mandatory)]
            [string[]] $CreationOutcome,

            $Target = $null,

            [int] $SchemaVersion = 1,

            # A paused run's account of the rewrite awaiting a decision, attached at the top level.
            $Adjudication = $null,

            # A rewrite already accepted, attached to the first creation so a resumeReady journal reads
            # the way the run leaves one.
            $AcceptedRewrite = $null,

            [string] $PlanHash = $null
        )

        if ($null -eq $Target) { $Target = $script:GitHubTracker }

        $operations = [System.Collections.Generic.List[object]]::new()
        $index = 1
        foreach ($outcome in $CreationOutcome) {
            $entry = [ordered]@{
                operation  = 'createItem'
                findingId  = "F-$index"
                title      = "Title of F-$index"
                outcome    = $outcome
                itemId     = if ($outcome -ceq 'succeeded') { "$((100 + $index))" } else { $null }
                itemNodeId = $null
                url        = $null
                message    = $null
            }
            if ($null -ne $AcceptedRewrite -and $index -eq 1) { $entry['acceptedRewrite'] = $AcceptedRewrite }
            $operations.Add($entry)
            $index++
        }
        $operations.Add([ordered]@{
                operation  = 'verifyItems'
                findingId  = $null
                title      = $null
                outcome    = 'pending'
                itemId     = $null
                itemNodeId = $null
                url        = $null
                message    = $null
            })

        Write-JsonFixture -Path (Get-TrackerPublicationResultPath -OutputDirectory $OutputDirectory) -Value ([ordered]@{
                schemaVersion    = $SchemaVersion
                startedUtc       = '2026-08-04T12:00:00.0000000Z'
                platform         = [string] $Target['platform']
                target           = $Target
                trackerUrl       = [string] $Target['webUrl']
                workItemType     = $null
                areaPath         = $null
                descriptionField = $null
                parentItemId     = $null
                planHash         = $PlanHash
                status           = $Status
                message          = $null
                operations       = @($operations.ToArray())
                verification     = $null
                adjudication     = $Adjudication
            }) | Out-Null
    }

    # --- Executable ------------------------------------------------------------------------------

    # Lays out a complete, valid set of inputs and hands back the paths, so a test that is about one
    # broken input changes only that input.
    function New-TrackerFixture {
        param(
            $Approved = $null,
            $Context = $null,
            $Diagnostics = $null
        )

        if ($null -eq $Approved) { $Approved = New-TestApproved -Context $Context }
        if ($null -eq $Diagnostics) { $Diagnostics = [ordered]@{ schemaVersion = 1; outcome = 'miss'; areaCount = 0 } }

        $root = New-ScratchDirectory
        $outputDirectory = Join-Path $root 'out'
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null

        $fixture = [ordered]@{
            root            = $root
            outputDirectory = $outputDirectory
            approvedPath    = Write-JsonFixture -Path (Join-Path $root 'approved-findings.json') -Value $Approved
            diagnosticsPath = Write-JsonFixture -Path (Join-Path $root 'focus-area-cache-diagnostics.json') -Value $Diagnostics
            contextPath     = ''
        }

        if ($null -ne $Context) {
            $fixture['contextPath'] = Write-JsonFixture -Path (Join-Path $root 'refresh-pr-context.json') -Value $Context
        }

        $fixture
    }

    # Runs the composition root in a child process and reports its streams separately.
    #
    # Every test that uses this asserts a refusal the run reaches before it builds a transport, so
    # the suite stays offline; a case that got past preflight would try to reach a service.
    function Invoke-PublishReviewTrackerItems {
        param(
            [Parameter(Mandatory)]
            [System.Collections.IDictionary] $Fixture,

            [string[]] $ExtraArgument = @()
        )

        $executablePath = Join-Path $script:ExecutableRoot 'Publish-ReviewTrackerItems.ps1'
        if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
            throw "The executable under test is missing: $executablePath"
        }

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $script:PowerShellPath
        $arguments = @(
            '-NoProfile'
            '-NonInteractive'
            '-File'
            $executablePath
            '-ApprovedFindingsPath'
            [string] $Fixture['approvedPath']
            '-DiagnosticsPath'
            [string] $Fixture['diagnosticsPath']
            '-OutputDirectory'
            [string] $Fixture['outputDirectory']
        )
        if ($Fixture['contextPath']) { $arguments += @('-ContextPath', [string] $Fixture['contextPath']) }
        $arguments += $ExtraArgument
        foreach ($argument in $arguments) {
            $startInfo.ArgumentList.Add($argument)
        }
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.RedirectStandardInput = $true
        $startInfo.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
        $startInfo.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)

        $process = [System.Diagnostics.Process]::Start($startInfo)
        try {
            $process.StandardInput.Close()

            # Both streams are drained concurrently, because a refusal writes a formatted error
            # record and reading one stream to its end before starting on the other stalls as soon
            # as either outgrows a pipe buffer.
            $outputTask = $process.StandardOutput.ReadToEndAsync()
            $errorTask = $process.StandardError.ReadToEndAsync()
            $standardOutput = $outputTask.GetAwaiter().GetResult()
            $standardErrorText = $errorTask.GetAwaiter().GetResult()
            $process.WaitForExit()

            [ordered]@{
                exitCode       = $process.ExitCode
                standardOutput = $standardOutput
                standardError  = $standardErrorText
            }
        } finally {
            $process.Dispose()
        }
    }

    function Get-SingleLineErrorText {
        param(
            [AllowEmptyString()]
            [AllowNull()]
            [string] $Text
        )

        (((([string] $Text) -replace '\x1b\[[0-9;]*m', '') -replace '(?m)^\s*\|', ' ') -replace '\s+', ' ').Trim()
    }
}

Describe 'Tracker target resolution' {
    Context 'GitHub URLs' {
        # A tracker URL is whatever a user pasted out of a browser, so the repository root, the
        # issue list and a pull request inside the repository all name the same tracker.
        It 'resolves the same tracker from every URL naming the repository' {
            foreach ($url in @(
                    'https://github.com/octo/widget'
                    'https://github.com/octo/widget/pull/7'
                    'https://github.com/octo/widget/issues'
                    'https://github.com/octo/widget/blob/main/src/app.ps1'
                )) {
                $target = Resolve-TrackerTarget -Url $url

                $target['platform'] | Should -BeExactly 'gitHub' -Because $url
                $target['collectionUrl'] | Should -BeNullOrEmpty -Because $url
                $target['organization'] | Should -BeExactly 'octo' -Because $url
                $target['project'] | Should -BeNullOrEmpty -Because $url
                $target['repository'] | Should -BeExactly 'widget' -Because $url
                $target['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/issues' -Because $url
            }
        }

        It 'unescapes an owner and repository the URL escaped' {
            $target = Resolve-TrackerTarget -Url 'https://github.com/octo/widget%2Ecore'

            $target['repository'] | Should -BeExactly 'widget.core'
        }

        # The documented fallback reads the repository's own 'origin' remote, and a GitHub clone URL
        # carries a suffix the repository name does not. GitHub accepts no repository name ending in
        # it, so trimming it can shorten no real name.
        It 'drops the clone suffix a remote URL carries' {
            foreach ($url in @(
                    'https://github.com/octo/widget.git'
                    'https://github.com/octo/widget.GIT'
                )) {
                $target = Resolve-TrackerTarget -Url $url

                $target['repository'] | Should -BeExactly 'widget' -Because $url
                $target['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/issues' -Because $url
            }
        }

        It 'drops the clone suffix from a repository named on the command line' {
            $target = Resolve-TrackerTarget -Platform 'gitHub' -Organization 'octo' -Repository 'widget.git'

            $target['repository'] | Should -BeExactly 'widget'
        }
    }

    Context 'Azure DevOps URLs' {
        It 'resolves the same tracker from every dev.azure.com URL naming the project' {
            foreach ($url in @(
                    'https://dev.azure.com/contoso/widgets'
                    'https://dev.azure.com/contoso/widgets/_git/widget/pullrequest/42'
                    'https://dev.azure.com/contoso/widgets/_workitems/edit/12'
                    'https://dev.azure.com/contoso/widgets/_boards/board/t/team/Stories'
                )) {
                $target = Resolve-TrackerTarget -Url $url

                $target['platform'] | Should -BeExactly 'azureDevOps' -Because $url
                $target['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/contoso' -Because $url
                $target['organization'] | Should -BeExactly 'contoso' -Because $url
                $target['project'] | Should -BeExactly 'widgets' -Because $url
                $target['repository'] | Should -BeNullOrEmpty -Because $url
                $target['webUrl'] | Should -BeExactly 'https://dev.azure.com/contoso/widgets/_workitems' -Because $url
            }
        }

        # A project name with a space is ordinary in Azure DevOps, and the recorded browser URL has
        # to be one a browser can open.
        It 'unescapes a project name in the coordinates and escapes it again in the browser URL' {
            $target = Resolve-TrackerTarget -Url 'https://dev.azure.com/contoso/Example%20Project/_workitems'

            $target['project'] | Should -BeExactly 'Example Project'
            $target['webUrl'] | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems'
        }

        # The older per-organization host carries the organization in the host name, and everything
        # ahead of the project is a collection that dev.azure.com does not have.
        It 'resolves a collection-qualified visualstudio.com URL' {
            $target = Resolve-TrackerTarget -Url 'https://example.visualstudio.com/DefaultCollection/Example%20Project/_git/example-repo'

            $target['platform'] | Should -BeExactly 'azureDevOps'
            $target['collectionUrl'] | Should -BeExactly 'https://example.visualstudio.com/DefaultCollection'
            $target['organization'] | Should -BeExactly 'example'
            $target['project'] | Should -BeExactly 'Example Project'
            $target['repository'] | Should -BeNullOrEmpty
            $target['webUrl'] | Should -BeExactly 'https://example.visualstudio.com/DefaultCollection/Example%20Project/_workitems'
        }

        It 'resolves a visualstudio.com URL that names only the project' {
            $target = Resolve-TrackerTarget -Url 'https://example.visualstudio.com/Example%20Project'

            $target['collectionUrl'] | Should -BeExactly 'https://example.visualstudio.com'
            $target['organization'] | Should -BeExactly 'example'
            $target['project'] | Should -BeExactly 'Example Project'
            $target['webUrl'] | Should -BeExactly 'https://example.visualstudio.com/Example%20Project/_workitems'
        }
    }

    Context 'refused URLs' {
        It 'refuses a GitHub URL that names no repository' {
            { Resolve-TrackerTarget -Url 'https://github.com/octo' } |
                Should -Throw -ExpectedMessage '*names no GitHub repository*'
        }

        It 'refuses an Azure DevOps URL that names only the organization' {
            { Resolve-TrackerTarget -Url 'https://dev.azure.com/contoso' } |
                Should -Throw -ExpectedMessage '*names no Azure DevOps project*'
        }

        # A service area where the project would be names no project: Azure DevOps writes such a URL
        # when the repository and the project share a name, and reading the repository out of it
        # would resolve every other such URL to something that is not a project at all.
        It 'refuses an Azure DevOps URL whose project position holds a service area' {
            foreach ($url in @(
                    'https://dev.azure.com/contoso/_settings/work'
                    'https://dev.azure.com/contoso/_git/widget'
                )) {
                { Resolve-TrackerTarget -Url $url } |
                    Should -Throw -ExpectedMessage '*names no Azure DevOps project*' -Because $url
            }
        }

        It 'refuses an Azure DevOps URL that names no organization' {
            { Resolve-TrackerTarget -Url 'https://dev.azure.com/_git/x' } |
                Should -Throw -ExpectedMessage '*names no Azure DevOps organization*'
        }

        It 'refuses a URL that is not absolute' {
            { Resolve-TrackerTarget -Url 'octo/widget/issues' } |
                Should -Throw -ExpectedMessage '*is not an absolute URL*'
        }
    }

    Context 'explicit coordinates' {
        It 'rescues a GitHub URL that names no repository' {
            $target = Resolve-TrackerTarget -Url 'https://github.com/octo' -Repository 'widget'

            $target['organization'] | Should -BeExactly 'octo'
            $target['repository'] | Should -BeExactly 'widget'
            $target['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/issues'
        }

        It 'rescues an Azure DevOps URL that names no project' {
            $target = Resolve-TrackerTarget -Url 'https://dev.azure.com/contoso/_git/widget' -Project 'widgets'

            $target['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/contoso'
            $target['organization'] | Should -BeExactly 'contoso'
            $target['project'] | Should -BeExactly 'widgets'
        }

        It 'overrides the repository the URL named' {
            $target = Resolve-TrackerTarget -Url 'https://github.com/octo/widget/pull/7' -Repository 'gadget' -Organization 'contoso'

            $target['organization'] | Should -BeExactly 'contoso'
            $target['repository'] | Should -BeExactly 'gadget'
            $target['webUrl'] | Should -BeExactly 'https://github.com/contoso/gadget/issues'
        }

        It 'overrides the project and the collection the URL named' {
            $target = Resolve-TrackerTarget `
                -Url 'https://dev.azure.com/contoso/widgets/_workitems' `
                -Project 'gadgets' `
                -CollectionUrl 'https://dev.azure.com/fabrikam/'

            $target['project'] | Should -BeExactly 'gadgets'
            $target['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/fabrikam'
            $target['webUrl'] | Should -BeExactly 'https://dev.azure.com/fabrikam/gadgets/_workitems'
        }

        # A GitHub tracker has no project and an Azure DevOps tracker has no repository, so an
        # argument belonging to the other platform leaves the resolved tracker untouched.
        It 'ignores a coordinate the platform does not have' {
            $gitHub = Resolve-TrackerTarget -Url 'https://github.com/octo/widget' -Project 'widgets'
            $azureDevOps = Resolve-TrackerTarget -Url 'https://dev.azure.com/contoso/widgets' -Repository 'widget'

            $gitHub['project'] | Should -BeNullOrEmpty
            $azureDevOps['repository'] | Should -BeNullOrEmpty
        }
    }

    Context 'no URL at all' {
        It 'resolves a GitHub tracker from explicit coordinates' {
            $target = Resolve-TrackerTarget -Platform 'gitHub' -Organization 'octo' -Repository 'widget'

            $target['platform'] | Should -BeExactly 'gitHub'
            $target['collectionUrl'] | Should -BeNullOrEmpty
            $target['organization'] | Should -BeExactly 'octo'
            $target['project'] | Should -BeNullOrEmpty
            $target['repository'] | Should -BeExactly 'widget'
            $target['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/issues'
        }

        # The collection is derived from the organization, because dev.azure.com is where an
        # organization named without one lives.
        It 'resolves an Azure DevOps tracker from explicit coordinates' {
            $target = Resolve-TrackerTarget -Platform 'azureDevOps' -Organization 'contoso' -Project 'Example Project'

            $target['platform'] | Should -BeExactly 'azureDevOps'
            $target['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/contoso'
            $target['organization'] | Should -BeExactly 'contoso'
            $target['project'] | Should -BeExactly 'Example Project'
            $target['repository'] | Should -BeNullOrEmpty
            $target['webUrl'] | Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems'
        }

        It 'takes an explicit collection over the one the organization implies' {
            $target = Resolve-TrackerTarget `
                -Platform 'azureDevOps' `
                -Organization 'example' `
                -Project 'Example Project' `
                -CollectionUrl 'https://example.visualstudio.com/DefaultCollection'

            $target['collectionUrl'] | Should -BeExactly 'https://example.visualstudio.com/DefaultCollection'
        }

        It 'refuses a resolution naming neither a URL nor a platform' {
            { Resolve-TrackerTarget -Organization 'octo' -Repository 'widget' } |
                Should -Throw -ExpectedMessage '*Supply either -Url or -Platform*'
        }

        It 'refuses a resolution naming no organization' {
            { Resolve-TrackerTarget -Platform 'gitHub' -Repository 'widget' } |
                Should -Throw -ExpectedMessage '*Supply -Organization*'
        }

        It 'refuses a GitHub tracker naming no repository' {
            { Resolve-TrackerTarget -Platform 'gitHub' -Organization 'octo' } |
                Should -Throw -ExpectedMessage '*A GitHub tracker requires a repository*'
        }

        It 'refuses an Azure DevOps tracker naming no project' {
            { Resolve-TrackerTarget -Platform 'azureDevOps' -Organization 'contoso' } |
                Should -Throw -ExpectedMessage '*An Azure DevOps tracker requires a project*'
        }
    }
}

Describe 'Tracker argument validation' {
    Context 'GitHub' {
        It 'refuses an Azure DevOps field for a GitHub tracker' {
            { Assert-TrackerArgument -Target $script:GitHubTracker -WorkItemType 'Bug' } |
                Should -Throw -ExpectedMessage '*-WorkItemType was supplied for a GitHub tracker*'
            { Assert-TrackerArgument -Target $script:GitHubTracker -AreaPath 'widgets\Core' } |
                Should -Throw -ExpectedMessage '*-AreaPath was supplied for a GitHub tracker*'
            { Assert-TrackerArgument -Target $script:GitHubTracker -DescriptionField 'System.Description' } |
                Should -Throw -ExpectedMessage '*-DescriptionField was supplied for a GitHub tracker*'
        }

        It 'refuses a parent for a GitHub tracker' {
            { Assert-TrackerArgument -Target $script:GitHubTracker -ParentWorkItemId 12 } |
                Should -Throw -ExpectedMessage '*-ParentWorkItemId was supplied for a GitHub tracker*'
        }

        It 'accepts a GitHub tracker carrying none of them' {
            { Assert-TrackerArgument -Target $script:GitHubTracker } | Should -Not -Throw
        }
    }

    Context 'Azure DevOps' {
        It 'requires a work item type' {
            { Assert-TrackerArgument -Target $script:AzureDevOpsTracker -AreaPath 'widgets' } |
                Should -Throw -ExpectedMessage '*requires -WorkItemType*'
        }

        It 'requires an area path' {
            { Assert-TrackerArgument -Target $script:AzureDevOpsTracker -WorkItemType 'Bug' } |
                Should -Throw -ExpectedMessage '*requires -AreaPath*'
        }

        # Azure DevOps roots every area path at the project name, and a path rooted elsewhere is
        # either refused later or resolved against this project and filed where nobody expects.
        It 'refuses an area path rooted at another project' {
            { Assert-TrackerArgument -Target $script:AzureDevOpsTracker -WorkItemType 'Bug' -AreaPath 'gadgets\Core' } |
                Should -Throw -ExpectedMessage "*rooted at 'gadgets'*"
        }

        It 'refuses an area path that names nothing at all' {
            { Assert-TrackerArgument -Target $script:AzureDevOpsTracker -WorkItemType 'Bug' -AreaPath '\\' } |
                Should -Throw -ExpectedMessage '*is not an area path*'
        }

        It 'accepts the project root and a path below it, whatever the casing' {
            foreach ($areaPath in @('widgets', 'widgets\Core', 'WIDGETS\Core\Storage')) {
                { Assert-TrackerArgument -Target $script:AzureDevOpsTracker -WorkItemType 'Bug' -AreaPath $areaPath } |
                    Should -Not -Throw -Because $areaPath
            }
        }
    }
}

Describe 'Tracker item web URL' {
    It 'builds a GitHub issue URL' {
        Get-TrackerItemWebUrl -Target $script:GitHubTracker -ItemId '101' |
            Should -BeExactly 'https://github.com/octo/widget/issues/101'
    }

    It 'builds an Azure DevOps work item URL' {
        Get-TrackerItemWebUrl -Target $script:AzureDevOpsTracker -ItemId '4711' |
            Should -BeExactly 'https://dev.azure.com/contoso/widgets/_workitems/edit/4711'
    }

    It 'escapes a project name that carries a space' {
        $target = Resolve-TrackerTarget -Platform 'azureDevOps' -Organization 'contoso' -Project 'Example Project'

        Get-TrackerItemWebUrl -Target $target -ItemId '9' |
            Should -BeExactly 'https://dev.azure.com/contoso/Example%20Project/_workitems/edit/9'
    }
}

Describe 'Approved artifact for a tracker' {
    BeforeAll {
        $script:NullTargetApproved = [ordered]@{
            schemaVersion           = 2
            target                  = $null
            reviewedSourceCommit    = $null
            reviewedMergeBaseCommit = $null
            reviewedContextHash     = $null
            findings                = @(New-TestFinding)
        }
    }

    # A review of content that is not a pull request records no target and no commits, and its
    # approved set is publishable as tracker items.
    It 'accepts an approved artifact naming no pull request and no commits' {
        $path = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'approved-findings.json') -Value $script:NullTargetApproved

        $approved = Read-ApprovedFindingArtifact -Path $path

        $approved.Contains('target') | Should -BeTrue
        Get-JsonProperty -Node $approved -Name 'target' | Should -BeNullOrEmpty
        Get-JsonProperty -Node $approved -Name 'reviewedSourceCommit' | Should -BeNullOrEmpty
        (Get-JsonArrayProperty -Node $approved -Name 'findings').Count | Should -Be 1
    }

    It 'leaves publication onto a pull request refusing the same artifact' {
        $path = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'approved-findings.json') -Value $script:NullTargetApproved
        $approved = Read-ApprovedFindingArtifact -Path $path

        $thrown = { Assert-PullRequestApprovedArtifact -Approved $approved -Path $path } | Should -Throw -PassThru

        $thrown.Exception.Message | Should -BeLike '*names no pull request*'
        $thrown.Exception.Message | Should -BeLike '*Publish-ReviewTrackerItems.ps1*'
    }

    # The field has to be present even when it holds nothing, because an artifact that never carried
    # it was written by something other than the approval step.
    It 'refuses an approved artifact that omits the target entirely' {
        $document = [ordered]@{
            schemaVersion           = 2
            reviewedSourceCommit    = $null
            reviewedMergeBaseCommit = $null
            reviewedContextHash     = $null
            findings                = @(New-TestFinding)
        }
        $path = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'approved-findings.json') -Value $document

        { Read-ApprovedFindingArtifact -Path $path } | Should -Throw -ExpectedMessage "*missing 'target'*"
    }

    It 'requires a fingerprint for PR-backed findings' {
        $approved = New-TestApproved -Context (New-TestContext)
        $approved['reviewedContextHash'] = $null
        $path = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'approved-findings.json') -Value $approved

        { Read-ApprovedFindingArtifact -Path $path } | Should -Throw '*requires a non-null reviewedContextHash*'
    }

    It 'accepts only the bound capture for a PR-backed review' {
        $context = New-TestContext -Thread @(New-TestThread -Id 'T1' -Comment @(New-TestComment -Id 'C1'))
        $approved = New-TestApproved -Context $context
        $path = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'approved-findings.json') -Value $approved
        $stored = Read-ApprovedFindingArtifact -Path $path

        { Assert-ApprovedFindingContext -Approved $stored -Context $context } | Should -Not -Throw
        { Assert-ApprovedFindingContext -Approved $stored -Context $null } | Should -Throw '*require -ContextPath*'
        $context['threads'][0]['comments'][0]['content'] = 'Different evidence with the same commit and comment IDs.'
        { Assert-ApprovedFindingContext -Approved $stored -Context $context } | Should -Throw '*not bound to this checked capture*'
    }

    It 'allows no capture only when the review is not PR-backed' {
        { Assert-ApprovedFindingContext -Approved $script:NullTargetApproved -Context $null } | Should -Not -Throw
        { Assert-ApprovedFindingContext -Approved $script:NullTargetApproved -Context (New-TestContext) } |
            Should -Throw '*not bound to this checked capture*'
    }
}

Describe 'Tracker item set' {
    It 'presents synthesis findings ahead of reviewer findings' {
        $items = New-TrackerItemSet -Finding @(
            (New-TestFinding -Id 'F-1' -Origin 'reviewer')
            (New-TestFinding -Id 'F-2' -Origin 'synthesis')
            (New-TestFinding -Id 'F-3' -Origin 'reviewer')
            (New-TestFinding -Id 'F-4' -Origin 'synthesis')
        )

        @($items | ForEach-Object { [string] $_['findingId'] }) -join ',' | Should -BeExactly 'F-2,F-4,F-1,F-3'
    }

    It 'carries the finding identifier, the title and the body of each item' {
        $items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-9' -Title 'Terminate the retry loop')

        $items.Count | Should -Be 1
        @($items[0].Keys) -join ',' | Should -BeExactly 'findingId,title,body'
        $items[0]['findingId'] | Should -BeExactly 'F-9'
        $items[0]['title'] | Should -BeExactly 'Terminate the retry loop'
        ([string] $items[0]['body']).Contains('Description of F-9', [StringComparison]::Ordinal) | Should -BeTrue
    }

    Context 'follow-up resolution' {
        BeforeAll {
            $script:LinkedContext = New-TestContext -Thread @(
                (New-TestThread -Id 'T1' -Comment @((New-TestComment -Id 'C1')))
                (New-TestThread -Id 'T2' -Comment @((New-TestComment -Id 'C2' -Url '')))
            )
        }

        It 'links the comment the finding answers' {
            $items = New-TrackerItemSet `
                -Finding @(New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1') `
                -Context $script:LinkedContext

            $body = [string] $items[0]['body']
            $body.Contains('**Follows up on**: https://example.invalid/comment/C1', [StringComparison]::Ordinal) | Should -BeTrue
            $body.Contains('**Reviewed in**: https://github.com/octo/widget/pull/7', [StringComparison]::Ordinal) | Should -BeTrue
        }

        # A capture that records no link for the comment still tells a reader where the discussion
        # is, which is worth more than filing an item that points nowhere.
        It 'falls back to the pull request where the comment carries no link' {
            $items = New-TrackerItemSet `
                -Finding @(New-TestFinding -Id 'F-1' -FollowUpThreadId 'T2' -FollowUpCommentId 'C2') `
                -Context $script:LinkedContext

            ([string] $items[0]['body']).Contains('**Follows up on**: https://github.com/octo/widget/pull/7', [StringComparison]::Ordinal) |
                Should -BeTrue
        }

        It 'states no follow-up for a finding that answers nothing' {
            $items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1') -Context $script:LinkedContext

            $body = [string] $items[0]['body']
            $body.Contains('Follows up on', [StringComparison]::Ordinal) | Should -BeFalse
            $body.Contains('**Reviewed in**: https://github.com/octo/widget/pull/7', [StringComparison]::Ordinal) | Should -BeTrue
        }

        It 'states neither link for a review of content that is no pull request' {
            $items = New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1')

            $body = [string] $items[0]['body']
            $body.Contains('Follows up on', [StringComparison]::Ordinal) | Should -BeFalse
            $body.Contains('Reviewed in', [StringComparison]::Ordinal) | Should -BeFalse
        }

        # A tracker item links rather than replies, so a thread that would refuse a reply is still
        # perfectly linkable; what is required is that the comment exists in the capture.
        It 'accepts a follow-up the capture holds' {
            { Assert-TrackerFollowUpTarget `
                    -Finding (New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1') `
                    -Context $script:LinkedContext } | Should -Not -Throw
        }

        It 'refuses a follow-up naming a comment the capture does not hold' {
            { Assert-TrackerFollowUpTarget `
                    -Finding (New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C9') `
                    -Context $script:LinkedContext } |
                Should -Throw -ExpectedMessage '*which the captured discussion does not contain*'
        }

        It 'refuses a follow-up when no capture was supplied' {
            { Assert-TrackerFollowUpTarget -Finding (New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1') } |
                Should -Throw -ExpectedMessage '*no captured discussion was supplied*'
        }
    }
}

Describe 'Audit document' {
    It 'labels each finding with the identifier the user replies by' {
        $document = ConvertTo-FindingAuditDocument `
            -Finding @((New-TestFinding -Id 'F-1'), (New-TestFinding -Id 'F-2')) `
            -Destination 'pullRequest'

        $document.Contains('**Finding `F-1`**', [StringComparison]::Ordinal) | Should -BeTrue
        $document.Contains('**Finding `F-2`**', [StringComparison]::Ordinal) | Should -BeTrue
    }

    # The audit presents the exact text publication files, so approving the document approves what a
    # reader receives.
    It 'carries each finding in the form it would be published in' {
        $findings = @((New-TestFinding -Id 'F-1'), (New-TestFinding -Id 'F-2'))
        $document = ConvertTo-FindingAuditDocument -Finding $findings -Destination 'pullRequest'

        foreach ($finding in $findings) {
            $body = ConvertTo-FindingDestinationText -Finding $finding -Destination 'pullRequest'
            $document.Contains($body, [StringComparison]::Ordinal) | Should -BeTrue -Because ([string] $finding['id'])
        }
    }

    It 'presents synthesis findings ahead of reviewer findings' {
        $document = ConvertTo-FindingAuditDocument -Finding @(
            (New-TestFinding -Id 'F-1' -Origin 'reviewer')
            (New-TestFinding -Id 'F-2' -Origin 'synthesis')
        ) -Destination 'pullRequest'

        $document.IndexOf('**Finding `F-2`**', [StringComparison]::Ordinal) |
            Should -BeLessThan $document.IndexOf('**Finding `F-1`**', [StringComparison]::Ordinal)
    }

    # A tracker audit shows the title field and the provenance links, because that is the form the
    # item is filed in.
    It 'renders a tracker destination as the tracker form under each label' {
        $context = New-TestContext -Thread @(
            (New-TestThread -Id 'T1' -Comment @((New-TestComment -Id 'C1')))
        )
        $document = ConvertTo-FindingAuditDocument `
            -Finding @(New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1') `
            -Destination 'tracker' `
            -Context $context

        $document.Contains('**Finding `F-1`**', [StringComparison]::Ordinal) | Should -BeTrue
        $document.Contains('Title: Title of F-1', [StringComparison]::Ordinal) | Should -BeTrue
        $document.Contains('**Follows up on**: https://example.invalid/comment/C1', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'renders numeric captured discussion identifiers through tracker document adapters' {
        $comment = New-TestComment -Id '7'
        $comment['id'] = 7
        $thread = New-TestThread -Id '42' -Comment @($comment)
        $thread['id'] = 42
        $thread['openingCommentId'] = 7
        $context = New-TestContext -Thread @($thread) `
            -PullRequestUrl 'https://dev.azure.com/contoso/widgets/_git/widget/pullrequest/42'
        $context['platform'] = 'azureDevOps'
        $context['repositoryUrl'] = 'https://dev.azure.com/contoso/widgets/_git/widget'
        $contextPath = Write-JsonFixture -Path (Join-Path (New-ScratchDirectory) 'capture.json') -Value $context
        $context = Read-PullRequestContextDocument -Path $contextPath
        $context['threads'][0]['id'] | Should -BeOfType [long]
        $context['threads'][0]['comments'][0]['id'] | Should -BeOfType [long]

        $followUp = New-TestFinding -Id 'F-1' -FollowUpThreadId '42' -FollowUpCommentId '7'
        $plain = New-TestFinding -Id 'F-2'
        $expected = '**Follows up on**: https://example.invalid/comment/7'
        $combined = ConvertTo-FindingDestinationDocument -Finding @($plain, $followUp) -Destination 'tracker' -Context $context
        $audit = ConvertTo-FindingAuditDocument -Finding @($plain, $followUp) -Destination 'tracker' -Context $context
        $combined.Contains($expected, [StringComparison]::Ordinal) | Should -BeTrue
        $audit.Contains($expected, [StringComparison]::Ordinal) | Should -BeTrue
        $audit.Contains('**Finding `F-2`**', [StringComparison]::Ordinal) | Should -BeTrue
        $withoutFollowUp = ConvertTo-FindingAuditDocument -Finding @($plain) -Destination 'tracker' -Context $context
        $withoutFollowUp.Contains('**Follows up on**', [StringComparison]::Ordinal) | Should -BeFalse
    }

    It 'renders an empty set as an empty document' {
        ConvertTo-FindingAuditDocument -Finding @() -Destination 'pullRequest' | Should -BeExactly ''
    }
}

Describe 'Tracker publication journal' {
    BeforeEach {
        Reset-TrackerState
    }

    It 'holds exactly these fields, in this order' {
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1'))

        @($run['stored'].Keys) -join ',' | Should -BeExactly 'schemaVersion,startedUtc,platform,target,trackerUrl,workItemType,areaPath,descriptionField,parentItemId,planHash,status,message,operations,verification,adjudication'
    }

    It 'records the tracker and the instant the run began' {
        $run = Invoke-TestTrackerRun

        $stored = $run['stored']
        Get-JsonProperty -Node $stored -Name 'schemaVersion' | Should -Be 1
        Get-JsonProperty -Node $stored -Name 'startedUtc' | Should -BeExactly '2026-08-04T12:00:00.0000000Z'
        Get-JsonProperty -Node $stored -Name 'platform' | Should -BeExactly 'gitHub'
        Get-JsonProperty -Node $stored -Name 'trackerUrl' | Should -BeExactly 'https://github.com/octo/widget/issues'
        Test-TrackerTargetsMatch -Left $script:GitHubTracker -Right (Get-JsonProperty -Node $stored -Name 'target') | Should -BeTrue
    }

    # The Azure DevOps coordinates are the state's rather than the target's, and a GitHub run has
    # none of them.
    It 'states no work item coordinates for a GitHub run' {
        $run = Invoke-TestTrackerRun

        foreach ($name in @('workItemType', 'areaPath', 'descriptionField', 'parentItemId')) {
            Get-JsonProperty -Node $run['stored'] -Name $name | Should -BeNullOrEmpty -Because $name
        }
    }

    It 'carries the Azure DevOps coordinates the state resolved' {
        $state = [ordered]@{
            platform         = 'azureDevOps'
            trackerName      = 'contoso/widgets'
            createPath       = 'widgets/_apis/wit/workitems/$Bug'
            readPathPrefix   = 'widgets/_apis/wit/workitems/'
            workItemType     = 'Bug'
            areaPath         = 'widgets\Core'
            descriptionField = 'Microsoft.VSTS.TCM.ReproSteps'
            parent           = [ordered]@{ itemId = '77'; title = 'Epic'; url = 'https://dev.azure.com/contoso/_apis/wit/workItems/77'; webUrl = 'x' }
        }

        $result = New-TrackerPublicationResult `
            -Target $script:AzureDevOpsTracker `
            -State $state `
            -Item (New-TrackerItemSet -Finding @(New-TestFinding)) `
            -NowUtc $script:FixedNowUtc

        $result['workItemType'] | Should -BeExactly 'Bug'
        $result['areaPath'] | Should -BeExactly 'widgets\Core'
        $result['descriptionField'] | Should -BeExactly 'Microsoft.VSTS.TCM.ReproSteps'
        $result['parentItemId'] | Should -BeExactly '77'
    }

    It 'states every intended creation as pending before anything is attempted' {
        $items = New-TrackerItemSet -Finding @(
            (New-TestFinding -Id 'F-1')
            (New-TestFinding -Id 'F-2')
        )

        $result = New-TrackerPublicationResult `
            -Target $script:GitHubTracker `
            -State (New-TestGitHubState) `
            -Item $items `
            -NowUtc $script:FixedNowUtc

        $operations = Get-JsonArrayProperty -Node $result -Name 'operations'
        $result['status'] | Should -BeExactly 'inProgress'
        @($operations | ForEach-Object { [string] $_['operation'] }) -join ',' | Should -BeExactly 'createItem,createItem,verifyItems'
        @($operations | ForEach-Object { [string] $_['outcome'] }) -join ',' | Should -BeExactly 'pending,pending,pending'
        @($operations[0].Keys) -join ',' | Should -BeExactly 'operation,findingId,title,outcome,itemId,itemNodeId,url,message'
        $operations[0]['findingId'] | Should -BeExactly 'F-1'
        $operations[0]['title'] | Should -BeExactly 'Title of F-1'
        $operations[2]['findingId'] | Should -BeNullOrEmpty
    }

    # The journal is the only account of what exists, so it is current at every moment a creation
    # could fail rather than only once the run has ended.
    It 'has already recorded the first creation by the time the second is sent' {
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $run['failure'] | Should -BeNullOrEmpty
        $script:TrackerJournalSnapshot.Count | Should -Be 2

        $atFirstCreation = Get-JsonArrayProperty -Node $script:TrackerJournalSnapshot[0] -Name 'operations'
        @($atFirstCreation | ForEach-Object { [string] $_['outcome'] }) -join ',' | Should -BeExactly 'pending,pending,pending'

        $atSecondCreation = Get-JsonArrayProperty -Node $script:TrackerJournalSnapshot[1] -Name 'operations'
        @($atSecondCreation | ForEach-Object { [string] $_['outcome'] }) -join ',' | Should -BeExactly 'succeeded,pending,pending'
        $atSecondCreation[0]['itemId'] | Should -BeExactly '101'
    }

    It 'records what became of every operation' {
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $stored = $run['stored']
        Get-JsonProperty -Node $stored -Name 'status' | Should -BeExactly 'succeeded'
        Get-JsonProperty -Node $stored -Name 'message' | Should -BeNullOrEmpty

        $operations = Get-JsonArrayProperty -Node $stored -Name 'operations'
        @($operations | ForEach-Object { [string] $_['outcome'] }) -join ',' | Should -BeExactly 'succeeded,succeeded,succeeded'
        $operations[0]['itemId'] | Should -BeExactly '101'
        $operations[0]['itemNodeId'] | Should -BeExactly 'I_101'
        $operations[0]['url'] | Should -BeExactly 'https://github.com/octo/widget/issues/101'
        $operations[1]['itemId'] | Should -BeExactly '102'

        $verification = Get-JsonArrayProperty -Node $stored -Name 'verification'
        $verification.Count | Should -Be 2
        @($verification[0].Keys) -join ',' | Should -BeExactly 'findingId,itemId,url,title,descriptionFormat'
        $verification[0]['findingId'] | Should -BeExactly 'F-1'
        $verification[0]['descriptionFormat'] | Should -BeExactly 'Markdown'
    }

    It 'records a refused creation and leaves the rest pending' {
        Add-TrackerFault -AtCreation 2 -Refused
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $run['failure'] | Should -Not -BeNullOrEmpty
        $stored = $run['stored']
        Get-JsonProperty -Node $stored -Name 'status' | Should -BeExactly 'failed'
        Get-JsonProperty -Node $stored -Name 'message' | Should -BeLike '*refused by the service*'

        $operations = Get-JsonArrayProperty -Node $stored -Name 'operations'
        @($operations | ForEach-Object { [string] $_['outcome'] }) -join ',' | Should -BeExactly 'succeeded,failed,pending'
        $operations[1]['message'] | Should -BeLike '*Body is too long*'
        $operations[1]['itemId'] | Should -BeNullOrEmpty
    }

    It 'records a creation whose outcome it could not determine' {
        Add-TrackerFault -AtCreation 1 -Lost
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1'))

        $operations = Get-JsonArrayProperty -Node $run['stored'] -Name 'operations'
        $operations[0]['outcome'] | Should -BeExactly 'ambiguous'
    }

    It 'refuses to record an operation the journal was not built for' {
        $result = New-TrackerPublicationResult `
            -Target $script:GitHubTracker `
            -State (New-TestGitHubState) `
            -Item (New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1')) `
            -NowUtc $script:FixedNowUtc

        { Set-TrackerPublicationOperationOutcome -Result $result -Operation 'createItem' -FindingId 'F-9' -Outcome 'succeeded' } |
            Should -Throw -ExpectedMessage "*holds no record of 'createItem' for finding 'F-9'*"
    }

    It 'writes the journal without a byte order mark and with newline endings' {
        $run = Invoke-TestTrackerRun

        $bytes = [System.IO.File]::ReadAllBytes([string] $run['resultPath'])
        $bytes[0] | Should -Not -Be 0xEF
        [System.Text.Encoding]::UTF8.GetString($bytes) | Should -Not -BeLike "*`r`n*"
    }

    It 'names the journal so the skill and its reader agree where it is' {
        $directory = New-ScratchDirectory

        Get-TrackerPublicationResultPath -OutputDirectory $directory |
            Should -BeExactly (Join-Path $directory 'tracker-publication-result.json')
    }
}

Describe 'Tracker publication rerun guard' {
    # The rule is finer than "refuse over any recorded creation". A refusal is the service stating
    # the item was not created, and a journal that recorded its own end never attempted the
    # creations still reading as pending. Both halves are stated here, because the difference
    # decides whether a review can be filed at all after a run that failed at its first item.
    It 'refuses a journal recording a filed item' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal -OutputDirectory $directory -Status 'failed' -CreationOutcome @('succeeded', 'pending')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it records 1 item filed and 0 creations whose outcome it could not determine*'
    }

    It 'refuses a journal recording a creation whose outcome is unknown' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal -OutputDirectory $directory -Status 'failed' -CreationOutcome @('ambiguous', 'pending')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it records 0 items filed and 1 creation whose outcome it could not determine*'
    }

    # A run still reading as in progress did not live long enough to say what became of the creation
    # it was making, so its pending entries are unknown rather than unattempted.
    It 'refuses a journal still reading as in progress' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal -OutputDirectory $directory -Status 'inProgress' -CreationOutcome @('pending', 'pending')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it records 0 items filed and 2 creations whose outcome it could not determine*'
    }

    It 'allows a rerun over a settled journal whose creations were all refused' {
        foreach ($status in @('failed', 'succeeded')) {
            $directory = New-ScratchDirectory
            Write-TrackerJournal -OutputDirectory $directory -Status $status -CreationOutcome @('failed', 'pending', 'pending')

            { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
                Should -Not -Throw -Because $status
        }
    }

    It 'allows a rerun where there is no journal at all' {
        { Assert-TrackerPublicationRecord -OutputDirectory (New-ScratchDirectory) -Target $script:GitHubTracker } |
            Should -Not -Throw
    }

    # The next run writes over this path, so a journal of another tracker cannot be passed: doing so
    # would destroy the only account of the items filed there.
    It 'refuses a journal naming another tracker that filed items' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal `
            -OutputDirectory $directory `
            -Status 'succeeded' `
            -CreationOutcome @('succeeded') `
            -Target $script:AzureDevOpsTracker

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*which is not the tracker this run was given*'
    }

    # A journal that filed nothing is no obstacle whichever tracker it names, because there is
    # nothing recorded in it to lose.
    It 'allows a journal naming another tracker that filed nothing' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal `
            -OutputDirectory $directory `
            -Status 'failed' `
            -CreationOutcome @('failed') `
            -Target $script:AzureDevOpsTracker

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Not -Throw
    }

    # Both services name a repository and a project case-insensitively, so a journal spelled
    # differently from the command line still describes this tracker.
    It 'reads a journal spelled in another casing as the same tracker' {
        $directory = New-ScratchDirectory
        $other = [ordered]@{
            platform      = 'gitHub'
            collectionUrl = $null
            organization  = 'OCTO'
            project       = $null
            repository    = 'Widget'
            webUrl        = 'https://github.com/OCTO/Widget/issues'
        }
        Write-TrackerJournal -OutputDirectory $directory -Status 'succeeded' -CreationOutcome @('succeeded') -Target $other

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*records 1 item filed*'
    }

    It 'refuses a journal it cannot read' {
        $directory = New-ScratchDirectory
        [System.IO.File]::WriteAllText((Get-TrackerPublicationResultPath -OutputDirectory $directory), 'not json at all')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it cannot be parsed*'
    }

    It 'refuses a journal that is not a JSON object' {
        $directory = New-ScratchDirectory
        [System.IO.File]::WriteAllText((Get-TrackerPublicationResultPath -OutputDirectory $directory), '[1,2]')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it is not a JSON object*'
    }

    It 'refuses a journal written to another schema' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal -OutputDirectory $directory -Status 'failed' -CreationOutcome @('failed') -SchemaVersion 2

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it reports version 2*'
    }

    It 'refuses a journal naming no tracker' {
        $directory = New-ScratchDirectory
        Write-JsonFixture `
            -Path (Get-TrackerPublicationResultPath -OutputDirectory $directory) `
            -Value ([ordered]@{ schemaVersion = 1; status = 'failed'; operations = @() }) | Out-Null

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*it names no tracker*'
    }

    It 'states that no switch skips the check' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal -OutputDirectory $directory -Status 'inProgress' -CreationOutcome @('pending')

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*No switch skips this check*'
    }

    # A journal paused for a decision on this tracker is one continuation away from finishing, so it is
    # resumed rather than moved aside. The refusal names the continuation and carries the coordinates a
    # person needs to accept the rewrite, read from the pending adjudication.
    It 'refuses a rerun over a journal paused for a decision and names the continuation' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal `
            -OutputDirectory $directory `
            -Status 'needsAdjudication' `
            -CreationOutcome @('succeeded', 'pending') `
            -Target $script:AzureDevOpsTracker `
            -Adjudication ([ordered]@{ findingId = 'F-1'; itemId = '4001'; expectedBodyHash = 'expected-hash'; storedBodyHash = 'stored-hash' })

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:AzureDevOpsTracker } |
            Should -Throw -ExpectedMessage '*-AcceptStoredRewrite -AcceptWorkItemId 4001 -AcceptExpectedBodyHash expected-hash -AcceptStoredBodyHash stored-hash*'
    }

    # Once a rewrite has been accepted the pending adjudication is gone, so the same continuation
    # coordinates are read back out of the accepted rewrite the resumable journal holds.
    It 'reads the continuation coordinates from an accepted rewrite once one is recorded' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal `
            -OutputDirectory $directory `
            -Status 'resumeReady' `
            -CreationOutcome @('succeeded', 'pending') `
            -Target $script:AzureDevOpsTracker `
            -AcceptedRewrite ([ordered]@{ expectedBodyHash = 'expected-hash'; storedBodyHash = 'stored-hash' })

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:AzureDevOpsTracker } |
            Should -Throw -ExpectedMessage '*-AcceptStoredRewrite -AcceptWorkItemId 101 -AcceptExpectedBodyHash expected-hash -AcceptStoredBodyHash stored-hash*'
    }

    # A paused journal that names another tracker is not this run's to continue, so it is refused for
    # the items it filed there rather than pointed at a continuation of this run.
    It 'refuses a paused journal of another tracker for its filed items' {
        $directory = New-ScratchDirectory
        Write-TrackerJournal `
            -OutputDirectory $directory `
            -Status 'needsAdjudication' `
            -CreationOutcome @('succeeded') `
            -Target $script:AzureDevOpsTracker `
            -Adjudication ([ordered]@{ findingId = 'F-1'; itemId = '4001'; expectedBodyHash = 'e'; storedBodyHash = 's' })

        { Assert-TrackerPublicationRecord -OutputDirectory $directory -Target $script:GitHubTracker } |
            Should -Throw -ExpectedMessage '*which is not the tracker this run was given*'
    }
}

Describe 'Tracker body canonicalization' {
    It 'normalizes every line ending to a line feed' {
        ConvertTo-TrackerCanonicalBody -Text "a`r`nb`rc`nd" | Should -BeExactly "a`nb`nc`nd"
    }

    # The sent side is canonicalized without decoding, so an approved text that itself contained a
    # literal entity is not silently read as the character it names.
    It 'leaves HTML entities intact when no decode is asked for' {
        ConvertTo-TrackerCanonicalBody -Text 'Arc&lt;Mutex&lt;T&gt;&gt; &amp; more' |
            Should -BeExactly 'Arc&lt;Mutex&lt;T&gt;&gt; &amp; more'
    }

    # The stored side is decoded, so the entities an Azure DevOps pipeline stored are read as the
    # markup they encode.
    It 'decodes HTML entities when a decode is asked for' {
        ConvertTo-TrackerCanonicalBody -Text 'Arc&lt;Mutex&lt;T&gt;&gt; &amp; more' -DecodeHtml |
            Should -BeExactly 'Arc<Mutex<T>> & more'
    }
}

Describe 'Tracker body deviation' {
    It 'reports an exact match as wholly similar' {
        $deviation = Measure-TrackerBodyDeviation -Sent 'the very same body' -Stored 'the very same body'

        [bool] $deviation['decided'] | Should -BeTrue
        [double] $deviation['similarity'] | Should -Be 1.0
        [int] $deviation['distance'] | Should -Be 0
    }

    # The shared head and tail are removed before the distance is measured, so the report points at
    # where the bodies first diverge and rates a single change as almost wholly similar.
    It 'measures only the region that differs and points at it' {
        $deviation = Measure-TrackerBodyDeviation -Sent 'commonXtail' -Stored 'commonYtail'

        [bool] $deviation['decided'] | Should -BeTrue
        [int] $deviation['distance'] | Should -Be 1
        [int] $deviation['firstDifference'] | Should -Be 6
        [double] $deviation['similarity'] | Should -BeGreaterThan 0.90
    }

    # More than one differing character is measured over the whole region, not just its first character.
    It 'measures a run of differing characters correctly' {
        $deviation = Measure-TrackerBodyDeviation -Sent 'commonABCtail' -Stored 'commonXYZtail'

        [int] $deviation['distance'] | Should -Be 3
        [int] $deviation['firstDifference'] | Should -Be 6
    }

    It 'measures two separated differences correctly' {
        $deviation = Measure-TrackerBodyDeviation -Sent 'aXbYc' -Stored 'aWbZc'

        [int] $deviation['distance'] | Should -Be 2
    }

    # A length gap of one leaves the similarity ceiling exactly at the threshold, which the gap alone
    # cannot settle. The distance is measured, so two short bodies that share nothing but differ in
    # length by one are seen as wholly dissimilar rather than adjudicated on the ceiling.
    It 'measures the distance rather than trusting a ceiling that ties the threshold' {
        $deviation = Measure-TrackerBodyDeviation -Sent 'ABCDEFGHIJ' -Stored 'KLMNOPQRS'

        [bool] $deviation['decided'] | Should -BeTrue
        [int] $deviation['distance'] | Should -Be 10
        [double] $deviation['similarity'] | Should -Be 0.0
    }

    # A length gap wider than the threshold allows settles the question without the table, because the
    # distance cannot be smaller than that gap.
    It 'decides a wide length gap without measuring the distance' {
        $deviation = Measure-TrackerBodyDeviation -Sent ('a' * 100) -Stored ''

        [bool] $deviation['decided'] | Should -BeTrue
        [double] $deviation['similarity'] | Should -Be 0.0
        $deviation['distance'] | Should -BeNullOrEmpty
    }
}

Describe 'Tracker publication plan hash' {
    It 'hashes the approved set to a lowercase 64-character digest' {
        $hash = Get-TrackerPublicationPlanHash -Item @(
            [ordered]@{ findingId = 'F-1'; title = 'First'; body = 'Body one' }
        )

        $hash | Should -Match '^[0-9a-f]{64}$'
    }

    It 'hashes the same approved set the same way' {
        $item = @(
            [ordered]@{ findingId = 'F-1'; title = 'First'; body = 'Body one' }
            [ordered]@{ findingId = 'F-2'; title = 'Second'; body = 'Body two' }
        )

        Get-TrackerPublicationPlanHash -Item $item | Should -BeExactly (Get-TrackerPublicationPlanHash -Item $item)
    }

    It 'hashes a changed body to a different digest' {
        $before = Get-TrackerPublicationPlanHash -Item @([ordered]@{ findingId = 'F-1'; title = 'First'; body = 'Body one' })
        $after = Get-TrackerPublicationPlanHash -Item @([ordered]@{ findingId = 'F-1'; title = 'First'; body = 'Body one changed' })

        $after | Should -Not -BeExactly $before
    }

    # The fields are length-prefixed rather than joined by a separator, so no character a field may hold
    # - including the control characters a separator scheme once used - can imitate a field boundary and
    # let a different split of the same characters hash the same.
    It 'distinguishes a set whose fields a separator scheme would concatenate the same' {
        $nul = [char] 0
        $left = Get-TrackerPublicationPlanHash -Item @([ordered]@{ findingId = 'F-1'; title = "x${nul}y"; body = 'z' })
        $right = Get-TrackerPublicationPlanHash -Item @([ordered]@{ findingId = 'F-1'; title = 'x'; body = "y${nul}z" })

        $right | Should -Not -BeExactly $left
    }
}

Describe 'Tracker continuation arguments' {
    It 'accepts a continuation carrying every coordinate' {
        { Assert-TrackerContinuationArgument -AcceptStoredRewrite -AcceptWorkItemId '4001' -AcceptExpectedBodyHash 'e' -AcceptStoredBodyHash 's' } |
            Should -Not -Throw
    }

    It 'refuses a continuation missing a coordinate' {
        { Assert-TrackerContinuationArgument -AcceptStoredRewrite -AcceptWorkItemId '4001' -AcceptExpectedBodyHash 'e' -AcceptStoredBodyHash '' } |
            Should -Throw -ExpectedMessage '*together with -AcceptStoredRewrite*'
    }

    It 'refuses an acceptance coordinate given without the continuation switch' {
        { Assert-TrackerContinuationArgument -AcceptWorkItemId '4001' -AcceptExpectedBodyHash '' -AcceptStoredBodyHash '' } |
            Should -Throw -ExpectedMessage '*apply only when continuing a paused run*'
    }

    It 'accepts a fresh run naming no acceptance at all' {
        { Assert-TrackerContinuationArgument -AcceptWorkItemId '' -AcceptExpectedBodyHash '' -AcceptStoredBodyHash '' } |
            Should -Not -Throw
    }
}

Describe 'Tracker publication outcome' {
    BeforeEach {
        Reset-TrackerState
    }

    It 'holds exactly these fields, in this order' {
        $run = Invoke-TestTrackerRun

        @($run['outcome'].Keys) -join ',' |
            Should -BeExactly 'schemaVersion,status,message,platform,trackerUrl,resultPath,publishedItemCount,failedOperationCount,ambiguousOperationCount,summary,adjudication,items'
    }

    It 'reports every filed item on success' {
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $outcome = $run['outcome']
        $outcome['status'] | Should -BeExactly 'succeeded'
        $outcome['message'] | Should -BeNullOrEmpty
        $outcome['platform'] | Should -BeExactly 'gitHub'
        $outcome['trackerUrl'] | Should -BeExactly 'https://github.com/octo/widget/issues'
        $outcome['resultPath'] | Should -BeExactly ([string] $run['resultPath'])
        $outcome['publishedItemCount'] | Should -Be 2
        $outcome['failedOperationCount'] | Should -Be 0
        $outcome['ambiguousOperationCount'] | Should -Be 0
        $outcome['summary'] | Should -BeExactly 'the summary'

        $items = @($outcome['items'])
        $items.Count | Should -Be 2
        @($items[0].Keys) -join ',' | Should -BeExactly 'findingId,itemId,url,title'
        $items[0]['findingId'] | Should -BeExactly 'F-1'
        $items[0]['itemId'] | Should -BeExactly '101'
        $items[0]['url'] | Should -BeExactly 'https://github.com/octo/widget/issues/101'
        $items[0]['title'] | Should -BeExactly 'Title of F-1'
    }

    # Nothing resumes a tracker run, so the list of what exists is the most valuable thing a failed
    # one can hand back.
    It 'reports what was filed before the failure' {
        Add-TrackerFault -AtCreation 3 -Refused
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
                (New-TestFinding -Id 'F-3')
            ))

        $outcome = $run['outcome']
        $outcome['status'] | Should -BeExactly 'failed'
        $outcome['message'] | Should -BeLike '*Body is too long*'
        $outcome['publishedItemCount'] | Should -Be 2
        $outcome['failedOperationCount'] | Should -Be 1
        $outcome['ambiguousOperationCount'] | Should -Be 0
        @($outcome['items'] | ForEach-Object { [string] $_['findingId'] }) -join ',' | Should -BeExactly 'F-1,F-2'
    }

    # The summary counts the whole approved set, so relaying it over a run that filed part of that
    # set would tell the reader a review was delivered that is not there.
    It 'withholds the summary from a run that filed part of the review' {
        Add-TrackerFault -AtCreation 2 -Refused
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $run['outcome']['summary'] | Should -BeNullOrEmpty
        $run['outcome']['publishedItemCount'] | Should -Be 1
        @($run['outcome'].Keys) | Should -Contain 'summary'
    }

    It 'counts a creation whose outcome is unknown apart from a refused one' {
        Add-TrackerFault -AtCreation 2 -Lost
        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            ))

        $run['outcome']['failedOperationCount'] | Should -Be 0
        $run['outcome']['ambiguousOperationCount'] | Should -Be 1
        $run['outcome']['publishedItemCount'] | Should -Be 1
    }

    It 'ends a failed run in failure rather than in success' {
        Add-TrackerFault -AtCreation 1 -Refused
        $run = Invoke-TestTrackerRun

        { Assert-TrackerPublicationComplete -Result $run['result'] } |
            Should -Throw -ExpectedMessage '*Body is too long*'
    }

    It 'ends a successful run without complaint' {
        $run = Invoke-TestTrackerRun

        { Assert-TrackerPublicationComplete -Result $run['result'] } | Should -Not -Throw
    }

    It 'reports a state it cannot explain rather than saying nothing' {
        $result = New-TrackerPublicationResult `
            -Target $script:GitHubTracker `
            -State (New-TestGitHubState) `
            -Item (New-TrackerItemSet -Finding @(New-TestFinding)) `
            -NowUtc $script:FixedNowUtc

        { Assert-TrackerPublicationComplete -Result $result } |
            Should -Throw -ExpectedMessage "*ended in state 'inProgress'*"
    }
}

Describe 'Tracker publication verification' {
    BeforeEach {
        Reset-TrackerState
    }

    It 'reads each item before creating the next and verifies the full set after all creations' {
        Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
            )) | Out-Null

        (Get-TrackerOperationSequence) -join ',' |
            Should -BeExactly 'createIssue,readIssue,createIssue,readIssue,readIssue,readIssue'
    }

    It 'stops before creating another item when the second item does not round-trip' {
        Add-TrackerBodyRewrite -AtCreation 2 -Body 'body changed by the service'

        $run = Invoke-TestTrackerRun -Item (New-TrackerItemSet -Finding @(
                (New-TestFinding -Id 'F-1')
                (New-TestFinding -Id 'F-2')
                (New-TestFinding -Id 'F-3')
            ))

        $run['failure'] | Should -Not -BeNullOrEmpty
        $run['failure'].Exception.Message | Should -Match 'body'
        $script:TrackerIssues.Count | Should -Be 2
        (Get-TrackerOperationSequence) -join ',' |
            Should -BeExactly 'createIssue,readIssue,createIssue,readIssue'
    }

    It 'refuses an item whose stored body is not the one that was sent' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'what the run sent' }
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_101'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = 'Terminate the retry loop'
            body              = 'what the service stored'
            descriptionFormat = 'Markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike "*Item 101, filed for finding 'F-1', holds a body the run did not send*"
    }

    It 'refuses an item whose stored title is not the one that was sent' {
        $item = [ordered]@{ findingId = 'F-1'; title = 'Terminate the retry loop'; body = 'the body' }
        $record = [ordered]@{
            itemId            = '101'
            itemNodeId        = 'I_101'
            url               = 'https://github.com/octo/widget/issues/101'
            title             = 'terminate the retry loop'
            body              = 'the body'
            descriptionFormat = 'Markdown'
        }

        $verdict = Get-TrackerItemStoredVerdict -Item $item -Record $record -Platform 'gitHub'
        $verdict['verdict'] | Should -BeExactly 'reject'
        $verdict['reason'] | Should -BeLike '*holds a title the run did not send*'
    }

    It 'records the verification failure and reports it' {
        $run = Invoke-TestTrackerRun

        # The service is made to disagree with what it stored, after the run's own read back.
        $stored = $script:TrackerIssues[0]
        $stored['body'] = 'rewritten by the service'

        $result = $run['result']
        $transport = New-TestTransport
        $thrown = { Invoke-TrackerPublicationVerification `
                -Transport $transport `
                -Target $script:GitHubTracker `
                -State (New-TestGitHubState) `
                -Item (New-TrackerItemSet -Finding @(New-TestFinding -Id 'F-1')) `
                -Result $result `
                -ResultPath ([string] $run['resultPath']) } | Should -Throw -PassThru

        $thrown.Exception.Message | Should -BeLike '*holds a body the run did not send*'
        $verifyOperation = Find-TrackerPublicationOperation -Result $result -FindingId '' -Operation 'verifyItems'
        $verifyOperation['outcome'] | Should -BeExactly 'failed'
        $verifyOperation['message'] | Should -BeLike '*holds a body the run did not send*'
    }
}

Describe 'Publication failure classification' {
    It 'reads a refusal the service stated as a definite failure' {
        $exception = New-PullRequestServiceRefusalException -Message 'the service refused'

        Get-PublicationFailureOutcome -Exception $exception | Should -BeExactly 'failed'
    }

    # A request that vanished between here and the service may have been applied before the answer
    # was lost, and recording it as a definite failure would invite a repeat that files the same
    # finding twice.
    It 'reads anything else as unknown' {
        Get-PublicationFailureOutcome -Exception ([System.IO.IOException]::new('the connection was closed')) |
            Should -BeExactly 'ambiguous'
    }

    It 'reads a wrapped refusal as a definite failure' {
        $inner = New-PullRequestServiceRefusalException -Message 'the service refused'
        $wrapped = [System.InvalidOperationException]::new('while publishing', $inner)

        Get-PublicationFailureOutcome -Exception $wrapped | Should -BeExactly 'failed'
    }

    It 'reads no exception at all as unknown' {
        Get-PublicationFailureOutcome -Exception $null | Should -BeExactly 'ambiguous'
    }
}

Describe 'Tracker publication composition root' {
    # The composition root cannot be run against a service from this suite, so the two orderings it
    # alone is responsible for are read out of its syntax tree. Both are guarantees a failed run
    # depends on: a journal that reached disk before the first creation, and an account of what
    # exists that reaches the caller before the failure is reported.
    BeforeAll {
        $script:ExecutableAst = [System.Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $script:ExecutableRoot 'Publish-ReviewTrackerItems.ps1'),
            [ref] $null,
            [ref] $null)

        function Find-ExecutableCommand {
            param([Parameter(Mandatory)][string] $Name)

            $found = $script:ExecutableAst.FindAll({
                    param($node)
                    $node -is [System.Management.Automation.Language.CommandAst] -and
                    [string] $node.GetCommandName() -ceq $Name
                }, $true)

            , @($found)
        }
    }

    It 'writes the journal before it files anything' {
        $write = Find-ExecutableCommand -Name 'Write-TrackerPublicationResult'
        $publish = Find-ExecutableCommand -Name 'Invoke-TrackerPublication'

        # The fresh run and the continuation each write the journal before the run begins; whichever
        # path is taken, the journal is on disk before anything is filed.
        $write.Count | Should -BeGreaterThan 0
        $publish.Count | Should -Be 1
        foreach ($entry in $write) {
            $entry.Extent.StartOffset | Should -BeLessThan $publish[0].Extent.StartOffset
        }
    }

    It 'checks the source snapshot before rendering, transport or continuation writes' {
        $binding = Find-ExecutableCommand -Name 'Assert-ApprovedFindingContext'
        $binding.Count | Should -Be 1
        foreach ($name in @('New-TrackerItemSet', 'New-PullRequestTransport', 'Write-TrackerPublicationResult', 'Set-TrackerPublicationAcceptedRewrite')) {
            $commands = Find-ExecutableCommand -Name $name
            $commands.Count | Should -BeGreaterThan 0
            foreach ($command in $commands) {
                $binding[0].Extent.StartOffset | Should -BeLessThan $command.Extent.StartOffset
            }
        }
    }

    It 'emits the outcome before it reports a failed run' {
        $publish = Find-ExecutableCommand -Name 'Invoke-TrackerPublication'
        $try = $publish[0].Parent
        while ($null -ne $try -and $try -isnot [System.Management.Automation.Language.TryStatementAst]) {
            $try = $try.Parent
        }

        $try | Should -Not -BeNullOrEmpty
        $try.CatchClauses.Count | Should -Be 1

        # The failure is held rather than raised, so the account of what exists reaches the caller
        # whatever became of the run.
        $catch = $try.CatchClauses[0]
        @($catch.Body.FindAll({
                    param($node)
                    $node -is [System.Management.Automation.Language.ThrowStatementAst]
                }, $true)).Count | Should -Be 0

        $outcome = Find-ExecutableCommand -Name 'New-TrackerPublicationOutcome'
        $assert = Find-ExecutableCommand -Name 'Assert-TrackerPublicationComplete'

        $outcome.Count | Should -Be 1
        $assert.Count | Should -Be 1
        $try.Extent.EndOffset | Should -BeLessThan $outcome[0].Extent.StartOffset
        $outcome[0].Extent.StartOffset | Should -BeLessThan $assert[0].Extent.StartOffset
    }
}

Describe 'Tracker publication preflight' {
    # Everything asserted here is refused before the run builds a transport, which is what keeps the
    # whole suite offline and is itself the property under test: a run that cannot proceed leaves
    # the tracker and publication records unchanged, retaining only the coordination lock.
    It 'refuses <Case> before tracker writes' -ForEach @(
        @{ Case = 'a null fingerprint'; Change = { param($approved, $context) $approved['reviewedContextHash'] = $null }; Expected = '*requires a non-null reviewedContextHash*'; OmitContext = $false }
        @{ Case = 'a missing fingerprint'; Change = { param($approved, $context) $approved.Remove('reviewedContextHash') }; Expected = "*missing 'reviewedContextHash'*"; OmitContext = $false }
        @{ Case = 'a missing capture'; Change = { param($approved, $context) }; Expected = '*require -ContextPath*'; OmitContext = $true }
        @{ Case = 'changed discussion'; Change = { param($approved, $context) $context['threads'][0]['comments'][0]['content'] = 'Unchecked edit.' }; Expected = '*not bound to this checked capture*'; OmitContext = $false }
        @{ Case = 'a changed source'; Change = { param($approved, $context) $context['pullRequest']['reviewedSourceCommit'] = 'unchecked-source' }; Expected = '*not bound to this checked capture*'; OmitContext = $false }
        @{ Case = 'another pull request'; Change = { param($approved, $context) $context['pullRequestUrl'] = 'https://github.com/octo/widget/pull/8' }; Expected = '*not bound to this checked capture*'; OmitContext = $false }
    ) {
        $context = New-TestContext -Thread @(New-TestThread -Id 'T1' -Comment @(New-TestComment -Id 'C1'))
        $approved = New-TestApproved -Context $context
        & $Change $approved $context
        $fixture = New-TrackerFixture -Approved $approved -Context $(if ($OmitContext) { $null } else { $context })
        # A missing later preflight input also keeps a regressed binding guard from reaching a service.
        Remove-Item -LiteralPath $fixture['diagnosticsPath']

        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget')

        $run['exitCode'] | Should -Not -Be 0
        $run['standardOutput'] | Should -BeExactly ''
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike $Expected
        @(Get-ChildItem -LiteralPath $fixture['outputDirectory'] -Force).Name | Should -Be @('tracker-publication-result.json.lock')
    }

    It 'accepts checked evidence independently of the tracker destination: <Destination>' -ForEach @(
        @{ Destination = 'GitHub'; Arguments = @('-Url', 'https://github.com/other/issues-repo') }
        @{ Destination = 'Azure DevOps'; Arguments = @('-Url', 'https://dev.azure.com/contoso/widgets', '-WorkItemType', 'Bug', '-AreaPath', 'widgets') }
    ) {
        $fixture = New-TrackerFixture -Context (New-TestContext)
        # Reaching this later local refusal proves valid evidence passed without contacting a service.
        Remove-Item -LiteralPath $fixture['diagnosticsPath']

        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument $Arguments

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*focus-area cache diagnostics record*'
        @(Get-ChildItem -LiteralPath $fixture['outputDirectory'] -Force).Name | Should -Be @('tracker-publication-result.json.lock')
    }

    It 'refuses a URL that names no repository' {
        $fixture = New-TrackerFixture
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo')

        $run['exitCode'] | Should -Not -Be 0
        $run['standardOutput'] | Should -BeExactly ''
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*names no GitHub repository*'
    }

    It 'refuses an Azure DevOps field for a GitHub tracker' {
        $fixture = New-TrackerFixture
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @(
            '-Url', 'https://github.com/octo/widget'
            '-WorkItemType', 'Bug'
        )

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*-WorkItemType was supplied for a GitHub tracker*'
    }

    It 'refuses an Azure DevOps tracker carrying no work item type' {
        $fixture = New-TrackerFixture
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @(
            '-Url', 'https://dev.azure.com/contoso/widgets'
        )

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*requires -WorkItemType*'
    }

    It 'refuses an approved artifact holding no findings' {
        $fixture = New-TrackerFixture -Approved ([ordered]@{
                schemaVersion           = 2
                target                  = $null
                reviewedSourceCommit    = $null
                reviewedMergeBaseCommit = $null
                reviewedContextHash     = $null
                findings                = @()
            })
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget')

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*holds no findings*'
    }

    It 'refuses a follow-up it was given no capture to resolve' {
        $fixture = New-TrackerFixture -Approved (New-TestApproved -Finding @(
                New-TestFinding -Id 'F-1' -FollowUpThreadId 'T1' -FollowUpCommentId 'C1'
            ))
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget')

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*no captured discussion was supplied*'
    }

    It 'refuses to file over the journal an earlier run left' {
        $fixture = New-TrackerFixture
        Write-TrackerJournal -OutputDirectory ([string] $fixture['outputDirectory']) -Status 'failed' -CreationOutcome @('succeeded')

        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget')

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*would file the review into the tracker a second time*'
    }

    # The journal is read before anything else is judged, because a new one is written over it and a
    # run that overwrote it would have destroyed the only account of what the earlier run filed.
    It 'leaves the journal an earlier run left exactly as it was' {
        $fixture = New-TrackerFixture
        $journalPath = Get-TrackerPublicationResultPath -OutputDirectory ([string] $fixture['outputDirectory'])
        Write-TrackerJournal -OutputDirectory ([string] $fixture['outputDirectory']) -Status 'failed' -CreationOutcome @('succeeded')
        $before = [System.Convert]::ToBase64String([System.IO.File]::ReadAllBytes($journalPath))

        Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget') | Out-Null

        [System.Convert]::ToBase64String([System.IO.File]::ReadAllBytes($journalPath)) | Should -BeExactly $before
    }

    It 'refuses a diagnostics record that is not there' {
        $fixture = New-TrackerFixture
        Remove-Item -LiteralPath ([string] $fixture['diagnosticsPath']) -Force

        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo/widget')

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*focus-area cache diagnostics record*'
    }

    It 'leaves only the coordination lock in the output directory when it refuses' {
        $fixture = New-TrackerFixture
        $run = Invoke-PublishReviewTrackerItems -Fixture $fixture -ExtraArgument @('-Url', 'https://github.com/octo')

        $run['exitCode'] | Should -Not -Be 0
        @(Get-ChildItem -LiteralPath ([string] $fixture['outputDirectory']) -Force).Name | Should -Be @('tracker-publication-result.json.lock')
    }
}
