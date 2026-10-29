BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in dependency
    # order, the same order the executables use.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')

    # The host running this suite stands in for the executables it launches. Naming the running
    # executable rather than the command 'pwsh' keeps the child on the same build as the parent and
    # removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # A fixed instant, so a recorded decision's timestamp is an assertion rather than a race.
    $script:FixedNowUtc = [DateTimeOffset]::Parse('2024-05-06T09:10:11.0000000Z', [cultureinfo]::InvariantCulture)

    # The instant every fixture commit is authored and committed at. Pinning it keeps the fixture
    # repository identical on every run and on every host, so nothing in these tests depends on a
    # clock.
    $script:FixedCommitDate = '2024-05-06T09:10:11+00:00'

    # Runs one Git command against a fixture repository and reports its output.
    #
    # The identity is supplied on the command rather than read from the host configuration, because
    # a host that has configured none would otherwise fail the commit rather than the assertion the
    # test is about.
    function Invoke-TestGit {
        param(
            [Parameter(Mandatory)]
            [string] $RepositoryPath,

            [Parameter(Mandatory)]
            [string[]] $ArgumentList
        )

        $arguments = @(
            '-C', $RepositoryPath
            '-c', 'user.name=Review Runner'
            '-c', 'user.email=runner@example.invalid'
        ) + $ArgumentList

        $previousAuthorDate = $env:GIT_AUTHOR_DATE
        $previousCommitterDate = $env:GIT_COMMITTER_DATE
        $env:GIT_AUTHOR_DATE = $script:FixedCommitDate
        $env:GIT_COMMITTER_DATE = $script:FixedCommitDate
        try {
            $output = @(& git @arguments 2>&1 | ForEach-Object { [string] $_ })
            if ($LASTEXITCODE -ne 0) {
                throw "git $($ArgumentList -join ' ') failed with exit code ${LASTEXITCODE}: $($output -join ' ')"
            }
            $output -join "`n"
        } finally {
            $env:GIT_AUTHOR_DATE = $previousAuthorDate
            $env:GIT_COMMITTER_DATE = $previousCommitterDate
        }
    }

    # Writes one fixture file with the line endings Git records, so a hunk covers the lines the test
    # states it covers on every host.
    function Write-TestRepositoryFile {
        param(
            [Parameter(Mandatory)]
            [string] $Path,

            [Parameter(Mandatory)]
            [string[]] $Line
        )

        [System.IO.File]::WriteAllText($Path, ($Line -join "`n") + "`n", [System.Text.UTF8Encoding]::new($false))
    }

    # Builds a repository holding the reviewed commits and reports both.
    #
    # The reviewed change touches the head of one file only. That gives the freshness check every
    # case at once: a file the diff still changes, lines of that file no hunk covers, and a file
    # that has left the diff altogether.
    function New-TestRepository {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        New-Item -ItemType Directory -Path (Join-Path $Path 'src') -Force | Out-Null
        Write-TestRepositoryFile -Path (Join-Path $Path 'src\retry.rs') -Line (1..60 | ForEach-Object { "let retry$_ = $_;" })
        Write-TestRepositoryFile -Path (Join-Path $Path 'src\parse.rs') -Line (1..20 | ForEach-Object { "let parse$_ = $_;" })

        Invoke-TestGit -RepositoryPath $Path -ArgumentList @('init', '--quiet', '--initial-branch=main') | Out-Null
        Invoke-TestGit -RepositoryPath $Path -ArgumentList @('add', '--all') | Out-Null
        Invoke-TestGit -RepositoryPath $Path -ArgumentList @('commit', '--quiet', '--message', 'Record the reviewed baseline') | Out-Null
        $mergeBaseCommit = Invoke-TestGit -RepositoryPath $Path -ArgumentList @('rev-parse', 'HEAD')

        Write-TestRepositoryFile -Path (Join-Path $Path 'src\retry.rs') -Line (1..60 | ForEach-Object {
                if ($_ -eq 5) { 'let retry5 = 500;' } else { "let retry$_ = $_;" }
            })
        Invoke-TestGit -RepositoryPath $Path -ArgumentList @('add', '--all') | Out-Null
        Invoke-TestGit -RepositoryPath $Path -ArgumentList @('commit', '--quiet', '--message', 'Raise the retry budget') | Out-Null
        $sourceCommit = Invoke-TestGit -RepositoryPath $Path -ArgumentList @('rev-parse', 'HEAD')

        [ordered]@{
            path            = $Path
            mergeBaseCommit = $mergeBaseCommit
            sourceCommit    = $sourceCommit
        }
    }

    function New-TestFinding {
        param(
            [System.Collections.IDictionary] $Override = @{}
        )

        $finding = [ordered]@{
            title             = 'Terminate the retry loop'
            description       = 'The loop condition is never updated.'
            reasoning         = 'The counter is incremented in a branch that cannot be reached.'
            consequence       = 'A transient failure hangs the caller indefinitely.'
            recommendedAction = 'Increment the counter outside the branch.'
            anchor            = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42 }
        }

        foreach ($key in $Override.Keys) { $finding[[string] $key] = $Override[$key] }

        $finding
    }

    function New-CaptureComment {
        param(
            [Parameter(Mandatory)]
            [string] $Id
        )

        [ordered]@{
            id                 = $Id
            url                = "https://example.invalid/comment/$Id"
            parentId           = $null
            author             = [ordered]@{
                id          = 'reviewer-1'
                displayName = 'Reviewer One'
                uniqueName  = 'reviewer@example.invalid'
                kind        = 'person'
            }
            content            = 'This allocation looks avoidable.'
            publishedUtc       = '2024-05-06T07:08:09.0000000Z'
            lastEditedUtc      = $null
            isEdited           = $false
            isAuthoredBySelf   = $false
            authorship         = 'human'
            authorshipEvidence = @()
        }
    }

    # Builds one thread in the shape the capture writes.
    #
    # The anchor is supplied rather than derived, because whether a thread carries one is exactly
    # what tells a GitHub review thread apart from a flat comment on the pull request.
    function New-CaptureThread {
        param(
            [Parameter(Mandatory)]
            [string] $Id,

            [Parameter(Mandatory)]
            [string] $OpeningCommentId,

            [string] $Kind = 'text'
        )

        [ordered]@{
            id               = $Id
            url              = "https://example.invalid/thread/$Id"
            publishedUtc     = '2024-05-06T07:08:09.0000000Z'
            lastUpdatedUtc   = '2024-05-06T07:08:09.0000000Z'
            eventUtc         = '2024-05-06T07:08:09.0000000Z'
            rawStatus        = 'active'
            resolution       = 'active'
            kind             = $Kind
            isOutdated       = $false
            isAuthoredBySelf = $false
            context          = [ordered]@{
                filePath          = '/src/retry.rs'
                side              = 'right'
                startLine         = 40
                endLine           = 42
                originalStartLine = 40
                originalEndLine   = 42
            }
            anchorKey        = $Id
            properties       = [ordered]@{}
            openingCommentId = $OpeningCommentId
            decision         = $null
            comments         = @(New-CaptureComment -Id $OpeningCommentId)
        }
    }

    # Builds a captured discussion holding the given threads.
    #
    # The schema version is read from the library rather than restated, so a capture format change
    # fails these fixtures at the version check instead of letting them assert against a shape the
    # capture no longer writes.
    function New-CaptureDocument {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Thread,

            [string] $SourceCommit = '1111111111111111111111111111111111111111',

            [string] $MergeBaseCommit = '2222222222222222222222222222222222222222',

            [string] $Platform = 'gitHub'
        )

        [ordered]@{
            schemaVersion         = $script:PullRequestContextSchemaVersion
            fetchedUtc            = '2024-05-06T08:00:00.0000000Z'
            platform              = $Platform
            repositoryUrl         = 'https://example.invalid/widgets'
            pullRequestUrl        = 'https://example.invalid/widgets/pull/7'
            target                = [ordered]@{
                platform      = $Platform
                collectionUrl = $null
                organization  = 'contoso'
                project       = 'widgets'
                repository    = 'widgets'
                pullRequestId = 7
                webUrl        = 'https://example.invalid/widgets/pull/7'
            }
            authenticatedIdentity = [ordered]@{
                id          = 'runner-1'
                displayName = 'Review Runner'
                uniqueName  = 'runner@example.invalid'
                kind        = 'service'
            }
            pullRequest           = [ordered]@{
                title                   = 'Add retry policy'
                description             = $null
                reviewedSourceCommit    = $SourceCommit
                reviewedMergeBaseCommit = $MergeBaseCommit
            }
            reviewers             = @()
            revisions             = @()
            threads               = $Thread
        }
    }

    function Write-TestJsonFile {
        param(
            [Parameter(Mandatory)]
            [string] $Path,

            [Parameter(Mandatory)]
            $Value
        )

        Write-PullRequestOutputFile -Path $Path -Content (ConvertTo-JsonText -Value $Value)
        $Path
    }

    function Read-TestJsonFile {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        ConvertFrom-JsonVerbatim -Text ([System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8))
    }

    # Gives each test its own working directory, so one test's records cannot be read by another.
    function Initialize-TestWorkspace {
        $script:WorkRoot = Join-Path $TestDrive ([guid]::NewGuid())
        New-Item -ItemType Directory -Path $script:WorkRoot -Force | Out-Null

        $script:ArtifactPath = Join-Path $script:WorkRoot 'consolidated.json'
        $script:DecisionsPath = Join-Path $script:WorkRoot 'audit-decisions.json'
        $script:ApprovedPath = Join-Path $script:WorkRoot 'approved-findings.json'
        $script:ContextPath = Join-Path $script:WorkRoot 'pr-context.json'
        $script:PlanPath = Join-Path $script:WorkRoot 'decision-plan.json'
    }

    # Writes a consolidated artifact holding the given findings, in the order given.
    function New-TestConsolidatedArtifact {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Finding
        )

        $document = New-FindingArtifactDocument -Kind 'consolidated'
        foreach ($entry in $Finding) {
            Add-FindingToArtifact -Document $document -Finding $entry -Origin 'reviewer' -SourceFindingId @('g1-1') | Out-Null
        }

        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
        $document
    }

    # Runs one of the skill's executables in a child process and reports its streams separately.
    #
    # Standard output is captured apart from standard error because the caller of these executables
    # parses standard output as JSON, so a refusal that writes anything at all there is a defect the
    # combined stream would hide.
    function Invoke-SkillExecutable {
        param(
            [Parameter(Mandatory)]
            [string] $Name,

            [string[]] $ArgumentList = @()
        )

        $executablePath = Join-Path $script:ExecutableRoot $Name
        if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
            throw "The executable under test is missing: $executablePath"
        }

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $script:PowerShellPath
        $arguments = @('-NoProfile', '-NonInteractive', '-File', $executablePath) + $ArgumentList
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

    # Renders a child process's error stream as a single line.
    function Get-SingleLineErrorText {
        param(
            [AllowEmptyString()]
            [AllowNull()]
            [string] $Text
        )

        (((([string] $Text) -replace '\x1b\[[0-9;]*m', '') -replace '(?m)^\s*\|', ' ') -replace '\s+', ' ').Trim()
    }
}

Describe 'Decision log' {
    BeforeEach { Initialize-TestWorkspace }

    It 'begins empty' {
        $document = New-FindingDecisionDocument

        (Get-JsonArrayProperty -Node $document -Name 'decisions').Count | Should -Be 0
        Resolve-FindingDecision -Document $document -FindingId 'F1' | Should -BeNullOrEmpty
    }

    It 'records the finding, the verdict and the instant' {
        $document = New-FindingDecisionDocument
        $record = Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc

        $record['findingId'] | Should -BeExactly 'F1'
        $record['decision'] | Should -BeExactly 'approved'
        $record['recordedUtc'] | Should -BeExactly '2024-05-06T09:10:11.0000000Z'
    }

    # The reason states why the run did what it did; the feedback is the user's own words, which the
    # pass that improves the review's instructions mines for what was missing.
    It 'carries the reason and the feedback the audit supplied' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord `
            -Document $document `
            -FindingId 'F1' `
            -Decision 'rejected' `
            -Reason 'The user disagreed with the premise.' `
            -Feedback 'We deliberately allow this in generated code.' `
            -NowUtc $script:FixedNowUtc | Out-Null

        $resolved = Resolve-FindingDecision -Document $document -FindingId 'F1'
        $resolved['reason'] | Should -BeExactly 'The user disagreed with the premise.'
        $resolved['feedback'] | Should -BeExactly 'We deliberately allow this in generated code.'
    }

    It 'omits a reason and a feedback the audit did not supply' {
        $document = New-FindingDecisionDocument
        $record = Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc

        $record.Contains('reason') | Should -BeFalse
        $record.Contains('feedback') | Should -BeFalse
    }

    # The log is replayed rather than reconciled, which is what lets the freshness check drop a
    # finding the audit approved without rewriting what the user was told at the time.
    It 'lets a later decision stand over an earlier one' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $document -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'dropped' -Reason 'The code moved on.' -NowUtc $script:FixedNowUtc | Out-Null

        (Resolve-FindingDecision -Document $document -FindingId 'F1')['decision'] | Should -BeExactly 'dropped'
        (Resolve-FindingDecision -Document $document -FindingId 'F2')['decision'] | Should -BeExactly 'approved'
        (Get-JsonArrayProperty -Node $document -Name 'decisions').Count | Should -Be 3
    }

    It 'keeps every entry, so the earlier verdict is still readable' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'dropped' -NowUtc $script:FixedNowUtc | Out-Null

        $decisions = Get-JsonArrayProperty -Node $document -Name 'decisions'
        @($decisions | ForEach-Object { $_['decision'] }) | Should -Be @('approved', 'dropped')
    }

    It 'reads a verdict the audit spelled with a capital' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'Approved' -NowUtc $script:FixedNowUtc | Out-Null

        (Resolve-FindingDecision -Document $document -FindingId 'F1')['decision'] | Should -BeExactly 'approved'
    }

    # Auto-approval is the audit approving an obvious finding on the user's behalf. It is a verdict of
    # its own so the log distinguishes it from a user approval, though its outcome is the same.
    It 'records an auto-approval as its own verdict' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'auto-approved' -Reason 'Obvious: aligns documentation with code.' -NowUtc $script:FixedNowUtc | Out-Null

        (Resolve-FindingDecision -Document $document -FindingId 'F1')['decision'] | Should -BeExactly 'auto-approved'
    }

    It 'refuses a verdict an audit does not reach' {
        $document = New-FindingDecisionDocument

        { Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'deferred' -NowUtc $script:FixedNowUtc } |
            Should -Throw "*'deferred'*'F1'*"
    }

    # Reading an unknown verdict as "not approved" would drop the finding from the approved set
    # while reporting nothing.
    It 'refuses a stored verdict the vocabulary does not know' {
        $document = [ordered]@{
            schemaVersion = 1
            decisions     = @([ordered]@{ findingId = 'F1'; decision = 'maybe'; recordedUtc = '2024-05-06T09:10:11.0000000Z' })
        }

        { Resolve-FindingDecision -Document $document -FindingId 'F1' } | Should -Throw "*'maybe'*'F1'*"
    }

    It 'reads back what it wrote' {
        $document = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $document -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $document

        $reloaded = Read-FindingDecisionDocument -Path $script:DecisionsPath
        (Resolve-FindingDecision -Document $reloaded -FindingId 'F1')['decision'] | Should -BeExactly 'approved'
    }

    It 'refuses a log that is not there' {
        { Read-FindingDecisionDocument -Path $script:DecisionsPath } | Should -Throw '*no decision log*'
    }
}

Describe 'Recording executable' {
    BeforeEach {
        Initialize-TestWorkspace
        New-TestConsolidatedArtifact -Finding @((New-TestFinding), (New-TestFinding)) | Out-Null
    }

    It 'begins a log and appends to it' {
        $first = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F1'
            '-Decision', 'approved'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )

        $first['exitCode'] | Should -Be 0
        $first['standardError'] | Should -BeNullOrEmpty
        $emitted = ConvertFrom-JsonVerbatim -Text $first['standardOutput']
        @($emitted['recorded']) | Should -Be @('F1')

        $second = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F2'
            '-Decision', 'rejected'
            '-Reason', 'The user disagreed.'
            '-Feedback', 'Generated code is exempt from this rule.'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )

        $second['exitCode'] | Should -Be 0

        $document = Read-FindingDecisionDocument -Path $script:DecisionsPath
        (Get-JsonArrayProperty -Node $document -Name 'decisions').Count | Should -Be 2
        (Resolve-FindingDecision -Document $document -FindingId 'F1')['decision'] | Should -BeExactly 'approved'
        $rejected = Resolve-FindingDecision -Document $document -FindingId 'F2'
        $rejected['decision'] | Should -BeExactly 'rejected'
        $rejected['feedback'] | Should -BeExactly 'Generated code is exempt from this rule.'
        $rejected['recordedUtc'] | Should -BeExactly '2024-05-06T09:10:11.0000000Z'
    }

    It 'records an auto-approval through the executable' {
        $result = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F1'
            '-Decision', 'auto-approved'
            '-Reason', 'Obvious: clarifies ambiguous documentation.'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $document = Read-FindingDecisionDocument -Path $script:DecisionsPath
        $recorded = Resolve-FindingDecision -Document $document -FindingId 'F1'
        $recorded['decision'] | Should -BeExactly 'auto-approved'
        $recorded['reason'] | Should -BeExactly 'Obvious: clarifies ambiguous documentation.'
    }

    It 'records a batch named in a plan' {        Write-TestJsonFile -Path $script:PlanPath -Value ([ordered]@{
                schemaVersion = 1; decisions = @(
                    [ordered]@{ findingId = 'F1'; decision = 'approved' }
                    [ordered]@{ findingId = 'F2'; decision = 'dropped'; reason = 'The code moved on.' }
                )
            }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-PlanPath', $script:PlanPath
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        @($emitted['recorded']) | Should -Be @('F1', 'F2')

        $document = Read-FindingDecisionDocument -Path $script:DecisionsPath
        (Resolve-FindingDecision -Document $document -FindingId 'F2')['reason'] | Should -BeExactly 'The code moved on.'
    }

    It 'refuses a request that names both a plan and a finding' {
        Write-TestJsonFile -Path $script:PlanPath -Value ([ordered]@{ schemaVersion = 1; decisions = @([ordered]@{ findingId = 'F1'; decision = 'approved' }) }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-PlanPath', $script:PlanPath
            '-FindingId', 'F1'
            '-Decision', 'approved'
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*not both*'
    }

    # Rewriting the log only after every decision is accepted keeps a rejected batch from disturbing
    # the decisions already reached.
    It 'leaves the log untouched when it refuses a batch' {
        Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F1'
            '-Decision', 'approved'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        ) | Out-Null

        Write-TestJsonFile -Path $script:PlanPath -Value ([ordered]@{
                schemaVersion = 1; decisions = @(
                    [ordered]@{ findingId = 'F2'; decision = 'approved' }
                    [ordered]@{ findingId = 'F3'; decision = 'deferred' }
                )
            }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-PlanPath', $script:PlanPath
        )

        $result['exitCode'] | Should -Not -Be 0
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*decisions/1/decision*'

        $document = Read-FindingDecisionDocument -Path $script:DecisionsPath
        (Get-JsonArrayProperty -Node $document -Name 'decisions').Count | Should -Be 1
    }
}

Describe 'Auto-approval polling' {
    BeforeEach {
        Initialize-TestWorkspace
        $script:Artifact = New-TestConsolidatedArtifact -Finding @(
            New-TestFinding -Override @{ title = 'Align the documented retry behavior' }
            New-TestFinding -Override @{ title = 'Bound the parser' }
        )
        $script:Decisions = New-FindingDecisionDocument
    }

    It 'returns one full finding without consuming it' {
        $first = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $again = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions

        @($first.Keys) | Should -Be @('isComplete', 'remainingCount', 'finding', 'auditFindingIds')
        $first['isComplete'] | Should -BeFalse
        $first['remainingCount'] | Should -Be 2
        $first['auditFindingIds'] | Should -Be @('F1', 'F2')
        (ConvertTo-JsonText -Value $first['finding']) | Should -BeExactly (ConvertTo-JsonText -Value $script:Artifact['findings'][0])
        (ConvertTo-JsonText -Value $again) | Should -BeExactly (ConvertTo-JsonText -Value $first)
        $script:Decisions['decisions'].Count | Should -Be 0
    }

    It 'advances only after a recorded classification and leaves important findings for the audit' {
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null

        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $next['finding']['id'] | Should -BeExactly 'F2'
        $next['remainingCount'] | Should -Be 1
        $next['auditFindingIds'] | Should -Be @('F2')

        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F2' -Decision 'needs-audit' -NowUtc $script:FixedNowUtc | Out-Null
        $done = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $done['isComplete'] | Should -BeTrue
        $done['remainingCount'] | Should -Be 0
        $done['finding'] | Should -BeNullOrEmpty
        $done['auditFindingIds'] | Should -Be @('F2')

        $summary = Get-FindingDecisionSummary -Artifact $script:Artifact -Decision $script:Decisions
        $summary['undecided'] | Should -Be @('F2')
        $summary['approvedFindingIds'] | Should -Be @('F1')
        $summary['rejected'] | Should -Be 0
        $summary['dropped'] | Should -Be 0
        { New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $script:Decisions } | Should -Throw "*'F2'*"
    }

    It 'preserves resumable state when a write fails after <SavedCount> saved classifications' -ForEach @(
        @{ SavedCount = 0; NextId = 'F1' }
        @{ SavedCount = 1; NextId = 'F2' }
    ) {
        $savedText = $null
        if ($SavedCount -gt 0) {
            Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
            Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $script:Decisions
            $savedText = [System.IO.File]::ReadAllText($script:DecisionsPath)
        }
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId $NextId -Decision 'needs-audit' -NowUtc $script:FixedNowUtc | Out-Null

        Mock Write-PullRequestStagingFile {
            param($Path, $Content)
            # Fail after creating a truncated document, not before writing any bytes.
            [System.IO.File]::WriteAllText($Path, '{')
            throw 'Simulated interruption during decision-log persistence.'
        }
        { Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $script:Decisions } |
            Should -Throw '*Simulated interruption*'

        if ($SavedCount -gt 0) {
            [System.IO.File]::ReadAllText($script:DecisionsPath) | Should -BeExactly $savedText
        } else {
            Test-Path -LiteralPath $script:DecisionsPath | Should -BeFalse
        }
        $reloaded = Open-FindingDecisionDocument -Path $script:DecisionsPath
        $reloaded['decisions'].Count | Should -Be $SavedCount
        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $reloaded
        $next['finding']['id'] | Should -BeExactly $NextId
        $next['remainingCount'] | Should -Be (2 - $SavedCount)
        @(Get-ChildItem -LiteralPath $script:WorkRoot -Filter '*.tmp' -Force).Count | Should -Be 0
    }

    It 'resumes a per-finding human audit without approving its remaining findings' {
        foreach ($findingId in @('F1', 'F2')) {
            Add-FindingDecisionRecord -Document $script:Decisions -FindingId $findingId -Decision 'needs-audit' -NowUtc $script:FixedNowUtc | Out-Null
        }
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $script:Decisions

        $pending = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision (Read-FindingDecisionDocument -Path $script:DecisionsPath)
        $pending.isComplete | Should -BeTrue
        $pending.auditFindingIds | Should -Be @('F1', 'F2')

        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $script:Decisions
        $resumed = Read-FindingDecisionDocument -Path $script:DecisionsPath
        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $resumed
        $next.auditFindingIds | Should -Be @('F2')
        { New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $resumed } | Should -Throw "*'F2'*"

        # Re-reading an unanswered item must not consume it or overwrite an earlier verdict.
        $again = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision (Read-FindingDecisionDocument -Path $script:DecisionsPath)
        $again.auditFindingIds | Should -Be @('F2')
        Add-FindingDecisionRecord -Document $resumed -FindingId 'F2' -Decision 'rejected' -Feedback 'Not applicable to this change.' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $resumed
        $finished = Read-FindingDecisionDocument -Path $script:DecisionsPath
        (Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $finished).auditFindingIds.Count | Should -Be 0
        $approved = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $finished
        @($approved.findings | ForEach-Object { $_.id }) | Should -Be @('F1')
        $finished.decisions[-1].feedback | Should -BeExactly 'Not applicable to this change.'
        Test-Path -LiteralPath (Join-Path $script:WorkRoot 'merge-approval.json') | Should -BeFalse
    }

    It 'preserves existing <Verdict> verdicts on resume' -ForEach @(
        @{ Verdict = 'approved' }
        @{ Verdict = 'auto-approved' }
        @{ Verdict = 'rejected' }
        @{ Verdict = 'dropped' }
    ) {
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'needs-audit' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision $Verdict -NowUtc $script:FixedNowUtc | Out-Null
        $before = ConvertTo-JsonText -Value $script:Decisions

        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $next['finding']['id'] | Should -BeExactly 'F2'
        $next['auditFindingIds'] | Should -Be @('F2')
        (ConvertTo-JsonText -Value $script:Decisions) | Should -BeExactly $before
    }

    It 'finishes classification when every finding is <Classification>' -ForEach @(
        @{ Classification = 'auto-approved'; AuditCount = 0 }
        @{ Classification = 'needs-audit'; AuditCount = 2 }
    ) {
        foreach ($findingId in @('F1', 'F2')) {
            Add-FindingDecisionRecord -Document $script:Decisions -FindingId $findingId -Decision $Classification -NowUtc $script:FixedNowUtc | Out-Null
        }

        $done = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $done['isComplete'] | Should -BeTrue
        $done['remainingCount'] | Should -Be 0
        $done['finding'] | Should -BeNullOrEmpty
        $done['auditFindingIds'].Count | Should -Be $AuditCount
    }

    It 'rejects a policy batch containing the user verdict <Verdict> before writing anything' -ForEach @(
        @{ Verdict = 'approved' }
        @{ Verdict = 'rejected' }
        @{ Verdict = 'dropped' }
    ) {
        $requests = @(
            (Get-FindingDecisionRequest -FindingId 'F1' -Decision 'auto-approved')[0]
            (Get-FindingDecisionRequest -FindingId 'F2' -Decision $Verdict)[0]
        )
        { Add-FindingDecisionBatch -Document $script:Decisions -Request $requests -PolicyClassification -NowUtc $script:FixedNowUtc } |
            Should -Throw '*Policy classification cannot record*'
        $script:Decisions['decisions'].Count | Should -Be 0
    }

    It 'rejects user feedback from a policy classification' {
        $requests = Get-FindingDecisionRequest -FindingId 'F1' -Decision 'auto-approved' -Feedback 'The user approved it.'
        { Add-FindingDecisionBatch -Document $script:Decisions -Request $requests -PolicyClassification -NowUtc $script:FixedNowUtc } |
            Should -Throw '*cannot record user feedback*'
        $script:Decisions['decisions'].Count | Should -Be 0
    }

    It 'enforces policy-only verdicts through the executable without changing an existing log' {
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $script:Decisions
        $before = [System.IO.File]::ReadAllText($script:DecisionsPath)
        $result = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F1'
            '-Decision', 'approved'
            '-PolicyClassification'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*Policy classification cannot record*'
        [System.IO.File]::ReadAllText($script:DecisionsPath) | Should -BeExactly $before
    }

    It 'requires renewed audit after a prior approval and includes newly added findings' {
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F2' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'needs-audit' -Reason 'The recommendation materially changed.' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingToArtifact -Document $script:Artifact -Finding (New-TestFinding) -Origin 'synthesis' -SourceFindingId @('g1-1') | Out-Null

        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $next['finding']['id'] | Should -BeExactly 'F3'
        $next['auditFindingIds'] | Should -Be @('F1', 'F3')
        { New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $script:Decisions } | Should -Throw "*'F1'*'F3'*"
    }

    It 'reports completion for an empty consolidated artifact without creating a log' {
        New-TestConsolidatedArtifact -Finding @() | Out-Null
        $result = Invoke-SkillExecutable -Name 'Get-NextAutoApprovalFinding.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['isComplete'] | Should -BeTrue
        $emitted['remainingCount'] | Should -Be 0
        $emitted['finding'] | Should -BeNullOrEmpty
        $emitted['auditFindingIds'].Count | Should -Be 0
        Test-Path -LiteralPath $script:DecisionsPath | Should -BeFalse
    }

    It 'does not return removed findings' {
        Remove-FindingFromArtifact -Document $script:Artifact -FindingId 'F1' -Reason 'Unsupported.' -NowUtc $script:FixedNowUtc | Out-Null
        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $next['finding']['id'] | Should -BeExactly 'F2'
        $next['remainingCount'] | Should -Be 1
        $next['auditFindingIds'] | Should -Be @('F2')
    }

    It 'presents only surviving findings after pre-audit source movement and binds the resulting approval' {
        $initial = New-CaptureDocument -Thread @()
        $checked = New-CaptureDocument -Thread @() -SourceCommit 'force-pushed-source'
        (Get-PullRequestContextDelta -Previous $initial -Current $checked).reviewedSourceCommitChanged | Should -BeTrue

        # Semantic revalidation supplies the retirement judgment; commands preserve it through audit.
        Remove-FindingFromArtifact -Document $script:Artifact -FindingId 'F1' -Reason 'Fixed by the checked source.' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F2' -Decision 'needs-audit' -NowUtc $script:FixedNowUtc | Out-Null
        $next = Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions
        $next.isComplete | Should -BeTrue
        $next.auditFindingIds | Should -Be @('F2')
        $script:Artifact.removed.Count | Should -Be 1
        Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        $approved = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $script:Decisions -Context $checked
        $approved.reviewedSourceCommit | Should -Be 'force-pushed-source'
        $approved.reviewedContextHash | Should -BeExactly (Get-PullRequestEvidenceHash -Document $checked)
        $approved.reviewedContextHash | Should -Not -BeExactly (Get-PullRequestEvidenceHash -Document $initial)
    }

    It 'refuses malformed <InputName> rather than reporting completion' -ForEach @(
        @{ InputName = 'artifact'; Message = '*findings array*' }
        @{ InputName = 'log'; Message = '*decisions array*' }
    ) {
        if ($InputName -eq 'artifact') {
            $script:Artifact.Remove('findings')
        } else {
            $script:Decisions.Remove('decisions')
        }

        { Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions } | Should -Throw $Message
    }

    It 'refuses reviewer artifacts and duplicate identifiers' {
        $reviewer = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'r1' -FocusArea 'Documentation'
        { Get-FindingAutoApprovalWork -Artifact $reviewer -Decision $script:Decisions } | Should -Throw '*consolidated*'

        $script:Artifact['findings'][1]['id'] = 'F1'
        { Get-FindingAutoApprovalWork -Artifact $script:Artifact -Decision $script:Decisions } | Should -Throw "*more than one finding 'F1'*"
    }

    It 'reports missing findings and corrupt logs on stderr without success output' {
        $missing = Invoke-SkillExecutable -Name 'Get-NextAutoApprovalFinding.ps1' -ArgumentList @(
            '-Path', (Join-Path $script:WorkRoot 'missing.json')
            '-DecisionsPath', $script:DecisionsPath
        )
        $missing['exitCode'] | Should -Not -Be 0
        $missing['standardOutput'] | Should -BeNullOrEmpty
        $missing['standardError'] | Should -Not -BeNullOrEmpty

        Write-TestJsonFile -Path $script:DecisionsPath -Value ([ordered]@{
                schemaVersion = 1
                decisions = @([ordered]@{ findingId = 'F1'; decision = 'maybe' })
            }) | Out-Null
        $corrupt = Invoke-SkillExecutable -Name 'Get-NextAutoApprovalFinding.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
        )
        $corrupt['exitCode'] | Should -Not -Be 0
        $corrupt['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $corrupt['standardError']) | Should -BeLike "*'maybe'*'F1'*"
    }

    It 'persists classification across processes and blocks publication until the user decides' {
        foreach ($choice in @(
                @{ id = 'F1'; decision = 'auto-approved'; remaining = 2 }
                @{ id = 'F2'; decision = 'needs-audit'; remaining = 1 }
            )) {
            $poll = Invoke-SkillExecutable -Name 'Get-NextAutoApprovalFinding.ps1' -ArgumentList @(
                '-Path', $script:ArtifactPath
                '-DecisionsPath', $script:DecisionsPath
            )
            $poll['exitCode'] | Should -Be 0
            $poll['standardError'] | Should -BeNullOrEmpty
            $item = ConvertFrom-JsonVerbatim -Text $poll['standardOutput']
            $item['finding']['id'] | Should -BeExactly $choice.id
            $item['remainingCount'] | Should -Be $choice.remaining

            $record = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
                '-OutputDirectory', $script:WorkRoot
                '-FindingId', $choice.id
                '-Decision', $choice.decision
                '-Reason', 'Policy classification.'
                '-PolicyClassification'
                '-NowUtc', '2024-05-06T09:10:11.0000000Z'
            )
            $record['exitCode'] | Should -Be 0
            $record['standardError'] | Should -BeNullOrEmpty
            (ConvertFrom-JsonVerbatim -Text $record['standardOutput'])['recorded'] | Should -Be @($choice.id)
        }

        $poll = Invoke-SkillExecutable -Name 'Get-NextAutoApprovalFinding.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
        )
        $poll['exitCode'] | Should -Be 0
        $done = ConvertFrom-JsonVerbatim -Text $poll['standardOutput']
        $done['isComplete'] | Should -BeTrue
        $done['auditFindingIds'] | Should -Be @('F2')

        $buildArguments = @('-Path', $script:ArtifactPath, '-DecisionsPath', $script:DecisionsPath, '-OutputPath', $script:ApprovedPath)
        $blocked = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList $buildArguments
        $blocked['exitCode'] | Should -Not -Be 0
        $blocked['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $blocked['standardError']) | Should -BeLike "*'F2'*"
        Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse

        $audit = Invoke-SkillExecutable -Name 'Add-FindingDecision.ps1' -ArgumentList @(
            '-OutputDirectory', $script:WorkRoot
            '-FindingId', 'F2'
            '-Decision', 'approved'
            '-NowUtc', '2024-05-06T09:10:11.0000000Z'
        )
        $audit['exitCode'] | Should -Be 0
        $built = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList $buildArguments
        $built['exitCode'] | Should -Be 0
        $summary = ConvertFrom-JsonVerbatim -Text $built['standardOutput']
        $summary['approved'] | Should -Be 2
        $summary['autoApproved'] | Should -Be 1
        $summary['manuallyApproved'] | Should -Be 1
        @((Read-TestJsonFile -Path $script:ApprovedPath)['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1', 'F2')
        Test-Path -LiteralPath (Join-Path $script:WorkRoot 'audit-feedback.json') | Should -BeFalse
        $log = Read-FindingDecisionDocument -Path $script:DecisionsPath
        $log['decisions'].Count | Should -Be 3
        @($log['decisions'] | Where-Object { $_.Contains('feedback') }).Count | Should -Be 0
    }
}

Describe 'Approved set' {
    BeforeEach {
        Initialize-TestWorkspace
        $script:Artifact = New-TestConsolidatedArtifact -Finding @(
            New-TestFinding -Override @{ title = 'Terminate the retry loop' }
            New-TestFinding -Override @{ title = 'Bound the parser' }
        )
    }

    # An interrupted audit would otherwise produce a set that looks complete while quietly leaving
    # out everything the user never saw.
    It 'refuses to build a set while any finding is undecided' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null

        { New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions } | Should -Throw "*'F2'*"
    }

    It 'keeps only what the audit approved' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'rejected' -NowUtc $script:FixedNowUtc | Out-Null

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions
        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'

        $findings.Count | Should -Be 1
        $findings[0]['id'] | Should -BeExactly 'F1'
    }

    It 'refuses malformed prose before it enters the approved set' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'rejected' -NowUtc $script:FixedNowUtc | Out-Null
        $finding = (Get-JsonArrayProperty -Node $script:Artifact -Name 'findings')[0]
        Set-JsonProperty -Node $finding -Name 'description' -Value "before$([char] 0x1B)after"

        { New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions } |
            Should -Throw -ExpectedMessage "*'F1'*'description'*U+001B*"
    }

    # An auto-approval is the audit approving an obvious finding on the user's behalf, so it reaches
    # the approved set exactly as a user approval does.
    It 'keeps a finding the audit auto-approved' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'rejected' -NowUtc $script:FixedNowUtc | Out-Null

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions
        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'

        $findings.Count | Should -Be 1
        $findings[0]['id'] | Should -BeExactly 'F1'
    }

    # The split reports how the written set's approval divides, so it counts only the findings that
    # set holds and names each half by the verdict that carried it there.
    It 'splits the written set into its manual and auto approvals' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions
        $split = Get-ApprovedFindingApprovalSplit -Document $document -Decision $decisions

        $split['autoApproved'] | Should -Be 1
        $split['manuallyApproved'] | Should -Be 1
    }

    # A finding the freshness check removes leaves the written document, so the split no longer counts
    # it: an approval recorded for a finding the document does not hold is absent from both halves.
    It 'omits an approval the written set does not hold' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null

        # A document that holds only F1 stands for the survived set after F2 has left the diff.
        $document = [ordered]@{ findings = @([ordered]@{ id = 'F1' }) }
        $split = Get-ApprovedFindingApprovalSplit -Document $document -Decision $decisions

        $split['autoApproved'] | Should -Be 1
        $split['manuallyApproved'] | Should -Be 0
    }

    It 'leaves out a finding an earlier approval no longer stands for' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'dropped' -NowUtc $script:FixedNowUtc | Out-Null

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions
        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'

        @($findings | ForEach-Object { $_['id'] }) | Should -Be @('F2')
    }

    It 'names no commit and no target where no capture was supplied' {
        $decisions = New-FindingDecisionDocument
        foreach ($findingId in @('F1', 'F2')) {
            Add-FindingDecisionRecord -Document $decisions -FindingId $findingId -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        }

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions

        $document['target'] | Should -BeNullOrEmpty
        $document['reviewedSourceCommit'] | Should -BeNullOrEmpty
        $document['reviewedMergeBaseCommit'] | Should -BeNullOrEmpty
    }

    # Binding the commits is what lets publication state which revision the review was performed
    # against.
    It 'binds the target and both commits from the capture' {
        $decisions = New-FindingDecisionDocument
        foreach ($findingId in @('F1', 'F2')) {
            Add-FindingDecisionRecord -Document $decisions -FindingId $findingId -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        }

        $capture = New-CaptureDocument -Thread @() -SourceCommit 'abc1230000000000000000000000000000000000' -MergeBaseCommit 'def4560000000000000000000000000000000000'
        Write-TestJsonFile -Path $script:ContextPath -Value $capture | Out-Null
        $context = Read-PullRequestContextDocument -Path $script:ContextPath

        $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $decisions -Context $context

        $document['reviewedSourceCommit'] | Should -BeExactly 'abc1230000000000000000000000000000000000'
        $document['reviewedMergeBaseCommit'] | Should -BeExactly 'def4560000000000000000000000000000000000'
        $document['target']['pullRequestId'] | Should -Be 7
        $document['target']['platform'] | Should -BeExactly 'gitHub'
    }

    Context 'follow-up rebinding' {
        BeforeEach {
            $script:Artifact = New-TestConsolidatedArtifact -Finding @(
                New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } }
            )

            $script:Decisions = New-FindingDecisionDocument
            Add-FindingDecisionRecord -Document $script:Decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        }

        It 'accepts a follow-up the refreshed capture still holds' {
            $capture = New-CaptureDocument -Thread @(New-CaptureThread -Id 'RT_kw1' -OpeningCommentId 'PRRC_1')
            Write-TestJsonFile -Path $script:ContextPath -Value $capture | Out-Null

            $document = New-ApprovedFindingDocument `
                -Artifact $script:Artifact `
                -Decision $script:Decisions `
                -Context (Read-PullRequestContextDocument -Path $script:ContextPath)

            (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 1
        }

        # This is the last moment the run can still act on a thread that disappeared while the
        # review was being produced, so the refusal names the finding that can no longer be posted.
        It 'refuses a follow-up whose thread the refreshed capture no longer holds' {
            $capture = New-CaptureDocument -Thread @(New-CaptureThread -Id 'RT_other' -OpeningCommentId 'PRRC_9')
            Write-TestJsonFile -Path $script:ContextPath -Value $capture | Out-Null

            {
                New-ApprovedFindingDocument `
                    -Artifact $script:Artifact `
                    -Decision $script:Decisions `
                    -Context (Read-PullRequestContextDocument -Path $script:ContextPath)
            } | Should -Throw "*'F1'*'RT_kw1'*"
        }

        It 'refuses a follow-up the refreshed capture no longer accepts a reply into' {
            $capture = New-CaptureDocument -Thread @(New-CaptureThread -Id 'RT_kw1' -OpeningCommentId 'PRRC_1' -Kind 'system')
            Write-TestJsonFile -Path $script:ContextPath -Value $capture | Out-Null

            {
                New-ApprovedFindingDocument `
                    -Artifact $script:Artifact `
                    -Decision $script:Decisions `
                    -Context (Read-PullRequestContextDocument -Path $script:ContextPath)
            } | Should -Throw "*'F1'*"
        }

        # Phase 7 has no refreshed capture yet, and a follow-up target is not rechecked until there
        # is one.
        It 'accepts a follow-up where no capture was supplied' {
            $document = New-ApprovedFindingDocument -Artifact $script:Artifact -Decision $script:Decisions

            (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 1
        }
    }
}

Describe 'Approval executable' {
    BeforeEach {
        Initialize-TestWorkspace
        New-TestConsolidatedArtifact -Finding @(
            New-TestFinding -Override @{ title = 'Terminate the retry loop' }
            New-TestFinding -Override @{ title = 'Bound the parser' }
        ) | Out-Null

        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'rejected' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions
    }

    It 'writes the approved set and reports what the audit decided' {
        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['approved'] | Should -Be 1
        $emitted['manuallyApproved'] | Should -Be 1
        $emitted['autoApproved'] | Should -Be 0
        $emitted['rejected'] | Should -Be 1
        $emitted['dropped'] | Should -Be 0
        @($emitted['findingId']) | Should -Be @('F1')

        # A run given no checkout checks no anchor, so it reports no finding as unanchored.
        (Get-JsonArrayProperty -Node $emitted -Name 'unanchoredFinding').Count | Should -Be 0
        $emitted['unanchored'] | Should -Be 0

        $document = Read-TestJsonFile -Path $script:ApprovedPath
        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
        $findings.Count | Should -Be 1
        $findings[0]['title'] | Should -BeExactly 'Terminate the retry loop'
    }

    # The diagnostic summary reports the approval split, so the user learns how much the audit
    # decided without them (SKILL.md, "Phase 7: audit").
    It 'reports how many findings were auto-approved and how many were approved in person' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['approved'] | Should -Be 2
        $emitted['autoApproved'] | Should -Be 1
        $emitted['manuallyApproved'] | Should -Be 1
        $emitted['rejected'] | Should -Be 0
        $emitted['dropped'] | Should -Be 0
    }

    It 'writes an empty approved set when the consolidated artifact has no findings and no decision log' {
        New-TestConsolidatedArtifact -Finding @() | Out-Null
        Remove-Item -LiteralPath $script:DecisionsPath

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['approved'] | Should -Be 0
        $emitted['rejected'] | Should -Be 0
        $emitted['dropped'] | Should -Be 0
        (Get-JsonArrayProperty -Node $emitted -Name 'findingId').Count | Should -Be 0
        (Get-JsonArrayProperty -Node $emitted -Name 'unanchoredFinding').Count | Should -Be 0

        $document = Read-TestJsonFile -Path $script:ApprovedPath
        (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 0
    }

    It 'rebinds the review to the commits of the refreshed capture' {
        $repository = New-TestRepository -Path (Join-Path $script:WorkRoot 'clone')
        Write-TestJsonFile -Path $script:ContextPath -Value (New-CaptureDocument `
                -Thread @() `
                -SourceCommit $repository['sourceCommit'] `
                -MergeBaseCommit $repository['mergeBaseCommit']) | Out-Null

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
            '-ContextPath', $script:ContextPath
            '-RepositoryPath', $repository['path']
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['reviewedSourceCommit'] | Should -BeExactly $repository['sourceCommit']

        $document = Read-TestJsonFile -Path $script:ApprovedPath
        (Get-JsonProperty -Node $document -Name 'reviewedMergeBaseCommit') | Should -BeExactly $repository['mergeBaseCommit']
        (Get-JsonProperty -Node (Get-JsonProperty -Node $document -Name 'target') -Name 'pullRequestId') | Should -Be 7
    }

    It 'writes nothing while any finding is undecided' {
        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'F2'*"
        Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse
    }

    It 'writes nothing when findings have no decision log' {
        Remove-Item -LiteralPath $script:DecisionsPath

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'F1' or 'F2'*"
        Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse
    }

    # The freshness check reads the reviewed diff and the capture states which commits to read it
    # between, so neither input answers the question alone.
    It 'refuses a refreshed capture supplied without a checkout' {
        Write-TestJsonFile -Path $script:ContextPath -Value (New-CaptureDocument -Thread @()) | Out-Null

        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
            '-ContextPath', $script:ContextPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*-RepositoryPath*'
        Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse
    }

    It 'refuses a checkout supplied without a refreshed capture' {
        $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
            '-RepositoryPath', $script:WorkRoot
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*-ContextPath*'
        Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse
    }

    Context 'anchor freshness' {
        BeforeEach {
            $script:Repository = New-TestRepository -Path (Join-Path $script:WorkRoot 'clone')

            # The first finding is anchored to lines the reviewed change does not cover in a file it
            # does change; the second to a file the reviewed change leaves alone.
            New-TestConsolidatedArtifact -Finding @(
                New-TestFinding -Override @{ title = 'Terminate the retry loop'; anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42 } }
                New-TestFinding -Override @{ title = 'Bound the parser'; anchor = [ordered]@{ kind = 'file'; path = 'src/parse.rs' } }
            ) | Out-Null

            $decisions = New-FindingDecisionDocument
            foreach ($findingId in @('F1', 'F2')) {
                Add-FindingDecisionRecord -Document $decisions -FindingId $findingId -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
            }
            Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions

            Write-TestJsonFile -Path $script:ContextPath -Value (New-CaptureDocument `
                    -Thread @() `
                    -SourceCommit $script:Repository['sourceCommit'] `
                    -MergeBaseCommit $script:Repository['mergeBaseCommit']) | Out-Null
        }

        # Publication refuses such an anchor, and refusing it there refuses the whole publication
        # after the user has approved every finding.
        It 'reports an approved finding whose file has left the reviewed diff as unanchored' {
            $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
                '-Path', $script:ArtifactPath
                '-DecisionsPath', $script:DecisionsPath
                '-OutputPath', $script:ApprovedPath
                '-ContextPath', $script:ContextPath
                '-RepositoryPath', $script:Repository['path']
            )

            $result['exitCode'] | Should -Be 0
            $result['standardError'] | Should -BeNullOrEmpty

            $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
            $emitted['approved'] | Should -Be 1
            @($emitted['findingId']) | Should -Be @('F1')

            $unanchored = Get-JsonArrayProperty -Node $emitted -Name 'unanchoredFinding'
            $unanchored.Count | Should -Be 1
            $emitted['unanchored'] | Should -Be 1
            $unanchored[0]['findingId'] | Should -BeExactly 'F2'
            $unanchored[0]['path'] | Should -BeExactly 'src/parse.rs'
            $unanchored[0]['reason'] | Should -BeLike "*does not change 'src/parse.rs'*"

            # An audit verdict and an unpublishable anchor are separate events, so a finding lost to
            # the diff is not counted among the findings somebody decided to drop.
            $emitted['dropped'] | Should -Be 0

            $document = Read-TestJsonFile -Path $script:ApprovedPath
            $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
            @($findings | ForEach-Object { $_['id'] }) | Should -Be @('F1')
        }

        # The split is taken over the written set, so a finding approved in person but then removed as
        # unanchored is not counted among the manually approved: manually-approved and auto-approved
        # add up to the published approved count rather than to what the audit decided.
        It 'splits only the approvals the written set kept' {
            $decisions = New-FindingDecisionDocument
            Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'auto-approved' -NowUtc $script:FixedNowUtc | Out-Null
            Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
            Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions

            $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
                '-Path', $script:ArtifactPath
                '-DecisionsPath', $script:DecisionsPath
                '-OutputPath', $script:ApprovedPath
                '-ContextPath', $script:ContextPath
                '-RepositoryPath', $script:Repository['path']
            )

            $result['exitCode'] | Should -Be 0
            $result['standardError'] | Should -BeNullOrEmpty

            $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
            @($emitted['findingId']) | Should -Be @('F1')
            $emitted['unanchored'] | Should -Be 1

            # F2 was approved in person but has left the diff, so it counts in neither half.
            $emitted['approved'] | Should -Be 1
            $emitted['autoApproved'] | Should -Be 1
            $emitted['manuallyApproved'] | Should -Be 0
            ([int] $emitted['manuallyApproved'] + [int] $emitted['autoApproved']) | Should -Be ([int] $emitted['approved'])
        }

        # Publication reports such a finding against the file it names, so the observation still
        # reaches the author.
        It 'keeps an approved finding whose lines no hunk covers' {
            $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
                '-Path', $script:ArtifactPath
                '-DecisionsPath', $script:DecisionsPath
                '-OutputPath', $script:ApprovedPath
                '-ContextPath', $script:ContextPath
                '-RepositoryPath', $script:Repository['path']
            )

            $result['exitCode'] | Should -Be 0

            $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
            @($emitted['findingId']) | Should -Be @('F1')
            @((Get-JsonArrayProperty -Node $emitted -Name 'unanchoredFinding') | ForEach-Object { $_['findingId'] }) | Should -Be @('F2')

            $document = Read-TestJsonFile -Path $script:ApprovedPath
            $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
            $anchor = Get-JsonProperty -Node $findings[0] -Name 'anchor'
            (Get-JsonProperty -Node $anchor -Name 'path') | Should -BeExactly 'src/retry.rs'
            (Get-JsonProperty -Node $anchor -Name 'startLine') | Should -Be 42
        }

        It 'refuses a checkout that does not hold the reviewed commits' {
            $result = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
                '-Path', $script:ArtifactPath
                '-DecisionsPath', $script:DecisionsPath
                '-OutputPath', $script:ApprovedPath
                '-ContextPath', $script:ContextPath
                '-RepositoryPath', $script:WorkRoot
            )

            $result['exitCode'] | Should -Not -Be 0
            (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*no readable Git repository*'
            Test-Path -LiteralPath $script:ApprovedPath | Should -BeFalse
        }
    }
}
