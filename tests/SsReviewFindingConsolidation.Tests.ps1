BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in dependency
    # order, the same order the executables use.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')

    # The host running this suite stands in for the executables it launches. Naming the running
    # executable rather than the command 'pwsh' keeps the child on the same build as the parent and
    # removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # Builds a finding as an author writes it, carrying no identifier and no provenance.
    function New-TestFinding {
        param(
            [System.Collections.IDictionary] $Override = @{},

            [string[]] $Remove = @()
        )

        $finding = [ordered]@{
            title             = 'Terminate the retry loop'
            description       = 'The loop condition is never updated.'
            reasoning         = 'The counter is incremented in a branch that cannot be reached.'
            consequence       = 'A transient failure hangs the caller indefinitely.'
            recommendedAction = 'Increment the counter outside the branch.'
            anchor            = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42 }
        }

        foreach ($name in $Remove) { $finding.Remove($name) }
        foreach ($key in $Override.Keys) { $finding[[string] $key] = $Override[$key] }

        $finding
    }

    # Writes one reviewer's artifact, holding the findings it is given in the order given.
    function New-TestReviewerArtifact {
        param(
            [Parameter(Mandatory)]
            [string] $Directory,

            [Parameter(Mandatory)]
            [string] $ReviewerId,

            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Finding,

            [string] $FocusArea = 'correctness',

            [string] $Origin = 'reviewer'
        )

        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId $ReviewerId -FocusArea $FocusArea
        foreach ($entry in $Finding) {
            Add-FindingToArtifact -Document $document -Finding $entry -Origin $Origin | Out-Null
        }

        $path = Join-Path $Directory "$ReviewerId.json"
        Write-FindingArtifact -Path $path -Document $document
        $path
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

    # Gives each test its own working directory, so one test's artifacts cannot be read by another.
    function Initialize-TestWorkspace {
        $script:WorkRoot = Join-Path $TestDrive ([guid]::NewGuid())
        $script:ReviewerRoot = Join-Path $script:WorkRoot 'reviewers'
        New-Item -ItemType Directory -Path $script:ReviewerRoot -Force | Out-Null

        $script:OutputPath = Join-Path $script:WorkRoot 'consolidated.json'
        $script:IndexPath = Join-Path $script:WorkRoot 'index.json'
        $script:PlanPath = Join-Path $script:WorkRoot 'merge-plan.json'
        $script:MergedPath = Join-Path $script:WorkRoot 'merged.json'
    }

    function New-TestMergePlan {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Group
        )

        Write-TestJsonFile -Path $script:PlanPath -Value ([ordered]@{ schemaVersion = 1; groups = $Group })
    }

    # Writes the finding an adjudicator produces for a merge group, covering both the retry loop
    # position and the backoff position the seeded findings name.
    function New-TestMergedFinding {
        param(
            [System.Collections.IDictionary] $Override = @{}
        )

        $finding = New-TestFinding -Override @{
            title             = 'The retry loop never terminates'
            anchor            = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; side = 'right' }
            impactedLocations = @(
                [ordered]@{ path = 'src/retry.rs'; startLine = 42; side = 'right' }
                [ordered]@{ path = 'src/backoff.rs'; startLine = 8; side = 'right' }
            )
        }
        foreach ($key in $Override.Keys) { $finding[[string] $key] = $Override[$key] }

        Write-TestJsonFile -Path $script:MergedPath -Value $finding
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

Describe 'Finding index' {
    BeforeEach { Initialize-TestWorkspace }

    It 'validates the generated envelope before returning even an empty index' {
        Mock Assert-WorkflowData { throw 'Index schema validation failed.' } -ParameterFilter { $Kind -ceq 'findingIndex' }
        { New-FindingIndexDocument -ReviewerArtifact @() } | Should -Throw '*Index schema validation failed*'
    }

    It 'lists every finding of every reviewer without its full body' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -FocusArea 'correctness' -Finding @(
            New-TestFinding -Override @{
                title             = 'Terminate the retry loop'
                impactedLocations = @(
                    [ordered]@{ path = 'src/retry.rs'; startLine = 42 }
                    [ordered]@{ path = 'src/backoff.rs'; startLine = 10; endLine = 20 }
                )
            }
        ) | Out-Null
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g2' -FocusArea 'security' -Finding @(
            New-TestFinding -Override @{ title = 'Bound the parser'; anchor = [ordered]@{ kind = 'file'; path = 'src/parse.rs' } }
        ) | Out-Null

        $artifacts = Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot
        $document = New-FindingIndexDocument -ReviewerArtifact $artifacts
        $records = Get-JsonArrayProperty -Node $document -Name 'findings'

        $records.Count | Should -Be 2

        @($records[0].Keys) | Should -Be @('id', 'reviewerId', 'focusArea', 'title', 'description', 'recommendedAction', 'anchorPath', 'impactedPaths')
        $records[0]['id'] | Should -BeExactly 'g1-1'
        $records[0]['reviewerId'] | Should -BeExactly 'g1'
        $records[0]['focusArea'] | Should -BeExactly 'correctness'
        $records[0]['title'] | Should -BeExactly 'Terminate the retry loop'
        $records[0]['description'] | Should -BeExactly 'The loop condition is never updated.'
        $records[0]['recommendedAction'] | Should -BeExactly 'Increment the counter outside the branch.'
        $records[0].Contains('reasoning') | Should -BeFalse
        $records[0].Contains('consequence') | Should -BeFalse
        $records[0]['anchorPath'] | Should -BeExactly 'src/retry.rs'
        @($records[0]['impactedPaths']) | Should -Be @('src/retry.rs', 'src/backoff.rs')

        $records[1]['id'] | Should -BeExactly 'g2-1'
        $records[1]['anchorPath'] | Should -BeExactly 'src/parse.rs'
        @($records[1]['impactedPaths']).Count | Should -Be 0
    }

    # The index is read whole by a process that pays for every token of it, so a field that says
    # nothing is not carried.
    It 'names a follow-up thread only where the finding answers one' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @(
            New-TestFinding
            New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } }
        ) | Out-Null

        $records = Get-JsonArrayProperty -Node (New-FindingIndexDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot)) -Name 'findings'

        $records[0].Contains('followUpThreadId') | Should -BeFalse
        $records[1]['followUpThreadId'] | Should -BeExactly 'RT_kw1'
    }

    It 'refuses two reviewers claiming the same identifier' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @(New-TestFinding) | Out-Null
        $duplicate = Read-TestJsonFile -Path (Join-Path $script:ReviewerRoot 'g1.json')
        Write-TestJsonFile -Path (Join-Path $script:ReviewerRoot 'g1-again.json') -Value $duplicate | Out-Null

        { New-FindingIndexDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) } |
            Should -Throw "*'g1-1'*"
    }

    # A run that wrote its consolidated result back into the reviewers' directory would count every
    # finding twice while looking like it had worked.
    It 'refuses a consolidated artifact among the reviewers' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @(New-TestFinding) | Out-Null
        Write-FindingArtifact -Path (Join-Path $script:ReviewerRoot 'consolidated.json') -Document (New-FindingArtifactDocument -Kind 'consolidated')

        { Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot } | Should -Throw "*'consolidated' artifact*"
    }

    It 'refuses a directory no reviewer wrote into' {
        { Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot } | Should -Throw '*no reviewer artifacts*'
    }

    It 'writes the index and reports what it holds' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @((New-TestFinding), (New-TestFinding)) | Out-Null
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g2' -Finding @(New-TestFinding) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Get-FindingIndex.ps1' -ArgumentList @(
            '-ReviewerArtifactDirectory', $script:ReviewerRoot
            '-OutputPath', $script:IndexPath
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $emitted['artifactCount'] | Should -Be 2
        $emitted['findingCount'] | Should -Be 3

        $index = Read-WorkflowData -Kind findingIndex -Path $script:IndexPath
        $records = Get-JsonArrayProperty -Node $index -Name 'findings'
        @($records | ForEach-Object { $_['id'] }) | Should -Be @('g1-1', 'g1-2', 'g2-1')
    }
}

Describe 'Consolidation' {
    BeforeEach { Initialize-TestWorkspace }

    # This inversion is the phase's most important property: a merge the adjudicator missed becomes
    # a cosmetic duplicate rather than silent data loss.
    It 'carries over every finding the plan does not name' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @((New-TestFinding), (New-TestFinding)) | Out-Null
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g2' -Finding @(New-TestFinding) | Out-Null

        $document = New-ConsolidatedFindingDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot)
        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'

        @($findings | ForEach-Object { $_['id'] }) | Should -Be @('F1', 'F2', 'F3')
        @($findings | ForEach-Object { @($_['sourceFindingIds'])[0] }) | Should -Be @('g1-1', 'g1-2', 'g2-1')
    }

    It 'refuses malformed prose carried over from a reviewer' {
        $path = New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @(New-TestFinding)
        $reviewer = Read-TestJsonFile -Path $path
        $finding = (Get-JsonArrayProperty -Node $reviewer -Name 'findings')[0]
        Set-JsonProperty -Node $finding -Name 'description' -Value "before$([char] 0x1B)after"
        Write-TestJsonFile -Path $path -Value $reviewer | Out-Null

        { New-ConsolidatedFindingDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) } |
            Should -Throw -ExpectedMessage "*finding 'g1-1'*'description'*U+001B*"
    }

    It 'carries a finding over exactly as its reviewer recorded it' {
        $original = New-TestFinding -Override @{
            title             = 'Bound the parser'
            description       = 'Input length is unchecked.'
            references        = @([ordered]@{ text = 'docs/design.md'; url = 'https://example.invalid/design' })
            impactedLocations = @([ordered]@{ path = 'src/parse.rs'; startLine = 10; endLine = 20; side = 'right' })
            followUp          = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' }
            anchor            = [ordered]@{ kind = 'line'; path = 'src/parse.rs'; startLine = 10; endLine = 20; side = 'right' }
        }
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Origin 'synthesis' -Finding @($original) | Out-Null

        $document = New-ConsolidatedFindingDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot)
        $carried = (Get-JsonArrayProperty -Node $document -Name 'findings')[0]

        $carried['id'] | Should -BeExactly 'F1'
        @($carried['sourceFindingIds']) | Should -Be @('g1-1')
        $carried['origin'] | Should -BeExactly 'synthesis'
        foreach ($name in @('title', 'description', 'reasoning', 'consequence', 'recommendedAction')) {
            $carried[$name] | Should -BeExactly $original[$name] -Because $name
        }
        $carried['anchor']['startLine'] | Should -Be 10
        $carried['anchor']['endLine'] | Should -Be 20
        @($carried['references'])[0]['url'] | Should -BeExactly 'https://example.invalid/design'
        @($carried['impactedLocations'])[0]['path'] | Should -BeExactly 'src/parse.rs'
        $carried['followUp']['threadId'] | Should -BeExactly 'RT_kw1'
    }

    It 'takes an omitted merge plan to mean nothing merged but rejects a missing named plan' {
        New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @((New-TestFinding), (New-TestFinding)) | Out-Null

        { Read-FindingMergePlan -Path (Join-Path $script:WorkRoot 'no-such-plan.json') } | Should -Throw '*no merge plan*'
        $groups = Read-FindingMergePlan -Path ''
        $groups.Count | Should -Be 0

        $summary = Get-FindingConsolidationSummary -Document (New-ConsolidatedFindingDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) -MergeGroup $groups)
        $summary['findingCount'] | Should -Be 2
        $summary['mergedCount'] | Should -Be 0
        $summary['carriedOverCount'] | Should -Be 2
    }

    It 'takes a plan listing no group to mean nothing merged' {
        Write-TestJsonFile -Path $script:PlanPath -Value ([ordered]@{ schemaVersion = 1; groups = @() }) | Out-Null

        (Read-FindingMergePlan -Path $script:PlanPath).Count | Should -Be 0
    }

    Context 'merging' {
        BeforeEach {
            New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g1' -Finding @(
                New-TestFinding -Override @{
                    title             = 'Terminate the retry loop'
                    impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 42; side = 'right' })
                }
                New-TestFinding -Override @{ title = 'Unrelated' ; anchor = [ordered]@{ kind = 'file'; path = 'src/other.rs' } }
            ) | Out-Null
            New-TestReviewerArtifact -Directory $script:ReviewerRoot -ReviewerId 'g2' -Finding @(
                New-TestFinding -Override @{
                    title             = 'The retry loop never ends'
                    anchor            = [ordered]@{ kind = 'line'; path = 'src/backoff.rs'; startLine = 8; side = 'right' }
                    impactedLocations = @([ordered]@{ path = 'src/backoff.rs'; startLine = 8; side = 'right' })
                }
            ) | Out-Null
        }

        It 'replaces the merged findings and records where they came from' {
            New-TestMergedFinding | Out-Null
            $groups = Read-FindingMergePlan -Path (New-TestMergePlan -Group @(
                    [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
                ))

            $document = New-ConsolidatedFindingDocument -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) -MergeGroup $groups
            $findings = Get-JsonArrayProperty -Node $document -Name 'findings'

            $findings.Count | Should -Be 2
            $findings[0]['title'] | Should -BeExactly 'The retry loop never terminates'
            @($findings[0]['sourceFindingIds']) | Should -Be @('g1-1', 'g2-1')
            $findings[1]['title'] | Should -BeExactly 'Unrelated'
        }

        It 'refuses a finding claimed by more than one group' {
            New-TestMergedFinding | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
                [ordered]@{ sourceFindingIds = @('g1-1', 'g1-2'); findingPath = $script:MergedPath }
            )

            { New-ConsolidatedFindingDocument `
                    -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) `
                    -MergeGroup (Read-FindingMergePlan -Path $plan) } |
                Should -Throw "*'g1-1' in more than one group*"
        }

        It 'refuses a finding no reviewer artifact holds' {
            New-TestMergedFinding | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g9-4'); findingPath = $script:MergedPath }
            )

            { New-ConsolidatedFindingDocument `
                    -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) `
                    -MergeGroup (Read-FindingMergePlan -Path $plan) } |
                Should -Throw "*'g9-4'*"
        }

        It 'refuses a group naming fewer than two findings' {
            New-TestMergedFinding | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1'); findingPath = $script:MergedPath }
            )

            { Read-FindingMergePlan -Path $plan } | Should -Throw
        }

        # A comment is posted into one thread, so merging findings that answer two would leave one
        # of the two authors without the reply the review owes them.
        It 'refuses sources that answer different discussion threads' {
            $reviewerRoot = Join-Path $script:WorkRoot 'threaded-reviewers'
            New-Item -ItemType Directory -Path $reviewerRoot -Force | Out-Null
            New-TestReviewerArtifact -Directory $reviewerRoot -ReviewerId 'g1' -Finding @(
                New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_first'; commentId = 'PRRC_1' } }
            ) | Out-Null
            New-TestReviewerArtifact -Directory $reviewerRoot -ReviewerId 'g2' -Finding @(
                New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_second'; commentId = 'PRRC_2' } }
            ) | Out-Null

            New-TestMergedFinding -Override @{ impactedLocations = @() } | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
            )

            { New-ConsolidatedFindingDocument `
                    -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $reviewerRoot) `
                    -MergeGroup (Read-FindingMergePlan -Path $plan) } |
                Should -Throw '*different discussion threads*'
        }

        # The reply destination is provenance the adjudicator is not asked to restate.
        It 'gives the merged finding the thread its sources agree on' {
            $reviewerRoot = Join-Path $script:WorkRoot 'agreeing-reviewers'
            New-Item -ItemType Directory -Path $reviewerRoot -Force | Out-Null
            New-TestReviewerArtifact -Directory $reviewerRoot -ReviewerId 'g1' -Finding @(
                New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_shared'; commentId = 'PRRC_1' } }
                New-TestFinding -Override @{ followUp = [ordered]@{ threadId = 'RT_shared'; commentId = 'PRRC_1' } }
            ) | Out-Null

            New-TestMergedFinding -Override @{ impactedLocations = @() } | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g1-2'); findingPath = $script:MergedPath }
            )

            $document = New-ConsolidatedFindingDocument `
                -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $reviewerRoot) `
                -MergeGroup (Read-FindingMergePlan -Path $plan)

            (Get-JsonArrayProperty -Node $document -Name 'findings')[0]['followUp']['threadId'] |
                Should -BeExactly 'RT_shared'
        }

        # A merge removes a repetition, not a claim.
        It 'refuses a merged finding that drops an impacted location' {
            New-TestMergedFinding -Override @{
                impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 42; side = 'right' })
            } | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
            )

            { New-ConsolidatedFindingDocument `
                    -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) `
                    -MergeGroup (Read-FindingMergePlan -Path $plan) } |
                Should -Throw '*src/backoff.rs:8*'
        }

        It 'accepts a merged finding that names a wider range than its sources did' {
            New-TestMergedFinding -Override @{
                impactedLocations = @(
                    [ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 50; side = 'right' }
                    [ordered]@{ path = 'src/backoff.rs' }
                )
            } | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
            )

            $document = New-ConsolidatedFindingDocument `
                -ReviewerArtifact (Read-FindingReviewerArtifactSet -Path $script:ReviewerRoot) `
                -MergeGroup (Read-FindingMergePlan -Path $plan)

            (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 2
        }

        It 'consolidates and reports what it did' {
            New-TestMergedFinding | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
            )

            $result = Invoke-SkillExecutable -Name 'New-ConsolidatedFindings.ps1' -ArgumentList @(
                '-ReviewerArtifactDirectory', $script:ReviewerRoot
                '-MergePlanPath', $plan
                '-OutputPath', $script:OutputPath
            )

            $result['exitCode'] | Should -Be 0
            $result['standardError'] | Should -BeNullOrEmpty

            $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
            $emitted['findingCount'] | Should -Be 2
            $emitted['mergedCount'] | Should -Be 1
            $emitted['carriedOverCount'] | Should -Be 1

            $document = Read-FindingArtifact -Path $script:OutputPath
            (Get-JsonProperty -Node $document -Name 'kind') | Should -BeExactly 'consolidated'
        }

        It 'consolidates without a merge plan' {
            $result = Invoke-SkillExecutable -Name 'New-ConsolidatedFindings.ps1' -ArgumentList @(
                '-ReviewerArtifactDirectory', $script:ReviewerRoot
                '-OutputPath', $script:OutputPath
            )

            $result['exitCode'] | Should -Be 0
            $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
            $emitted['findingCount'] | Should -Be 3
            $emitted['mergedCount'] | Should -Be 0
        }

        # Writing the artifact only after every group is accepted keeps a rejected plan from
        # leaving a half-consolidated set behind for the next phase to read.
        It 'writes nothing when it refuses the plan' {
            New-TestMergedFinding | Out-Null
            $plan = New-TestMergePlan -Group @(
                [ordered]@{ sourceFindingIds = @('g1-1', 'g2-1'); findingPath = $script:MergedPath }
                [ordered]@{ sourceFindingIds = @('g1-1', 'g1-2'); findingPath = $script:MergedPath }
            )

            $result = Invoke-SkillExecutable -Name 'New-ConsolidatedFindings.ps1' -ArgumentList @(
                '-ReviewerArtifactDirectory', $script:ReviewerRoot
                '-MergePlanPath', $plan
                '-OutputPath', $script:OutputPath
            )

            $result['exitCode'] | Should -Not -Be 0
            $result['standardOutput'] | Should -BeNullOrEmpty
            (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'g1-1'*"
            Test-Path -LiteralPath $script:OutputPath | Should -BeFalse
        }
    }
}
