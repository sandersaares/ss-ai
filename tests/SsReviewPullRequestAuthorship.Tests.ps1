#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Fixture = Join-Path $PSScriptRoot 'fixtures/ss-review-native-capture/github-expected.json'
    $script:Owned = [Collections.Generic.List[string]]::new()
    $script:NonAsciiBody = "Consider the $([char]::ConvertFromUtf32(0x1F916)) path $([char]0x2014) see $([char]0x201C) Registered I/O $([char]0x201D) below."

    function New-CaptureFile {
        param([Parameter(Mandatory)] [string] $Path, [AllowNull()] $SchemaVersion = $null)

        $document = [IO.File]::ReadAllText($script:Fixture) | ConvertFrom-Json -AsHashtable -DateKind String
        $thread = $document.threads[0]
        $thread.id = '1'
        $thread.openingCommentId = '1'
        $first = $thread.comments[0]
        $first.id = '1'
        $first.content = $script:NonAsciiBody
        $agent = ($first | ConvertTo-Json -Depth 100) | ConvertFrom-Json -AsHashtable -DateKind String
        $agent.id = '2'
        $agent.authorship = 'agent'
        $agent.authorshipEvidence = @('identity:service')
        $thread.comments = @($first, $agent)
        $other = ($thread | ConvertTo-Json -Depth 100) | ConvertFrom-Json -AsHashtable -DateKind String
        $other.id = '2'
        $other.openingCommentId = '3'
        $other.comments = @($other.comments[0])
        $other.comments[0].id = '3'
        $document.threads = @($thread, $other)
        if ($null -ne $SchemaVersion) { $document.schemaVersion = $SchemaVersion }
        [IO.File]::WriteAllText($Path, ($document | ConvertTo-Json -Depth 100 -Compress), [Text.UTF8Encoding]::new($false))
        foreach ($suffix in @('', '.lock', '.authorship.json', '.authorship.json.lock')) {
            $script:Owned.Add("$Path$suffix")
        }
        $Path
    }

    function ConvertTo-ClassificationText {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Entry
        )

        ConvertTo-Json -InputObject ([object[]] $Entry) -Depth 32 -Compress
    }

    function New-Classification {
        param(
            [Parameter(Mandatory)] [string] $ThreadId,
            [Parameter(Mandatory)] [string] $CommentId,
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $Kind
        )

        [ordered]@{ threadId = $ThreadId; commentId = $CommentId; kind = $Kind }
    }

    function Invoke-Native {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $ArgumentList
        )

        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $script:Native
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
        $startInfo.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)

        foreach ($argument in $ArgumentList) {
            $startInfo.ArgumentList.Add($argument)
        }

        $process = [Diagnostics.Process]::Start($startInfo)
        try {
            # Draining both streams concurrently avoids a full stderr pipe blocking stdout.
            $standardOutput = $process.StandardOutput.ReadToEndAsync()
            $standardError = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()

            [pscustomobject]@{
                ExitCode       = $process.ExitCode
                StandardOutput = $standardOutput.GetAwaiter().GetResult()
                StandardError  = $standardError.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }

    # Translate existing test inputs to the native authorship command and bind its capture snapshot.
    function Invoke-AuthorshipProcess {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [string[]] $ArgumentList,
            [switch] $UnreadableCapture
        )

        $capturePathIndex = [array]::IndexOf($ArgumentList, '-Path')
        $capturePath = $ArgumentList[$capturePathIndex + 1]
        $page = Invoke-Native -ArgumentList @('context', 'read', '--path', $capturePath, '--query', 'metadata', '--field', 'title')
        $snapshot = if ($page.ExitCode -eq 0) {
            (Get-SingleJsonResult -StandardOutput $page.StandardOutput).snapshotId
        } else {
            if (-not $UnreadableCapture) { throw "Reading capture metadata failed: $($page.StandardError)" }
            # The unreadable capture case must still exercise the consumer's rejection path.
            'invalid-snapshot'
        }
        $translated = foreach ($argument in $ArgumentList) {
            switch -Exact ($argument) {
                '-Path' { '--path' }
                '-Classification' { '--classification' }
                '-ClassificationPath' { '--classification-path' }
                default { $argument }
            }
        }
        Invoke-Native -ArgumentList (@('context', 'set-authorship', '--snapshot-id', $snapshot) + $translated)
    }

    # PowerShell colours a terminating error and wraps it into a gutter-decorated block when it
    # renders to a redirected stream, so the message is compared with the colour codes stripped and
    # that layout collapsed.
    function Get-ErrorText {
        param(
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $Text
        )

        $plain = $Text -replace "$([char]27)\[[0-9;]*m", ''
        ($plain -replace '[\r\n]+\s*\|\s*', ' ') -replace '\s+', ' '
    }

    # The single JSON object the executable writes to standard output, refusing anything else on the
    # stream. The calling agent parses this output, so a stray value would corrupt the result.
    function Get-SingleJsonResult {
        param(
            [Parameter(Mandatory)] [AllowEmptyString()] [string] $StandardOutput
        )

        $text = $StandardOutput.Trim()
        $lines = @($text -split "`r?`n")
        if ($lines.Count -ne 1) {
            throw "Standard output carries $($lines.Count) lines rather than one compressed JSON object."
        }
        $lines[0] | ConvertFrom-Json -AsHashtable -DateKind String
    }

    function Get-CaptureComment {
        param(
            [Parameter(Mandatory)] [System.Collections.IDictionary] $Document,
            [Parameter(Mandatory)] [string] $ThreadId,
            [Parameter(Mandatory)] [string] $CommentId
        )

        $result = Invoke-Native -ArgumentList @('context', 'read', '--path', $Document.path,
            '--query', 'comments', '--thread-id', $ThreadId, '--comment-id', $CommentId)
        if ($result.ExitCode -ne 0) { throw $result.StandardError }
        $page = Get-SingleJsonResult -StandardOutput $result.StandardOutput
        if ($page.items.Count -ne 1) { throw "Expected one selected comment." }
        $page.items[0]
    }

    # Consumers read the immutable capture with its separately validated semantic overlay.
    function Read-CaptureFileDocument {
        param(
            [Parameter(Mandatory)] [string] $Path
        )

        @{ path = $Path }
    }

    function Get-JsonProperty {
        param([System.Collections.IDictionary] $Node, [string] $Name)
        $Node[$Name]
    }

    function Get-JsonArrayProperty {
        param([System.Collections.IDictionary] $Node, [string] $Name)
        return ,@($Node[$Name])
    }

    function Get-FileFingerprint {
        param(
            [Parameter(Mandatory)] [string] $Path
        )

        [System.Convert]::ToBase64String([System.IO.File]::ReadAllBytes($Path))
    }
}

Describe 'Set-CommentAuthorship' {
    BeforeAll {
        $script:WorkRoot = Join-Path $script:RepoRoot 'target'
        [void] [IO.Directory]::CreateDirectory($script:WorkRoot)
        $script:Prefix = 'authorship-cli-' + [guid]::NewGuid().ToString('N')
        $script:CaptureIndex = 0
    }

    AfterAll {
        foreach ($path in $script:Owned) {
            if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
        }
    }

    BeforeEach {
        # Every test owns its capture and semantic decisions.
        $script:CaptureIndex++
        $script:CapturePath = Join-Path $script:WorkRoot "$($script:Prefix)-capture-$script:CaptureIndex.json"
    }

    It 'records classification separately and exposes it through the capture reader' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
        Test-Path -LiteralPath "$script:CapturePath.authorship.json" | Should -BeTrue
        $document = Read-CaptureFileDocument -Path $script:CapturePath
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') | Should -BeExactly 'agent'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '2') -Name 'authorship') | Should -BeExactly 'agent'

        # The reader's judgement is recorded alongside the evidence the capture collected.
        $evidence = Get-JsonArrayProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorshipEvidence'
        $evidence -ccontains 'classification:reviewed' | Should -BeTrue
    }

    It 'writes exactly one compressed JSON object to standard output' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $result.StandardError | Should -BeNullOrEmpty
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput

        @($parsed.Keys) | Should -BeExactly @('contextPath', 'applied', 'unchanged', 'indeterminate', 'isComplete')
        $parsed['contextPath'] | Should -BeExactly ([System.IO.Path]::GetFullPath($script:CapturePath))
        $parsed['applied'] | Should -Be 1
        $parsed['unchanged'] | Should -Be 0
        $parsed['indeterminate'] | Should -Be 1
        $parsed['isComplete'] | Should -BeFalse
    }

    It 'counts unsettled comments without dumping their identifiers' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput
        $parsed['indeterminate'] | Should -Be 1
        $parsed.Contains('indeterminateComments') | Should -BeFalse
    }

    It 'reports a capture whose every comment is settled as complete' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '2' -Kind 'agent')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $parsed = Get-SingleJsonResult -StandardOutput $result.StandardOutput
        $parsed['applied'] | Should -Be 2
        # Restating what the capture already proved is accepted and changes nothing.
        $parsed['unchanged'] | Should -Be 1
        $parsed['indeterminate'] | Should -Be 0
        $parsed['isComplete'] | Should -BeTrue
        $parsed.Contains('indeterminateComments') | Should -BeFalse
    }

    It 'preserves non-ASCII capture bytes through classification' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)

        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before

        $document = Read-CaptureFileDocument -Path $script:CapturePath
        $comment = Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1'
        (Get-JsonProperty -Node $comment -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node $comment -Name 'content') | Should -BeExactly $script:NonAsciiBody

        # The immutable capture keeps its UTF-8 encoding and line-ending contract.
        $bytes = [System.IO.File]::ReadAllBytes($script:CapturePath)
        $bytes[0] | Should -Not -Be 0xEF
        $bytes | Should -Not -Contain 13
    }

    It 'accepts the classifications from a file' {
        # A capture with many comments produces a batch that exceeds what a command line accepts, so
        # the file form has to reach the same result as the inline form.
        $inlinePath = $script:CapturePath
        $filePath = Join-Path $script:WorkRoot "$($script:Prefix)-capture-$script:CaptureIndex-file.json"
        New-CaptureFile -Path $inlinePath | Out-Null
        New-CaptureFile -Path $filePath | Out-Null

        $entries = @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent')
        )
        $classification = ConvertTo-ClassificationText -Entry $entries
        $classificationPath = Join-Path $script:WorkRoot "$($script:Prefix)-classification-$script:CaptureIndex.json"
        $script:Owned.Add($classificationPath)
        [System.IO.File]::WriteAllText($classificationPath, $classification, [System.Text.UTF8Encoding]::new($false))

        $inlineResult = Invoke-AuthorshipProcess -ArgumentList @('-Path', $inlinePath, '-Classification', $classification)
        $fileResult = Invoke-AuthorshipProcess -ArgumentList @('-Path', $filePath, '-ClassificationPath', $classificationPath)

        $inlineResult.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $inlineResult.StandardError)
        $fileResult.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $fileResult.StandardError)

        $inlineParsed = Get-SingleJsonResult -StandardOutput $inlineResult.StandardOutput
        $fileParsed = Get-SingleJsonResult -StandardOutput $fileResult.StandardOutput

        # Anchored on what the batch actually does, so that the comparison between the two forms
        # cannot be satisfied by both of them doing nothing.
        $inlineParsed['applied'] | Should -Be 2
        $inlineParsed['isComplete'] | Should -BeTrue
        foreach ($name in @('applied', 'unchanged', 'indeterminate', 'isComplete')) {
            $fileParsed[$name] | Should -Be $inlineParsed[$name] -Because "the two forms report the same $name"
        }

        # The decision record holds no local path, so equivalent batches are byte-identical.
        Get-FileFingerprint -Path $filePath | Should -BeExactly (Get-FileFingerprint -Path $inlinePath)
        Get-FileFingerprint -Path "$filePath.authorship.json" | Should -BeExactly (Get-FileFingerprint -Path "$inlinePath.authorship.json")
        foreach ($path in @($inlinePath, $filePath)) {
            $comment = Get-CaptureComment -Document (Read-CaptureFileDocument -Path $path) -ThreadId '1' -CommentId '1'
            (Get-JsonProperty -Node $comment -Name 'authorship') | Should -BeExactly 'human'
        }
    }

    It 'refuses both classification forms at once' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))
        $classificationPath = Join-Path $script:WorkRoot "$($script:Prefix)-classification-both-$script:CaptureIndex.json"
        $script:Owned.Add($classificationPath)
        [System.IO.File]::WriteAllText($classificationPath, $classification, [System.Text.UTF8Encoding]::new($false))

        $result = Invoke-AuthorshipProcess -ArgumentList @(
            '-Path', $script:CapturePath
            '-Classification', $classification
            '-ClassificationPath', $classificationPath
        )

        $result.ExitCode | Should -Not -Be 0
        $message = Get-ErrorText -Text $result.StandardError
        $message | Should -Match 'cannot be used with'
        $message | Should -Match '--classification-path'
    }

    It 'refuses to run without any classification' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError) | Should -Match '--classification-path'
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'refuses a classification payload that is not a list' {
        New-CaptureFile -Path $script:CapturePath | Out-Null

        $result = Invoke-AuthorshipProcess -ArgumentList @(
            '-Path', $script:CapturePath
            '-Classification', '{"threadId":"1","commentId":"1","kind":"human"}'
        )

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('JSON array', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'accepts a capitalized authorship and stores it in lower case' {
        # The batch is written by a language model, which capitalizes a word as readily as not, and
        # refusing it would discard the whole pass over a single letter.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'Human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'AGENT')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Be 0 -Because (Get-ErrorText -Text $result.StandardError)
        $document = Read-CaptureFileDocument -Path $script:CapturePath
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') | Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-CaptureComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') | Should -BeExactly 'agent'
    }

    It 'leaves the capture untouched when a batch names an authorship outside the vocabulary' {
        # Leniency about case is not leniency about the word: an unrecognized authorship is a mistake
        # the caller has to hear about rather than a comment silently left unsettled.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'Robot')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        $message = Get-ErrorText -Text $result.StandardError
        $message.Contains("authorship 'Robot'", [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains("Use 'agent' or 'human'", [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'leaves the capture untouched when a batch names a comment it does not contain' {
        # The document is rewritten only after every classification has been accepted, so a batch
        # whose later entry is wrong must not half-update the file.
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '98' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains("no comment '98'", [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'leaves the capture untouched when a batch contradicts what the capture proved' {
        New-CaptureFile -Path $script:CapturePath | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @(
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '2' -Kind 'human')
        )

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification)

        $result.ExitCode | Should -Not -Be 0
        (Get-ErrorText -Text $result.StandardError).Contains('cannot be reclassified', [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }

    It 'refuses a capture written to a schema version it does not read' {
        New-CaptureFile -Path $script:CapturePath -SchemaVersion 99 | Out-Null
        $before = Get-FileFingerprint -Path $script:CapturePath
        $classification = ConvertTo-ClassificationText -Entry @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $result = Invoke-AuthorshipProcess -ArgumentList @('-Path', $script:CapturePath, '-Classification', $classification) -UnreadableCapture

        $result.ExitCode | Should -Not -Be 0
        $message = Get-ErrorText -Text $result.StandardError
        $message.Contains('version 99', [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains('Capture the pull request again', [StringComparison]::Ordinal) | Should -BeTrue
        Get-FileFingerprint -Path $script:CapturePath | Should -BeExactly $before
    }
}
