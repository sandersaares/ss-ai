BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in the order the
    # composition root uses: value handling and failure signaling first, then identity and target
    # coordinates, each transport with the adapter that uses it, the shared models, and finally the
    # dispatch and the publication rules.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')
    . (Join-Path $script:LibraryRoot 'FindingMarkdown.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTransportHost.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')
    . (Join-Path $script:LibraryRoot 'ReviewPublication.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPublication.ps1')

    # The host running this suite stands in for the program the executable tests launch. Naming the
    # running executable rather than the command 'pwsh' keeps the child on the same build as the
    # parent and removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # Nothing in this file reads a clock. Every record that carries an instant is given this one, so
    # a run at any moment produces the same document.
    $script:FixedNowUtc = [DateTimeOffset]::new(2026, 3, 9, 8, 30, 0, [TimeSpan]::Zero)

    $script:SourceCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    $script:MergeBaseCommit = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'

    $script:GitHubTarget = [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = 'octo'
        project       = $null
        repository    = 'widget'
        pullRequestId = 7
        webUrl        = 'https://github.com/octo/widget/pull/7'
    }

    $script:AzureDevOpsTarget = [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = 'https://dev.azure.com/contoso'
        organization  = 'contoso'
        project       = 'widgets'
        repository    = 'widget'
        pullRequestId = 42
        webUrl        = 'https://dev.azure.com/contoso/widgets/_git/widget/pullrequest/42'
    }

    $script:DiffText = @'
diff --git a/src/app.ps1 b/src/app.ps1
index 1111111..2222222 100644
--- a/src/app.ps1
+++ b/src/app.ps1
@@ -1,4 +1,5 @@
 line one
-line two
+line two changed
+line two added
 line three
 line four
diff --git a/docs/readme.md b/docs/readme.md
index 3333333..4444444 100644
--- a/docs/readme.md
+++ b/docs/readme.md
@@ -10,3 +10,4 @@
 alpha
 beta
+gamma
 delta
'@

    # Answers the Git calls the diff read makes from recorded output, so no assertion depends on what
    # any clone on this machine happens to contain.
    function New-FakeGitInvoker {
        param(
            [string] $DiffText = $script:DiffText,

            [string[]] $PresentCommit = @($script:SourceCommit, $script:MergeBaseCommit),

            [switch] $NoRepository
        )

        $bytes = [Text.Encoding]::UTF8.GetBytes($DiffText)
        $present = [string[]] @($PresentCommit)
        $hasRepository = -not $NoRepository

        {
            param(
                [Parameter(Mandatory)]
                [string[]] $ArgumentList
            )

            $arguments = @($ArgumentList)

            # The first two arguments name the repository, so the verb is the third.
            $verb = $arguments[2]

            if ($verb -eq 'rev-parse') {
                if ($hasRepository) {
                    return [ordered]@{ exitCode = 0; standardOutputBytes = [Text.Encoding]::UTF8.GetBytes(".git`n"); standardErrorText = '' }
                }
                return [ordered]@{ exitCode = 128; standardOutputBytes = [byte[]]::new(0); standardErrorText = 'fatal: not a git repository' }
            }

            if ($verb -eq 'cat-file') {
                $commit = $arguments[$arguments.Count - 1] -replace '\^\{commit\}$', ''
                if ($present -contains $commit) {
                    return [ordered]@{ exitCode = 0; standardOutputBytes = [byte[]]::new(0); standardErrorText = '' }
                }
                return [ordered]@{ exitCode = 1; standardOutputBytes = [byte[]]::new(0); standardErrorText = "fatal: Not a valid object name $commit" }
            }

            return [ordered]@{ exitCode = 0; standardOutputBytes = $bytes; standardErrorText = '' }
        }.GetNewClosure()
    }

    function New-TestDiff {
        param([string] $DiffText = $script:DiffText)

        @{
            mergeBaseCommit = $script:MergeBaseCommit
            sourceCommit = $script:SourceCommit
            files = ConvertFrom-GitDiffText -Text $DiffText
        }
    }

    function New-TestFinding {
        param(
            [string] $Id = 'F-1',
            [string] $Path = 'src/app.ps1',
            [ValidateSet('line', 'file')]
            [string] $Kind = 'line',
            [int] $StartLine = 3,
            [int] $EndLine = 3,
            [string] $FollowUpThreadId = '',
            [string] $FollowUpCommentId = ''
        )

        $anchor = if ($Kind -eq 'file') {
            [ordered]@{ kind = 'file'; path = $Path }
        } else {
            [ordered]@{ kind = 'line'; path = $Path; startLine = $StartLine; endLine = $EndLine; side = 'right' }
        }

        $finding = [ordered]@{
            schemaVersion     = 1
            id                = $Id
            sourceFindingIds  = @()
            origin            = 'reviewer'
            title             = "Title of $Id"
            description       = "Description of $Id"
            reasoning         = "Reasoning of $Id"
            consequence       = "Consequence of $Id"
            recommendedAction = "Recommended action of $Id"
            references        = @()
            impactedLocations = @()
            anchor            = $anchor
        }

        if ($FollowUpThreadId) {
            $finding['followUp'] = [ordered]@{ threadId = $FollowUpThreadId; commentId = $FollowUpCommentId }
        }

        $finding
    }

    function New-TestApproved {
        param(
            [object[]] $Finding = @(),
            $Target = $script:GitHubTarget,
            [string] $SourceCommit = $script:SourceCommit,
            [string] $MergeBaseCommit = $script:MergeBaseCommit
        )

        if ($Finding.Count -eq 0) { $Finding = @(New-TestFinding) }

        [ordered]@{
            schemaVersion           = 2
            target                  = $Target
            reviewedSourceCommit    = $SourceCommit
            reviewedMergeBaseCommit = $MergeBaseCommit
            reviewedContextHash     = Get-PullRequestEvidenceHash -Document (New-TestContext -Target $Target)
            findings                = @($Finding)
        }
    }

    function New-TestThread {
        param(
            [string] $Id = 'PRRT_active',
            [string] $Resolution = 'active',
            [string] $Kind = 'text',
            [string] $OpeningCommentId = 'PRRC_opening',
            $Context = $null
        )

        if ($null -eq $Context) { $Context = [ordered]@{ path = 'src/app.ps1'; rightLine = 3 } }

        [ordered]@{
            id               = $Id
            url              = "https://example.invalid/thread/$Id"
            rawStatus        = $Resolution
            resolution       = $Resolution
            kind             = $Kind
            isOutdated       = $false
            context          = $Context
            anchorKey        = "src/app.ps1:3"
            properties       = [ordered]@{}
            openingCommentId = $OpeningCommentId
            decision         = $null
            comments         = @()
        }
    }

    function New-TestContext {
        param(
            $Target = $script:GitHubTarget,
            [string] $Platform = 'gitHub',
            [object[]] $Thread = @()
        )

        [ordered]@{
            schemaVersion        = 3
            fetchedUtc           = '2026-03-09T08:00:00.0000000Z'
            platform             = $Platform
            repositoryUrl        = 'https://github.com/octo/widget'
            pullRequestUrl       = 'https://github.com/octo/widget/pull/7'
            target               = $Target
            authenticatedIdentity = [ordered]@{ uniqueName = 'runner' }
            pullRequest          = [ordered]@{
                reviewedSourceCommit    = $script:SourceCommit
                reviewedMergeBaseCommit = $script:MergeBaseCommit
                isDraft                 = $false
                mergeState              = 'clean'
            }
            reviewers            = @()
            revisions            = @()
            threads              = @($Thread)
        }
    }

    function New-TestState {
        param(
            [string] $Status = 'open',
            [bool] $IsActive = $true,
            [string] $SourceCommit = $script:SourceCommit,
            [string] $MergeBaseCommit = $script:MergeBaseCommit,
            $PendingReviewId = $null
        )

        [ordered]@{
            platform          = 'gitHub'
            pullRequestNodeId = 'PR_node'
            url               = 'https://github.com/octo/widget/pull/7'
            status            = $Status
            isActive          = $IsActive
            isDraft           = $false
            sourceCommit      = $SourceCommit
            mergeBaseCommit   = $MergeBaseCommit
            identity          = 'runner'
            pendingReviewId   = $PendingReviewId
        }
    }

    function New-TestBodyMap {
        param([object[]] $Finding)

        $map = [ordered]@{}
        foreach ($item in $Finding) {
            $map[[string] $item['id']] = "body of $($item['id'])"
        }
        $map
    }

    function New-TestPublicationPlanEntry {
        param(
            [ValidateSet('newLineThread', 'newFileThread', 'reply')]
            [string] $Operation = 'newLineThread',

            [string] $FindingId = 'F-1',

            [switch] $RequiresReopen
        )

        [ordered]@{
            operation        = $Operation
            findingId        = $FindingId
            body             = "body of $FindingId"
            anchor           = if ($Operation -eq 'reply') { $null } else { [ordered]@{ kind = 'line'; path = 'src/app.ps1'; startLine = 3; endLine = 3; side = 'right' } }
            wasDowngraded    = $false
            threadId         = if ($Operation -eq 'reply') { 'PRRT_active' } else { $null }
            commentId        = if ($Operation -eq 'reply') { 'PRRC_opening' } else { $null }
            requiresReopen   = [bool] $RequiresReopen
            changeTrackingId = $null
        }
    }

    # Builds a scratch directory that is deleted with the test run, so nothing this file writes
    # survives it and no assertion depends on what a previous run left behind.
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

    function Write-TextFixture {
        param(
            [Parameter(Mandatory)]
            [string] $Path,

            [Parameter(Mandatory)]
            [AllowEmptyString()]
            [string] $Text
        )

        [System.IO.File]::WriteAllText($Path, $Text, [System.Text.UTF8Encoding]::new($false))
        $Path
    }

    # Lays out a complete, valid set of inputs and hands back the paths, so a test that is about one
    # broken input changes only that input.
    function New-PublicationFixture {
        param(
            $Approved = $null,
            $Context = $null,
            $Diagnostics = $null
        )

        if ($null -eq $Approved) { $Approved = New-TestApproved }
        if ($null -eq $Context) { $Context = New-TestContext }
        if ($null -eq $Diagnostics) { $Diagnostics = [ordered]@{ schemaVersion = 1; outcome = 'miss'; areaCount = 0 } }

        $root = New-ScratchDirectory
        $outputDirectory = Join-Path $root 'out'
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null

        [ordered]@{
            root            = $root
            outputDirectory = $outputDirectory
            resultPath      = Join-Path $outputDirectory 'publication-result.json'
            approvedPath    = Write-JsonFixture -Path (Join-Path $root 'approved-findings.json') -Value $Approved
            contextPath     = Write-JsonFixture -Path (Join-Path $root 'refresh-pr-context.json') -Value $Context
            diagnosticsPath = Write-JsonFixture -Path (Join-Path $root 'focus-area-cache-diagnostics.json') -Value $Diagnostics
            repositoryPath  = $root
        }
    }

    function Invoke-PublishPullRequestReview {
        param(
            [Parameter(Mandatory)]
            [System.Collections.IDictionary] $Fixture,

            [string] $Url = 'https://github.com/octo/widget/pull/7',

            [string] $RepositoryPath = '',

            [string] $MergeApprovalPath
        )

        $executablePath = Join-Path $script:ExecutableRoot 'Publish-PullRequestReview.ps1'
        if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
            throw "The executable under test is missing: $executablePath"
        }

        if (-not $RepositoryPath) { $RepositoryPath = [string] $Fixture['repositoryPath'] }

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $script:PowerShellPath
        $arguments = @(
            '-NoProfile'
            '-NonInteractive'
            '-File'
            $executablePath
            '-Url'
            $Url
            '-ApprovedFindingsPath'
            [string] $Fixture['approvedPath']
            '-ContextPath'
            [string] $Fixture['contextPath']
            '-DiagnosticsPath'
            [string] $Fixture['diagnosticsPath']
            '-RepositoryPath'
            $RepositoryPath
            '-OutputDirectory'
            [string] $Fixture['outputDirectory']
        )
        if ($MergeApprovalPath) { $arguments += '-MergeApprovalPath', $MergeApprovalPath }
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

            # Both streams are drained concurrently: a refusal writes a formatted error record, and
            # reading one stream to its end before starting on the other stalls as soon as either
            # outgrows a pipe buffer.
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

Describe 'Approved artifact preflight' {
    It 'refuses an artifact whose schema version is not the one publication reads' {
        $fixture = New-PublicationFixture
        $document = New-TestApproved
        $document['schemaVersion'] = 99
        Write-JsonFixture -Path $fixture['approvedPath'] -Value $document | Out-Null

        { Read-ApprovedFindingArtifact -Path $fixture['approvedPath'] } |
            Should -Throw -ExpectedMessage '*reports version 99*'
    }

    It 'refuses the working finding artifact in place of the approved set' {
        $fixture = New-PublicationFixture
        $document = New-TestApproved
        $document['kind'] = 'consolidated'
        Write-JsonFixture -Path $fixture['approvedPath'] -Value $document | Out-Null

        { Read-ApprovedFindingArtifact -Path $fixture['approvedPath'] } |
            Should -Throw -ExpectedMessage "*reports kind 'consolidated'*"
    }

    It 'refuses an artifact carrying a field publication does not know' {
        $fixture = New-PublicationFixture
        $document = New-TestApproved
        $document['reviewedBy'] = 'somebody'
        Write-JsonFixture -Path $fixture['approvedPath'] -Value $document | Out-Null

        { Read-ApprovedFindingArtifact -Path $fixture['approvedPath'] } |
            Should -Throw -ExpectedMessage "*unknown field 'reviewedBy'*"
    }

    It 'refuses an artifact that records no reviewed commit' {
        $fixture = New-PublicationFixture
        $document = New-TestApproved
        $document['reviewedSourceCommit'] = ''
        Write-JsonFixture -Path $fixture['approvedPath'] -Value $document | Out-Null

        $approved = Read-ApprovedFindingArtifact -Path $fixture['approvedPath']

        { Assert-PullRequestApprovedArtifact -Approved $approved -Path $fixture['approvedPath'] } |
            Should -Throw -ExpectedMessage "*reports no 'reviewedSourceCommit'*"
    }

    It 'refuses an artifact naming no pull request when publishing onto one' {
        $fixture = New-PublicationFixture
        $document = New-TestApproved
        $document['target'] = $null
        Write-JsonFixture -Path $fixture['approvedPath'] -Value $document | Out-Null

        $approved = Read-ApprovedFindingArtifact -Path $fixture['approvedPath']

        { Assert-PullRequestApprovedArtifact -Approved $approved -Path $fixture['approvedPath'] } |
            Should -Throw -ExpectedMessage '*names no pull request*'
    }

    It 'refuses two findings sharing one identifier' {
        $findings = @((New-TestFinding -Id 'F-1'), (New-TestFinding -Id 'F-1' -StartLine 4 -EndLine 4))

        { Assert-ApprovedFinding -Finding $findings -Path 'approved-findings.json' } |
            Should -Throw -ExpectedMessage "*more than one finding 'F-1'*"
    }

    It 'refuses a finding that is missing a section the comment would need' {
        $finding = New-TestFinding
        $finding.Remove('recommendedAction')

        { Assert-ApprovedFinding -Finding @($finding) -Path 'approved-findings.json' } |
            Should -Throw -ExpectedMessage '*recommendedAction*'
    }

    It 'refuses an approved set that holds nothing' {
        { Assert-ApprovedFinding -Finding @() -Path 'approved-findings.json' } |
            Should -Throw -ExpectedMessage '*holds no findings*'
    }

    It 'accepts an approved set that holds nothing when the caller allows it' {
        { Assert-ApprovedFinding -Finding @() -Path 'approved-findings.json' -AllowEmpty } |
            Should -Not -Throw
    }
}

Describe 'Merge approval preflight' {
    It 'reads merge decisions through prepared commands and refuses stale approval before publication' {
        $fixture = New-PublicationFixture
        $approvalPath = Join-Path $fixture.outputDirectory 'merge-approval.json'
        $context = New-TestContext
        Write-JsonFixture -Path $approvalPath -Value @{
            schemaVersion = 1; pullRequestUrl = $context.pullRequestUrl
            approveMerge = $true; approvedSourceCommit = ('c' * 40)
        } | Out-Null
        $result = Invoke-PublishPullRequestReview -Fixture $fixture -MergeApprovalPath $approvalPath
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'different source commit'
        Test-Path -LiteralPath $fixture.resultPath | Should -BeFalse
    }

    It 'rejects string-valued merge decisions before any remote publication' {
        $fixture = New-PublicationFixture
        $approvalPath = Join-Path $fixture.outputDirectory 'merge-approval.json'
        $context = New-TestContext
        Write-JsonFixture -Path $approvalPath -Value @{
            schemaVersion = 1; pullRequestUrl = $context.pullRequestUrl
            approveMerge = 'false'; approvedSourceCommit = $script:SourceCommit
        } | Out-Null
        $result = Invoke-PublishPullRequestReview -Fixture $fixture -MergeApprovalPath $approvalPath
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'schema'
        Test-Path -LiteralPath $fixture.resultPath | Should -BeFalse
    }

    It 'refuses approving the merge of a GitHub pull request the running account opened' {
        $context = [ordered]@{ platform = 'gitHub'; pullRequest = [ordered]@{ isAuthoredBySelf = $true } }

        { Assert-PullRequestMergeApproval -Context $context -ApproveMerge } |
            Should -Throw -ExpectedMessage '*does not approve the merge of their own pull request*'
    }

    It 'allows approving the merge of a GitHub pull request someone else opened' {
        $context = [ordered]@{ platform = 'gitHub'; pullRequest = [ordered]@{ isAuthoredBySelf = $false } }

        { Assert-PullRequestMergeApproval -Context $context -ApproveMerge } | Should -Not -Throw
    }

    It 'refuses approving the merge of an Azure DevOps pull request the running account opened' {
        # A reviewer does not approve the merge of their own pull request on either platform, even
        # though Azure DevOps would accept the vote.
        $context = [ordered]@{ platform = 'azureDevOps'; pullRequest = [ordered]@{ isAuthoredBySelf = $true } }

        { Assert-PullRequestMergeApproval -Context $context -ApproveMerge } |
            Should -Throw -ExpectedMessage '*does not approve the merge of their own pull request*'
    }

    It 'allows approving the merge of an Azure DevOps pull request someone else opened' {
        $context = [ordered]@{ platform = 'azureDevOps'; pullRequest = [ordered]@{ isAuthoredBySelf = $false } }

        { Assert-PullRequestMergeApproval -Context $context -ApproveMerge } | Should -Not -Throw
    }

    It 'ignores self-authorship when the merge is not being approved' {
        $context = [ordered]@{ platform = 'gitHub'; pullRequest = [ordered]@{ isAuthoredBySelf = $true } }

        { Assert-PullRequestMergeApproval -Context $context } | Should -Not -Throw
    }
}

Describe 'Target agreement' {
    It 'accepts a command line, an artifact and a capture naming one pull request' {
        $approved = New-TestApproved
        $context = New-TestContext

        { Assert-PullRequestPublicationTarget -Target $script:GitHubTarget -Approved $approved -Context $context } |
            Should -Not -Throw
    }

    It 'ignores casing in the coordinates the services treat as case-insensitive' {
        $target = [ordered]@{
            platform      = 'gitHub'
            organization  = 'OCTO'
            project       = $null
            repository    = 'Widget'
            pullRequestId = 7
        }

        { Assert-PullRequestPublicationTarget -Target $target -Approved (New-TestApproved) -Context (New-TestContext) } |
            Should -Not -Throw
    }

    It 'refuses an artifact produced for a different pull request' {
        $otherTarget = [ordered]@{
            platform      = 'gitHub'
            collectionUrl = $null
            organization  = 'octo'
            project       = $null
            repository    = 'widget'
            pullRequestId = 9
            webUrl        = 'https://github.com/octo/widget/pull/9'
        }

        { Assert-PullRequestPublicationTarget `
                -Target $script:GitHubTarget `
                -Approved (New-TestApproved -Target $otherTarget) `
                -Context (New-TestContext) } |
            Should -Throw -ExpectedMessage '*the approved finding artifact is 9*'
    }

    It 'refuses a capture taken from a different repository' {
        $otherTarget = [ordered]@{
            platform      = 'gitHub'
            collectionUrl = $null
            organization  = 'octo'
            project       = $null
            repository    = 'gadget'
            pullRequestId = 7
            webUrl        = 'https://github.com/octo/gadget/pull/7'
        }

        { Assert-PullRequestPublicationTarget `
                -Target $script:GitHubTarget `
                -Approved (New-TestApproved) `
                -Context (New-TestContext -Target $otherTarget) } |
            Should -Throw -ExpectedMessage '*the captured discussion differ in repository*'
    }
}

Describe 'Live pull request state' {
    It 'accepts an open pull request still at the reviewed commits' {
        { Assert-PullRequestPublicationState -State (New-TestState) -Approved (New-TestApproved) } |
            Should -Not -Throw
    }

    It 'refuses a pull request that is no longer open' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'closed' -IsActive $false) `
                -Approved (New-TestApproved) } |
            Should -Throw -ExpectedMessage '*is closed rather than open*'
    }

    It 'names both ways forward when it refuses a closed pull request' {
        $message = $null
        try {
            Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'closed' -IsActive $false) `
                -Approved (New-TestApproved)
        } catch {
            $message = $_.Exception.Message
        }

        $message | Should -BeLike '*-PublishToClosedPullRequest*'
        $message | Should -BeLike '*Publish-ReviewTrackerItems.ps1*'
    }

    It 'publishes onto a closed pull request when told to' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'completed' -IsActive $false) `
                -Approved (New-TestApproved) `
                -AllowClosed } |
            Should -Not -Throw
    }

    # A completed pull request has merged, which moves the merge base by definition. Comparing it
    # would refuse every pull request the override exists for.
    It 'stops comparing the merge base once a closed pull request is allowed' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'completed' -IsActive $false -MergeBaseCommit 'dddddddddddddddddddddddddddddddddddddddd') `
                -Approved (New-TestApproved) `
                -AllowClosed } |
            Should -Not -Throw
    }

    # A source commit that moved means the author pushed code the review never saw, which the state
    # of the pull request does nothing to excuse.
    It 'keeps comparing the source commit once a closed pull request is allowed' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'completed' -IsActive $false -SourceCommit 'cccccccccccccccccccccccccccccccccccccccc') `
                -Approved (New-TestApproved) `
                -AllowClosed } |
            Should -Throw -ExpectedMessage '*source commit is now cccccccccccccccccccccccccccccccccccccccc*'
    }

    # The relaxation answers a merge base moved by a merge that happened. An open pull request has
    # not merged, so a caller who passed the override pre-emptively keeps the comparison.
    It 'keeps comparing the merge base of a pull request that is still open' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -MergeBaseCommit 'dddddddddddddddddddddddddddddddddddddddd') `
                -Approved (New-TestApproved) `
                -AllowClosed } |
            Should -Throw -ExpectedMessage '*merge base is now dddddddddddddddddddddddddddddddddddddddd*'
    }

    It 'refuses a source commit that moved since the approval' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -SourceCommit 'cccccccccccccccccccccccccccccccccccccccc') `
                -Approved (New-TestApproved) } |
            Should -Throw -ExpectedMessage "*source commit is now cccccccccccccccccccccccccccccccccccccccc*"
    }

    It 'refuses a merge base that moved since the approval' {
        { Assert-PullRequestPublicationState `
                -State (New-TestState -MergeBaseCommit 'dddddddddddddddddddddddddddddddddddddddd') `
                -Approved (New-TestApproved) } |
            Should -Throw -ExpectedMessage '*merge base is now dddddddddddddddddddddddddddddddddddddddd*'
    }

    It 'records nothing while refusing, so a refused run has written no result' {
        $fixture = New-PublicationFixture

        { Assert-PullRequestPublicationState `
                -State (New-TestState -Status 'merged' -IsActive $false) `
                -Approved (New-TestApproved) } | Should -Throw

        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }
}

Describe 'Pending review clearance' {
    It 'passes through when the identity holds no draft' {
        $state = New-TestState
        $cleared = Clear-PullRequestPendingReview `
            -Transport ([ordered]@{}) `
            -Target $script:GitHubTarget `
            -State $state

        $cleared['pendingReviewId'] | Should -BeNullOrEmpty
    }

    It 'refuses to proceed past a draft the identity already holds' {
        { Clear-PullRequestPendingReview `
                -Transport ([ordered]@{}) `
                -Target $script:GitHubTarget `
                -State (New-TestState -PendingReviewId 'PRR_existing') } |
            Should -Throw -ExpectedMessage '*-DiscardPendingReview*'
    }
}

Describe 'Local repository preflight' {
    It 'names the checkout when there is no repository to read the diff from' {
        { Assert-PullRequestPublicationRepository `
                -Invoker (New-FakeGitInvoker -NoRepository) `
                -RepositoryPath 'C:\clone' `
                -MergeBaseCommit $script:MergeBaseCommit `
                -SourceCommit $script:SourceCommit } |
            Should -Throw -ExpectedMessage "*no readable Git repository at 'C:\clone'*"
    }

    It 'names both commits to fetch when one of them is absent' {
        { Assert-PullRequestPublicationRepository `
                -Invoker (New-FakeGitInvoker -PresentCommit @($script:MergeBaseCommit)) `
                -RepositoryPath 'C:\clone' `
                -MergeBaseCommit $script:MergeBaseCommit `
                -SourceCommit $script:SourceCommit } |
            Should -Throw -ExpectedMessage "*git fetch*$($script:MergeBaseCommit)*$($script:SourceCommit)*"
    }
}

Describe 'Diagnostics preflight' {
    It 'refuses a diagnostics record that is not there' {
        $root = New-ScratchDirectory

        { Read-PublicationDiagnostics -Path (Join-Path $root 'missing.json') } |
            Should -Throw -ExpectedMessage '*no focus-area cache diagnostics record*'
    }

    It 'refuses a diagnostics record that cannot be parsed' {
        $root = New-ScratchDirectory
        $path = Write-TextFixture -Path (Join-Path $root 'focus-area-cache-diagnostics.json') -Text '{ not json'

        { Read-PublicationDiagnostics -Path $path } |
            Should -Throw -ExpectedMessage '*cannot be read*'
    }

    It 'refuses a diagnostics record that is not a JSON object' {
        $root = New-ScratchDirectory
        $path = Write-TextFixture -Path (Join-Path $root 'focus-area-cache-diagnostics.json') -Text '[1,2,3]'

        { Read-PublicationDiagnostics -Path $path } |
            Should -Throw -ExpectedMessage '*is not a JSON object*'
    }
}

Describe 'Output directory preflight' {
    It 'leaves nothing behind after proving the directory accepts a write' {
        $root = New-ScratchDirectory
        $outputDirectory = Join-Path $root 'out'

        Assert-PublicationOutputDirectory -OutputDirectory $outputDirectory

        Test-Path -LiteralPath $outputDirectory -PathType Container | Should -BeTrue
        @(Get-ChildItem -LiteralPath $outputDirectory -Force).Count | Should -Be 0
    }
}

Describe 'Publication record preflight' {
    It 'refuses a record with a published finding' {
        $outputDirectory = New-ScratchDirectory
        $plan = @(New-TestPublicationPlanEntry)
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Throw -ExpectedMessage '*1 finding published*'
    }

    It 'accepts a record whose only settled operation is a started review' {
        $outputDirectory = New-ScratchDirectory
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan @() `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'startReview' -FindingId '' -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Not -Throw
    }

    It 'accepts a record whose failed verification read is the only failed operation' {
        $outputDirectory = New-ScratchDirectory
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan @() `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'startReview' -FindingId '' -Outcome 'succeeded'
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'verifyFindings' -FindingId '' -Outcome 'failed'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Not -Throw
    }

    It 'refuses a record with an ambiguous started review' {
        $outputDirectory = New-ScratchDirectory
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan @() `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'startReview' -FindingId '' -Outcome 'ambiguous'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Throw -ExpectedMessage '*0 findings published*1 operation whose outcome it could not determine*'
    }

    It 'accepts a record whose only publishing operation failed definitively' {
        $outputDirectory = New-ScratchDirectory
        $plan = @(New-TestPublicationPlanEntry)
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'failed'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Not -Throw
    }

    It 'refuses a record whose only publishing operation is ambiguous' {
        $outputDirectory = New-ScratchDirectory
        $plan = @(New-TestPublicationPlanEntry)
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'ambiguous'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Throw -ExpectedMessage '*0 findings published*1 operation whose outcome it could not determine*'
    }

    It 'ignores a record naming a different pull request' {
        $outputDirectory = New-ScratchDirectory
        $otherTarget = [ordered]@{
            platform      = 'gitHub'
            collectionUrl = $null
            organization  = 'octo'
            project       = $null
            repository    = 'widget'
            pullRequestId = 8
            webUrl        = 'https://github.com/octo/widget/pull/8'
        }
        $plan = @(New-TestPublicationPlanEntry)
        $result = New-PullRequestPublicationResult `
            -Target $otherTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'succeeded'
        Write-PullRequestPublicationResult -Result $result -Path (Get-PullRequestPublicationResultPath -OutputDirectory $outputDirectory)

        { Assert-PullRequestPublicationRecord -OutputDirectory $outputDirectory -Target $script:GitHubTarget } |
            Should -Not -Throw
    }
}

Describe 'Anchor resolution' {
    It 'keeps a line anchor the reviewed diff changes' {
        $anchored = Resolve-PullRequestFindingAnchor -Finding @(New-TestFinding -StartLine 3 -EndLine 3) -Diff (New-TestDiff)

        $anchored[0]['anchor']['kind'] | Should -Be 'line'
        $anchored[0]['anchor']['endLine'] | Should -Be 3
        $anchored[0]['anchor']['wasDowngraded'] | Should -BeFalse
    }

    It 'downgrades a line outside every hunk to the file and records the downgrade' {
        $anchored = Resolve-PullRequestFindingAnchor -Finding @(New-TestFinding -StartLine 90 -EndLine 90) -Diff (New-TestDiff)

        $anchored[0]['anchor']['kind'] | Should -Be 'file'
        $anchored[0]['anchor']['wasDowngraded'] | Should -BeTrue

        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding @(New-TestFinding)) `
            -Context (New-TestContext)

        $plan[0]['operation'] | Should -Be 'newFileThread'
        $plan[0]['wasDowngraded'] | Should -BeTrue

        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        $recorded = @($result['operations'] | Where-Object { $_['operation'] -eq 'newFileThread' })
        $recorded.Count | Should -Be 1
        $recorded[0]['wasDowngraded'] | Should -BeTrue
        $recorded[0]['anchor']['downgradeReason'] | Should -Match 'line 90'
    }

    It 'refuses a finding anchored to a file the pull request does not change' {
        { Resolve-PullRequestFindingAnchor `
                -Finding @(New-TestFinding -Path 'src/absent.ps1') `
                -Diff (New-TestDiff) } |
            Should -Throw -ExpectedMessage "*Finding 'F-1' cannot be anchored*"
    }

    It 'leaves a follow-up unanchored, because its position is the thread it answers' {
        $finding = New-TestFinding -Id 'F-2' -Path 'src/absent.ps1' -FollowUpThreadId 'PRRT_active' -FollowUpCommentId 'PRRC_opening'
        $anchored = Resolve-PullRequestFindingAnchor -Finding @($finding) -Diff (New-TestDiff)

        $anchored[0]['anchor'] | Should -BeNullOrEmpty
    }
}

Describe 'Publication plan' {
    It 'publishes new threads before replies, each in the order it was approved' {
        $findings = @(
            (New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
            (New-TestFinding -Id 'F-2' -FollowUpThreadId 'PRRT_active' -FollowUpCommentId 'PRRC_opening')
            (New-TestFinding -Id 'F-3' -Path 'docs/readme.md' -Kind 'file')
        )
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)
        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext -Thread @(New-TestThread))

        @($plan | ForEach-Object { $_['findingId'] }) | Should -Be @('F-1', 'F-3', 'F-2')
        @($plan | ForEach-Object { $_['operation'] }) | Should -Be @('newLineThread', 'newFileThread', 'reply')
    }

    It 'marks a reply into a resolved thread for reopening and one into an open thread not' {
        $findings = @(
            (New-TestFinding -Id 'F-1' -FollowUpThreadId 'PRRT_active' -FollowUpCommentId 'PRRC_opening')
            (New-TestFinding -Id 'F-2' -FollowUpThreadId 'PRRT_resolved' -FollowUpCommentId 'PRRC_opening')
        )
        $threads = @(
            (New-TestThread -Id 'PRRT_active' -Resolution 'active')
            (New-TestThread -Id 'PRRT_resolved' -Resolution 'resolved')
        )
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)
        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext -Thread $threads)

        $plan[0]['requiresReopen'] | Should -BeFalse
        $plan[1]['requiresReopen'] | Should -BeTrue
    }

    It 'refuses a finding that rendered to nothing' {
        $findings = @(New-TestFinding -Id 'F-1')
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)

        { New-PullRequestPublicationPlan `
                -AnchoredFinding $anchored `
                -Body ([ordered]@{ 'F-1' = '   ' }) `
                -Context (New-TestContext) } |
            Should -Throw -ExpectedMessage "*'F-1' rendered to no text*"
    }

    It 'names the finding whose body cannot be rendered' {
        $finding = New-TestFinding -Id 'F-1'
        $finding['impactedLocations'] = @([ordered]@{ path = 'src/app.ps1'; startLine = 'not a line number' })

        { New-PullRequestFindingBody -Finding (, $finding) } |
            Should -Throw -ExpectedMessage "*Finding 'F-1' cannot be rendered for publication*"
    }

    It 'lists the anchored files once each, so identity is asked about exactly what will be written' {
        $findings = @(
            (New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
            (New-TestFinding -Id 'F-2' -StartLine 4 -EndLine 4)
            (New-TestFinding -Id 'F-3' -Path 'docs/readme.md' -Kind 'file')
        )
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)

        $paths = Get-PullRequestPublicationAnchoredPath -AnchoredFinding $anchored
        ($paths -join ',') | Should -Be 'src/app.ps1,docs/readme.md'
    }

    It 'counts the findings that answer an existing thread' {
        $findings = @(
            (New-TestFinding -Id 'F-1')
            (New-TestFinding -Id 'F-2' -FollowUpThreadId 'PRRT_active' -FollowUpCommentId 'PRRC_opening')
        )

        Get-FindingFollowUpCount -Finding $findings | Should -Be 1
    }
}

Describe 'Follow-up targets' {
    It 'accepts a GitHub review thread on the code' {
        $finding = New-TestFinding -Id 'F-1' -FollowUpThreadId 'PRRT_active' -FollowUpCommentId 'PRRC_opening'

        { Assert-PullRequestFollowUpTarget -Finding @($finding) -Context (New-TestContext -Thread @(New-TestThread)) } |
            Should -Not -Throw
    }

    It 'refuses a GitHub thread the capture never recorded' {
        $finding = New-TestFinding -Id 'F-1' -FollowUpThreadId 'PRRT_absent' -FollowUpCommentId 'PRRC_opening'

        { Assert-PullRequestFollowUpTarget -Finding @($finding) -Context (New-TestContext -Thread @(New-TestThread)) } |
            Should -Throw -ExpectedMessage "*Finding 'F-1' cannot be published as a follow-up*"
    }

    It 'refuses a GitHub comment on the pull request rather than on the code' {
        $finding = New-TestFinding -Id 'F-1' -FollowUpThreadId 'IC_1' -FollowUpCommentId 'PRRC_opening'
        $thread = New-TestThread -Id 'IC_1' -Context ([System.Management.Automation.Internal.AutomationNull]::Value)
        $thread['context'] = $null

        { Assert-PullRequestFollowUpTarget -Finding @($finding) -Context (New-TestContext -Thread @($thread)) } |
            Should -Throw -ExpectedMessage '*review thread*'
    }

    It 'accepts an Azure DevOps discussion thread that carries no file anchor' {
        $finding = New-TestFinding -Id 'F-1' -FollowUpThreadId '11' -FollowUpCommentId '1'
        $thread = New-TestThread -Id '11' -OpeningCommentId '1'
        $thread['context'] = $null
        $context = New-TestContext -Platform 'azureDevOps' -Target $script:AzureDevOpsTarget -Thread @($thread)

        { Assert-PullRequestFollowUpTarget -Finding @($finding) -Context $context } | Should -Not -Throw
    }

    It 'refuses an Azure DevOps thread the service raised for its own activity' {
        $finding = New-TestFinding -Id 'F-1' -FollowUpThreadId '11' -FollowUpCommentId '1'
        $thread = New-TestThread -Id '11' -Kind 'system' -OpeningCommentId '1'
        $context = New-TestContext -Platform 'azureDevOps' -Target $script:AzureDevOpsTarget -Thread @($thread)

        { Assert-PullRequestFollowUpTarget -Finding @($finding) -Context $context } |
            Should -Throw -ExpectedMessage '*service activity*'
    }
}

Describe 'Platform dispatch' {
    It 'reaches the GitHub adapter for a GitHub target' {
        $requested = [System.Collections.Generic.List[string]]::new()
        $transport = New-GitHubCliTransport -InvokeGh {
            param([string[]] $ArgumentList, [string] $StandardInput)

            $requested.Add(($ArgumentList -join ' ') + ' ' + [string] $StandardInput)
            [ordered]@{
                exitCode            = 0
                standardOutputBytes = [Text.Encoding]::UTF8.GetBytes('{"data":{"addPullRequestReview":{"pullRequestReview":{"id":"PRR_1","state":"PENDING"}}}}')
                standardErrorText   = ''
            }
        }.GetNewClosure()

        $review = Start-PullRequestReviewForTarget `
            -Transport $transport `
            -Target $script:GitHubTarget `
            -State (New-TestState)

        $review['reviewId'] | Should -Be 'PRR_1'
        @($requested | Where-Object { $_ -match 'addPullRequestReview' }).Count | Should -Be 1
    }

    It 'reaches the Azure DevOps adapter for an Azure DevOps target' {
        # The fake refuses every request, which is the assertion: beginning a review on a service
        # that holds no draft must write nothing and ask nothing.
        $transport = New-AdoRestTransport `
            -CollectionUrl 'https://dev.azure.com/contoso' `
            -SendRequest {
                param([string] $Method, [string] $Url, [hashtable] $Headers, $Body)

                throw "Beginning an Azure DevOps review reached the service at '$Url'."
            } `
            -GetAccessToken { 'token' }

        $state = New-TestState
        $state['iterationId'] = 3

        $review = Start-PullRequestReviewForTarget `
            -Transport $transport `
            -Target $script:AzureDevOpsTarget `
            -State $state

        $review['reviewId'] | Should -Be 'iteration/3'
        $review['iterationId'] | Should -Be 3
        @($review['published']).Count | Should -Be 0
    }

    It 'refuses a platform nobody serves' {
        $target = [ordered]@{ platform = 'bitbucket'; organization = 'octo'; repository = 'widget'; pullRequestId = 7 }

        { Confirm-PullRequestReviewForTarget `
                -Transport ([ordered]@{}) `
                -Target $target `
                -Review ([ordered]@{ reviewId = 'PRR_1' }) } |
            Should -Throw -ExpectedMessage "*'bitbucket'*"
    }

    It 'answers the file identity question on GitHub with nothing to add' {
        $identity = Get-PullRequestFileIdentityForTarget `
            -Transport ([ordered]@{}) `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Path @('src/app.ps1')

        $identity.Count | Should -Be 0
    }

    It 'plans an anchored finding the platform supplied no identity for' {
        # The empty map GitHub answers with is the ordinary case, not a gap: a service that
        # addresses a file by path supplies an identity for no file at all. A platform that requires
        # one refuses in its own file identity read, before the plan is built, so this builder is
        # never asked to tell the two apart.
        $findings = @(New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)

        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext) `
            -FileIdentity ([ordered]@{})

        @($plan).Count | Should -Be 1
        $plan[0]['operation'] | Should -Be 'newLineThread'
        $plan[0]['changeTrackingId'] | Should -BeNullOrEmpty
    }

    It 'carries the identity the platform supplied for an anchored file' {
        $findings = @(New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)

        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext) `
            -FileIdentity ([ordered]@{ 'src/app.ps1' = 11 })

        $plan[0]['changeTrackingId'] | Should -Be 11
    }
}

Describe 'Result record' {
    It 'states the whole intended sequence before anything is attempted' {
        $findings = @(
            (New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
            (New-TestFinding -Id 'F-2' -FollowUpThreadId 'PRRT_resolved' -FollowUpCommentId 'PRRC_opening')
        )
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)
        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext -Thread @(New-TestThread -Id 'PRRT_resolved' -Resolution 'resolved'))

        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc

        @($result['operations'] | ForEach-Object { $_['operation'] }) |
            Should -Be @('startReview', 'newLineThread', 'reply', 'verifyFindings', 'completeReview', 'reopenThread')
        @($result['operations'] | ForEach-Object { $_['outcome'] } | Sort-Object -Unique) | Should -Be @('pending')
        $result['status'] | Should -Be 'inProgress'
        $result['startedUtc'] | Should -Be '2026-03-09T08:30:00.0000000Z'
    }

    It 'classifies a refusal the service marked as a definite failure' {
        Get-PublicationFailureOutcome -Exception (New-PullRequestServiceRefusalException -Message 'Field is not defined.') |
            Should -Be 'failed'
    }

    It 'classifies a lost response as ambiguous, because nobody can tell whether it landed' {
        Get-PublicationFailureOutcome -Exception ([InvalidOperationException]::new('The GitHub CLI could not be run: the pipe was closed.')) |
            Should -Be 'ambiguous'
    }

    It 'refuses to record an operation the plan never held' {
        $plan = @()
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc

        { Set-PullRequestPublicationOperationOutcome `
                -Result $result `
                -Operation 'newLineThread' `
                -FindingId 'F-9' `
                -Outcome 'succeeded' } |
            Should -Throw -ExpectedMessage '*was not built from the plan being published*'
    }
}

Describe 'Verification' {
    It 'refuses a review the service still reports as unpublished' {
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan @() `
            -NowUtc $script:FixedNowUtc

        { Assert-PullRequestPublicationVerified `
                -Result $result `
                -Plan @() `
                -Verification ([ordered]@{ state = 'PENDING'; isPublished = $false; body = ''; comments = @() }) `
                -SummaryBody '' } |
            Should -Throw -ExpectedMessage "*reports state 'PENDING'*"
    }

    It 'refuses a review carrying fewer published comments than it accepted' {
        $findings = @(New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)
        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext)
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'succeeded' -ThreadId 'PRRT_1' -CommentId 'PRRC_1'

        { Assert-PullRequestPublicationVerified `
                -Result $result `
                -Plan $plan `
                -Verification ([ordered]@{ state = 'COMMENTED'; isPublished = $true; body = 'the summary'; comments = @() }) `
                -SummaryBody 'the summary' } |
            Should -Throw -ExpectedMessage '*published fewer comments than it was given*'
    }

    It 'counts only the comments the service reports as visible' {
        $findings = @(New-TestFinding -Id 'F-1' -StartLine 3 -EndLine 3)
        $anchored = Resolve-PullRequestFindingAnchor -Finding $findings -Diff (New-TestDiff)
        $plan = New-PullRequestPublicationPlan `
            -AnchoredFinding $anchored `
            -Body (New-TestBodyMap -Finding $findings) `
            -Context (New-TestContext)
        $result = New-PullRequestPublicationResult `
            -Target $script:GitHubTarget `
            -State (New-TestState) `
            -Plan $plan `
            -NowUtc $script:FixedNowUtc
        Set-PullRequestPublicationOperationOutcome -Result $result -Operation 'newLineThread' -FindingId 'F-1' -Outcome 'succeeded' -ThreadId 'PRRT_1' -CommentId 'PRRC_1'

        $verification = [ordered]@{
            state       = 'COMMENTED'
            isPublished = $true
            body        = 'the summary'
            comments    = @([ordered]@{ id = 'PRRC_1'; isPublished = $true; body = 'body of F-1'; path = 'src/app.ps1'; line = 3; subjectType = 'LINE' })
        }

        { Assert-PullRequestPublicationVerified -Result $result -Plan $plan -Verification $verification -SummaryBody 'the summary' } | Should -Not -Throw
    }
}

Describe 'Preflight performs no write' {
    It 'refuses an approved artifact of the wrong schema version and writes nothing' {
        $document = New-TestApproved
        $document['schemaVersion'] = 99
        $fixture = New-PublicationFixture -Approved $document

        $run = Invoke-PublishPullRequestReview -Fixture $fixture

        $run['exitCode'] | Should -Not -Be 0
        $run['standardOutput'] | Should -BeNullOrEmpty
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*reports version 99*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses duplicate identifiers and writes nothing' {
        $document = New-TestApproved -Finding @((New-TestFinding -Id 'F-1'), (New-TestFinding -Id 'F-1'))
        $fixture = New-PublicationFixture -Approved $document

        $run = Invoke-PublishPullRequestReview -Fixture $fixture

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike "*more than one finding 'F-1'*"
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses a follow-up naming a thread the capture never recorded and writes nothing' {
        $document = New-TestApproved -Finding @(New-TestFinding -Id 'F-1' -FollowUpThreadId 'PRRT_absent' -FollowUpCommentId 'PRRC_opening')
        $fixture = New-PublicationFixture -Approved $document

        $run = Invoke-PublishPullRequestReview -Fixture $fixture

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*cannot be published as a follow-up*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses a pull request the artifact was not produced for and writes nothing' {
        $fixture = New-PublicationFixture

        $run = Invoke-PublishPullRequestReview -Fixture $fixture -Url 'https://github.com/octo/widget/pull/9'

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*the approved finding artifact is 7*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses an unreadable diagnostics record and writes nothing' {
        $fixture = New-PublicationFixture
        Write-TextFixture -Path $fixture['diagnosticsPath'] -Text '{ not json' | Out-Null

        $run = Invoke-PublishPullRequestReview -Fixture $fixture

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*cannot be read*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses a checkout that does not hold the reviewed commits and writes nothing' {
        $fixture = New-PublicationFixture

        # The scratch directory is not a repository at all, which is the state a reviewer reaches by
        # naming the wrong checkout.
        $run = Invoke-PublishPullRequestReview -Fixture $fixture

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*no readable Git repository*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }

    It 'refuses a commit the checkout does not hold and names what to fetch' {
        $fixture = New-PublicationFixture

        $run = Invoke-PublishPullRequestReview -Fixture $fixture -RepositoryPath $script:RepoRoot

        $run['exitCode'] | Should -Not -Be 0
        Get-SingleLineErrorText -Text $run['standardError'] | Should -BeLike '*git fetch*'
        Test-Path -LiteralPath $fixture['resultPath'] | Should -BeFalse
    }
}
