#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    # Content that exercises quoting and allowed formatting through serialization: a review of code
    # quotes code, and prose that loses a backslash, backtick, or line feed misreports what the
    # reviewed file says.
    $script:ExoticContent = @(
        'A "quoted" phrase, a `backtick`, an escape \n that is not one, and a path C:\src\lib.rs.'
        'A second line, an emoji 🚀, and a trailing `code span`.'
    ) -join "`n"

    # Builds a finding as an author writes it, carrying no identifier and no provenance.
    #
    # Overrides are applied onto the minimal valid finding, so a test states only the field it is
    # about and every other field remains valid; a test asserting a rejection then cannot pass
    # because of some unrelated omission.
    function New-TestFinding {
        param(
            [System.Collections.IDictionary] $Override = @{},

            [string[]] $Remove = @()
        )

        $finding = [ordered]@{
            title             = 'The retry loop never terminates'
            description       = 'The loop condition is never updated.'
            reasoning         = 'The counter is incremented in a branch that cannot be reached.'
            consequence       = 'A transient failure hangs the caller indefinitely.'
            recommendedAction = 'Increment the counter outside the branch.'
            anchor            = [ordered]@{
                kind      = 'line'
                path      = 'src/retry.rs'
                startLine = 42
            }
        }

        foreach ($name in $Remove) { $finding.Remove($name) }
        foreach ($key in $Override.Keys) { $finding[[string] $key] = $Override[$key] }

        $finding
    }

    # --- Capture fixtures -----------------------------------------------------------------------

    # Builds one comment in the shape the capture writes.
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
            [string] $Kind,

            [Parameter(Mandatory)]
            [string] $OpeningCommentId,

            [AllowNull()]
            $ThreadContext = $null
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
            context          = $ThreadContext
            anchorKey        = $Id
            properties       = [ordered]@{}
            openingCommentId = $OpeningCommentId
            decision         = $null
            comments         = @(New-CaptureComment -Id $OpeningCommentId)
        }
    }

    function New-CaptureThreadContext {
        param(
            [string] $FilePath = '/src/retry.rs'
        )

        [ordered]@{
            filePath          = $FilePath
            side              = 'right'
            startLine         = 40
            endLine           = 42
            originalStartLine = 40
            originalEndLine   = 42
        }
    }

    # Builds a captured discussion holding the given threads.
    #
    # The fixture uses the published captured discussion schema.
    function New-CaptureDocument {
        param(
            [Parameter(Mandatory)]
            [ValidateSet('gitHub', 'azureDevOps')]
            [string] $Platform,

            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Thread
        )

        [ordered]@{
            schemaVersion         = 3
            fetchedUtc            = '2024-05-06T08:00:00.0000000Z'
            platform              = $Platform
            repositoryUrl         = 'https://example.invalid/widgets'
            pullRequestUrl        = 'https://example.invalid/widgets/pull/7'
            target                = [ordered]@{
                platform      = $Platform
                collectionUrl = $null
                organization  = 'contoso'
                project        = 'widgets'
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
                reviewedSourceCommit    = $null
                reviewedMergeBaseCommit = $null
            }
            reviewers             = @()
            revisions             = @()
            threads               = $Thread
        }
    }

    # --- Filesystem helpers ---------------------------------------------------------------------

    function Write-TestJsonFile {
        param(
            [Parameter(Mandatory)]
            [string] $Path,

            [Parameter(Mandatory)]
            $Value
        )

        [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
        $json = ConvertTo-Json -InputObject $Value -Depth 100
        [IO.File]::WriteAllText($Path, $json, [Text.UTF8Encoding]::new($false))
        $Path
    }

    function Read-TestJsonFile {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8)) -AsHashtable -DateKind String
    }

    # --- Child process helpers ------------------------------------------------------------------

    # Renders a child process's error stream as a single line.
    #
    # The concise error view wraps a message to the console width and prefixes each continuation
    # line with a pipe, so a phrase the caller has to recognize arrives split across lines whenever
    # the message is long - and these messages quote a full path, so they always are.
    # Exercises the native gate while preserving its JSON result and exit status separately.
    function Invoke-FindingValidation {
        param([string] $Path, [string[]] $ReviewerId)

        $native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $start = [Diagnostics.ProcessStartInfo]::new($native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        foreach ($argument in @('workflow', 'validate-findings', '--path', $Path, '--fail-on-invalid')) {
            $start.ArgumentList.Add($argument)
        }
        foreach ($id in $ReviewerId) {
            $start.ArgumentList.Add('--reviewer-id')
            $start.ArgumentList.Add($id)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                standardOutput = $output.GetAwaiter().GetResult()
                standardError = $errors.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }

    # Exercises native finding commands with distinct output and error streams.
    function Invoke-NativeFinding {
        param([string[]] $Arguments, [string] $WorkingDirectory)

        $native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $start = [Diagnostics.ProcessStartInfo]::new($native)
        if ($WorkingDirectory) { $start.WorkingDirectory = $WorkingDirectory }
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                standardOutput = $output.GetAwaiter().GetResult()
                standardError = $errors.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
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

Describe 'Follow-up targets' {
    BeforeAll {
        $script:ReviewThread = New-CaptureThread -Id 'RT_kw1' -Kind 'text' -OpeningCommentId 'PRRC_1' -ThreadContext (New-CaptureThreadContext)
        $script:IssueCommentThread = New-CaptureThread -Id 'IC_kw2' -Kind 'text' -OpeningCommentId 'IC_2'
        $script:ReviewSummaryThread = New-CaptureThread -Id 'PRR_kw3' -Kind 'reviewSummary' -OpeningCommentId 'PRR_kw3'
        $script:RefUpdateThread = New-CaptureThread -Id 'RU_kw4' -Kind 'refUpdate' -OpeningCommentId 'RU_4'

        $script:AdoFileThread = New-CaptureThread -Id '11' -Kind 'text' -OpeningCommentId '1' -ThreadContext (New-CaptureThreadContext)
        $script:AdoOverallThread = New-CaptureThread -Id '12' -Kind 'text' -OpeningCommentId '1'
        $script:AdoSystemThread = New-CaptureThread -Id '13' -Kind 'system' -OpeningCommentId '1'

        $script:AdoCapture = New-CaptureDocument -Platform 'azureDevOps' -Thread @(
            $script:AdoFileThread
            $script:AdoOverallThread
            $script:AdoSystemThread
        )

    }

    BeforeEach {
        $script:FollowUpRoot = Join-Path $TestDrive ([guid]::NewGuid())
        $created = Invoke-NativeFinding -Arguments @(
            'initialize-finding', '--output-directory', $script:FollowUpRoot,
            '--reviewer-id', 'reviewer', '--focus-area', 'correctness'
        )
        $created.exitCode | Should -Be 0 -Because $created.standardError
        $script:FollowUpArtifact = (ConvertFrom-Json -InputObject $created.standardOutput -AsHashtable).artifactPath
    }

    It 'checks <Case> follow-up against the native captured discussion' -ForEach @(
        @{ Case = 'no'; Platform = 'gitHub'; Thread = $null; Comment = $null; Accepted = $true }
        @{ Case = 'GitHub review'; Platform = 'gitHub'; Thread = 'RT_kw1'; Comment = 'PRRC_1'; Accepted = $true }
        @{ Case = 'GitHub issue comment'; Platform = 'gitHub'; Thread = 'IC_kw2'; Comment = 'IC_2'; Accepted = $false; ExpectedError = 'review thread' }
        @{ Case = 'GitHub review summary'; Platform = 'gitHub'; Thread = 'PRR_kw3'; Comment = 'PRR_kw3'; Accepted = $false; ExpectedError = 'discussion thread' }
        @{ Case = 'GitHub service activity'; Platform = 'gitHub'; Thread = 'RU_kw4'; Comment = 'RU_4'; Accepted = $false; ExpectedError = 'discussion thread' }
        @{ Case = 'Azure file thread'; Platform = 'azureDevOps'; Thread = '11'; Comment = '1'; Accepted = $true }
        @{ Case = 'Azure overall thread'; Platform = 'azureDevOps'; Thread = '12'; Comment = '1'; Accepted = $true }
        @{ Case = 'Azure service activity'; Platform = 'azureDevOps'; Thread = '13'; Comment = '1'; Accepted = $false; ExpectedError = 'discussion thread' }
        @{ Case = 'missing thread'; Platform = 'gitHub'; Thread = 'RT_missing'; Comment = 'PRRC_1'; Accepted = $false; ExpectedError = 'RT_missing' }
        @{ Case = 'non-opening comment'; Platform = 'gitHub'; Thread = 'RT_kw1'; Comment = 'PRRC_9'; Accepted = $false; ExpectedError = 'opening comment' }
        @{ Case = 'case-sensitive thread'; Platform = 'gitHub'; Thread = 'rt_kw1'; Comment = 'PRRC_1'; Accepted = $false; ExpectedError = 'rt_kw1' }
        @{ Case = 'unsupported platform'; Platform = 'gitLab'; Thread = 'RT_kw1'; Comment = 'PRRC_1'; Accepted = $false; ExpectedError = 'Unsupported capture platform' }
    ) {
        $capture = if ($Platform -eq 'azureDevOps') {
            $script:AdoCapture
        } else {
            New-CaptureDocument -Platform 'gitHub' -Thread @(
                $script:ReviewThread, $script:IssueCommentThread, $script:ReviewSummaryThread, $script:RefUpdateThread
            )
        }
        if ($Platform -eq 'gitLab') { $capture.platform = $Platform }
        $finding = if ($null -eq $Thread) {
            New-TestFinding
        } else {
            New-TestFinding -Override @{ followUp = @{ threadId = $Thread; commentId = $Comment } }
        }
        $contextPath = Write-TestJsonFile -Path (Join-Path $script:FollowUpRoot 'context.json') -Value $capture
        $findingPath = Write-TestJsonFile -Path (Join-Path $script:FollowUpRoot 'finding.json') -Value $finding
        $before = [IO.File]::ReadAllText($script:FollowUpArtifact)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:FollowUpArtifact, '--finding-path', $findingPath,
            '--origin', 'reviewer', '--context-path', $contextPath
        )
        if ($Accepted) {
            $result.exitCode | Should -Be 0 -Because $result.standardError
        } else {
            $result.exitCode | Should -Not -Be 0
            $result.standardOutput | Should -BeNullOrEmpty
            $result.standardError | Should -Match ([regex]::Escape($ExpectedError))
            [IO.File]::ReadAllText($script:FollowUpArtifact) | Should -BeExactly $before
        }
    }

    It 'preserves every optional field in an authored finding' {
        $finding = New-TestFinding -Override @{
            references = @(
                @{ text = 'docs/design.md, "Retry policy"' }
                @{ text = 'The retry RFC'; url = 'https://example.invalid/rfc' }
            )
            impactedLocations = @(
                @{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' }
                @{ path = 'src/legacy.rs' }
            )
            followUp = @{ threadId = 'RT_kw1'; commentId = 'PRRC_1' }
        }
        $capture = New-CaptureDocument -Platform 'gitHub' -Thread @($script:ReviewThread)
        $contextPath = Write-TestJsonFile -Path (Join-Path $script:FollowUpRoot 'context.json') -Value $capture
        $findingPath = Write-TestJsonFile -Path (Join-Path $script:FollowUpRoot 'finding.json') -Value $finding
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:FollowUpArtifact, '--finding-path', $findingPath,
            '--origin', 'reviewer', '--context-path', $contextPath
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError
        $stored = (Read-TestJsonFile -Path $script:FollowUpArtifact).findings[0]
        $stored.references.Count | Should -Be 2
        $stored.references[0].text | Should -BeExactly $finding.references[0].text
        $stored.references[1].url | Should -BeExactly $finding.references[1].url
        $stored.impactedLocations.Count | Should -Be 2
        $stored.impactedLocations[0].endLine | Should -Be 44
        $stored.impactedLocations[1].path | Should -BeExactly 'src/legacy.rs'
        $stored.followUp.threadId | Should -BeExactly 'RT_kw1'
        $stored.followUp.commentId | Should -BeExactly 'PRRC_1'
    }
}

Describe 'Artifact set validation' {
    BeforeAll {
        # Builds artifacts through native commands so validation receives actual producer output.
        function New-TestArtifactSet {
            param(
                [Parameter(Mandatory)]
                [string] $Root,

                [Parameter(Mandatory)]
                [AllowEmptyCollection()]
                [object[]] $Reviewer
            )

            $directory = Join-Path $Root 'findings'

            # The loop variable differs from the parameter name because PowerShell keeps the
            # parameter's type constraint on the variable, so reusing the name would wrap each
            # description in an array.
            foreach ($description in $Reviewer) {
                $id = [string] $description['reviewerId']
                $created = Invoke-NativeFinding -Arguments @(
                    'initialize-finding', '--output-directory', $directory,
                    '--reviewer-id', $id, '--focus-area', 'correctness'
                )
                $created.exitCode | Should -Be 0 -Because $created.standardError
                $artifact = (ConvertFrom-Json -InputObject $created.standardOutput -AsHashtable).artifactPath
                $findingPath = Write-TestJsonFile -Path (Join-Path $Root "$id-input.json") -Value (New-TestFinding)

                for ($index = 0; $index -lt [int] $description['findingCount']; $index++) {
                    $added = Invoke-NativeFinding -Arguments @(
                        'add-finding', '--artifact-path', $artifact,
                        '--finding-path', $findingPath, '--origin', 'reviewer'
                    )
                    $added.exitCode | Should -Be 0 -Because $added.standardError
                }
                if ($description['isComplete']) {
                    $queue = Write-TestJsonFile -Path (Join-Path $Root "$id-queue.json") -Value @{
                        schemaVersion = 3; kind = 'fileGroups'; items = @(); inProgress = $null
                        completedGroupIds = @(); assessments = @()
                    }
                    $completed = Invoke-NativeFinding -Arguments @(
                        'complete-finding', '--artifact-path', $artifact, '--queue-path', $queue
                    )
                    $completed.exitCode | Should -Be 0 -Because $completed.standardError
                }
            }

            $directory
        }
    }

    It 'reports a complete set as valid' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'valid') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 2; isComplete = $true }
            @{ reviewerId = 'g1-security'; findingCount = 0; isComplete = $true }
        )

        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness', 'g1-security')
        $validation.exitCode | Should -Be 0 -Because $validation.standardError
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeTrue
        $result['findingCount'] | Should -Be 2
        $result['artifacts'].Count | Should -Be 2
        $result['errors'].Count | Should -Be 0
        $result['missingReviewers'].Count | Should -Be 0
        $result['incompleteReviewers'].Count | Should -Be 0
        $result['unexpectedReviewers'].Count | Should -Be 0
    }

    # An empty artifact cannot say whether the reviewer found nothing or died partway, which is the
    # failure the completion state exists to expose.
    It 'reports a reviewer that never reported completion' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'incomplete') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $false }
        )

        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        @($result['incompleteReviewers']) | Should -Be @('g1-correctness')
    }

    It 'reports a reviewer that wrote no artifact at all' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'missing') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness', 'g1-security')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        @($result['missingReviewers']) | Should -Be @('g1-security')
    }

    It 'reports an artifact from a reviewer that was never dispatched' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'unexpected') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $true }
            @{ reviewerId = 'g9-invented'; findingCount = 0; isComplete = $true }
        )

        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        @($result['unexpectedReviewers']) | Should -Be @('g9-invented')
    }

    It 'reports every defect in one pass rather than stopping at the first' {
        $root = Join-Path $TestDrive 'defects'
        $directory = New-TestArtifactSet -Root $root -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        $damaged = Read-TestJsonFile -Path (Join-Path $directory 'g1-correctness.json')
        $finding = $damaged.findings[0]
        $finding.Remove('title')
        $finding.severity = 'high'
        $damaged.status = 'partial'
        Write-TestJsonFile -Path (Join-Path $directory 'g1-correctness.json') -Value $damaged | Out-Null

        $validation = Invoke-FindingValidation -Path $directory
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        $result['errors'].Count | Should -BeGreaterThan 1
        ($result['errors'] -join ' ') | Should -BeLike "*'severity'*"
        ($result['errors'] -join ' ') | Should -BeLike "*'partial'*"
    }

    It 'validates a single artifact named directly' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'single') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 3; isComplete = $true }
        )

        $validation = Invoke-FindingValidation -Path (Join-Path $directory 'g1-correctness.json')
        $validation.exitCode | Should -Be 0 -Because $validation.standardError
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeTrue
        $result['findingCount'] | Should -Be 3
    }

    It 'ignores writer staging files but reports unrelated Windows entries' -Skip:(-not $IsWindows) {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'concurrent-staging') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $true }
        )
        $native = '.ss-ai-transaction-aB1234567890abcdefgh1234.tmp'
        $powershell = '.reviewer.json.0123456789abcdef0123456789abcdef.tmp'
        $foreign = '.ss-ai-transaction-aB1234567890abcdefgh123.tmp'
        $foreignDirectory = '.ss-ai-transaction-zB1234567890abcdefgh1234.tmp'
        Set-Content -LiteralPath (Join-Path $directory $native) -Value 'partial'
        Set-Content -LiteralPath (Join-Path $directory $powershell) -Value 'partial'
        Set-Content -LiteralPath (Join-Path $directory 'reviewer.JSON.LOCK') -Value 'lock'
        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness')
        $validation.exitCode | Should -Be 0 -Because $validation.standardError
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String
        $result['isValid'] | Should -BeTrue

        Set-Content -LiteralPath (Join-Path $directory $foreign) -Value 'foreign'
        New-Item -ItemType Directory -Path (Join-Path $directory $foreignDirectory) | Out-Null
        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String
        $result['isValid'] | Should -BeFalse
        ($result['errors'] -join ' ') | Should -BeLike "*$foreign*"
        ($result['errors'] -join ' ') | Should -BeLike "*$foreignDirectory*"
        ($result['errors'] -join ' ') | Should -Not -BeLike "*'$native'*"
        ($result['errors'] -join ' ') | Should -Not -BeLike "*'$powershell'*"
    }

    It 'reports malformed staging names on Windows: <Name>' -Skip:(-not $IsWindows) -ForEach @(
        @{ Name = '.tmp123456' }
        @{ Name = '.ss-ai-transaction-aB1234567890abcdefgh123.tmp' }
        @{ Name = '.ss-ai-transaction-aB1234567890abcdefgh12345.tmp' }
        @{ Name = '.ss-ai-transaction-aB1234567890abcdefghi-34.tmp' }
        @{ Name = '.ss-ai-transaction-aB1234567890abcdefgh1234.TMP' }
        @{ Name = '.ss-ai-transaction-aB1234567890abcdefgh1234.tmp.bak' }
        @{ Name = '.reviewer.json.0123456789abcdef0123456789abcdeg.tmp' }
    ) {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive ([guid]::NewGuid())) -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $true }
        )
        Set-Content -LiteralPath (Join-Path $directory $Name) -Value 'foreign'
        $validation = Invoke-FindingValidation -Path $directory
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String
        $result.isValid | Should -BeFalse
        ($result.errors -join ' ') | Should -BeLike "*$Name*"
    }

    # A session-state directory is not sealed, so the gate states what it found rather than passing
    # a set it only partly read or reporting the reviewer as simply missing.
    It 'refuses a directory holding anything but reviewer artifacts' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'foreign') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        New-Item -ItemType Directory -Path (Join-Path $directory 'staging') -Force | Out-Null
        Set-Content -Path (Join-Path $directory 'notes.md') -Value 'still drafting'

        $validation = Invoke-FindingValidation -Path $directory -ReviewerId @('g1-correctness')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        ($result['errors'] -join ' ') | Should -BeLike "*'staging'*"
        ($result['errors'] -join ' ') | Should -BeLike "*'notes.md'*"
        $result['artifacts'].Count | Should -Be 1
        $result['findingCount'] | Should -Be 1
    }

    It 'refuses a JSON file that is not an artifact, naming it' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'foreign-json') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $true }
        )

        Write-TestJsonFile -Path (Join-Path $directory 'scratch.json') -Value ([ordered]@{ note = 'still drafting' }) | Out-Null
        Set-Content -Path (Join-Path $directory 'truncated.json') -Value '{ "kind": '

        $validation = Invoke-FindingValidation -Path $directory
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        ($result['errors'] -join ' ') | Should -BeLike '*scratch.json*'
        ($result['errors'] -join ' ') | Should -BeLike '*truncated.json*'
        $result['artifacts'].Count | Should -Be 1
    }

    It 'reports a path that holds nothing' {
        $validation = Invoke-FindingValidation -Path (Join-Path $TestDrive 'absent\findings')
        $validation.exitCode | Should -Not -Be 0
        $result = ConvertFrom-Json -InputObject $validation.standardOutput -AsHashtable -DateKind String

        $result['isValid'] | Should -BeFalse
        $result['errors'].Count | Should -Be 1
    }
}

Describe 'Content fidelity' {
    BeforeEach {
        $script:FidelityDirectory = Join-Path $TestDrive ([guid]::NewGuid())
        $result = Invoke-NativeFinding -Arguments @(
            'initialize-finding', '--output-directory', $script:FidelityDirectory,
            '--reviewer-id', 'g1', '--focus-area', 'correctness'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError
        $script:FidelityPath = (ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable).artifactPath
    }

    It 'reproduces prose exactly through a write and read cycle' {
        $title = 'A "quoted" title with a `backtick` and a path C:\src\lib.rs.'
        $finding = New-TestFinding -Override @{
            title             = $title
            description       = $script:ExoticContent
            reasoning         = $script:ExoticContent
            consequence       = $script:ExoticContent
            recommendedAction = $script:ExoticContent
            references        = @([ordered]@{ text = $script:ExoticContent; url = 'https://example.invalid/a?b=c&d=e' })
        }

        $inputPath = Write-TestJsonFile -Path (Join-Path $script:FidelityDirectory 'input.json') -Value $finding
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:FidelityPath, '--finding-path', $inputPath, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError

        $stored = (Read-TestJsonFile -Path $script:FidelityPath).findings[0]
        $stored.title | Should -BeExactly $title
        foreach ($name in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
            $stored[$name] | Should -BeExactly $script:ExoticContent -Because $name
        }
        $stored.references[0].text | Should -BeExactly $script:ExoticContent
    }

    # A body that is nothing but a date is the case ConvertFrom-Json corrupts, and a reviewer
    # quoting a timestamp out of a log writes exactly that.
    It 'reproduces content that looks like a timestamp' {
        $inputPath = Write-TestJsonFile -Path (Join-Path $script:FidelityDirectory 'input.json') `
            -Value (New-TestFinding -Override @{ title = '2024-05-06T07:08:09Z' })
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:FidelityPath, '--finding-path', $inputPath, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError

        $stored = (Read-TestJsonFile -Path $script:FidelityPath).findings[0]
        $stored.title | Should -BeExactly '2024-05-06T07:08:09Z'
    }

    It 'writes UTF-8 without a byte order mark and with newline endings' {
        $bytes = [System.IO.File]::ReadAllBytes($script:FidelityPath)
        @($bytes[0], $bytes[1], $bytes[2]) | Should -Not -Be @(0xEF, 0xBB, 0xBF)
        ([System.IO.File]::ReadAllText($script:FidelityPath)) | Should -Not -BeLike "*`r`n*"
    }

    It 'replaces a finding artifact without leaving a partial-write file' {
        $result = Invoke-NativeFinding -Arguments @(
            'initialize-finding', '--output-directory', $script:FidelityDirectory, '--reviewer-id', 'g1',
            '--focus-area', 'security', '--force'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError
        (Read-TestJsonFile -Path $script:FidelityPath).focusArea | Should -BeExactly 'security'
        @(Get-ChildItem -LiteralPath $script:FidelityDirectory -Force -Filter '*.tmp').Count |
            Should -Be 0
    }
}

Describe 'Finding executables' {
    BeforeAll {
        # Creates the artifact every executable test starts from.
        function Initialize-TestArtifact {
            param(
                [string[]] $ArgumentList = @()
            )

            Invoke-NativeFinding -Arguments (@(
                    'initialize-finding', '--output-directory', $script:FindingsDirectory
                    '--reviewer-id', 'g1-correctness'
                    '--focus-area', 'correctness'
                    '--queue-path', $script:FileGroupQueuePath
                ) + $ArgumentList)
        }

        function New-TestFindingFile {
            param(
                [Parameter(Mandatory)]
                [string] $Name,

                [System.Collections.IDictionary] $Override = @{}
            )

            $path = Join-Path $script:WorkRoot $Name
            Write-TestJsonFile -Path $path -Value (New-TestFinding -Override $Override)
        }
    }

    BeforeEach {
        $script:WorkRoot = Join-Path $TestDrive ([guid]::NewGuid())
        New-Item -ItemType Directory -Path $script:WorkRoot -Force | Out-Null
        # Left uncreated, so that every test starting from a new artifact exercises the run's first
        # reviewer arriving before anything has made the directory.
        $script:FindingsDirectory = Join-Path $script:WorkRoot 'findings'
        $script:ArtifactPath = Join-Path $script:FindingsDirectory 'g1-correctness.json'
        $script:FileGroupQueuePath = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'files.json') -Value @{
            schemaVersion = 3
            kind = 'fileGroups'
            items = @()
            inProgress = $null
            completedGroupIds = @()
            assessments = @()
        }
    }

    # The path this reports is the one the reviewer is dispatched with and the one the gate later
    # enumerates, so an artifact one directory away from where it was asked for is a reviewer that
    # silently goes missing.
    It 'creates an artifact and reports where it wrote it' {
        $result = Initialize-TestArtifact

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.reviewerId | Should -BeExactly 'g1-correctness'
        $emitted.artifactPath | Should -BeExactly $script:ArtifactPath
        Test-Path -LiteralPath $script:ArtifactPath -PathType Leaf | Should -BeTrue
        (Split-Path -Parent $script:ArtifactPath) | Should -BeExactly $script:FindingsDirectory
        $stored = Read-TestJsonFile -Path $script:ArtifactPath
        $stored.kind | Should -BeExactly 'reviewer'
        $stored.reviewerId | Should -BeExactly 'g1-correctness'
        $stored.Contains('fileGroup') | Should -BeFalse
        $stored.focusArea | Should -BeExactly 'correctness'
        $stored.status | Should -BeExactly 'open'
        $stored.findings.Count | Should -Be 0
    }

    It 'rejects reviewer identifier <ReviewerId> that cannot name an artifact' -ForEach @(
        @{ ReviewerId = 'g1/correctness' }
        @{ ReviewerId = 'g1\correctness' }
        @{ ReviewerId = '..' }
        @{ ReviewerId = 'g1..correctness' }
        @{ ReviewerId = '' }
    ) {
        $result = Invoke-NativeFinding -Arguments @(
            'initialize-finding', '--output-directory', $script:FindingsDirectory,
            '--reviewer-id', $ReviewerId, '--focus-area', 'correctness'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        @(Get-ChildItem -LiteralPath $script:WorkRoot -Filter '*.json' -Recurse |
            Where-Object Name -ne 'files.json').Count | Should -Be 0
    }

    It 'preserves consolidated identity and provenance through updates' {
        [IO.Directory]::CreateDirectory($script:FindingsDirectory) | Out-Null
        Write-TestJsonFile -Path $script:ArtifactPath -Value @{
            schemaVersion = 1; kind = 'consolidated'; findings = @(); removed = @()
        } | Out-Null
        foreach ($title in @('First', 'Second', 'Third')) {
            $path = New-TestFindingFile -Name "$title.json" -Override @{ title = $title }
            $result = Invoke-NativeFinding -Arguments @(
                'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path,
                '--origin', 'synthesis', '--source-id', 'g1-1', '--source-id', 'g2-3'
            )
            $result.exitCode | Should -Be 0 -Because $result.standardError
        }
        $artifact = Read-TestJsonFile -Path $script:ArtifactPath
        @($artifact.findings | ForEach-Object { $_.id }) | Should -Be @('F1', 'F2', 'F3')
        @($artifact.findings | ForEach-Object { $_.title }) | Should -Be @('First', 'Second', 'Third')
        $replacement = New-TestFindingFile -Name 'replacement.json' -Override @{
            title = 'Rewritten'; anchor = @{ kind = 'file'; path = 'src/other.rs' }
        }
        $updated = Invoke-NativeFinding -Arguments @(
            'update-finding', '--artifact-path', $script:ArtifactPath, '--finding-id', 'F1', '--finding-path', $replacement
        )
        $updated.exitCode | Should -Be 0 -Because $updated.standardError
        $artifact = Read-TestJsonFile -Path $script:ArtifactPath
        $artifact.findings[0].id | Should -BeExactly 'F1'
        $artifact.findings[0].origin | Should -BeExactly 'synthesis'
        $artifact.findings[0].sourceFindingIds | Should -Be @('g1-1', 'g2-3')
        $artifact.findings[0].schemaVersion | Should -Be 1
        $artifact.findings[0].title | Should -BeExactly 'Rewritten'
        $artifact.findings[0].anchor.kind | Should -BeExactly 'file'
        $artifact.findings[1].title | Should -BeExactly 'Second'
    }

    It 'rejects malformed artifact <Case>' -ForEach @(
        @{ Case = 'schema version'; Schema = 2; Kind = 'reviewer' }
        @{ Case = 'kind'; Schema = 1; Kind = 'summary' }
    ) {
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'invalid-artifact.json') -Value @{
            schemaVersion = $Schema; kind = $Kind; reviewerId = 'g1'; focusArea = 'correctness'
            status = 'complete'; findings = @(); removed = @()
        }
        $result = Invoke-FindingValidation -Path $path
        $result.exitCode | Should -Not -Be 0
        $report = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $report.isValid | Should -BeFalse
        $report.errors.Count | Should -BeGreaterThan 0
    }

    It 'rejects reviewer metadata on a consolidated artifact: <Field>' -ForEach @(
        @{ Field = 'reviewerId'; Value = 'g1' }
        @{ Field = 'focusArea'; Value = 'correctness' }
    ) {
        $document = @{ schemaVersion = 1; kind = 'consolidated'; findings = @(); removed = @() }
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'consolidated.json') -Value $document
        $baseline = Invoke-FindingValidation -Path $path
        $baseline.exitCode | Should -Be 0 -Because $baseline.standardError
        $document[$Field] = $Value
        Write-TestJsonFile -Path $path -Value $document | Out-Null
        $result = Invoke-FindingValidation -Path $path
        $result.exitCode | Should -Not -Be 0
        (ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String).isValid | Should -BeFalse
    }

    It 'rejects a reviewer artifact without a focus area' {
        $document = @{
            schemaVersion = 1; kind = 'reviewer'; reviewerId = 'g1'; focusArea = 'correctness'
            status = 'complete'; findings = @(); removed = @()
        }
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'reviewer.json') -Value $document
        $baseline = Invoke-FindingValidation -Path $path
        $baseline.exitCode | Should -Be 0 -Because $baseline.standardError
        $document.Remove('focusArea')
        Write-TestJsonFile -Path $path -Value $document | Out-Null
        $result = Invoke-FindingValidation -Path $path
        $result.exitCode | Should -Not -Be 0
        (ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String).isValid | Should -BeFalse
    }

    It 'refuses to complete a consolidated artifact' {
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'consolidated.json') -Value @{
            schemaVersion = 1; kind = 'consolidated'; findings = @(); removed = @()
        }
        $before = [IO.File]::ReadAllText($path)
        $result = Invoke-NativeFinding -Arguments @(
            'complete-finding', '--artifact-path', $path, '--queue-path', $script:FileGroupQueuePath
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        [IO.File]::ReadAllText($path) | Should -BeExactly $before
    }

    It 'rejects <Case> without changing the artifact' -ForEach @(
        @{ Case = 'unknown update identifier'; Operation = 'update'; Id = 'g1-correctness-9'; InvalidField = $false; Reason = 'Unused' }
        @{ Case = 'unknown removal identifier'; Operation = 'remove'; Id = 'g1-correctness-9'; InvalidField = $false; Reason = 'Out of scope.' }
        @{ Case = 'blank removal reason'; Operation = 'remove'; Id = 'g1-correctness-1'; InvalidField = $false; Reason = '  ' }
        @{ Case = 'tool-owned rewrite identifier'; Operation = 'update'; Id = 'g1-correctness-1'; InvalidField = $true; Reason = 'Unused' }
    ) {
        Initialize-TestArtifact | Out-Null
        $original = New-TestFindingFile -Name 'original.json'
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $original, '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        if ($Operation -eq 'update') {
            $override = if ($InvalidField) { @{ id = 'g1-correctness-1' } } else { @{} }
            $replacement = New-TestFindingFile -Name 'replacement.json' -Override $override
            $arguments = @('update-finding', '--artifact-path', $script:ArtifactPath, '--finding-id', $Id, '--finding-path', $replacement)
        } else {
            $plan = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'remove.json') -Value @{
                schemaVersion = 1; removals = @(@{ findingId = $Id; reason = $Reason })
            }
            $arguments = @('remove-finding', '--artifact-path', $script:ArtifactPath, '--plan-path', $plan, '--now-utc', '2024-05-06T07:08:09Z')
        }
        $result = Invoke-NativeFinding -Arguments $arguments
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Not -BeNullOrEmpty
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    # A subagent that creates its artifact a second time would otherwise discard everything it had
    # already recorded, with nothing in the output to show that it had.
    It 'refuses to replace an artifact that already exists' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null

        $result = Initialize-TestArtifact

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*--force*'

        $stored = Read-TestJsonFile -Path $script:ArtifactPath
        $stored.findings.Count | Should -Be 1
    }

    It 'replaces an artifact when forced' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null

        $result = Initialize-TestArtifact -ArgumentList @('--force')

        $result['exitCode'] | Should -Be 0
        $stored = Read-TestJsonFile -Path $script:ArtifactPath
        $stored.findings.Count | Should -Be 0
    }

    It 'resumes without discarding findings and continues assigning distinct identifiers' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null
        $before = Read-TestJsonFile -Path $script:ArtifactPath
        $result = Initialize-TestArtifact -ArgumentList @('--resume')
        $result.exitCode | Should -Be 0
        (ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String).findingCount | Should -Be 1
        (ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String).resumedExisting | Should -BeTrue
        $after = Read-TestJsonFile -Path $script:ArtifactPath
        (ConvertTo-Json -InputObject $after.findings -Depth 100) |
            Should -BeExactly (ConvertTo-Json -InputObject $before.findings -Depth 100)
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'replayed-finding.json'), '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0
        (ConvertFrom-Json -InputObject $added.standardOutput -AsHashtable -DateKind String).findingId | Should -Be 'g1-correctness-2'
        (Read-TestJsonFile -Path $script:ArtifactPath).findings.Count | Should -Be 2
    }

    It 'creates missing initial state on resume and reopens a completed artifact without data loss' {
        (Initialize-TestArtifact -ArgumentList @('--resume')).exitCode | Should -Be 0
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath,
            '--finding-path', (New-TestFindingFile -Name 'resume.json'), '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        $completed = Invoke-NativeFinding -Arguments @(
            'complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath
        )
        $completed.exitCode | Should -Be 0 -Because $completed.standardError
        (Initialize-TestArtifact -ArgumentList @('--resume')).exitCode | Should -Be 0
        $resumed = Read-TestJsonFile -Path $script:ArtifactPath
        $resumed.status | Should -Be 'open'
        $resumed.findings.Count | Should -Be 1
    }

    It 'refuses mismatched or malformed resume artifacts without overwriting them' {
        Initialize-TestArtifact | Out-Null
        $document = Read-TestJsonFile -Path $script:ArtifactPath
        $document.focusArea = 'another-focus-area'
        Write-TestJsonFile -Path $script:ArtifactPath -Value $document | Out-Null
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        (Initialize-TestArtifact -ArgumentList @('--resume')).exitCode | Should -Not -Be 0
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
        [IO.File]::WriteAllText($script:ArtifactPath, '{broken')
        (Initialize-TestArtifact -ArgumentList @('--resume')).exitCode | Should -Not -Be 0
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly '{broken'
    }

    It 'refuses a missing artifact alongside <State> group work' -ForEach @(
        @{ State = 'claimed' }, @{ State = 'completed' }
    ) {
        $queue = Read-TestJsonFile -Path $script:FileGroupQueuePath
        if ($State -eq 'claimed') {
            $queue.inProgress = @{ groupId = 'group-001'; files = @(@{ path = 'review.ps1'; sizeBytes = 1; change = 'modified' }) }
        } else {
            $queue.completedGroupIds = @('group-001')
        }
        Write-TestJsonFile -Path $script:FileGroupQueuePath -Value $queue | Out-Null
        $result = Initialize-TestArtifact -ArgumentList @('--resume')
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'missing despite journaled work'
        Test-Path -LiteralPath $script:ArtifactPath | Should -BeFalse
    }

    It 'refuses a missing journal alongside recorded findings' {
        Initialize-TestArtifact | Out-Null
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath,
            '--finding-path', (New-TestFindingFile -Name 'journaled.json'), '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        Remove-Item -LiteralPath $script:FileGroupQueuePath
        $result = Initialize-TestArtifact -ArgumentList @('--resume')
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'missing despite recorded findings'
        (Read-TestJsonFile -Path $script:ArtifactPath).findings.Count | Should -Be 1
    }
    It 'appends a finding and reports its identifier' {
        Initialize-TestArtifact | Out-Null

        $first = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'first.json' -Override @{ title = 'First' }), '--origin', 'reviewer'
        )
        $second = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'second.json' -Override @{ title = 'Second' }), '--origin', 'reviewer'
        )

        $first['exitCode'] | Should -Be 0
        $second['exitCode'] | Should -Be 0

        $firstEmitted = ConvertFrom-Json -InputObject $first.standardOutput -AsHashtable -DateKind String
        $firstEmitted.findingId | Should -BeExactly 'g1-correctness-1'
        $firstEmitted.Contains('index') | Should -BeFalse

        $secondEmitted = ConvertFrom-Json -InputObject $second.standardOutput -AsHashtable -DateKind String
        $secondEmitted.findingId | Should -BeExactly 'g1-correctness-2'
        $secondEmitted.Contains('index') | Should -BeFalse
        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings
        $stored[0].sourceFindingIds.Count | Should -Be 0
        $stored[1].sourceFindingIds.Count | Should -Be 0
    }

    It 'stores canonical forward-slash paths in anchors and impacted locations' {
        Initialize-TestArtifact | Out-Null
        $path = New-TestFindingFile -Name 'paths.json' -Override @{
            anchor = @{ kind = 'line'; path = './src/retry.rs'; startLine = 42 }
            impactedLocations = @(@{ path = 'src//legacy/mod.rs' })
        }
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError
        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.anchor.path | Should -BeExactly 'src/retry.rs'
        $stored.impactedLocations[0].path | Should -BeExactly 'src/legacy/mod.rs'
        $plainPath = New-TestFindingFile -Name 'plain-path.json' -Override @{
            anchor = @{ kind = 'file'; path = 'src/retry/mod.rs' }
        }
        $plain = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $plainPath, '--origin', 'reviewer'
        )
        $plain.exitCode | Should -Be 0 -Because $plain.standardError
        (Read-TestJsonFile -Path $script:ArtifactPath).findings[1].anchor.path | Should -BeExactly 'src/retry/mod.rs'
    }

    It 'rejects a noncanonical path <Path> in <Field>' -ForEach @(
        @{ Field = 'anchor'; Path = 'src\retry.rs' }
        @{ Field = 'anchor'; Path = '.\src\retry.rs' }
        @{ Field = 'anchor'; Path = 'src/retry\mod.rs' }
        @{ Field = 'anchor'; Path = 'C:\src\retry.rs' }
        @{ Field = 'anchor'; Path = 'C:/src/retry.rs' }
        @{ Field = 'anchor'; Path = '\\server\share\retry.rs' }
        @{ Field = 'anchor'; Path = '/src/retry.rs' }
        @{ Field = 'anchor'; Path = 'src/../../secrets.txt' }
        @{ Field = 'anchor'; Path = '' }
        @{ Field = 'anchor'; Path = '   ' }
        @{ Field = 'anchor'; Path = './' }
        @{ Field = 'impactedLocations'; Path = 'src\legacy\mod.rs' }
    ) {
        Initialize-TestArtifact | Out-Null
        $override = if ($Field -eq 'anchor') {
            @{ anchor = @{ kind = 'file'; path = $Path } }
        } else {
            @{ impactedLocations = @(@{ path = $Path }) }
        }
        $findingPath = New-TestFindingFile -Name 'invalid-path.json' -Override $override
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $findingPath, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match ([regex]::Escape($findingPath))
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'refuses a finding and leaves the artifact untouched' {
        Initialize-TestArtifact | Out-Null
        $path = Join-Path $script:WorkRoot 'invalid.json'
        Write-TestJsonFile -Path $path -Value (New-TestFinding -Override @{ priority = 'high' }) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', $path, '--origin', 'reviewer'
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'priority'*"
        (Read-TestJsonFile -Path $script:ArtifactPath).findings.Count | Should -Be 0
    }

    It 'names the source and <Field> when authored content has <Case>' -ForEach @(
        @{ Field = 'title'; Case = 'a missing required field'; Missing = $true }
        @{ Field = 'description'; Case = 'a missing required field'; Missing = $true }
        @{ Field = 'reasoning'; Case = 'a missing required field'; Missing = $true }
        @{ Field = 'consequence'; Case = 'a missing required field'; Missing = $true }
        @{ Field = 'recommendedAction'; Case = 'a missing required field'; Missing = $true }
        @{ Field = 'severity'; Case = 'an unknown field'; Missing = $false }
        @{ Field = 'priority'; Case = 'an unknown field'; Missing = $false }
        @{ Field = 'category'; Case = 'an unknown field'; Missing = $false }
        @{ Field = 'id'; Case = 'a tool-owned field'; Missing = $false }
        @{ Field = 'origin'; Case = 'a tool-owned field'; Missing = $false }
        @{ Field = 'schemaVersion'; Case = 'a tool-owned field'; Missing = $false }
        @{ Field = 'sourceFindingIds'; Case = 'a tool-owned field'; Missing = $false }
    ) {
        Initialize-TestArtifact | Out-Null
        $finding = if ($Missing) {
            New-TestFinding -Remove @($Field)
        } else {
            New-TestFinding -Override @{ $Field = 'unexpected' }
        }
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'authored.json') -Value $finding
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match ([regex]::Escape($path))
        $result.standardError | Should -Match ([regex]::Escape($Field))
        if ($Case -eq 'a tool-owned field') {
            $result.standardError | Should -Match 'which the tooling assigns'
        } elseif ($Case -eq 'an unknown field') {
            $result.standardError | Should -Match 'unknown field'
        }
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'rejects <Case> for required prose without changing the artifact' -ForEach @(
        @{ Case = 'empty description'; Field = 'description'; Value = '' }
        @{ Case = 'blank description'; Field = 'description'; Value = '   ' }
        @{ Case = 'non-string consequence'; Field = 'consequence'; Value = 7 }
    ) {
        Initialize-TestArtifact | Out-Null
        $path = New-TestFindingFile -Name 'invalid-prose.json' -Override @{ $Field = $Value }
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match ([regex]::Escape($Field))
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'identifies malformed <Case> in authored input' -ForEach @(
        @{ Case = 'anchor start'; Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 0 } }; Expected = 'start line' }
        @{ Case = 'reference text'; Override = @{ references = @(@{ url = 'https://example.invalid' }) }; Expected = 'reference text' }
        @{ Case = 'location path'; Override = @{ impactedLocations = @(@{ startLine = 3 }) }; Expected = 'path' }
        @{ Case = 'follow-up comment'; Override = @{ followUp = @{ threadId = 'T1' } }; Expected = 'commentId' }
    ) {
        Initialize-TestArtifact | Out-Null
        $path = New-TestFindingFile -Name 'structured.json' -Override $Override
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match ([regex]::Escape($Expected))
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'rejects malformed authored structure: <Case>' -ForEach @(
        @{ Case = 'missing anchor'; Remove = @('anchor'); Override = @{} }
        @{ Case = 'unknown anchor kind'; Remove = @(); Override = @{ anchor = @{ kind = 'range'; path = 'src/a.rs' } } }
        @{ Case = 'file start line'; Remove = @(); Override = @{ anchor = @{ kind = 'file'; path = 'src/a.rs'; startLine = 3 } } }
        @{ Case = 'file end line'; Remove = @(); Override = @{ anchor = @{ kind = 'file'; path = 'src/a.rs'; endLine = 3 } } }
        @{ Case = 'file side'; Remove = @(); Override = @{ anchor = @{ kind = 'file'; path = 'src/a.rs'; side = 'right' } } }
        @{ Case = 'missing start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs' } } }
        @{ Case = 'zero start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 0 } } }
        @{ Case = 'negative start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = -1 } } }
        @{ Case = 'string start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = '42' } } }
        @{ Case = 'fractional start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 4.5 } } }
        @{ Case = 'reversed lines'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 42; endLine = 41 } } }
        @{ Case = 'unknown side'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 42; side = 'RIGHT' } } }
        @{ Case = 'unknown anchor field'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 42; commitId = 'abc' } } }
        @{ Case = 'end without start'; Remove = @(); Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; endLine = 42 } } }
        @{ Case = 'missing reference text'; Remove = @(); Override = @{ references = @(@{ url = 'https://example.invalid' }) } }
        @{ Case = 'unknown reference field'; Remove = @(); Override = @{ references = @(@{ text = 'x'; title = 'y' }) } }
        @{ Case = 'missing location path'; Remove = @(); Override = @{ impactedLocations = @(@{ startLine = 3 }) } }
        @{ Case = 'location end without start'; Remove = @(); Override = @{ impactedLocations = @(@{ path = 'src/a.rs'; endLine = 3 }) } }
        @{ Case = 'location side without line'; Remove = @(); Override = @{ impactedLocations = @(@{ path = 'src/a.rs'; side = 'left' }) } }
        @{ Case = 'missing follow-up thread'; Remove = @(); Override = @{ followUp = @{ commentId = 'C1' } } }
        @{ Case = 'missing follow-up comment'; Remove = @(); Override = @{ followUp = @{ threadId = 'T1' } } }
        @{ Case = 'unknown follow-up field'; Remove = @(); Override = @{ followUp = @{ threadId = 'T1'; commentId = 'C1'; resolve = $true } } }
    ) {
        Initialize-TestArtifact | Out-Null
        $path = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'invalid-structure.json') -Value (
            New-TestFinding -Remove $Remove -Override $Override
        )
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match ([regex]::Escape($path))
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'accepts authored structure: <Case>' -ForEach @(
        @{ Case = 'file anchor'; Override = @{ anchor = @{ kind = 'file'; path = 'src/a.rs' } } }
        @{ Case = 'left side'; Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 42; side = 'left' } } }
        @{ Case = 'right side'; Override = @{ anchor = @{ kind = 'line'; path = 'src/a.rs'; startLine = 42; side = 'right' } } }
        @{ Case = 'empty lists'; Override = @{ references = @(); impactedLocations = @() } }
    ) {
        Initialize-TestArtifact | Out-Null
        $path = New-TestFindingFile -Name 'valid-structure.json' -Override $Override
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $path, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError
    }

    It 'names a malformed finding file when appending' {
        Initialize-TestArtifact | Out-Null
        $path = Join-Path $script:WorkRoot 'malformed.json'
        Set-Content -LiteralPath $path -Value '{ "title": "broken", }' -NoNewline -Encoding utf8NoBOM

        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', $path, '--origin', 'reviewer'
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*$path*"
        (Read-TestJsonFile -Path $script:ArtifactPath).findings.Count | Should -Be 0
    }

    It 'checks a follow-up against the captured discussion' {
        Initialize-TestArtifact | Out-Null

        $capturePath = Join-Path $script:WorkRoot 'pr-context.json'
        Write-TestJsonFile -Path $capturePath -Value (New-CaptureDocument -Platform 'gitHub' -Thread @(
                New-CaptureThread -Id 'RT_kw1' -Kind 'text' -OpeningCommentId 'PRRC_1' -ThreadContext (New-CaptureThreadContext)
            )) | Out-Null

        $accepted = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'good.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } })
            '--context-path', $capturePath, '--origin', 'reviewer'
        )
        $refused = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'bad.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_typo'; commentId = 'PRRC_1' } })
            '--context-path', $capturePath, '--origin', 'reviewer'
        )

        $accepted['exitCode'] | Should -Be 0
        $refused['exitCode'] | Should -Not -Be 0
        (Get-SingleLineErrorText -Text $refused['standardError']) | Should -BeLike "*'RT_typo'*"
    }

    It 'rejects malformed captured discussion without coercing fields' {
        Initialize-TestArtifact | Out-Null
        $capture = New-CaptureDocument -Platform 'gitHub' -Thread @(
            New-CaptureThread -Id 'RT_kw1' -Kind 'text' -OpeningCommentId 'PRRC_1' -ThreadContext (New-CaptureThreadContext)
        )
        $capture['pullRequest']['description'] = 42
        $capturePath = Join-Path $script:WorkRoot 'invalid-capture.json'
        Write-TestJsonFile -Path $capturePath -Value $capture | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'follow-up.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } })
            '--context-path', $capturePath, '--origin', 'reviewer'
        )

        $result['exitCode'] | Should -Not -Be 0
        (Read-TestJsonFile -Path $script:ArtifactPath).findings.Count | Should -Be 0
    }

    It 'records a finding produced by synthesis with its provenance' {
        Initialize-TestArtifact | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'synthesis.json')
            '--origin', 'synthesis'
            '--source-id', 'g2-1', '--source-id', 'g3-4'
        )

        $result['exitCode'] | Should -Be 0
        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.origin | Should -BeExactly 'synthesis'
        $sourceFindingIds = $stored.sourceFindingIds
        $sourceFindingIds | Should -Be @('g2-1', 'g3-4')
    }

    It 'completes an artifact and reports what it holds' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null

        $result = Invoke-NativeFinding -Arguments @('complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath)

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.status | Should -BeExactly 'complete'
        $emitted.findingCount | Should -Be 1
        (Read-TestJsonFile -Path $script:ArtifactPath).status | Should -BeExactly 'complete'
    }

    It 'refuses a finding once the reviewer is complete without modifying the artifact' {
        Initialize-TestArtifact | Out-Null
        $completed = Invoke-NativeFinding -Arguments @(
            'complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath
        )
        $completed.exitCode | Should -Be 0 -Because $completed.standardError
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        $finding = New-TestFindingFile -Name 'after-completion.json'
        $result = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath, '--finding-path', $finding, '--origin', 'reviewer'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'complete'
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'rewrites one finding' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'update-finding', '--artifact-path', $script:ArtifactPath
            '--finding-id', 'g1-correctness-1'
            '--finding-path', (New-TestFindingFile -Name 'rewritten.json' -Override @{ title = 'Rewritten' })
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $updated = $emitted.updated
        $updated | Should -Be @('g1-correctness-1')

        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.title | Should -BeExactly 'Rewritten'
        $stored.id | Should -BeExactly 'g1-correctness-1'
    }

    It 'rewrites a batch named by a plan' {
        Initialize-TestArtifact | Out-Null
        foreach ($name in @('first', 'second')) {
            Invoke-NativeFinding -Arguments @(
                'add-finding', '--artifact-path', $script:ArtifactPath
                '--finding-path', (New-TestFindingFile -Name "$name.json" -Override @{ title = $name }), '--origin', 'reviewer'
            ) | Out-Null
        }

        New-TestFindingFile -Name 'first-rewritten.json' -Override @{ title = 'First rewritten' } | Out-Null
        New-TestFindingFile -Name 'second-rewritten.json' -Override @{ title = 'Second rewritten' } | Out-Null

        # The plan names its findings relatively, which is how a subagent that wrote both into one
        # directory refers to them.
        $planPath = Join-Path $script:WorkRoot 'plan.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; updates = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; findingPath = 'first-rewritten.json' }
                    [ordered]@{ findingId = 'g1-correctness-2'; findingPath = 'second-rewritten.json' }
                )
            }) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'update-finding', '--artifact-path', $script:ArtifactPath
            '--plan-path', $planPath
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $updated = $emitted.updated
        $updated | Should -Be @('g1-correctness-1', 'g1-correctness-2')

        $findings = (Read-TestJsonFile -Path $script:ArtifactPath).findings
        @($findings | ForEach-Object { $_.title }) |
            Should -Be @('First rewritten', 'Second rewritten')
    }

    It 'names a malformed finding file from an update plan' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json' -Override @{ title = 'Original' }), '--origin', 'reviewer'
        ) | Out-Null

        $badFindingPath = Join-Path $script:WorkRoot 'bad-rewrite.json'
        Set-Content -LiteralPath $badFindingPath -Value '{ "title": "broken", }' -NoNewline -Encoding utf8NoBOM

        $planPath = Join-Path $script:WorkRoot 'plan.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; updates = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; findingPath = 'bad-rewrite.json' }
                )
            }) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'update-finding', '--artifact-path', $script:ArtifactPath
            '--plan-path', $planPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*$badFindingPath*"
        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.title | Should -BeExactly 'Original'
    }

    It 'leaves the artifact untouched when a batch names a finding it does not hold' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json' -Override @{ title = 'Original' }), '--origin', 'reviewer'
        ) | Out-Null
        New-TestFindingFile -Name 'rewritten.json' -Override @{ title = 'Rewritten' } | Out-Null

        $planPath = Join-Path $script:WorkRoot 'plan.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; updates = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; findingPath = 'rewritten.json' }
                    [ordered]@{ findingId = 'g1-correctness-9'; findingPath = 'rewritten.json' }
                )
            }) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'update-finding', '--artifact-path', $script:ArtifactPath
            '--plan-path', $planPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.title | Should -BeExactly 'Original'
    }

    It 'rejects a duplicate finding in a <Operation> plan without changing the artifact' -ForEach @(
        @{ Operation = 'update'; Property = 'updates' }
        @{ Operation = 'remove'; Property = 'removals' }
    ) {
        Initialize-TestArtifact | Out-Null
        $findingPath = New-TestFindingFile -Name 'original.json'
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath,
            '--finding-path', $findingPath, '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        $entry = if ($Operation -eq 'update') {
            @{ findingId = 'g1-correctness-1'; findingPath = 'original.json' }
        } else {
            @{ findingId = 'g1-correctness-1'; reason = 'Out of scope.' }
        }
        $plan = @{ schemaVersion = 1; $Property = @($entry, $entry) }
        $planPath = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'duplicate.json') -Value $plan
        $before = [IO.File]::ReadAllText($script:ArtifactPath)

        $arguments = @(
            "$Operation-finding", '--artifact-path', $script:ArtifactPath, '--plan-path', $planPath
        )
        if ($Operation -eq 'remove') {
            $arguments += @('--now-utc', '2024-05-06T07:08:09Z')
        }
        $result = Invoke-NativeFinding -Arguments $arguments

        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        $result.standardError | Should -Match '(?i)duplicate|repeated'
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
    }

    It 'discards a finding with a reason and a moment from a read-only working directory' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'finding.json'), '--origin', 'reviewer'
        ) | Out-Null

        $planPath = Join-Path $script:WorkRoot 'single-removal.json'
        Write-TestJsonFile -Path $planPath -Value @{
            schemaVersion = 1
            removals = @(@{ findingId = 'g1-correctness-1'; reason = 'Duplicates a finding from another reviewer.' })
        } | Out-Null
        $directory = Join-Path $script:WorkRoot 'read-only-working-directory'
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        if (-not $IsWindows) {
            $originalMode = [IO.File]::GetUnixFileMode($directory)
            [IO.File]::SetUnixFileMode($directory, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserExecute)
        }
        try {
            $result = Invoke-NativeFinding -WorkingDirectory $directory -Arguments @(
                'remove-finding', '--artifact-path', $script:ArtifactPath
                '--plan-path', $planPath, '--now-utc', '2024-05-06T07:08:09Z'
            )
        } finally {
            if (-not $IsWindows) { [IO.File]::SetUnixFileMode($directory, $originalMode) }
        }

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $removedIds = $emitted.removed
        $removedIds | Should -Be @('g1-correctness-1')

        $stored = Read-TestJsonFile -Path $script:ArtifactPath
        $stored.findings.Count | Should -Be 0
        $removed = $stored.removed[0]
        $removed.finding.id | Should -BeExactly 'g1-correctness-1'
        $removed.reason | Should -BeExactly 'Duplicates a finding from another reviewer.'
        $removed.removedUtc | Should -BeExactly '2024-05-06T07:08:09.0000000Z'
    }

    It 'discards a batch named by a plan' {
        Initialize-TestArtifact | Out-Null
        foreach ($name in @('first', 'second')) {
            Invoke-NativeFinding -Arguments @(
                'add-finding', '--artifact-path', $script:ArtifactPath
                '--finding-path', (New-TestFindingFile -Name "$name.json" -Override @{ title = $name }), '--origin', 'reviewer'
            ) | Out-Null
        }

        $planPath = Join-Path $script:WorkRoot 'removals.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; removals = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; reason = 'Out of scope.' }
                    [ordered]@{ findingId = 'g1-correctness-2'; reason = 'Already reported.' }
                )
            }) | Out-Null

        $result = Invoke-NativeFinding -Arguments @(
            'remove-finding', '--artifact-path', $script:ArtifactPath
            '--plan-path', $planPath, '--now-utc', '2024-05-06T07:08:09Z'
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.removed | Should -Be @('g1-correctness-1', 'g1-correctness-2')
        $stored = Read-TestJsonFile -Path $script:ArtifactPath
        $stored.findings.Count | Should -Be 0
        $stored.removed.Count | Should -Be 2
    }

    It 'validates persisted removal history with <Case>' -ForEach @(
        @{ Case = 'identifier only'; Mutate = { param($entry) $entry.finding = @{ id = 'g1-correctness-1' } } }
        @{ Case = 'missing finding'; Mutate = { param($entry) $entry.finding = $null } }
        @{ Case = 'invalid prose'; Mutate = { param($entry) $entry.finding.title = '' } }
        @{ Case = 'invalid anchor'; Mutate = { param($entry) $entry.finding.anchor.kind = 'unknown' } }
    ) {
        Initialize-TestArtifact | Out-Null
        $added = Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath,
            '--finding-path', (New-TestFindingFile -Name 'history-finding.json'), '--origin', 'reviewer'
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        $plan = Write-TestJsonFile -Path (Join-Path $script:WorkRoot 'history-removal.json') -Value @{
            schemaVersion = 1; removals = @(@{ findingId = 'g1-correctness-1'; reason = 'Out of scope.' })
        }
        $removed = Invoke-NativeFinding -Arguments @(
            'remove-finding', '--artifact-path', $script:ArtifactPath,
            '--plan-path', $plan, '--now-utc', '2024-05-06T07:08:09Z'
        )
        $removed.exitCode | Should -Be 0 -Because $removed.standardError
        $completed = Invoke-NativeFinding -Arguments @(
            'complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath
        )
        $completed.exitCode | Should -Be 0 -Because $completed.standardError
        $before = Invoke-FindingValidation -Path $script:ArtifactPath
        $before.exitCode | Should -Be 0 -Because $before.standardError
        $artifact = Read-TestJsonFile -Path $script:ArtifactPath
        & $Mutate $artifact.removed[0]
        Write-TestJsonFile -Path $script:ArtifactPath -Value $artifact | Out-Null

        $after = Invoke-FindingValidation -Path $script:ArtifactPath
        $after.exitCode | Should -Not -Be 0
        (ConvertFrom-Json -InputObject $after.standardOutput -AsHashtable -DateKind String).isValid | Should -BeFalse
    }

    It 'preserves caller-owned removal inputs and artifact when the native plan fails' {
        Initialize-TestArtifact | Out-Null
        $planPath = Join-Path $script:WorkRoot 'invalid-removal.json'
        Write-TestJsonFile -Path $planPath -Value @{
            schemaVersion = 1
            removals = @(@{ findingId = 'g1-correctness-99'; reason = 'Not present.' })
        } | Out-Null
        $planBefore = [IO.File]::ReadAllText($planPath)
        $artifactBefore = [IO.File]::ReadAllText($script:ArtifactPath)
        $directory = Join-Path $script:WorkRoot 'caller-working-directory'
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        $result = Invoke-NativeFinding -WorkingDirectory $directory -Arguments @(
            'remove-finding', '--artifact-path', $script:ArtifactPath,
            '--plan-path', $planPath, '--now-utc', '2024-05-06T07:08:09Z'
        )
        $result.exitCode | Should -Not -Be 0
        $result.standardOutput | Should -BeNullOrEmpty
        [IO.File]::ReadAllText($planPath) | Should -BeExactly $planBefore
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $artifactBefore
        @(Get-ChildItem -LiteralPath $directory -Force).Count | Should -Be 0
    }

    It 'reports a valid artifact set and exits successfully' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @('complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath) | Out-Null

        $result = Invoke-FindingValidation -Path $script:FindingsDirectory -ReviewerId @('g1-correctness')

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.isValid | Should -BeTrue
    }

    # A session-state directory is not sealed, so the gate has to fail on a directory it cannot read
    # as a complete set rather than report a count that looks plausible.
    It 'exits unsuccessfully on a findings directory holding a foreign entry' {
        Initialize-TestArtifact | Out-Null
        Invoke-NativeFinding -Arguments @('complete-finding', '--artifact-path', $script:ArtifactPath, '--queue-path', $script:FileGroupQueuePath) | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:FindingsDirectory 'staging') -Force | Out-Null

        $result = Invoke-FindingValidation -Path $script:FindingsDirectory -ReviewerId @('g1-correctness')

        $result['exitCode'] | Should -Not -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.isValid | Should -BeFalse
        $errors = $emitted.errors
        ($errors -join ' ') | Should -BeLike "*'staging'*"
    }

    # The review uses this as a gate, so an invalid set has to fail the command rather than report a
    # status the caller might read past.
    It 'exits unsuccessfully on an invalid artifact set' {
        Initialize-TestArtifact | Out-Null

        $result = Invoke-FindingValidation -Path $script:FindingsDirectory -ReviewerId @('g1-correctness', 'g1-security')

        $result['exitCode'] | Should -Not -Be 0
        $emitted = ConvertFrom-Json -InputObject $result.standardOutput -AsHashtable -DateKind String
        $emitted.isValid | Should -BeFalse
        $missingReviewers = $emitted.missingReviewers
        $missingReviewers | Should -Be @('g1-security')
        $incompleteReviewers = $emitted.incompleteReviewers
        $incompleteReviewers | Should -Be @('g1-correctness')
    }

    It 'carries exotic content through the executables unchanged' {
        Initialize-TestArtifact | Out-Null

        Invoke-NativeFinding -Arguments @(
            'add-finding', '--artifact-path', $script:ArtifactPath
            '--finding-path', (New-TestFindingFile -Name 'exotic.json' -Override @{ description = $script:ExoticContent }), '--origin', 'reviewer'
        ) | Out-Null

        $stored = (Read-TestJsonFile -Path $script:ArtifactPath).findings[0]
        $stored.description | Should -BeExactly $script:ExoticContent
    }
}
