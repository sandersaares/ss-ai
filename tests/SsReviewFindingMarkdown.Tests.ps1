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
    . (Join-Path $script:LibraryRoot 'FindingMarkdown.ps1')
    . (Join-Path $script:LibraryRoot 'FocusAreaCacheDiagnostics.ps1')

    # The host running this suite stands in for the executables it launches. Naming the running
    # executable rather than the command 'pwsh' keeps the child on the same build as the parent and
    # removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # Builds a stored finding record, as an artifact holds one.
    #
    # Overrides are applied onto a minimal record, so a test states only the field it is about and
    # every other field remains present; a test asserting an omission then cannot pass because of
    # some unrelated absence.
    function New-TestRecord {
        param(
            [System.Collections.IDictionary] $Override = @{},

            [string[]] $Remove = @()
        )

        $record = [ordered]@{
            schemaVersion     = 1
            id                = 'F1'
            sourceFindingIds  = @('g1-1')
            origin            = 'reviewer'
            title             = 'Terminate the retry loop'
            description       = 'The loop condition is never updated.'
            reasoning         = 'The counter is incremented in a branch that cannot be reached.'
            consequence       = 'A transient failure hangs the caller indefinitely.'
            recommendedAction = 'Increment the counter outside the branch.'
            references        = @()
            impactedLocations = @()
            anchor            = [ordered]@{ kind = 'line'; path = 'src/retry.rs'; startLine = 42; endLine = 42; side = 'right' }
        }

        foreach ($name in $Remove) { $record.Remove($name) }
        foreach ($key in $Override.Keys) { $record[[string] $key] = $Override[$key] }

        $record
    }

    # Builds a finding as an author writes it, carrying no identifier and no provenance.
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

Describe 'Finding rendering' {
    # Pins the whole body rather than a fragment of it. Every other assertion here names one section
    # at a time, so a change to the blocks between them, to their order or to the blank lines that
    # separate them would pass unnoticed while altering the text a reviewer approved and the text a
    # reader receives.
    It 'renders a fully populated finding as exactly this text' {
        $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                references        = @(
                    [ordered]@{ text = 'docs/design.md, "Retry policy"' }
                    [ordered]@{ text = 'The retry RFC'; url = 'https://example.invalid/rfc' }
                )
                impactedLocations = @(
                    [ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' }
                    [ordered]@{ path = 'src/legacy.rs' }
                )
            })

        $expected = @(
            '[Copilot speaking]'
            ''
            '# Terminate the retry loop'
            ''
            'The loop condition is never updated.'
            ''
            '**Reproducible reasoning**:'
            ''
            'The counter is incremented in a branch that cannot be reached.'
            ''
            '**Consequence**:'
            ''
            'A transient failure hangs the caller indefinitely.'
            ''
            '**Recommended action**:'
            ''
            'Increment the counter outside the branch.'
            ''
            '**References**:'
            ''
            '- docs/design.md, "Retry policy"'
            '- [The retry RFC](https://example.invalid/rfc)'
            ''
            '**Impacted locations**:'
            ''
            '- `src/retry.rs:40-44`'
            '- `src/legacy.rs`'
        ) -join "`n"

        $text | Should -BeExactly $expected
    }

    It 'begins every body with the authorship prefix' {
        $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord)

        $text.Split("`n")[0] | Should -BeExactly '[Copilot speaking]'
    }

    It 'renders the same input the same way' {
        $finding = New-TestRecord -Override @{
            references        = @([ordered]@{ text = 'docs/design.md'; url = 'https://example.invalid/design' })
            impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' })
        }

        (ConvertTo-FindingMarkdownText -Finding $finding) |
            Should -BeExactly (ConvertTo-FindingMarkdownText -Finding $finding)
    }

    It 'renders the sections in a fixed order under their labels' {
        $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                references        = @([ordered]@{ text = 'docs/design.md' })
                impactedLocations = @([ordered]@{ path = 'src/retry.rs' })
            })

        $positions = @(
            $text.IndexOf('# Terminate the retry loop')
            $text.IndexOf('The loop condition is never updated.')
            $text.IndexOf('**Reproducible reasoning**:')
            $text.IndexOf('**Consequence**:')
            $text.IndexOf('**Recommended action**:')
            $text.IndexOf('**References**:')
            $text.IndexOf('**Impacted locations**:')
        )

        foreach ($position in $positions) { $position | Should -BeGreaterThan -1 }
        $sorted = @($positions | Sort-Object)
        ($positions -join ',') | Should -BeExactly ($sorted -join ',')
    }

    It 'omits an optional section that has no content' {
        $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord)

        $text | Should -Not -BeLike '*References*'
        $text | Should -Not -BeLike '*Impacted locations*'
    }

    # Check parsed block structure as well as text fidelity: an inline label can preserve every
    # character while turning the first list item or code fence into ordinary paragraph text.
    It 'renders <name> as blocks in every prose field and destination' -ForEach @(
        @{
            name = 'paragraphs'
            content = "First paragraph.`n`nSecond paragraph."
            html = '<p>First paragraph\.</p>\s*<p>Second paragraph\.</p>'
        }
        @{
            name = 'an unordered list'
            content = "- First item`n- Second item"
            html = '<ul>\s*<li>First item</li>\s*<li>Second item</li>\s*</ul>'
        }
        @{
            name = 'an ordered list'
            content = "1. First step`n2. Second step"
            html = '<ol>\s*<li>First step</li>\s*<li>Second step</li>\s*</ol>'
        }
        @{
            name = 'a fenced code block'
            content = @('```text', 'First line', 'Second line', '```') -join "`n"
            html = '<pre><code class="language-text">First line\s+Second line\s*</code></pre>'
        }
        @{
            name = 'a block quote'
            content = "> First line`n>`n> Second line"
            html = '<blockquote>\s*<p>First line</p>\s*<p>Second line</p>\s*</blockquote>'
        }
        @{
            name = 'a table'
            content = "| Input | Result |`n| --- | --- |`n| Timeout | Retry |"
            html = '<table>[\s\S]*<th>Input</th>[\s\S]*<td>Retry</td>[\s\S]*</table>'
        }
        @{
            name = 'a single-line list'
            content = '- Only item'
            html = '<ul>\s*<li>Only item</li>\s*</ul>'
        }
    ) {
        foreach ($field in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
            $finding = New-TestRecord -Override @{ $field = $content }
            foreach ($destination in @('pullRequest', 'tracker')) {
                $blocks = Get-FindingMarkdownBlock -Finding $finding -Destination $destination
                $section = @($script:FindingMarkdownProseSection | Where-Object { $_.field -ceq $field })
                $expectedBlock = if ($field -ceq 'description') {
                    $content
                } else {
                    "**$($section[0].label)**:`n`n$content"
                }

                $blocks | Should -Contain $expectedBlock -Because "$destination/$field preserves authored Markdown"
                $rendered = ConvertFrom-Markdown -InputObject ($blocks -join "`n`n")
                $rendered.Html | Should -Match $html -Because "$destination/$field retains its block structure"
            }
        }
    }

    It 'omits a section whose prose is absent or empty' {
        foreach ($name in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
            $absent = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Remove @($name))
            $empty = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{ $name = '   ' })
            $present = ConvertTo-FindingMarkdownText -Finding (New-TestRecord)

            $absent | Should -BeExactly $empty -Because $name
            $absent.Length | Should -BeLessThan $present.Length -Because $name
        }
    }

    # A follow-up names where the reply is posted, which the publication path acts on. Stating it in
    # the body as well would show the reader an identifier that means nothing to them.
    It 'renders nothing for a follow-up target' {
        $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' }
            })

        $text | Should -BeExactly (ConvertTo-FindingMarkdownText -Finding (New-TestRecord))
    }

    Context 'references' {
        It 'renders a reference carrying a URL as a link' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    references = @([ordered]@{ text = 'The retry RFC'; url = 'https://example.invalid/rfc' })
                })

            $text.Split("`n") | Should -Contain '- [The retry RFC](https://example.invalid/rfc)'
        }

        It 'renders a reference without a URL as plain text' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    references = @([ordered]@{ text = 'docs/design.md, "Retry policy"' })
                })

            $text | Should -BeLike '*- docs/design.md, "Retry policy"*'
            $text | Should -Not -BeLike '*](*'
        }

        It 'renders every reference in the order the finding lists them' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    references = @(
                        [ordered]@{ text = 'first' }
                        [ordered]@{ text = 'second'; url = 'https://example.invalid/second' }
                    )
                })

            $text.IndexOf('- first') | Should -BeLessThan $text.IndexOf('- [second]')
        }
    }

    Context 'impacted locations' {
        It 'renders a single line as the path and that line' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 42; endLine = 42; side = 'right' })
                })

            $text.Split("`n") | Should -Contain '- `src/retry.rs:42`'
        }

        It 'renders a range as the path and both lines' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' })
                })

            $text.Split("`n") | Should -Contain '- `src/retry.rs:40-44`'
        }

        It 'renders a location naming no line as the bare path' {
            $text = ConvertTo-FindingMarkdownText -Finding (New-TestRecord -Override @{
                    impactedLocations = @([ordered]@{ path = 'src/legacy.rs' })
                })

            $text.Split("`n") | Should -Contain '- `src/legacy.rs`'
            $text.Contains('src/legacy.rs:') | Should -BeFalse
        }
    }
}

Describe 'Tracker item rendering' {
    # Pins the whole item rather than a fragment of it, for the same reason the comment body is
    # pinned: every other assertion here names one block at a time, so a change to the blocks
    # between them, to their order or to the blank lines that separate them would pass unnoticed
    # while altering what a user approved and what a tracker stores.
    It 'renders a fully populated finding as exactly this title and this body' {
        $item = ConvertTo-FindingTrackerItem `
            -Finding (New-TestRecord -Override @{
                    references        = @(
                        [ordered]@{ text = 'docs/design.md, "Retry policy"' }
                        [ordered]@{ text = 'The retry RFC'; url = 'https://example.invalid/rfc' }
                    )
                    impactedLocations = @(
                        [ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' }
                        [ordered]@{ path = 'src/legacy.rs' }
                    )
                }) `
            -FollowUpUrl 'https://example.invalid/pull/7#discussion_r1' `
            -PullRequestUrl 'https://example.invalid/pull/7'

        $expected = @(
            '[Copilot speaking]'
            ''
            'The loop condition is never updated.'
            ''
            '**Location**: `src/retry.rs:42`'
            ''
            '**Reproducible reasoning**:'
            ''
            'The counter is incremented in a branch that cannot be reached.'
            ''
            '**Consequence**:'
            ''
            'A transient failure hangs the caller indefinitely.'
            ''
            '**Recommended action**:'
            ''
            'Increment the counter outside the branch.'
            ''
            '**References**:'
            ''
            '- docs/design.md, "Retry policy"'
            '- [The retry RFC](https://example.invalid/rfc)'
            ''
            '**Impacted locations**:'
            ''
            '- `src/retry.rs:40-44`'
            '- `src/legacy.rs`'
            ''
            '**Follows up on**: https://example.invalid/pull/7#discussion_r1'
            ''
            '**Reviewed in**: https://example.invalid/pull/7'
        ) -join "`n"

        @($item.Keys) | Should -Be @('title', 'body')
        $item['title'] | Should -BeExactly 'Terminate the retry loop'
        $item['body'] | Should -BeExactly $expected
    }

    # A tracker holds the title in a field of its own, so a body opening with a heading would show a
    # reader the same line twice.
    It 'carries the title as a field rather than as a heading' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord)

        $item['title'] | Should -BeExactly 'Terminate the retry loop'
        $item['body'].Contains('# Terminate the retry loop') | Should -BeFalse
        $item['body'].Split("`n")[0] | Should -BeExactly '[Copilot speaking]'
    }

    # The item hangs off nothing, so a reader who is not looking at a diff is told where the code is
    # before being told why it matters.
    It 'states the location directly after the description' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord)

        $body = [string] $item['body']
        $body.IndexOf('**Location**: `src/retry.rs:42`') |
            Should -BeGreaterThan ($body.IndexOf('The loop condition is never updated.'))
        $body.IndexOf('**Location**') | Should -BeLessThan ($body.IndexOf('**Reproducible reasoning**'))
    }

    It 'renders a location covering a range as both lines' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord -Override @{
                anchor = [ordered]@{ kind = 'range'; path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' }
            })

        $item['body'].Split("`n") | Should -Contain '**Location**: `src/retry.rs:40-44`'
    }

    It 'renders a location naming no line as the bare path' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord -Override @{
                anchor = [ordered]@{ kind = 'file'; path = 'src/legacy.rs'; side = 'right' }
            })

        $item['body'].Split("`n") | Should -Contain '**Location**: `src/legacy.rs`'
    }

    It 'states no location for a finding carrying no anchor' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord -Remove @('anchor'))

        $item['body'].Contains('**Location**') | Should -BeFalse
    }

    # A tracker item outlives the review that produced it, and a reader meeting it later has no
    # other way back to the discussion.
    It 'closes with the links back to the review' {
        $item = ConvertTo-FindingTrackerItem `
            -Finding (New-TestRecord) `
            -FollowUpUrl 'https://example.invalid/pull/7#discussion_r1' `
            -PullRequestUrl 'https://example.invalid/pull/7'

        $lines = $item['body'].Split("`n")
        $lines[$lines.Count - 3] | Should -BeExactly '**Follows up on**: https://example.invalid/pull/7#discussion_r1'
        $lines[$lines.Count - 1] | Should -BeExactly '**Reviewed in**: https://example.invalid/pull/7'
    }

    It 'states only the pull request for a finding that answers nothing' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord) -PullRequestUrl 'https://example.invalid/pull/7'

        $item['body'].Contains('**Follows up on**') | Should -BeFalse
        $item['body'].Split("`n") | Should -Contain '**Reviewed in**: https://example.invalid/pull/7'
    }

    # A review of content that is not a pull request has neither link, and its absence costs a link
    # rather than the item.
    It 'states neither link for a caller that supplied neither' {
        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord)

        $item['body'].Contains('**Follows up on**') | Should -BeFalse
        $item['body'].Contains('**Reviewed in**') | Should -BeFalse
    }

    It 'renders the same input the same way' {
        $finding = New-TestRecord -Override @{
            references        = @([ordered]@{ text = 'docs/design.md'; url = 'https://example.invalid/design' })
            impactedLocations = @([ordered]@{ path = 'src/retry.rs'; startLine = 40; endLine = 44; side = 'right' })
        }

        $first = ConvertTo-FindingTrackerItem -Finding $finding -PullRequestUrl 'https://example.invalid/pull/7'
        $second = ConvertTo-FindingTrackerItem -Finding $finding -PullRequestUrl 'https://example.invalid/pull/7'

        $second['title'] | Should -BeExactly ([string] $first['title'])
        $second['body'] | Should -BeExactly ([string] $first['body'])
    }

    # The prose is emitted as the author wrote it, so the item a tracker receives holds the same code
    # points the reviewer approved rather than a transliteration of them.
    It 'carries text outside ASCII through unchanged' {
        $emDash = [string] [char]0x2014
        $robot = [char]::ConvertFromUtf32(0x1F916)
        $leftQuote = [string] [char]0x201C
        $rightQuote = [string] [char]0x201D
        $description = "The retry loop $emDash the one $leftQuote fast path $rightQuote $robot $emDash never terminates."

        $item = ConvertTo-FindingTrackerItem -Finding (New-TestRecord -Override @{
                title       = "Terminate the retry loop $robot"
                description = $description
            })

        $item['title'] | Should -BeExactly "Terminate the retry loop $robot"
        $item['body'].Contains($description) | Should -BeTrue
        $item['body'].Split("`n") | Should -Contain $description
    }

    # Both published forms are built from one block builder, so no destination can restate a finding
    # differently. What differs is where the title goes and which blocks surround the prose.
    It 'emits the same prose as a comment body does' {
        $finding = New-TestRecord -Override @{
            references        = @([ordered]@{ text = 'docs/design.md' })
            impactedLocations = @([ordered]@{ path = 'src/retry.rs' })
        }

        $item = ConvertTo-FindingTrackerItem -Finding $finding
        $comment = ConvertTo-FindingBodyMarkdown -Finding $finding

        foreach ($section in $script:FindingMarkdownProseSection) {
            $block = "**$($section.label)**:`n`n$($finding[$section.field])"
            $comment.Contains($block, [StringComparison]::Ordinal) | Should -BeTrue
            $item['body'].Contains($block, [StringComparison]::Ordinal) | Should -BeTrue
        }
    }
}

Describe 'Tracker item blocks' {
    It 'omits the heading a comment body opens with' {
        $blocks = Get-FindingMarkdownBlock -Finding (New-TestRecord) -Destination 'tracker'

        $blocks[0] | Should -BeExactly 'The loop condition is never updated.'
        @($blocks | Where-Object { $_.StartsWith('# ', [StringComparison]::Ordinal) }).Count | Should -Be 0
    }

    It 'states neither the location nor the provenance for a comment body' {
        $blocks = Get-FindingMarkdownBlock `
            -Finding (New-TestRecord) `
            -Destination 'pullRequest' `
            -FollowUpUrl 'https://example.invalid/pull/7#discussion_r1' `
            -PullRequestUrl 'https://example.invalid/pull/7'

        @($blocks | Where-Object { $_.Contains('**Location**') }).Count | Should -Be 0
        @($blocks | Where-Object { $_.Contains('**Follows up on**') }).Count | Should -Be 0
        @($blocks | Where-Object { $_.Contains('**Reviewed in**') }).Count | Should -Be 0
    }

    # The separator is applied in one place, so a document collecting comment bodies and a document
    # collecting tracker items divide their findings the same way.
    It 'divides findings by a rule surrounded by blank lines' {
        Join-FindingMarkdownDocument -Text @('first', 'second') |
            Should -BeExactly ("first`n`n---`n`nsecond")
    }

    It 'renders a single finding as itself' {
        Join-FindingMarkdownDocument -Text @('only') | Should -BeExactly 'only'
    }

    It 'renders an empty set as an empty document' {
        Join-FindingMarkdownDocument -Text @() | Should -BeExactly ''
    }
}

Describe 'Combined document' {
    It 'presents synthesis findings ahead of reviewer findings' {
        $findings = @(
            New-TestRecord -Override @{ id = 'F1'; origin = 'reviewer'; title = 'Reviewer one' }
            New-TestRecord -Override @{ id = 'F2'; origin = 'synthesis'; title = 'Synthesis one' }
            New-TestRecord -Override @{ id = 'F3'; origin = 'reviewer'; title = 'Reviewer two' }
            New-TestRecord -Override @{ id = 'F4'; origin = 'synthesis'; title = 'Synthesis two' }
        )

        $document = ConvertTo-FindingMarkdownDocument -Finding $findings

        $positions = @('Synthesis one', 'Synthesis two', 'Reviewer one', 'Reviewer two' | ForEach-Object { $document.IndexOf("# $_") })
        foreach ($position in $positions) { $position | Should -BeGreaterThan -1 }
        $sorted = @($positions | Sort-Object)
        ($positions -join ',') | Should -BeExactly ($sorted -join ',')
    }

    It 'carries every finding in the form it would be published in' {
        $findings = @(
            New-TestRecord -Override @{ id = 'F1'; title = 'First' }
            New-TestRecord -Override @{ id = 'F2'; title = 'Second' }
        )

        $document = ConvertTo-FindingMarkdownDocument -Finding $findings

        foreach ($finding in $findings) {
            $document.Contains((ConvertTo-FindingBodyMarkdown -Finding $finding)) | Should -BeTrue
        }
    }

    It 'states its author once rather than once per finding' {
        $findings = @(
            New-TestRecord -Override @{ id = 'F1'; title = 'First' }
            New-TestRecord -Override @{ id = 'F2'; title = 'Second' }
        )

        $document = ConvertTo-FindingMarkdownDocument -Finding $findings

        $document | Should -BeLike '[[]Copilot speaking[]]*'
        ([regex]::Matches($document, [regex]::Escape('[Copilot speaking]'))).Count | Should -Be 1
    }

    It 'renders an empty set as an empty document' {
        ConvertTo-FindingMarkdownDocument -Finding @() | Should -BeExactly ''
    }
}

Describe 'Review summary' {
    It 'states how much was published and how much followed up' {
        $summary = New-ReviewSummaryMarkdown `
            -Finding @((New-TestRecord), (New-TestRecord), (New-TestRecord)) `
            -FollowUpCount 2 `
            -Diagnostics @{ cache = 'Hit' } `
            -Platform 'gitHub'

        $summary | Should -BeLike '*Published 3 findings.*'
        $summary | Should -BeLike '*2 findings follow up on existing discussion threads.*'
    }

    It 'states a single finding and a single follow-up in the singular' {
        $summary = New-ReviewSummaryMarkdown `
            -Finding @(New-TestRecord) `
            -FollowUpCount 1 `
            -Diagnostics @{ cache = 'Hit' } `
            -Platform 'gitHub'

        $summary | Should -BeLike '*Published 1 finding.*'
        $summary | Should -BeLike '*One finding follows up on an existing discussion thread.*'
    }

    It 'states plainly that nothing followed up' {
        $summary = New-ReviewSummaryMarkdown `
            -Finding @((New-TestRecord), (New-TestRecord)) `
            -FollowUpCount 0 `
            -Diagnostics @{ cache = 'Hit' } `
            -Platform 'gitHub'

        $summary | Should -BeLike '*No finding follows up on an existing discussion thread.*'
    }

    It 'begins with the authorship prefix' {
        $summary = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform 'gitHub'

        $summary.Split("`n")[0] | Should -BeExactly '[Copilot speaking]'
    }

    It 'states that a clean review raises no findings' {
        $summary = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform 'gitHub'

        $summary | Should -BeLike '*This review raises no findings.*'
        $summary | Should -Not -BeLike '*Published 0*'
    }

    # The summary points at the findings; each one is published in full where the code is, and a
    # summary restating them would give the reader two accounts to reconcile.
    It 'says nothing about what the findings contain' {
        $summary = New-ReviewSummaryMarkdown `
            -Finding @(New-TestRecord) `
            -Diagnostics @{ cache = 'Hit' } `
            -Platform 'gitHub'

        $summary | Should -Not -BeLike '*Terminate the retry loop*'
        $summary | Should -Not -BeLike '*src/retry.rs*'
    }

    Context 'diagnostics' {
        It 'collapses the table on GitHub' {
            $summary = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform 'gitHub'

            $summary | Should -BeLike '*<details><summary>See diagnostics</summary>*'
            $summary | Should -BeLike '*</details>*'
            $summary.IndexOf('| Cache | Hit |') | Should -BeGreaterThan $summary.IndexOf('<details>')
            $summary.IndexOf('| Cache | Hit |') | Should -BeLessThan $summary.IndexOf('</details>')
        }

        # Azure DevOps renders collapsible sections inconsistently, so hiding the table there risks
        # showing the reader raw markup instead of diagnostics.
        It 'inlines the table on Azure DevOps' {
            $summary = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform 'azureDevOps'

            $summary | Should -Not -BeLike '*<details>*'
            $summary | Should -Not -BeLike '*</details>*'
            $summary | Should -BeLike '*| Cache | Hit |*'
        }

        It 'reports the cache outcome on both platforms' {
            foreach ($platform in @('gitHub', 'azureDevOps')) {
                (New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform $platform) |
                    Should -BeLike '*| Cache | Hit |*' -Because $platform
                (New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Miss' } -Platform $platform) |
                    Should -BeLike '*| Cache | Miss |*' -Because $platform
            }
        }

        # A session that recorded no restore reused nothing, so the honest report is a miss.
        It 'reports a miss where the record says nothing about the cache' {
            New-ReviewSummaryMarkdown -Diagnostics @{} -Platform 'gitHub' | Should -BeLike '*| Cache | Miss |*'
        }

        It 'reports degraded instruction expansion only where expansion was degraded' {
            $degraded = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit'; inputState = 'Degraded' } -Platform 'gitHub'
            $complete = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit'; inputState = 'Complete' } -Platform 'gitHub'
            $unstated = New-ReviewSummaryMarkdown -Diagnostics @{ cache = 'Hit' } -Platform 'gitHub'

            $degraded | Should -BeLike '*| Instruction expansion | Degraded |*'
            $complete | Should -Not -BeLike '*Instruction expansion*'
            $unstated | Should -Not -BeLike '*Instruction expansion*'
        }

        It 'reads the record the cache commands write' {
            $sessionState = Join-Path $TestDrive ([guid]::NewGuid())
            Write-FocusAreaCacheDiagnostics -SessionStateDirectory $sessionState -Diagnostics @{
                cache      = 'Hit'
                reason     = 'Fresh'
                inputState = 'Degraded'
                store      = 'Bypassed'
            }

            $summary = New-ReviewSummaryMarkdown `
                -Diagnostics (Get-FocusAreaCacheDiagnostics -SessionStateDirectory $sessionState) `
                -Platform 'gitHub'

            $summary | Should -BeLike '*| Cache | Hit |*'
            $summary | Should -BeLike '*| Instruction expansion | Degraded |*'
        }
    }
}

Describe 'Rendering executable' {
    BeforeEach {
        $script:WorkRoot = Join-Path $TestDrive ([guid]::NewGuid())
        New-Item -ItemType Directory -Path $script:WorkRoot -Force | Out-Null

        $script:ArtifactPath = Join-Path $script:WorkRoot 'consolidated.json'
        $script:MarkdownRoot = Join-Path $script:WorkRoot 'markdown'

        $document = New-FindingArtifactDocument -Kind 'consolidated'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = 'Reviewer finding' }) -Origin 'reviewer' -SourceFindingId @('g1-1') | Out-Null
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = 'Synthesis finding' }) -Origin 'synthesis' -SourceFindingId @('g1-1', 'g2-1') | Out-Null
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
    }

    # Exercise the documented file input rather than a serializer-generated substitute, so JSON
    # escapes and the exact prose survive the real add, rewrite, and document-rendering commands.
    It 'preserves the documented multiline finding through file-based commands' {
        $reviewPath = Join-Path $script:ExecutableRoot '..\review.md'
        $examples = [regex]::Matches([IO.File]::ReadAllText($reviewPath), '(?ms)^```json\r?\n(?<json>.*?)^```')
        $examples.Count | Should -Be 1
        $json = $examples[0].Groups['json'].Value
        $finding = ConvertFrom-JsonVerbatim -Text $json
        $findingPath = Join-Path $script:WorkRoot 'finding input.json'
        Write-PullRequestOutputFile -Path $findingPath -Content $json

        $added = Invoke-SkillExecutable -Name 'Add-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingPath', $findingPath
        )
        $added.exitCode | Should -Be 0 -Because $added.standardError
        $addedId = (ConvertFrom-JsonVerbatim -Text $added.standardOutput).findingId

        $updated = Invoke-SkillExecutable -Name 'Update-Finding.ps1' -ArgumentList @(
            '-ArtifactPath', $script:ArtifactPath
            '-FindingId', 'F1'
            '-FindingPath', $findingPath
        )
        $updated.exitCode | Should -Be 0 -Because $updated.standardError

        $stored = Get-JsonArrayProperty -Node (Read-FindingArtifact -Path $script:ArtifactPath) -Name 'findings'
        $records = @($stored | Where-Object { $_.id -cin @('F1', $addedId) })
        $records.Count | Should -Be 2
        foreach ($record in $records) {
            foreach ($field in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
                $finding[$field].Contains("`n") | Should -BeTrue -Because "$field demonstrates multiline input"
                $record[$field] | Should -BeExactly $finding[$field]
            }
        }

        $combinedPath = Join-Path $script:WorkRoot 'findings.md'
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'
        $rendered = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
            '-AuditDocumentPath', $auditPath
        )
        $rendered.exitCode | Should -Be 0 -Because $rendered.standardError

        foreach ($path in @($combinedPath, $auditPath, (Join-Path $script:MarkdownRoot "$addedId.md"))) {
            $text = [IO.File]::ReadAllText($path)
            foreach ($field in @('description', 'reasoning', 'consequence', 'recommendedAction')) {
                $text.Contains($finding[$field], [StringComparison]::Ordinal) | Should -BeTrue -Because "$path preserves $field"
            }
            $html = (ConvertFrom-Markdown -InputObject $text).Html
            $html | Should -Match '<ol>'
            $html | Should -Match '<ul>'
            $html | Should -Match '<pre><code class="language-text">'
        }
    }

    It 'writes one file per finding, named by identifier' {
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
        )

        $result['exitCode'] | Should -Be 0
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $rendered = Get-JsonArrayProperty -Node $emitted -Name 'findings'
        $rendered.Count | Should -Be 2
        (Get-JsonProperty -Node $emitted -Name 'combinedPath') | Should -BeNullOrEmpty
        (Get-JsonProperty -Node $emitted -Name 'auditDocumentPath') | Should -BeNullOrEmpty
        (Get-JsonProperty -Node $emitted -Name 'auditDocumentUri') | Should -BeNullOrEmpty

        foreach ($findingId in @('F1', 'F2')) {
            $path = Join-Path $script:MarkdownRoot "$findingId.md"
            Test-Path -LiteralPath $path -PathType Leaf | Should -BeTrue -Because $findingId
            [System.IO.File]::ReadAllText($path) | Should -BeLike '`[Copilot speaking`]*'
        }
    }

    It 'writes the combined document when asked for one' {
        $combinedPath = Join-Path $script:WorkRoot 'findings.md'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
        )

        $result['exitCode'] | Should -Be 0
        $combined = [System.IO.File]::ReadAllText($combinedPath)
        $combined | Should -BeLike '*# Reviewer finding*'
        $combined | Should -BeLike '*# Synthesis finding*'
        $combined.IndexOf('# Synthesis finding') | Should -BeLessThan $combined.IndexOf('# Reviewer finding')
    }

    It 'writes the audit document when asked for one, labelling each finding by identifier' {
        $auditDirectory = Join-Path $script:WorkRoot 'audit files (local)'
        $auditPath = Join-Path $auditDirectory 'audit-findings.md'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-AuditDocumentPath', $auditPath
        )

        $result['exitCode'] | Should -Be 0 -Because $result['standardError']
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonProperty -Node $emitted -Name 'auditDocumentPath') | Should -BeExactly $auditPath
        (Get-JsonProperty -Node $emitted -Name 'auditDocumentUri') |
            Should -BeExactly ([uri] $auditPath).AbsoluteUri
        (Get-JsonProperty -Node $emitted -Name 'auditDocumentUri') |
            Should -Match '/audit%20files%20\(local\)/audit-findings\.md$'

        $audit = [System.IO.File]::ReadAllText($auditPath)
        $audit.Contains('**Finding `F1`**', [StringComparison]::Ordinal) | Should -BeTrue
        $audit.Contains('**Finding `F2`**', [StringComparison]::Ordinal) | Should -BeTrue
        $audit | Should -BeLike '*# Reviewer finding*'
        $audit | Should -BeLike '*# Synthesis finding*'
    }

    # The audit document holds only the findings that need a verdict, so it honours -FindingId where
    # the combined deliverable, which is the whole review, does not.
    It 'scopes the audit document to the findings it was told to render' {
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-AuditDocumentPath', $auditPath
            '-FindingId', 'F1'
        )

        $result['exitCode'] | Should -Be 0 -Because $result['standardError']
        $audit = [System.IO.File]::ReadAllText($auditPath)
        $audit.Contains('**Finding `F1`**', [StringComparison]::Ordinal) | Should -BeTrue
        $audit.Contains('**Finding `F2`**', [StringComparison]::Ordinal) | Should -BeFalse
    }

    # The walkthrough consumes emitted paths, including after a rewrite, rather than stale directory
    # entries. Its displayed text must be the same complete destination text as the document audit.
    It 'maps only selected in-session findings to the exact <Destination> audit text' -ForEach @(
        @{ Destination = 'pullRequest' }
        @{ Destination = 'tracker' }
    ) {
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'
        $first = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-Destination', $Destination
        )
        $first.exitCode | Should -Be 0 -Because $first.standardError

        $document = Read-FindingArtifact -Path $script:ArtifactPath
        $document.findings[0].description = "Clarified claim.`n`n- Supporting evidence`n- Further evidence"
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-AuditDocumentPath', $auditPath
            '-FindingId', 'F1'
            '-Destination', $Destination
        )
        $result.exitCode | Should -Be 0 -Because $result.standardError

        $emitted = ConvertFrom-JsonVerbatim -Text $result.standardOutput
        $emitted.findings.Count | Should -Be 1
        $emitted.findings[0].findingId | Should -BeExactly 'F1'
        [IO.Path]::IsPathFullyQualified($emitted.findings[0].path) | Should -BeTrue
        $text = [IO.File]::ReadAllText($emitted.findings[0].path)
        $text.Contains($document.findings[0].description, [StringComparison]::Ordinal) | Should -BeTrue
        [IO.File]::ReadAllText($auditPath) | Should -BeExactly ("**Finding ``F1``**`n`n" + $text)
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F2.md') | Should -BeTrue
    }

    # The overview is for the auditor, so it opens the audit document and is omitted from the
    # bodies publication posts and from the combined deliverable the author receives.
    It 'prepends the content overview to the audit document and nowhere else' {
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'
        $combinedPath = Join-Path $script:WorkRoot 'findings.md'
        $overviewPath = Join-Path $script:WorkRoot 'content-overview.md'
        $overview = @(
            '# Content overview'
            ''
            '## Mission'
            ''
            'The frobnicator turns widgets into gleems.'
        ) -join "`n"
        Write-PullRequestOutputFile -Path $overviewPath -Content $overview

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
            '-AuditDocumentPath', $auditPath
            '-OverviewPath', $overviewPath
        )

        $result['exitCode'] | Should -Be 0 -Because $result['standardError']

        $audit = [System.IO.File]::ReadAllText($auditPath)
        $audit.StartsWith($overview, [StringComparison]::Ordinal) | Should -BeTrue
        $audit.IndexOf($overview, [StringComparison]::Ordinal) |
            Should -BeLessThan $audit.IndexOf('**Finding `F1`**', [StringComparison]::Ordinal)

        $combined = [System.IO.File]::ReadAllText($combinedPath)
        $combined.Contains('The frobnicator turns widgets into gleems.', [StringComparison]::Ordinal) | Should -BeFalse
        $findingBody = [System.IO.File]::ReadAllText((Join-Path $script:MarkdownRoot 'F1.md'))
        $findingBody.Contains('The frobnicator turns widgets into gleems.', [StringComparison]::Ordinal) | Should -BeFalse
    }

    It 'refuses a content overview without an audit document' {
        $overviewPath = Join-Path $script:WorkRoot 'content-overview.md'
        Write-PullRequestOutputFile -Path $overviewPath -Content '# Content overview'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-OverviewPath', $overviewPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*audit document*'
    }

    It 'refuses a missing content overview' {
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'
        $combinedPath = Join-Path $script:WorkRoot 'findings.md'
        $overviewPath = Join-Path $script:WorkRoot 'content-overview.md'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
            '-AuditDocumentPath', $auditPath
            '-OverviewPath', $overviewPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*does not exist*'
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F1.md') | Should -BeFalse
        Test-Path -LiteralPath $combinedPath | Should -BeFalse
        Test-Path -LiteralPath $auditPath | Should -BeFalse
    }

    It 'refuses an empty content overview' {
        $auditPath = Join-Path $script:WorkRoot 'audit-findings.md'
        $combinedPath = Join-Path $script:WorkRoot 'findings.md'
        $overviewPath = Join-Path $script:WorkRoot 'content-overview.md'
        Write-PullRequestOutputFile -Path $overviewPath -Content " `n`t "

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
            '-AuditDocumentPath', $auditPath
            '-OverviewPath', $overviewPath
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike '*is empty*'
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F1.md') | Should -BeFalse
        Test-Path -LiteralPath $combinedPath | Should -BeFalse
        Test-Path -LiteralPath $auditPath | Should -BeFalse
    }

    It 'renders only the findings it was told to' {
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-FindingId', 'F2'
        )

        $result['exitCode'] | Should -Be 0
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F2.md') -PathType Leaf | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F1.md') -PathType Leaf | Should -BeFalse
    }

    # The combined document is delivered as the review itself, so naming a finding narrows which
    # bodies are re-rendered and never what the review says. Narrowing it too would let an amended
    # finding overwrite the deliverable with a document holding only that finding.
    It 'keeps the combined document covering the whole artifact while rendering one finding' {
        $combinedPath = Join-Path $script:WorkRoot 'findings.md'

        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $combinedPath
            '-FindingId', 'F2'
        )

        $result['exitCode'] | Should -Be 0
        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F1.md') -PathType Leaf | Should -BeFalse

        $combined = [System.IO.File]::ReadAllText($combinedPath)
        $combined | Should -BeLike '*# Reviewer finding*'
        $combined | Should -BeLike '*# Synthesis finding*'
    }

    It 'renders several named findings supplied as one comma separated value' {
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-FindingId', 'F1,F2'
        )

        $result['exitCode'] | Should -Be 0
        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        (Get-JsonArrayProperty -Node $emitted -Name 'findings').Count | Should -Be 2
    }

    It 'refuses a finding the artifact does not hold' {
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-OutputDirectory', $script:MarkdownRoot
            '-FindingId', 'F9'
        )

        $result['exitCode'] | Should -Not -Be 0
        $result['standardOutput'] | Should -BeNullOrEmpty
        (Get-SingleLineErrorText -Text $result['standardError']) | Should -BeLike "*'F9'*"
    }
}

# The skill delivers a review whose target is not a pull request by hand: phase 7 builds the
# approved set and phase 9 renders that set into one document the user posts. The two commands are
# joined only by the phase text, so the join is asserted here.
Describe 'Approved set delivery' {
    BeforeAll {
        . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
        . (Join-Path $script:LibraryRoot 'ReviewPublication.ps1')
        . (Join-Path $script:LibraryRoot 'PullRequestPublication.ps1')

        # A fixed instant, so a recorded decision carries no dependency on the host clock.
        $script:FixedNowUtc = [DateTimeOffset]::Parse('2024-05-06T09:10:11.0000000Z', [cultureinfo]::InvariantCulture)
    }

    BeforeEach {
        $script:WorkRoot = Join-Path $TestDrive ([guid]::NewGuid())
        New-Item -ItemType Directory -Path $script:WorkRoot -Force | Out-Null

        $script:ArtifactPath = Join-Path $script:WorkRoot 'consolidated.json'
        $script:DecisionsPath = Join-Path $script:WorkRoot 'decisions.json'
        $script:ApprovedPath = Join-Path $script:WorkRoot 'approved-findings.json'
        $script:MarkdownRoot = Join-Path $script:WorkRoot 'rendered'
        $script:CombinedPath = Join-Path $script:WorkRoot 'findings.md'

        $document = New-FindingArtifactDocument -Kind 'consolidated'
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = 'Reviewer finding' }) -Origin 'reviewer' -SourceFindingId @('g1-1') | Out-Null
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = 'Synthesis finding' }) -Origin 'synthesis' -SourceFindingId @('g1-1', 'g2-1') | Out-Null
        Add-FindingToArtifact -Document $document -Finding (New-TestFinding -Override @{ title = 'Rejected finding' }) -Origin 'reviewer' -SourceFindingId @('g2-1') | Out-Null
        Write-FindingArtifact -Path $script:ArtifactPath -Document $document

        $decisions = New-FindingDecisionDocument
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F1' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F2' -Decision 'approved' -NowUtc $script:FixedNowUtc | Out-Null
        Add-FindingDecisionRecord -Document $decisions -FindingId 'F3' -Decision 'rejected' -NowUtc $script:FixedNowUtc | Out-Null
        Write-FindingDecisionDocument -Path $script:DecisionsPath -Document $decisions

        $approval = Invoke-SkillExecutable -Name 'New-ApprovedFindings.ps1' -ArgumentList @(
            '-Path', $script:ArtifactPath
            '-DecisionsPath', $script:DecisionsPath
            '-OutputPath', $script:ApprovedPath
        )
        $approval['exitCode'] | Should -Be 0 -Because $approval['standardError']
    }

    It 'renders the approved set the phase hands to the user' {
        $result = Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ApprovedPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $script:CombinedPath
        )

        $result['exitCode'] | Should -Be 0 -Because $result['standardError']
        $result['standardError'] | Should -BeNullOrEmpty

        $emitted = ConvertFrom-JsonVerbatim -Text $result['standardOutput']
        $rendered = Get-JsonArrayProperty -Node $emitted -Name 'findings'
        @($rendered | ForEach-Object { $_['findingId'] }) | Should -Be @('F1', 'F2')
        (Get-JsonProperty -Node $emitted -Name 'combinedPath') | Should -BeExactly $script:CombinedPath

        Test-Path -LiteralPath (Join-Path $script:MarkdownRoot 'F3.md') -PathType Leaf | Should -BeFalse
    }

    # The user delivers the combined document as the review, so a finding the audit rejected
    # appearing in it publishes an observation the audit refused.
    It 'carries every approved finding into the combined document and nothing else' {
        Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ApprovedPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $script:CombinedPath
        ) | Out-Null

        $combined = [System.IO.File]::ReadAllText($script:CombinedPath)
        $combined | Should -BeLike '*# Synthesis finding*'
        $combined | Should -BeLike '*# Reviewer finding*'
        $combined | Should -Not -BeLike '*# Rejected finding*'
    }

    # The per-finding files and the combined document are two renderings of one review, so a reader
    # of either meets the same findings saying the same thing, presented in the same order.
    It 'repeats each approved body verbatim in presentation order' {
        Invoke-SkillExecutable -Name 'ConvertTo-FindingMarkdown.ps1' -ArgumentList @(
            '-Path', $script:ApprovedPath
            '-OutputDirectory', $script:MarkdownRoot
            '-CombinedPath', $script:CombinedPath
        ) | Out-Null

        $combined = [System.IO.File]::ReadAllText($script:CombinedPath)
        $lastIndex = -1
        foreach ($findingId in @('F2', 'F1')) {
            $body = [System.IO.File]::ReadAllText((Join-Path $script:MarkdownRoot "$findingId.md"))
            $body = $body.Substring($body.IndexOf('# '))

            $index = $combined.IndexOf($body)
            $index | Should -BeGreaterThan $lastIndex -Because $findingId
            $lastIndex = $index
        }
    }

    # Publication reads the same file this rendering reads, and refusing the working artifact there
    # is what keeps a review from publishing a finding the audit never approved.
    It 'leaves publication refusing a document that is not an approved set' {
        { Read-ApprovedFindingArtifact -Path $script:ArtifactPath } |
            Should -Throw "*reports kind 'consolidated'*"
    }
}
