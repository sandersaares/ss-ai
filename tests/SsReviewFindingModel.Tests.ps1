BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in dependency
    # order, the same order the executables use: value handling first, then the shared model whose
    # path and file primitives the finding model builds on, then the finding model.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')

    # The host running this suite stands in for the executables it launches. Naming the running
    # executable rather than the command 'pwsh' keeps the child on the same build as the parent and
    # removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

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
    # The schema version is read from the library rather than restated, so a capture format change
    # fails these fixtures at the version check instead of letting them assert against a shape the
    # capture no longer writes.
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
            schemaVersion         = $script:PullRequestContextSchemaVersion
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

    # --- Child process helpers ------------------------------------------------------------------

    # Runs one of the skill's executables in a child process and reports its streams separately.
    #
    # Standard output is captured apart from standard error because the caller of these executables
    # parses standard output as JSON, so a refusal that writes anything at all there is a defect the
    # combined stream would hide.
    #
    # The child gets no profile, so a host profile cannot contribute output of its own; it gets
    # -NonInteractive and a closed standard input, so a missing mandatory parameter fails instead of
    # prompting for a value that will never arrive. The script is named by absolute path, because a
    # child process inherits the .NET current directory rather than the caller's location.
    function Invoke-SkillExecutable {
        param(
            [Parameter(Mandatory)]
            [string] $Name,

            [string[]] $ArgumentList = @(),

            [string] $WorkingDirectory
        )

        $executablePath = Join-Path $script:ExecutableRoot $Name
        if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
            throw "The executable under test is missing: $executablePath"
        }

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $script:PowerShellPath
        if ($WorkingDirectory) { $startInfo.WorkingDirectory = $WorkingDirectory }
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
    #
    # The concise error view wraps a message to the console width and prefixes each continuation
    # line with a pipe, so a phrase the caller has to recognize arrives split across lines whenever
    # the message is long - and these messages quote a full path, so they always are.
    function Get-SingleLineErrorText {
        param(
            [AllowEmptyString()]
            [AllowNull()]
            [string] $Text
        )

        (((([string] $Text) -replace '\x1b\[[0-9;]*m', '') -replace '(?m)^\s*\|', ' ') -replace '\s+', ' ').Trim()
    }
}

Describe 'Finding content validation' {
    It 'refuses a finding that omits a required field, naming the field' {
        foreach ($name in @('title', 'description', 'reasoning', 'consequence', 'recommendedAction')) {
            $finding = New-TestFinding -Remove @($name)
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'$name'*" -Because $name
        }
    }

    It 'names the finding in the rejection' {
        $finding = New-TestFinding -Remove @('title')
        { Assert-FindingContent -Finding $finding -SourceDescription "'reviews/g1-correctness.json'" } |
            Should -Throw -ExpectedMessage '*reviews/g1-correctness.json*'
    }

    It 'refuses a required field that is present but empty' {
        foreach ($value in @('', '   ')) {
            $finding = New-TestFinding -Override @{ description = $value }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'description'*"
        }
    }

    It 'refuses a required field that is not a string' {
        $finding = New-TestFinding -Override @{ consequence = 7 }
        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
            Should -Throw -ExpectedMessage "*'consequence'*"
    }

    It 'refuses C0 controls other than tab and line feed in multiline prose' {
        $disallowedCodePoints = @(0..8) + @(11..31)
        foreach ($name in @($script:FindingProseFields | Where-Object { $_ -cne 'title' })) {
            foreach ($codePoint in $disallowedCodePoints) {
                $finding = New-TestFinding -Override @{ $name = "before$([char] $codePoint)after" }
                $formattedCodePoint = 'U+{0:X4}' -f $codePoint

                { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                    Should -Throw -ExpectedMessage "*'$name'*$formattedCodePoint*" -Because "$name contains $formattedCodePoint"
            }
        }
    }

    It 'refuses every C0 control in a title' {
        foreach ($codePoint in 0..31) {
            $finding = New-TestFinding -Override @{ title = "before$([char] $codePoint)after" }
            $formattedCodePoint = 'U+{0:X4}' -f $codePoint

            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'title'*$formattedCodePoint*" -Because "title contains $formattedCodePoint"
        }
    }

    It 'accepts tab and line feed in multiline prose' {
        foreach ($name in @($script:FindingProseFields | Where-Object { $_ -cne 'title' })) {
            foreach ($value in @("before`tafter", "before`nafter")) {
                $finding = New-TestFinding -Override @{ $name = $value }

                { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                    Should -Not -Throw -Because $name
            }
        }
    }

    It 'refuses an unknown top-level field' {
        $finding = New-TestFinding -Override @{ severity = 'high' }
        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
            Should -Throw -ExpectedMessage "*unknown field 'severity'*"
    }

    # Priority and category are the two fields reviewers invent most often, and both belong to a
    # triage the maintainer performs rather than to the finding.
    It 'refuses priority and category' {
        foreach ($name in @('priority', 'category')) {
            $finding = New-TestFinding -Override @{ $name = 'whatever' }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*unknown field '$name'*" -Because $name
        }
    }

    It 'refuses a field the tooling assigns' {
        foreach ($name in @('id', 'origin', 'schemaVersion', 'sourceFindingIds')) {
            $finding = New-TestFinding -Override @{ $name = 'x' }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'$name', which the tooling assigns*" -Because $name
        }
    }

    It 'accepts a finding carrying every optional field' {
        $finding = New-TestFinding -Override @{
            references        = @(
                [ordered]@{ text = 'docs/design.md, "Retry policy"' }
                [ordered]@{ text = 'The retry RFC'; url = 'https://example.invalid/rfc' }
            )
            impactedLocations = @(
                [ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' }
                [ordered]@{ path = 'src/legacy.rs' }
            )
            followUp          = [ordered]@{ threadId = 'T1'; commentId = 'C1' }
        }

        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
    }

    # A title is a heading in a comment and a field of its own in a tracker item, and a tracker
    # refuses one it cannot hold. Refusing it here reports it to the author, who can still fix it,
    # rather than partway through a publication run that has already created items.
    Context 'title length' {
        It 'accepts a title at the bound' {
            $finding = New-TestFinding -Override @{ title = 'x' * $script:FindingTitleMaximumLength }

            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
        }

        It 'refuses a title one character past the bound' {
            $finding = New-TestFinding -Override @{ title = 'x' * ($script:FindingTitleMaximumLength + 1) }

            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*$($script:FindingTitleMaximumLength + 1) characters*"
        }

        It 'names the finding and the bound in the refusal' {
            $finding = New-TestFinding -Override @{ title = 'x' * 400 }

            { Assert-FindingContent -Finding $finding -SourceDescription "'reviews/g1-correctness.json'" } |
                Should -Throw -ExpectedMessage '*reviews/g1-correctness.json*'
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*at most $($script:FindingTitleMaximumLength)*"
        }

        # The bound counts characters rather than bytes, so a title of text outside ASCII is not
        # refused for a length its author cannot see.
        It 'counts a title in characters rather than bytes' {
            $emDash = [string] [char]0x2014
            $finding = New-TestFinding -Override @{ title = $emDash * $script:FindingTitleMaximumLength }

            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
        }

        It 'refuses an over-long title reaching the tooling through an artifact' {
            $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1-correctness' -FocusArea 'correctness'
            Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' | Out-Null
            Complete-FindingArtifactDocument -Document $document
            $stored = (Get-JsonArrayProperty -Node $document -Name 'findings')[0]
            Set-JsonProperty -Node $stored -Name 'title' -Value ('x' * ($script:FindingTitleMaximumLength + 1))

            $directory = Join-Path $TestDrive 'longtitle\findings'
            Write-FindingArtifact `
                -Path (Get-FindingArtifactPath -OutputDirectory $directory -ReviewerId 'g1-correctness') `
                -Document $document

            $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')

            $result['isValid'] | Should -BeFalse
            (@($result['errors']) -join "`n") | Should -BeLike "*a title holds at most $($script:FindingTitleMaximumLength)*"
        }

        # The bound is stated once and read on both paths, so a record that is already stored is
        # held to the same rule as the finding an author writes.
        It 'holds a stored record to the same bound' {
            $stored = [ordered]@{
                schemaVersion     = $script:FindingArtifactSchemaVersion
                id                = 'F1'
                sourceFindingIds  = @('g1-1')
                origin            = 'reviewer'
                title             = 'x' * ($script:FindingTitleMaximumLength + 1)
                description       = 'The loop condition is never updated.'
                reasoning         = 'The counter is incremented in a branch that cannot be reached.'
                consequence       = 'A transient failure hangs the caller indefinitely.'
                recommendedAction = 'Increment the counter outside the branch.'
                anchor            = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; endLine = 42; side = 'right' }
            }

            { Assert-FindingShape -Finding $stored -SourceDescription "'approved.json' (finding 'F1')" -IncludeToolingField $true } |
                Should -Throw -ExpectedMessage "*a title holds at most $($script:FindingTitleMaximumLength)*"

            Set-JsonProperty -Node $stored -Name 'title' -Value ('x' * $script:FindingTitleMaximumLength)
            { Assert-FindingShape -Finding $stored -SourceDescription "'approved.json' (finding 'F1')" -IncludeToolingField $true } |
                Should -Not -Throw
        }
    }

    Context 'anchor' {
        It 'requires an anchor' {
            { Assert-FindingContent -Finding (New-TestFinding -Remove @('anchor')) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor'*"
        }

        It 'refuses an unknown anchor kind' {
            $finding = New-TestFinding -Override @{ anchor = [ordered]@{ kind = 'range'; path = 'src/retry.rs' } }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.kind'*"
        }

        It 'accepts a file anchor that names no position' {
            $finding = New-TestFinding -Override @{ anchor = [ordered]@{ kind = 'file'; path = 'src/retry.rs' } }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
        }

        It 'refuses a position on a file anchor' {
            foreach ($name in @('startLine', 'endLine', 'side')) {
                $anchor = [ordered]@{ kind = 'file'; path = 'src/retry.rs' }
                $anchor[$name] = if ($name -eq 'side') { 'right' } else { 3 }
                { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                    Should -Throw -ExpectedMessage "*'anchor.$name'*" -Because $name
            }
        }

        It 'requires a start line on a line anchor' {
            $finding = New-TestFinding -Override @{ anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs' } }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.startLine'*"
        }

        It 'refuses a line number that is not a positive integer' {
            foreach ($value in @(0, -1, '42', 4.5)) {
                $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = $value }
                { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                    Should -Throw -ExpectedMessage "*'anchor.startLine'*" -Because ([string] $value)
            }
        }

        It 'refuses an end line before the start line' {
            $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; endLine = 41 }
            { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.endLine'*"
        }

        It 'accepts both diff sides and refuses any other' {
            foreach ($side in @('right', 'left')) {
                $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; side = $side }
                { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                    Should -Not -Throw -Because $side
            }

            $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; side = 'RIGHT' }
            { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.side'*"
        }

        It 'refuses an unknown anchor field' {
            $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; commitId = 'abc' }
            { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.commitId'*"
        }

        It 'refuses an end line without a start line' {
            $anchor = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; endLine = 42 }
            { Assert-FindingContent -Finding (New-TestFinding -Override @{ anchor = $anchor }) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'anchor.startLine'*"
        }
    }

    Context 'references and impacted locations' {
        # The wildcard patterns are single-quoted with escaped brackets, because a bracketed index
        # is a character class to the wildcard matcher and would match a message that never
        # mentioned the index at all.
        It 'requires text on a reference' {
            $finding = New-TestFinding -Override @{ references = @([ordered]@{ url = 'https://example.invalid' }) }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage '*''references`[0`].text''*'
        }

        It 'refuses an unknown reference field' {
            $finding = New-TestFinding -Override @{ references = @([ordered]@{ text = 'x'; title = 'y' }) }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage '*''references`[0`].title''*'
        }

        It 'requires a path on an impacted location' {
            $finding = New-TestFinding -Override @{ impactedLocations = @([ordered]@{ startLine = 3 }) }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage '*''impactedLocations`[0`].path''*'
        }

        It 'refuses an end line without a start line on an impacted location' {
            $finding = New-TestFinding -Override @{ impactedLocations = @([ordered]@{ path = 'src/a.rs'; endLine = 3 }) }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage '*''impactedLocations`[0`].endLine''*'
        }

        It 'refuses a diff side on a location that names no line' {
            $finding = New-TestFinding -Override @{ impactedLocations = @([ordered]@{ path = 'src/a.rs'; side = 'left' }) }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage '*''impactedLocations`[0`].side''*'
        }

        It 'accepts empty reference and location lists' {
            $finding = New-TestFinding -Override @{ references = @(); impactedLocations = @() }
            { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
        }
    }

    Context 'follow-up' {
        It 'requires both identifiers' {
            foreach ($name in @('threadId', 'commentId')) {
                $followUp = [ordered]@{ threadId = 'T1'; commentId = 'C1' }
                $followUp.Remove($name)
                { Assert-FindingContent -Finding (New-TestFinding -Override @{ followUp = $followUp }) -SourceDescription "'reviewer.json'" } |
                    Should -Throw -ExpectedMessage "*'followUp.$name'*" -Because $name
            }
        }

        It 'refuses an unknown follow-up field' {
            $followUp = [ordered]@{ threadId = 'T1'; commentId = 'C1'; resolve = $true }
            { Assert-FindingContent -Finding (New-TestFinding -Override @{ followUp = $followUp }) -SourceDescription "'reviewer.json'" } |
                Should -Throw -ExpectedMessage "*'followUp.resolve'*"
        }
    }
}

Describe 'Path canonicalization' {
    It 'reduces redundant segments to one spelling' {
        ConvertTo-CanonicalFindingPath -Path './src/retry.rs' | Should -BeExactly 'src/retry.rs'
        ConvertTo-CanonicalFindingPath -Path 'src//retry.rs' | Should -BeExactly 'src/retry.rs'
        ConvertTo-CanonicalFindingPath -Path 'src/retry/mod.rs' | Should -BeExactly 'src/retry/mod.rs'
    }

    # Reviewer subagents run on Windows, so this is the form they produce unless they are stopped.
    # A backslash path matches no file in the Git diff an anchor is resolved against, and the
    # finding would fail during publication, after the user had already approved it.
    It 'refuses a path that separates its directories with backslashes' {
        foreach ($path in @('src\retry.rs', '.\src\retry.rs', 'src/retry\mod.rs')) {
            $message = { ConvertTo-CanonicalFindingPath -Path $path } | Should -Throw -PassThru
            $message.Exception.Message | Should -BeLike "*'$path'*" -Because $path
            $message.Exception.Message | Should -BeLike '*backslash*' -Because $path
            $message.Exception.Message | Should -BeLike '*relative to the repository root*' -Because $path
        }
    }

    # Phase 1 writes absolute paths into the file inventory, so a reviewer copying one out of it
    # reaches this refusal in the ordinary course of a run and has to be told what to write instead.
    It 'refuses an absolute path and states the accepted form' {
        foreach ($path in @('C:\src\retry.rs', 'C:/src/retry.rs', '\\server\share\retry.rs', '/src/retry.rs')) {
            $message = { ConvertTo-CanonicalFindingPath -Path $path } | Should -Throw -PassThru
            $message.Exception.Message | Should -BeLike '*absolute*' -Because $path
            $message.Exception.Message | Should -BeLike "*relative to the repository root*" -Because $path
        }
    }

    It 'refuses a path that leaves the repository' {
        { ConvertTo-CanonicalFindingPath -Path 'src/../../secrets.txt' } | Should -Throw -ExpectedMessage "*'..'*"
    }

    It 'refuses a path that names nothing' {
        foreach ($path in @('', '   ', './')) {
            { ConvertTo-CanonicalFindingPath -Path $path } | Should -Throw -Because "'$path'"
        }
    }

    It 'names the source in the refusal' {
        { ConvertTo-CanonicalFindingPath -Path 'C:\src\retry.rs' -SourceDescription "'reviews/g1.json'" } |
            Should -Throw -ExpectedMessage '*reviews/g1.json*'
    }

    It 'stores the canonical form of every path a finding carries' {
        $document = New-FindingArtifactDocument -Kind 'consolidated'
        $finding = New-TestFinding -Override @{
            anchor            = [ordered]@{ kind = 'line'; path = './src/retry.rs'; startLine = 42 }
            impactedLocations = @([ordered]@{ path = 'src//legacy/mod.rs' })
        }

        Add-FindingToArtifact -Document $document -Finding $finding -Origin 'reviewer' | Out-Null

        $stored = (Get-JsonArrayProperty -Node $document -Name 'findings')[0]
        (Get-JsonProperty -Node (Get-JsonProperty -Node $stored -Name 'anchor') -Name 'path') |
            Should -BeExactly 'src/retry.rs'
        (Get-JsonProperty -Node (Get-JsonArrayProperty -Node $stored -Name 'impactedLocations')[0] -Name 'path') |
            Should -BeExactly 'src/legacy/mod.rs'
    }

    It 'refuses a finding whose anchor path is absolute' {
        $document = New-FindingArtifactDocument -Kind 'consolidated'
        $finding = New-TestFinding -Override @{
            anchor = [ordered]@{ kind = 'line'; path = 'C:\repo\src\retry.rs'; startLine = 42 }
        }

        { Add-FindingToArtifact -Document $document -Finding $finding -Origin 'reviewer' } |
            Should -Throw -ExpectedMessage '*absolute*'
        (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 0
    }

    It 'refuses a finding whose anchor path carries a backslash' {
        $finding = New-TestFinding -Override @{
            anchor = [ordered]@{ kind = 'file'; path = 'src\retry.rs' }
        }

        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
            Should -Throw -ExpectedMessage "*'src\retry.rs' from 'reviewer.json' separates its directories with backslashes*"
    }

    It 'refuses a finding whose impacted location carries a backslash' {
        $finding = New-TestFinding -Override @{
            impactedLocations = @(
                [ordered]@{ path = 'src/retry.rs' }
                [ordered]@{ path = 'src\legacy\mod.rs' }
            )
        }

        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } |
            Should -Throw -ExpectedMessage '*backslash*'
    }

    It 'accepts a finding whose paths use forward slashes' {
        $finding = New-TestFinding -Override @{
            anchor            = [ordered]@{ kind = 'line'; path = 'src/retry/mod.rs'; startLine = 42 }
            impactedLocations = @([ordered]@{ path = 'src/legacy/mod.rs' })
        }

        { Assert-FindingContent -Finding $finding -SourceDescription "'reviewer.json'" } | Should -Not -Throw
    }
}

Describe 'Identifier lists' {
    It 'reads several identifiers from one command line value' {
        $identifiers = ConvertTo-FindingIdentifierList -Value @('g1-1, g2-3')
        $identifiers | Should -Be @('g1-1', 'g2-3')
    }

    It 'reads identifiers supplied as an array' {
        $identifiers = ConvertTo-FindingIdentifierList -Value @('g1-1', 'g2-3')
        $identifiers | Should -Be @('g1-1', 'g2-3')
    }

    It 'reads nothing from an empty value' {
        $identifiers = ConvertTo-FindingIdentifierList -Value @()
        $identifiers.Count | Should -Be 0

        $identifiers = ConvertTo-FindingIdentifierList -Value @('')
        $identifiers.Count | Should -Be 0
    }
}

Describe 'Finding artifacts' {
    It 'creates a reviewer artifact that is open and empty' {
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1-correctness' -FocusArea 'correctness'

        (Get-JsonProperty -Node $document -Name 'kind') | Should -BeExactly 'reviewer'
        (Get-JsonProperty -Node $document -Name 'reviewerId') | Should -BeExactly 'g1-correctness'
        $document.Contains('fileGroup') | Should -BeFalse
        (Get-JsonProperty -Node $document -Name 'focusArea') | Should -BeExactly 'correctness'
        (Get-JsonProperty -Node $document -Name 'status') | Should -BeExactly 'open'
        (Get-JsonArrayProperty -Node $document -Name 'findings').Count | Should -Be 0
    }

    It 'refuses reviewer fields on a consolidated artifact' {
        { New-FindingArtifactDocument -Kind 'consolidated' -ReviewerId 'g1' } | Should -Throw -ExpectedMessage '*-ReviewerId*'
        { New-FindingArtifactDocument -Kind 'consolidated' -FocusArea 'correctness' } | Should -Throw -ExpectedMessage '*-FocusArea*'
    }

    It 'refuses a reviewer artifact that names no focus area' {
        { New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' } | Should -Throw -ExpectedMessage '*-FocusArea*'
    }

    # The reviewer identifier names the artifact file, so one carrying a separator would write outside
    # the findings directory.
    It 'refuses a reviewer identifier that cannot name a file' {
        foreach ($reviewerId in @('g1/correctness', 'g1\correctness', '..', 'g1..correctness', '')) {
            { New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId $reviewerId -FocusArea 'correctness' } |
                Should -Throw -Because "'$reviewerId'"
        }
    }

    It 'numbers reviewer findings from the reviewer identifier' {
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1-correctness' -FocusArea 'correctness'

        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' |
            Should -BeExactly 'g1-correctness-1'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' |
            Should -BeExactly 'g1-correctness-2'
    }

    It 'numbers consolidated findings from a single letter' {
        $document = New-FindingArtifactDocument -Kind 'consolidated'

        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'synthesis' | Should -BeExactly 'F1'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'synthesis' | Should -BeExactly 'F2'
    }

    It 'appends in the order the findings arrive' {
        $document = New-FindingArtifactDocument -Kind 'consolidated'
        foreach ($title in @('First', 'Second', 'Third')) {
            Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = $title }) -Origin 'reviewer' | Out-Null
        }

        $findings = Get-JsonArrayProperty -Node $document -Name 'findings'
        @($findings | ForEach-Object { Get-JsonProperty -Node $_ -Name 'title' }) |
            Should -Be @('First', 'Second', 'Third')
        @($findings | ForEach-Object { Get-JsonProperty -Node $_ -Name 'id' }) |
            Should -Be @('F1', 'F2', 'F3')
    }

    It 'records provenance and origin' {
        $document = New-FindingArtifactDocument -Kind 'consolidated'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'synthesis' -SourceFindingId @('g1-1', 'g2-3') | Out-Null

        $stored = (Get-JsonArrayProperty -Node $document -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'origin') | Should -BeExactly 'synthesis'
        # Assigned before it is asserted on, because a command that emits a single array emits it as
        # one object and wrapping the call in @( ) produces an array holding that array.
        $sourceFindingIds = Get-JsonArrayProperty -Node $stored -Name 'sourceFindingIds'
        $sourceFindingIds | Should -Be @('g1-1', 'g2-3')
        (Get-JsonProperty -Node $stored -Name 'schemaVersion') | Should -Be $script:FindingArtifactSchemaVersion
    }

    It 'leaves provenance empty for a reviewer finding' {
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' | Out-Null

        $stored = (Get-JsonArrayProperty -Node $document -Name 'findings')[0]
        (Get-JsonArrayProperty -Node $stored -Name 'sourceFindingIds').Count | Should -Be 0
    }

    It 'refuses a finding once the reviewer is complete' {
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness'
        Complete-FindingArtifactDocument -Document $document

        (Get-JsonProperty -Node $document -Name 'status') | Should -BeExactly 'complete'
        { Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' } |
            Should -Throw -ExpectedMessage '*complete*'
    }

    It 'records completion only on a reviewer artifact' {
        { Complete-FindingArtifactDocument -Document (New-FindingArtifactDocument -Kind 'consolidated') } |
            Should -Throw -ExpectedMessage '*consolidated*'
    }

    It 'refuses an artifact written to another schema version' {
        $path = Join-Path $TestDrive 'version\reviewer.json'
        Write-TestJsonFile -Path $path -Value ([ordered]@{
                schemaVersion = $script:FindingArtifactSchemaVersion + 1
                kind          = 'reviewer'
                reviewerId        = 'g1'
                focusArea     = 'correctness'
                status        = 'open'
                findings      = @()
            }) | Out-Null

        { Read-FindingArtifact -Path $path } | Should -Throw -ExpectedMessage "*$path*"
    }

    It 'refuses an artifact of an unknown kind' {
        $path = Join-Path $TestDrive 'kind\reviewer.json'
        Write-TestJsonFile -Path $path -Value ([ordered]@{
                schemaVersion = $script:FindingArtifactSchemaVersion
                kind          = 'summary'
                findings      = @()
            }) | Out-Null

        { Read-FindingArtifact -Path $path } | Should -Throw -ExpectedMessage "*'summary'*"
    }
}

Describe 'Updating and removing findings' {
    BeforeEach {
        $script:Document = New-FindingArtifactDocument -Kind 'consolidated'
        Add-FindingToArtifact -Document $script:Document -Finding (New-TestFinding -Override @{ title = 'First' }) -Origin 'synthesis' -SourceFindingId @('g1-1') | Out-Null
        Add-FindingToArtifact -Document $script:Document -Finding (New-TestFinding -Override @{ title = 'Second' }) -Origin 'reviewer' | Out-Null
    }

    It 'preserves identity and provenance across a rewrite' {
        $replacement = New-TestFinding -Override @{
            title  = 'Rewritten'
            anchor = [ordered]@{ kind = 'file'; path = 'src/other.rs' }
        }

        Set-FindingInArtifact -Document $script:Document -FindingId 'F1' -Finding $replacement

        $stored = (Get-JsonArrayProperty -Node $script:Document -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'id') | Should -BeExactly 'F1'
        (Get-JsonProperty -Node $stored -Name 'origin') | Should -BeExactly 'synthesis'
        $sourceFindingIds = Get-JsonArrayProperty -Node $stored -Name 'sourceFindingIds'
        $sourceFindingIds | Should -Be @('g1-1')
        (Get-JsonProperty -Node $stored -Name 'schemaVersion') | Should -Be $script:FindingArtifactSchemaVersion
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly 'Rewritten'
        (Get-JsonProperty -Node (Get-JsonProperty -Node $stored -Name 'anchor') -Name 'kind') | Should -BeExactly 'file'
    }

    It 'leaves the other findings where they were' {
        Set-FindingInArtifact -Document $script:Document -FindingId 'F1' -Finding (New-TestFinding -Override @{ title = 'Rewritten' })

        $findings = Get-JsonArrayProperty -Node $script:Document -Name 'findings'
        $findings.Count | Should -Be 2
        (Get-JsonProperty -Node $findings[1] -Name 'title') | Should -BeExactly 'Second'
    }

    It 'refuses a rewrite that carries an identifier' {
        $replacement = New-TestFinding -Override @{ id = 'F1' }
        { Set-FindingInArtifact -Document $script:Document -FindingId 'F1' -Finding $replacement } |
            Should -Throw -ExpectedMessage '*tooling assigns*'
    }

    It 'refuses an unknown identifier' {
        { Set-FindingInArtifact -Document $script:Document -FindingId 'F9' -Finding (New-TestFinding) } |
            Should -Throw -ExpectedMessage "*'F9'*"
        { Remove-FindingFromArtifact -Document $script:Document -FindingId 'F9' -Reason 'no longer relevant' } |
            Should -Throw -ExpectedMessage "*'F9'*"
    }

    It 'records the discarded finding, the reason and the moment' {
        Remove-FindingFromArtifact `
            -Document $script:Document `
            -FindingId 'F1' `
            -Reason 'Duplicates F2.' `
            -NowUtc ([DateTimeOffset]::new(2024, 5, 6, 7, 8, 9, [timespan]::Zero))

        $findings = Get-JsonArrayProperty -Node $script:Document -Name 'findings'
        $findings.Count | Should -Be 1
        (Get-JsonProperty -Node $findings[0] -Name 'id') | Should -BeExactly 'F2'

        $removed = Get-JsonArrayProperty -Node $script:Document -Name 'removed'
        $removed.Count | Should -Be 1
        (Get-JsonProperty -Node (Get-JsonProperty -Node $removed[0] -Name 'finding') -Name 'id') | Should -BeExactly 'F1'
        (Get-JsonProperty -Node $removed[0] -Name 'reason') | Should -BeExactly 'Duplicates F2.'
        (Get-JsonProperty -Node $removed[0] -Name 'removedUtc') | Should -BeExactly '2024-05-06T07:08:09.0000000Z'
    }

    # A discarded finding is still quoted in its removal record, so reusing its number would attach
    # that account to a different finding.
    It 'does not reuse the identifier of a discarded finding' {
        Remove-FindingFromArtifact -Document $script:Document -FindingId 'F2' -Reason 'Out of scope.'
        Add-FindingToArtifact -Document $script:Document -Finding (New-TestFinding) -Origin 'reviewer' |
            Should -BeExactly 'F3'
    }

    It 'validates persisted finding data in removal history: <Case>' -ForEach @(
        @{ Case = 'identifier only'; Mutate = { param($entry) $entry.finding = @{ id = 'F1' } } }
        @{ Case = 'missing finding'; Mutate = { param($entry) $entry.finding = $null } }
        @{ Case = 'invalid prose'; Mutate = { param($entry) $entry.finding.title = '' } }
        @{ Case = 'invalid anchor'; Mutate = { param($entry) $entry.finding.anchor.kind = 'unknown' } }
    ) {
        Remove-FindingFromArtifact -Document $script:Document -FindingId 'F1' -Reason 'Out of scope.'
        (Get-FindingArtifactProblem -Document $script:Document -Path 'artifact.json').Count | Should -Be 0
        & $Mutate $script:Document.removed[0]
        (Get-FindingArtifactProblem -Document $script:Document -Path 'artifact.json').Count | Should -BeGreaterThan 0
    }

    It 'requires a reason' {
        { Remove-FindingFromArtifact -Document $script:Document -FindingId 'F1' -Reason '  ' } |
            Should -Throw -ExpectedMessage '*reason*'
    }

    It 'discards every finding a batch names' {
        $removed = Remove-FindingBatchFromArtifact `
            -Document $script:Document `
            -Removal @(
                [ordered]@{ findingId = 'F1'; reason = 'Duplicates F2.' }
                [ordered]@{ findingId = 'F2'; reason = 'Out of scope.' }
            ) `
            -NowUtc ([DateTimeOffset]::new(2024, 5, 6, 7, 8, 9, [timespan]::Zero))

        $removed | Should -Be @('F1', 'F2')
        (Get-JsonArrayProperty -Node $script:Document -Name 'findings').Count | Should -Be 0
        (Get-JsonArrayProperty -Node $script:Document -Name 'removed').Count | Should -Be 2
    }

    # A batch is applied as a whole, so an identifier named twice would make the outcome depend on
    # the order the entries are read in.
    It 'refuses a batch that names one finding twice' {
        { Set-FindingBatchInArtifact -Document $script:Document -Update @(
                [ordered]@{ findingId = 'F1'; findingPath = "'a.json'"; finding = (New-TestFinding) }
                [ordered]@{ findingId = 'F1'; findingPath = "'b.json'"; finding = (New-TestFinding) }
            ) } | Should -Throw -ExpectedMessage "*'F1'*"

        { Remove-FindingBatchFromArtifact -Document $script:Document -Removal @(
                [ordered]@{ findingId = 'F1'; reason = 'Duplicates F2.' }
                [ordered]@{ findingId = 'F1'; reason = 'Out of scope.' }
            ) } | Should -Throw -ExpectedMessage "*'F1'*"

        (Get-JsonArrayProperty -Node $script:Document -Name 'findings').Count | Should -Be 2
        (Get-JsonArrayProperty -Node $script:Document -Name 'removed').Count | Should -Be 0
    }
}

Describe 'Follow-up targets' {
    BeforeAll {
        $script:ReviewThread = New-CaptureThread -Id 'RT_kw1' -Kind 'text' -OpeningCommentId 'PRRC_1' -ThreadContext (New-CaptureThreadContext)
        $script:IssueCommentThread = New-CaptureThread -Id 'IC_kw2' -Kind 'text' -OpeningCommentId 'IC_2'
        $script:ReviewSummaryThread = New-CaptureThread -Id 'PRR_kw3' -Kind 'reviewSummary' -OpeningCommentId 'PRR_kw3'
        $script:RefUpdateThread = New-CaptureThread -Id 'RU_kw4' -Kind 'refUpdate' -OpeningCommentId 'RU_4'

        $script:GitHubCapture = New-CaptureDocument -Platform 'gitHub' -Thread @(
            $script:ReviewThread
            $script:IssueCommentThread
            $script:ReviewSummaryThread
            $script:RefUpdateThread
        )

        $script:AdoFileThread = New-CaptureThread -Id '11' -Kind 'text' -OpeningCommentId '1' -ThreadContext (New-CaptureThreadContext)
        $script:AdoOverallThread = New-CaptureThread -Id '12' -Kind 'text' -OpeningCommentId '1'
        $script:AdoSystemThread = New-CaptureThread -Id '13' -Kind 'system' -OpeningCommentId '1'

        $script:AdoCapture = New-CaptureDocument -Platform 'azureDevOps' -Thread @(
            $script:AdoFileThread
            $script:AdoOverallThread
            $script:AdoSystemThread
        )

        function New-FollowUpFinding {
            param(
                [Parameter(Mandatory)]
                [string] $ThreadId,

                [Parameter(Mandatory)]
                [string] $CommentId
            )

            New-TestFinding -Override @{ followUp = [ordered]@{ threadId = $ThreadId; commentId = $CommentId } }
        }
    }

    It 'passes a finding that carries no follow-up' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-TestFinding) } | Should -Not -Throw
    }

    It 'accepts a GitHub review thread' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'RT_kw1' -CommentId 'PRRC_1') } |
            Should -Not -Throw
    }

    # A GitHub text thread with no anchor is a flat comment on the pull request, and the service
    # accepts no reply posted into it.
    It 'refuses a GitHub comment on the pull request' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'IC_kw2' -CommentId 'IC_2') } |
            Should -Throw -ExpectedMessage '*review thread*'
    }

    It 'refuses a GitHub review summary' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'PRR_kw3' -CommentId 'PRR_kw3') } |
            Should -Throw -ExpectedMessage "*'reviewSummary'*"
    }

    It 'refuses a GitHub thread recording service activity' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'RU_kw4' -CommentId 'RU_4') } |
            Should -Throw -ExpectedMessage "*'refUpdate'*"
    }

    It 'accepts an Azure DevOps thread anchored to a file' {
        { Assert-FindingFollowUpTarget -Context $script:AdoCapture -Finding (New-FollowUpFinding -ThreadId '11' -CommentId '1') } |
            Should -Not -Throw
    }

    # Azure DevOps accepts a reply into a thread raised against the pull request as a whole, which
    # carries no anchor. The GitHub rule must not be applied to it.
    It 'accepts an Azure DevOps thread on the pull request as a whole' {
        { Assert-FindingFollowUpTarget -Context $script:AdoCapture -Finding (New-FollowUpFinding -ThreadId '12' -CommentId '1') } |
            Should -Not -Throw
    }

    It 'refuses an Azure DevOps thread recording service activity' {
        { Assert-FindingFollowUpTarget -Context $script:AdoCapture -Finding (New-FollowUpFinding -ThreadId '13' -CommentId '1') } |
            Should -Throw -ExpectedMessage "*'system'*"
    }

    It 'refuses a thread the capture does not contain' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'RT_missing' -CommentId 'PRRC_1') } |
            Should -Throw -ExpectedMessage "*'RT_missing'*"
    }

    It 'refuses a comment that did not open the thread' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'RT_kw1' -CommentId 'PRRC_9') } |
            Should -Throw -ExpectedMessage "*'PRRC_9'*"
    }

    # Thread identifiers are opaque case-sensitive strings on GitHub, so two that differ only in
    # case belong to two different threads.
    It 'compares identifiers case-sensitively' {
        { Assert-FindingFollowUpTarget -Context $script:GitHubCapture -Finding (New-FollowUpFinding -ThreadId 'rt_kw1' -CommentId 'PRRC_1') } |
            Should -Throw -ExpectedMessage "*'rt_kw1'*"
    }

    It 'refuses a capture from a platform it cannot reply on' {
        $capture = New-CaptureDocument -Platform 'gitHub' -Thread @($script:ReviewThread)
        Set-JsonProperty -Node $capture -Name 'platform' -Value 'gitLab'

        { Assert-FindingFollowUpTarget -Context $capture -Finding (New-FollowUpFinding -ThreadId 'RT_kw1' -CommentId 'PRRC_1') } |
            Should -Throw -ExpectedMessage "*'gitLab'*"
    }
}

Describe 'Artifact set validation' {
    BeforeAll {
        # Builds a directory of reviewer artifacts through the model, so the fixture cannot describe a
        # shape the writing path never produces.
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
                $document = New-FindingArtifactDocument `
                    -Kind 'reviewer' `
                    -ReviewerId ([string] $description['reviewerId']) `
                    -FocusArea 'correctness'

                for ($index = 0; $index -lt [int] $description['findingCount']; $index++) {
                    Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin 'reviewer' | Out-Null
                }
                if ($description['isComplete']) { Complete-FindingArtifactDocument -Document $document }

                Write-FindingArtifact `
                    -Path (Get-FindingArtifactPath -OutputDirectory $directory -ReviewerId ([string] $description['reviewerId'])) `
                    -Document $document
            }

            $directory
        }
    }

    It 'reports a complete set as valid' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'valid') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 2; isComplete = $true }
            @{ reviewerId = 'g1-security'; findingCount = 0; isComplete = $true }
        )

        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness', 'g1-security')

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

        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')

        $result['isValid'] | Should -BeFalse
        @($result['incompleteReviewers']) | Should -Be @('g1-correctness')
    }

    It 'reports a reviewer that wrote no artifact at all' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'missing') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness', 'g1-security')

        $result['isValid'] | Should -BeFalse
        @($result['missingReviewers']) | Should -Be @('g1-security')
    }

    It 'reports an artifact from a reviewer that was never dispatched' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'unexpected') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 0; isComplete = $true }
            @{ reviewerId = 'g9-invented'; findingCount = 0; isComplete = $true }
        )

        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')

        $result['isValid'] | Should -BeFalse
        @($result['unexpectedReviewers']) | Should -Be @('g9-invented')
    }

    It 'reports every defect in one pass rather than stopping at the first' {
        $root = Join-Path $TestDrive 'defects'
        $directory = New-TestArtifactSet -Root $root -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        $damaged = Read-TestJsonFile -Path (Join-Path $directory 'g1-correctness.json')
        $finding = (Get-JsonArrayProperty -Node $damaged -Name 'findings')[0]
        $finding.Remove('title')
        Set-JsonProperty -Node $finding -Name 'severity' -Value 'high'
        Set-JsonProperty -Node $damaged -Name 'status' -Value 'partial'
        Write-TestJsonFile -Path (Join-Path $directory 'g1-correctness.json') -Value $damaged | Out-Null

        $result = Get-FindingArtifactValidation -Path $directory

        $result['isValid'] | Should -BeFalse
        $result['errors'].Count | Should -BeGreaterThan 1
        ($result['errors'] -join ' ') | Should -BeLike "*'severity'*"
        ($result['errors'] -join ' ') | Should -BeLike "*'partial'*"
    }

    It 'validates a single artifact named directly' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'single') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 3; isComplete = $true }
        )

        $result = Get-FindingArtifactValidation -Path (Join-Path $directory 'g1-correctness.json')

        $result['isValid'] | Should -BeTrue
        $result['findingCount'] | Should -Be 3
    }

    It 'recognizes only staging names produced by the shared writers' {
        Test-FindingArtifactStagingName -Name '.reviewer.json.0123456789abcdef0123456789abcdef.tmp' | Should -BeTrue
        Test-FindingArtifactStagingName -Name '.ss-ai-transaction-aB1234567890abcdefgh1234.tmp' | Should -BeTrue
        foreach ($name in @(
            '.tmp123456'
            '.ss-ai-transaction-aB1234567890abcdefgh123.tmp'
            '.ss-ai-transaction-aB1234567890abcdefgh12345.tmp'
            '.ss-ai-transaction-aB1234567890abcdefghi-34.tmp'
            '.ss-ai-transaction-aB1234567890abcdefgh1234.TMP'
            '.ss-ai-transaction-aB1234567890abcdefgh1234.tmp.bak'
            '.reviewer.json.0123456789abcdef0123456789abcdeg.tmp'
        )) {
            Test-FindingArtifactStagingName -Name $name | Should -BeFalse -Because $name
        }
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
        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')
        $result['isValid'] | Should -BeTrue

        Set-Content -LiteralPath (Join-Path $directory $foreign) -Value 'foreign'
        New-Item -ItemType Directory -Path (Join-Path $directory $foreignDirectory) | Out-Null
        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')
        $result['isValid'] | Should -BeFalse
        ($result['errors'] -join ' ') | Should -BeLike "*$foreign*"
        ($result['errors'] -join ' ') | Should -BeLike "*$foreignDirectory*"
        ($result['errors'] -join ' ') | Should -Not -BeLike "*'$native'*"
        ($result['errors'] -join ' ') | Should -Not -BeLike "*'$powershell'*"
    }

    # A session-state directory is not sealed, so the gate states what it found rather than passing
    # a set it only partly read or reporting the reviewer as simply missing.
    It 'refuses a directory holding anything but reviewer artifacts' {
        $directory = New-TestArtifactSet -Root (Join-Path $TestDrive 'foreign') -Reviewer @(
            @{ reviewerId = 'g1-correctness'; findingCount = 1; isComplete = $true }
        )

        New-Item -ItemType Directory -Path (Join-Path $directory 'staging') -Force | Out-Null
        Set-Content -Path (Join-Path $directory 'notes.md') -Value 'still drafting'

        $result = Get-FindingArtifactValidation -Path $directory -ExpectedReviewerId @('g1-correctness')

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

        $result = Get-FindingArtifactValidation -Path $directory

        $result['isValid'] | Should -BeFalse
        ($result['errors'] -join ' ') | Should -BeLike '*scratch.json*'
        ($result['errors'] -join ' ') | Should -BeLike '*truncated.json*'
        $result['artifacts'].Count | Should -Be 1
    }

    It 'reports a path that holds nothing' {
        $result = Get-FindingArtifactValidation -Path (Join-Path $TestDrive 'absent\findings')

        $result['isValid'] | Should -BeFalse
        $result['errors'].Count | Should -Be 1
    }
}

Describe 'Content fidelity' {
    It 'reproduces prose exactly through a write and read cycle' {
        $path = Join-Path $TestDrive 'fidelity\findings\g1.json'
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness'
        $title = 'A "quoted" title with a `backtick` and a path C:\src\lib.rs.'
        $finding = New-TestFinding -Override @{
            title             = $title
            description       = $script:ExoticContent
            reasoning         = $script:ExoticContent
            consequence       = $script:ExoticContent
            recommendedAction = $script:ExoticContent
            references        = @([ordered]@{ text = $script:ExoticContent; url = 'https://example.invalid/a?b=c&d=e' })
        }

        Add-FindingToArtifact -Document $document -Finding $finding -Origin 'reviewer' | Out-Null
        Write-FindingArtifact -Path $path -Document $document

        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $path) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly $title
        foreach ($name in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
            (Get-JsonProperty -Node $stored -Name $name) | Should -BeExactly $script:ExoticContent -Because $name
        }
        (Get-JsonProperty -Node (Get-JsonArrayProperty -Node $stored -Name 'references')[0] -Name 'text') |
            Should -BeExactly $script:ExoticContent
    }

    # A body that is nothing but a date is the case ConvertFrom-Json corrupts, and a reviewer
    # quoting a timestamp out of a log writes exactly that.
    It 'reproduces content that looks like a timestamp' {
        $path = Join-Path $TestDrive 'timestamp\findings\g1.json'
        $document = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness'
        Add-FindingToArtifact `
            -Document $document `
            -Finding (New-TestFinding -Override @{ title = '2024-05-06T07:08:09Z' }) `
            -Origin 'reviewer' | Out-Null
        Write-FindingArtifact -Path $path -Document $document

        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $path) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly '2024-05-06T07:08:09Z'
    }

    It 'writes UTF-8 without a byte order mark and with newline endings' {
        $path = Join-Path $TestDrive 'encoding\findings\g1.json'
        Write-FindingArtifact `
            -Path $path `
            -Document (New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness')

        $bytes = [System.IO.File]::ReadAllBytes($path)
        @($bytes[0], $bytes[1], $bytes[2]) | Should -Not -Be @(0xEF, 0xBB, 0xBF)
        ([System.IO.File]::ReadAllText($path)) | Should -Not -BeLike "*`r`n*"
    }

    It 'replaces a finding artifact without leaving a partial-write file' {
        $path = Join-Path $TestDrive 'replacement\findings\g1.json'
        $first = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'correctness'
        $second = New-FindingArtifactDocument -Kind 'reviewer' -ReviewerId 'g1' -FocusArea 'security'
        Write-FindingArtifact -Path $path -Document $first

        Write-FindingArtifact -Path $path -Document $second

        (Get-JsonProperty -Node (Read-FindingArtifact -Path $path) -Name 'focusArea') |
            Should -BeExactly 'security'
        @(Get-ChildItem -LiteralPath (Split-Path -Parent $path) -Force -Filter '*.tmp').Count |
            Should -Be 0
    }
}

Describe 'Native workflow private requests' {
    BeforeAll {
        . (Join-Path $script:LibraryRoot 'NativeWorkflowBridge.ps1')
    }

    BeforeEach {
        $script:RequestPath = $null
        $script:RequestContent = $null
        $script:RequestMode = $null
        $script:ExclusiveCreationRejected = $false
        Mock Invoke-NativeReviewWorkflow {
            param([string[]] $Arguments)

            $script:RequestPath = $Arguments[-1].Substring('--plan-path='.Length)
            $script:RequestContent = [IO.File]::ReadAllText($script:RequestPath) | ConvertFrom-Json
            if (-not $IsWindows) {
                $script:RequestMode = [IO.File]::GetUnixFileMode($script:RequestPath)
            }
            try {
                $collision = [IO.File]::Open($script:RequestPath, [IO.FileMode]::CreateNew)
                $collision.Dispose()
            } catch [IO.IOException] {
                $script:ExclusiveCreationRejected = $true
            }
            if ($script:FailNativeRequest) { throw 'native request failed' }
            'ok'
        }
    }

    It 'creates a private request outside a read-only working directory and cleans it up' {
        $script:FailNativeRequest = $false
        $directory = Join-Path $TestDrive 'read-only-working-directory'
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        if (-not $IsWindows) {
            $originalMode = [IO.File]::GetUnixFileMode($directory)
            [IO.File]::SetUnixFileMode($directory, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserExecute)
        }
        Push-Location -LiteralPath $directory
        try {
            $result = Invoke-NativeReviewWorkflowWithJson -Arguments @('remove-finding') -PathOption 'plan-path' -Value @{
                removals = @(@{ findingId = 'F1'; reason = 'Duplicate.' })
            }
        } finally {
            Pop-Location
            if (-not $IsWindows) { [IO.File]::SetUnixFileMode($directory, $originalMode) }
        }

        $result | Should -BeExactly 'ok'
        [IO.Path]::GetDirectoryName($script:RequestPath) | Should -BeExactly ([IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetTempPath()))
        $script:RequestContent.removals[0].reason | Should -BeExactly 'Duplicate.'
        $script:ExclusiveCreationRejected | Should -BeTrue
        if (-not $IsWindows) {
            $otherPermissions = [IO.UnixFileMode]::GroupRead -bor [IO.UnixFileMode]::GroupWrite -bor [IO.UnixFileMode]::GroupExecute -bor
                [IO.UnixFileMode]::OtherRead -bor [IO.UnixFileMode]::OtherWrite -bor [IO.UnixFileMode]::OtherExecute
            ($script:RequestMode -band $otherPermissions) | Should -Be 0
        }
        [IO.File]::Exists($script:RequestPath) | Should -BeFalse
        @(Get-ChildItem -LiteralPath $directory -Force).Count | Should -Be 0
    }

    It 'cleans up the request when the native operation fails' {
        $script:FailNativeRequest = $true
        { Invoke-NativeReviewWorkflowWithJson -Arguments @('remove-finding') -PathOption 'plan-path' -Value @{ removals = @() } } |
            Should -Throw '*native request failed*'
        $script:RequestPath | Should -Not -BeNullOrEmpty
        [IO.File]::Exists($script:RequestPath) | Should -BeFalse
    }
}

Describe 'Finding executables' {
    BeforeAll {
        # Creates the artifact every executable test starts from.
        function Initialize-TestArtifact {
            param(
                [string[]] $ArgumentList = @()
            )

            Invoke-SkillExecutable -Name 'New-FindingArtifact.ps1' -ArgumentList (@(
                    '-OutputDirectory', $script:FindingsDirectory
                    '-ReviewerId', 'g1-correctness'
                    '-FocusArea', 'correctness'
                    '-FileGroupQueuePath', $script:FileGroupQueuePath
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

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'reviewerId') | Should -BeExactly 'g1-correctness'
        (Get-JsonProperty -Node $emitted -Name 'artifactPath') | Should -BeExactly $script:ArtifactPath
        Test-Path -LiteralPath $script:ArtifactPath -PathType Leaf | Should -BeTrue
        (Split-Path -Parent $script:ArtifactPath) | Should -BeExactly $script:FindingsDirectory
    }

    # A subagent that creates its artifact a second time would otherwise discard everything it had
    # already recorded, with nothing in the output to show that it had.
    It 'refuses to replace an artifact that already exists' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null

        $result = Initialize-TestArtifact

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*-Force*'

        $stored = Read-FindingArtifact -Path $script:ArtifactPath
        (Get-JsonArrayProperty -Node $stored -Name 'findings').Count | Should -Be 1
    }

    It 'replaces an artifact when forced' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null

        $result = Initialize-TestArtifact -ArgumentList @('-Force')

        $result['exitCode'] | Should -Be 0
        $stored = Read-FindingArtifact -Path $script:ArtifactPath
        (Get-JsonArrayProperty -Node $stored -Name 'findings').Count | Should -Be 0
    }

    It 'resumes without discarding findings and continues assigning distinct identifiers' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null
        $before = Read-FindingArtifact -Path $script:ArtifactPath
        $result = Initialize-TestArtifact -ArgumentList @('-Resume')
        $result.exitCode | Should -Be 0
        (ConvertFrom-JsonVerbatim -Text $result.standardOutput).findingCount | Should -Be 1
        (ConvertFrom-JsonVerbatim -Text $result.standardOutput).resumedExisting | Should -BeTrue
        $after = Read-FindingArtifact -Path $script:ArtifactPath
        (ConvertTo-JsonText -Value $after.findings) | Should -BeExactly (ConvertTo-JsonText -Value $before.findings)
        $added = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'replayed-finding.json')
        )
        $added.exitCode | Should -Be 0
        (ConvertFrom-JsonVerbatim -Text $added.standardOutput).findingId | Should -Be 'g1-correctness-2'
        (Read-FindingArtifact -Path $script:ArtifactPath).findings.Count | Should -Be 2
    }

    It 'creates missing initial state on resume and reopens a completed artifact without data loss' {
        (Initialize-TestArtifact -ArgumentList @('-Resume')).exitCode | Should -Be 0
        $document = Read-FindingArtifact -Path $script:ArtifactPath
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin reviewer | Out-Null
        Complete-FindingArtifactDocument -Document $document
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
        (Initialize-TestArtifact -ArgumentList @('-Resume')).exitCode | Should -Be 0
        $resumed = Read-FindingArtifact -Path $script:ArtifactPath
        $resumed.status | Should -Be 'open'
        $resumed.findings.Count | Should -Be 1
    }

    It 'refuses mismatched or malformed resume artifacts without overwriting them' {
        Initialize-TestArtifact | Out-Null
        $document = Read-FindingArtifact -Path $script:ArtifactPath
        $document.focusArea = 'another-focus-area'
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
        $before = [IO.File]::ReadAllText($script:ArtifactPath)
        (Initialize-TestArtifact -ArgumentList @('-Resume')).exitCode | Should -Not -Be 0
        [IO.File]::ReadAllText($script:ArtifactPath) | Should -BeExactly $before
        [IO.File]::WriteAllText($script:ArtifactPath, '{broken')
        (Initialize-TestArtifact -ArgumentList @('-Resume')).exitCode | Should -Not -Be 0
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
        $result = Initialize-TestArtifact -ArgumentList @('-Resume')
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'missing despite journaled work'
        Test-Path -LiteralPath $script:ArtifactPath | Should -BeFalse
    }

    It 'refuses a missing journal alongside recorded findings' {
        Initialize-TestArtifact | Out-Null
        $document = Read-FindingArtifact -Path $script:ArtifactPath
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding) -Origin reviewer | Out-Null
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
        Remove-Item -LiteralPath $script:FileGroupQueuePath
        $result = Initialize-TestArtifact -ArgumentList @('-Resume')
        $result.exitCode | Should -Not -Be 0
        $result.standardError | Should -Match 'missing despite recorded findings'
        (Read-FindingArtifact -Path $script:ArtifactPath).findings.Count | Should -Be 1
    }
    It 'appends a finding and reports its identifier' {
        Initialize-TestArtifact | Out-Null

        $first = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'first.json' -Override @{ title = 'First' })
        )
        $second = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'second.json' -Override @{ title = 'Second' })
        )

        $first['exitCode'] | Should -Be 0
        $second['exitCode'] | Should -Be 0

        $firstEmitted = ConvertFrom-JsonVerbatim -Text $first['standardOutput']
        (Get-JsonProperty -Node $firstEmitted -Name 'findingId') | Should -BeExactly 'g1-correctness-1'
        Test-JsonProperty -Node $firstEmitted -Name 'index' | Should -BeFalse

        $secondEmitted = ConvertFrom-JsonVerbatim -Text $second['standardOutput']
        (Get-JsonProperty -Node $secondEmitted -Name 'findingId') | Should -BeExactly 'g1-correctness-2'
        Test-JsonProperty -Node $secondEmitted -Name 'index' | Should -BeFalse
    }

    It 'refuses a finding and leaves the artifact untouched' {
        Initialize-TestArtifact | Out-Null
        $path = Join-Path $script:WorkRoot 'invalid.json'
        Write-TestJsonFile -Path $path -Value (New-TestFinding -Override @{ priority = 'high' }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', $path
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'priority'*"
        (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings').Count | Should -Be 0
    }

    It 'names a malformed finding file when appending' {
        Initialize-TestArtifact | Out-Null
        $path = Join-Path $script:WorkRoot 'malformed.json'
        Set-Content -LiteralPath $path -Value '{ "title": "broken", }' -NoNewline -Encoding utf8NoBOM

        $result = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', $path
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*$path*"
        (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings').Count | Should -Be 0
    }

    It 'checks a follow-up against the captured discussion' {
        Initialize-TestArtifact | Out-Null

        $capturePath = Join-Path $script:WorkRoot 'pr-context.json'
        Write-TestJsonFile -Path $capturePath -Value (New-CaptureDocument -Platform 'gitHub' -Thread @(
                New-CaptureThread -Id 'RT_kw1' -Kind 'text' -OpeningCommentId 'PRRC_1' -ThreadContext (New-CaptureThreadContext)
            )) | Out-Null

        $accepted = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'good.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } })
            '-ContextPath', $capturePath
        )
        $refused = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'bad.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_typo'; commentId = 'PRRC_1' } })
            '-ContextPath', $capturePath
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

        $result = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'follow-up.json' -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } })
            '-ContextPath', $capturePath
        )

        $result['exitCode'] | Should -Not -Be 0
        (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings').Count | Should -Be 0
    }

    It 'records a finding produced by synthesis with its provenance' {
        Initialize-TestArtifact | Out-Null

        $result = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'synthesis.json')
            '-Origin', 'synthesis'
            '-SourceFindingId', 'g2-1,g3-4'
        )

        $result['exitCode'] | Should -Be 0
        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'origin') | Should -BeExactly 'synthesis'
        $sourceFindingIds = Get-JsonArrayProperty -Node $stored -Name 'sourceFindingIds'
        $sourceFindingIds | Should -Be @('g2-1', 'g3-4')
    }

    It 'completes an artifact and reports what it holds' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Complete-FindingArtifact.ps1' -ArgumentList @('-ArtifactPath', $script:ArtifactPath, '-FileGroupQueuePath', $script:FileGroupQueuePath)

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'status') | Should -BeExactly 'complete'
        (Get-JsonProperty -Node $emitted -Name 'findingCount') | Should -Be 1
        (Get-JsonProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'status') | Should -BeExactly 'complete'
    }

    It 'rewrites one finding' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Update-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingId', 'g1-correctness-1'
            '-FindingPath', (New-TestFindingFile -Name 'rewritten.json' -Override @{ title = 'Rewritten' })
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $updated = Get-JsonArrayProperty -Node $emitted -Name 'updated'
        $updated | Should -Be @('g1-correctness-1')

        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly 'Rewritten'
        (Get-JsonProperty -Node $stored -Name 'id') | Should -BeExactly 'g1-correctness-1'
    }

    It 'rewrites a batch named by a plan' {
        Initialize-TestArtifact | Out-Null
        foreach ($name in @('first', 'second')) {
            Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
                '-ArtifactPath', $script:ArtifactPath
                '-FindingPath', (New-TestFindingFile -Name "$name.json" -Override @{ title = $name })
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

        $result = Invoke-SkillExecutable -Name 'Update-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-PlanPath', $planPath
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $updated = Get-JsonArrayProperty -Node $emitted -Name 'updated'
        $updated | Should -Be @('g1-correctness-1', 'g1-correctness-2')

        $findings = Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings'
        @($findings | ForEach-Object { Get-JsonProperty -Node $_ -Name 'title' }) |
            Should -Be @('First rewritten', 'Second rewritten')
    }

    It 'names a malformed finding file from an update plan' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json' -Override @{ title = 'Original' })
        ) | Out-Null

        $badFindingPath = Join-Path $script:WorkRoot 'bad-rewrite.json'
        Set-Content -LiteralPath $badFindingPath -Value '{ "title": "broken", }' -NoNewline -Encoding utf8NoBOM

        $planPath = Join-Path $script:WorkRoot 'plan.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; updates = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; findingPath = 'bad-rewrite.json' }
                )
            }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Update-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-PlanPath', $planPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*$badFindingPath*"
        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly 'Original'
    }

    It 'leaves the artifact untouched when a batch names a finding it does not hold' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json' -Override @{ title = 'Original' })
        ) | Out-Null
        New-TestFindingFile -Name 'rewritten.json' -Override @{ title = 'Rewritten' } | Out-Null

        $planPath = Join-Path $script:WorkRoot 'plan.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; updates = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; findingPath = 'rewritten.json' }
                    [ordered]@{ findingId = 'g1-correctness-9'; findingPath = 'rewritten.json' }
                )
            }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Update-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-PlanPath', $planPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'title') | Should -BeExactly 'Original'
    }

    It 'discards a finding with a reason and a moment from a read-only working directory' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'finding.json')
        ) | Out-Null

        $directory = Join-Path $script:WorkRoot 'read-only-working-directory'
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        if (-not $IsWindows) {
            $originalMode = [IO.File]::GetUnixFileMode($directory)
            [IO.File]::SetUnixFileMode($directory, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserExecute)
        }
        try {
            $result = Invoke-SkillExecutable -Name 'Remove-Finding.ps1' -WorkingDirectory $directory -ArgumentList @(
                '-ArtifactPath', $script:ArtifactPath
                '-FindingId', 'g1-correctness-1'
                '-Reason', 'Duplicates a finding from another reviewer.'
                '-NowUtc', '2024-05-06T07:08:09Z'
            )
        } finally {
            if (-not $IsWindows) { [IO.File]::SetUnixFileMode($directory, $originalMode) }
        }

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $removedIds = Get-JsonArrayProperty -Node $emitted -Name 'removed'
        $removedIds | Should -Be @('g1-correctness-1')

        $stored = Read-FindingArtifact -Path $script:ArtifactPath
        (Get-JsonArrayProperty -Node $stored -Name 'findings').Count | Should -Be 0
        $removed = (Get-JsonArrayProperty -Node $stored -Name 'removed')[0]
        (Get-JsonProperty -Node $removed -Name 'reason') | Should -BeExactly 'Duplicates a finding from another reviewer.'
        (Get-JsonProperty -Node $removed -Name 'removedUtc') | Should -BeExactly '2024-05-06T07:08:09.0000000Z'
    }

    It 'discards a batch named by a plan' {
        Initialize-TestArtifact | Out-Null
        foreach ($name in @('first', 'second')) {
            Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
                '-ArtifactPath', $script:ArtifactPath
                '-FindingPath', (New-TestFindingFile -Name "$name.json" -Override @{ title = $name })
            ) | Out-Null
        }

        $planPath = Join-Path $script:WorkRoot 'removals.json'
        Write-TestJsonFile -Path $planPath -Value ([ordered]@{
                schemaVersion = 1; removals = @(
                    [ordered]@{ findingId = 'g1-correctness-1'; reason = 'Out of scope.' }
                    [ordered]@{ findingId = 'g1-correctness-2'; reason = 'Already reported.' }
                )
            }) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Remove-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-PlanPath', $planPath
        )

        $result['exitCode'] | Should -Be 0
        $stored = Read-FindingArtifact -Path $script:ArtifactPath
        (Get-JsonArrayProperty -Node $stored -Name 'findings').Count | Should -Be 0
        (Get-JsonArrayProperty -Node $stored -Name 'removed').Count | Should -Be 2
    }

    It 'reports a valid artifact set and exits successfully' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Complete-FindingArtifact.ps1' -ArgumentList @('-ArtifactPath', $script:ArtifactPath, '-FileGroupQueuePath', $script:FileGroupQueuePath) | Out-Null

        $result = Invoke-SkillExecutable -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:FindingsDirectory
            '-ExpectedReviewerId', 'g1-correctness'
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'isValid') | Should -BeTrue
    }

    # A session-state directory is not sealed, so the gate has to fail on a directory it cannot read
    # as a complete set rather than report a count that looks plausible.
    It 'exits unsuccessfully on a findings directory holding a foreign entry' {
        Initialize-TestArtifact | Out-Null
        Invoke-SkillExecutable -Name 'Complete-FindingArtifact.ps1' -ArgumentList @('-ArtifactPath', $script:ArtifactPath, '-FileGroupQueuePath', $script:FileGroupQueuePath) | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:FindingsDirectory 'staging') -Force | Out-Null

        $result = Invoke-SkillExecutable -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:FindingsDirectory
            '-ExpectedReviewerId', 'g1-correctness'
        )

        $result['exitCode'] | Should -Not -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'isValid') | Should -BeFalse
        $errors = Get-JsonArrayProperty -Node $emitted -Name 'errors'
        ($errors -join ' ') | Should -BeLike "*'staging'*"
    }

    # The review uses this as a gate, so an invalid set has to fail the command rather than report a
    # status the caller might read past.
    It 'exits unsuccessfully on an invalid artifact set' {
        Initialize-TestArtifact | Out-Null

        $result = Invoke-SkillExecutable -Name 'Test-FindingArtifact.ps1' -ArgumentList @(
            '-Path', $script:FindingsDirectory
            '-ExpectedReviewerId', 'g1-correctness,g1-security'
        )

        $result['exitCode'] | Should -Not -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'isValid') | Should -BeFalse
        $missingReviewers = Get-JsonArrayProperty -Node $emitted -Name 'missingReviewers'
        $missingReviewers | Should -Be @('g1-security')
        $incompleteReviewers = Get-JsonArrayProperty -Node $emitted -Name 'incompleteReviewers'
        $incompleteReviewers | Should -Be @('g1-correctness')
    }

    It 'carries exotic content through the executables unchanged' {
        Initialize-TestArtifact | Out-Null

        Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', (New-TestFindingFile -Name 'exotic.json' -Override @{ description = $script:ExoticContent })
        ) | Out-Null

        $stored = (Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings')[0]
        (Get-JsonProperty -Node $stored -Name 'description') | Should -BeExactly $script:ExoticContent
    }
}
