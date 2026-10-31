BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'WorkflowData.ps1')
    . (Join-Path $script:LibraryRoot 'FindingModel.ps1')
    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')
    . (Join-Path $script:LibraryRoot 'NativeWorkflowBridge.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestAnchor.ps1')

    # The diff is parsed from recorded Git output, exactly as the composition root would parse the
    # output of a real invocation, so the anchor rules are exercised against the same shape they meet
    # in production without any repository being read.
    $script:DiffText = @'
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,7 +10,8 @@ fn existing() {
 context one
 context two
 context three
-removed line
+added line
+another added line
 context four
 context five
 context six
diff --git a/src/old-name.rs b/src/new-name.rs
similarity index 88%
rename from src/old-name.rs
rename to src/new-name.rs
index 5555555..6666666 100644
--- a/src/old-name.rs
+++ b/src/new-name.rs
@@ -1,4 +1,4 @@
 alpha
-beta
+BETA
 gamma
 delta
diff --git a/old/gone.txt b/old/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/old/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-alpha
-beta
'@

    # Assigned to a variable before it reaches the document: a command that emits a single array
    # emits it as one object, so wrapping the call in @( ) would store an array holding the array.
    $script:DiffFiles = ConvertFrom-GitDiffText -Text $script:DiffText

    $script:Diff = [ordered]@{
        mergeBaseCommit = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
        sourceCommit    = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
        files           = $script:DiffFiles
    }

    function New-TestAnchor {
        param(
            [Parameter(Mandatory)]
            [ValidateSet('line', 'file')]
            [string] $Kind,

            [Parameter(Mandatory)]
            [string] $Path,

            [int] $StartLine = 0,

            [int] $EndLine = 0,

            [string] $Side
        )

        $anchor = [ordered]@{
            kind = $Kind
            path = $Path
        }
        if ($StartLine -gt 0) { $anchor['startLine'] = $StartLine }
        if ($EndLine -gt 0) { $anchor['endLine'] = $EndLine }
        if (-not [string]::IsNullOrWhiteSpace($Side)) { $anchor['side'] = $Side }
        $anchor
    }

    # Builds an approved set holding the given findings, in the shape New-ApprovedFindings.ps1
    # writes it.
    function New-TestApprovedDocument {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyCollection()]
            [object[]] $Finding
        )

        [ordered]@{
            schemaVersion           = 1
            target                  = [ordered]@{ platform = 'gitHub'; pullRequestId = 7 }
            reviewedSourceCommit    = $script:Diff['sourceCommit']
            reviewedMergeBaseCommit = $script:Diff['mergeBaseCommit']
            findings                = $Finding
        }
    }

    # Builds a stored finding record carrying the given anchor.
    function New-TestApprovedFinding {
        param(
            [Parameter(Mandatory)]
            [string] $Id,

            [Parameter(Mandatory)]
            [System.Collections.IDictionary] $Anchor,

            [System.Collections.IDictionary] $Override = @{}
        )

        $finding = [ordered]@{
            schemaVersion     = 1
            id                = $Id
            sourceFindingIds  = @('g1-1')
            origin            = 'reviewer'
            title             = "Finding $Id"
            description       = 'The loop condition is never updated.'
            reasoning         = 'The counter is incremented in a branch that cannot be reached.'
            consequence       = 'A transient failure hangs the caller indefinitely.'
            recommendedAction = 'Increment the counter outside the branch.'
            anchor            = $Anchor
        }

        foreach ($key in $Override.Keys) { $finding[[string] $key] = $Override[$key] }

        $finding
    }
}

Describe 'ss-review pull request anchor resolution' {
    It 'preserves timestamp-shaped paths in native anchor responses' {
        Mock Invoke-NativeReviewWorkflowWithJson {
            param($Arguments, $PathOption, $Value)
            if ($Value.ContainsKey('anchors')) {
                return '{"results":[{"anchor":{"kind":"file","path":"2026-10-07T00:00:00Z","wasDowngraded":false}}]}'
            }
            return '{"kind":"file","path":"2026-10-07T00:00:00Z","wasDowngraded":false}'
        }
        $anchor = New-TestAnchor -Kind 'file' -Path '2026-10-07T00:00:00Z'

        (Resolve-PullRequestAnchor -Anchor $anchor -DiffFile $script:DiffFiles[0])['path'] | Should -BeExactly $anchor['path']
        (Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff)['path'] | Should -BeExactly $anchor['path']
        (Resolve-PullRequestAnchorsInDiff -Anchor @($anchor) -Diff $script:Diff)[0]['anchor']['path'] |
            Should -BeExactly $anchor['path']
    }

    It 'passes a line anchor inside a hunk through unchanged' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 14 -EndLine 15 -Side 'right'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'line'
        $resolved['path'] | Should -Be 'src/lib.rs'
        $resolved['startLine'] | Should -Be 14
        $resolved['endLine'] | Should -Be 15
        $resolved['side'] | Should -Be 'right'
        $resolved['wasDowngraded'] | Should -BeFalse
        $resolved.Contains('downgradeReason') | Should -BeFalse
    }

    It 'accepts an anchor on a context line, which the reviewed diff shows and the service accepts' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 10

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'line'
        $resolved['startLine'] | Should -Be 10
        $resolved['endLine'] | Should -Be 10
        $resolved['wasDowngraded'] | Should -BeFalse
    }

    It 'takes the post-change side for a line anchor that names none' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 17

        (Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff)['side'] | Should -Be 'right'
    }

    It 'resolves a line anchor on the pre-change side against the pre-change positions' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 16 -Side 'left'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'line'
        $resolved['side'] | Should -Be 'left'
        $resolved['wasDowngraded'] | Should -BeFalse
    }

    It 'downgrades a line anchor outside every hunk to a file anchor and records the downgrade' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 300 -EndLine 305 -Side 'right'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'file'
        $resolved['path'] | Should -Be 'src/lib.rs'
        $resolved['wasDowngraded'] | Should -BeTrue
        $resolved['downgradeReason'] | Should -BeLike "*No hunk*lines 300-305*right*src/lib.rs*reported against the file*"
        $resolved.Contains('startLine') | Should -BeFalse
        $resolved.Contains('side') | Should -BeFalse
    }

    It 'downgrades a range that spans two hunks, since the text between them is not commentable' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 15 -EndLine 25

        (Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff)['kind'] | Should -Be 'file'
    }

    It 'matches a renamed file by its post-change path' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/new-name.rs' -StartLine 2

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'line'
        $resolved['path'] | Should -Be 'src/new-name.rs'
    }

    It 'fails a finding that names a renamed file by the path it no longer has' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/old-name.rs' -StartLine 2

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage "*does not change 'src/old-name.rs'*"
    }

    It 'resolves a left-side line anchor on a deleted file against the removed lines' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'old/gone.txt' -StartLine 1 -EndLine 2 -Side 'left'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'line'
        $resolved['path'] | Should -Be 'old/gone.txt'
        $resolved['startLine'] | Should -Be 1
        $resolved['endLine'] | Should -Be 2
        $resolved['side'] | Should -Be 'left'
        $resolved['wasDowngraded'] | Should -BeFalse
    }

    It 'downgrades a right-side line anchor on a deleted file to a file anchor' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'old/gone.txt' -StartLine 1 -Side 'right'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'file'
        $resolved['path'] | Should -Be 'old/gone.txt'
        $resolved['wasDowngraded'] | Should -BeTrue
        $resolved['downgradeReason'] | Should -BeLike "*No hunk*line 1*right*old/gone.txt*reported against the file*"
    }

    It 'accepts a file anchor on a file the pull request deletes' {
        $anchor = New-TestAnchor -Kind 'file' -Path 'old/gone.txt'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'file'
        $resolved['path'] | Should -Be 'old/gone.txt'
        $resolved['wasDowngraded'] | Should -BeFalse
    }

    It 'fails a finding anchored to a file the pull request does not change' {
        $anchor = New-TestAnchor -Kind 'file' -Path 'src/untouched.rs'

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage "*does not change 'src/untouched.rs'*"
    }

    It 'reads a path written with segments Git does not print' {
        $anchor = New-TestAnchor -Kind 'file' -Path './src/lib.rs'

        (Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff)['path'] | Should -Be 'src/lib.rs'
    }

    It 'refuses a path that separates its directories with backslashes' {
        $anchor = New-TestAnchor -Kind 'file' -Path '.\src\lib.rs'

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage '*backslash*'
    }

    It 'passes a file anchor through without consulting the hunks' {
        $anchor = New-TestAnchor -Kind 'file' -Path 'src/lib.rs'

        $resolved = Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff

        $resolved['kind'] | Should -Be 'file'
        $resolved['wasDowngraded'] | Should -BeFalse
        $resolved.Contains('startLine') | Should -BeFalse
    }

    It 'ends a single-line anchor on the line it starts on' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 12

        (Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff)['endLine'] | Should -Be 12
    }

    It 'refuses to resolve an anchor against the diff entry for another file' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 12
        $other = Get-GitDiffFile -Diff $script:Diff -Path 'src/new-name.rs'

        { Resolve-PullRequestAnchor -Anchor $anchor -DiffFile $other } |
            Should -Throw -ExpectedMessage "*cannot be resolved against the diff entry for 'src/new-name.rs'*"
    }

    It 'refuses an anchor of a kind that is neither a line nor a file' {
        $anchor = [ordered]@{ kind = 'range'; path = 'src/lib.rs' }

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage "*kind 'range'*"
    }

    It 'refuses a line anchor that names no line' {
        $anchor = [ordered]@{ kind = 'line'; path = 'src/lib.rs' }

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage '*names no line*'
    }

    It 'refuses a line anchor on a side that is neither the post-change nor the pre-change one' {
        $anchor = New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 12 -Side 'middle'

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage "*side 'middle'*"
    }

    It 'refuses an anchor that names no file at all' {
        $anchor = [ordered]@{ kind = 'file'; path = '' }

        { Resolve-PullRequestAnchorInDiff -Anchor $anchor -Diff $script:Diff } |
            Should -Throw -ExpectedMessage '*names no file*'
    }
}

Describe 'ss-review approved finding freshness against the reviewed diff' {
    It 'processes multiple anchors and follow-ups together without losing downgrades' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path './src/lib.rs')
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'SRC/lib.rs')
            New-TestApprovedFinding -Id 'F3' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 300)
            New-TestApprovedFinding -Id 'F4' -Anchor (New-TestAnchor -Kind 'file' -Path 'missing') -Override @{ followUp = @{ threadId = 'T' } }
            New-TestApprovedFinding -Id 'F5' -Anchor (New-TestAnchor -Kind 'file' -Path './src/old-name.rs')
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        @($unanchored | ForEach-Object { $_['findingId'] }) | Should -Be @('F2', 'F5')
        $unanchored[1]['path'] | Should -BeExactly 'src/old-name.rs'
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1', 'F3', 'F4')
    }

    It 'refuses a malformed diff without changing the approved document' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/untouched.rs')
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs')
        )
        $diff = @{ files = @($script:DiffFiles[0], @{ path = 'malformed' }) }

        { Remove-ApprovedFindingOutsideDiff -Document $document -Diff $diff } | Should -Throw
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1', 'F2')
    }

    It 'refuses missing and non-object anchors instead of discarding them' -TestCases @(
        @{ value = $null }
        @{ value = 'malformed' }
    ) {
        param($value)
        $finding = New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs')
        $finding['anchor'] = $value
        $document = New-TestApprovedDocument -Finding @($finding)

        { Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff } | Should -Throw
        $document['findings'].Count | Should -Be 1
    }

    It 'refuses malformed native results instead of dropping findings' {
        Mock Invoke-NativeReviewWorkflowWithJson { '{"results":[]}' }
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs')
        )
        { Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff } |
            Should -Throw -ExpectedMessage '*Invalid native anchor resolution response*'
        $document['findings'].Count | Should -Be 1
        Should -Invoke Invoke-NativeReviewWorkflowWithJson -Times 1 -Exactly
    }

    It 'propagates a native failure without changing the document' {
        Mock Invoke-NativeReviewWorkflowWithJson { throw 'native process failed' }
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs')
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/untouched.rs')
        )
        { Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff } |
            Should -Throw -ExpectedMessage '*native process failed*'
        $document['findings'].Count | Should -Be 2
        Should -Invoke Invoke-NativeReviewWorkflowWithJson -Times 1 -Exactly
    }

    It 'uses one native request for multiple findings and omits follow-ups' {
        Mock Invoke-NativeReviewWorkflowWithJson {
            '{"results":[{"anchor":{"kind":"file","path":"src/lib.rs","wasDowngraded":false}},{"error":"Unresolvable anchor: missing"}]}'
        }
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs')
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'missing')
            New-TestApprovedFinding -Id 'F3' -Anchor (New-TestAnchor -Kind 'file' -Path 'missing') -Override @{ followUp = @{ threadId = 'T' } }
        )
        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff
        $unanchored[0]['findingId'] | Should -BeExactly 'F2'
        Should -Invoke Invoke-NativeReviewWorkflowWithJson -Times 1 -Exactly -ParameterFilter {
            $Value['anchors'].Count -eq 2
        }
    }

    It 'skips native resolution when every finding is a follow-up' {
        Mock Invoke-NativeReviewWorkflowWithJson { throw 'unexpected native invocation' }
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'missing') -Override @{ followUp = @{ threadId = 'T' } }
        )
        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff @{ files = 'malformed' }
        $unanchored.Count | Should -Be 0
        Should -Invoke Invoke-NativeReviewWorkflowWithJson -Times 0 -Exactly
    }

    # The pull request keeps moving while the review is produced, so a file that was under review
    # when a finding was written can be gone from the diff by the time the audit approves it.
    It 'reports a finding anchored to a file the reviewed diff does not change as unanchored' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 14)
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/untouched.rs')
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 1
        $unanchored[0]['findingId'] | Should -BeExactly 'F2'
        $unanchored[0]['path'] | Should -BeExactly 'src/untouched.rs'
        $unanchored[0]['reason'] | Should -BeLike "*does not change 'src/untouched.rs'*"

        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    # Publication reports such a finding against the file, so the observation still reaches the
    # author and marking it unanchored here would lose a finding nothing is wrong with.
    It 'keeps a finding whose lines fall outside every hunk of a file the diff changes' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 300 -EndLine 305)
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 0
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    It 'reports a finding as unanchored when the publication resolver rejects its anchor' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 14)
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 14 -Side 'middle')
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 1
        $unanchored[0]['findingId'] | Should -BeExactly 'F2'
        $unanchored[0]['path'] | Should -BeExactly 'src/lib.rs'
        $unanchored[0]['reason'] | Should -BeLike "*side 'middle'*"
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    # A follow-up is published into a thread that already exists, so the diff has no say over where
    # it goes, and marking it unanchored would discard a reply the review owes.
    It 'keeps a follow-up anchored to a file the reviewed diff does not change' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding `
                -Id 'F1' `
                -Anchor (New-TestAnchor -Kind 'file' -Path 'src/untouched.rs') `
                -Override @{ followUp = [ordered]@{ threadId = 'RT_kw1'; commentId = 'PRRC_1' } }
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 0
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    # A finding may cite evidence in a file it does not comment on. Only the anchor states where the
    # finding is published.
    It 'ignores the locations a finding cites beyond its anchor' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding `
                -Id 'F1' `
                -Anchor (New-TestAnchor -Kind 'file' -Path 'src/lib.rs') `
                -Override @{ impactedLocations = @([ordered]@{ path = 'src/untouched.rs'; startLine = 12 }) }
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 0
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    It 'matches a renamed file by its post-change path' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/new-name.rs')
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'src/old-name.rs')
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        @($unanchored | ForEach-Object { $_['findingId'] }) | Should -Be @('F2')
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1')
    }

    It 'reports nothing where the diff still changes every anchored file' {
        $document = New-TestApprovedDocument -Finding @(
            New-TestApprovedFinding -Id 'F1' -Anchor (New-TestAnchor -Kind 'line' -Path 'src/lib.rs' -StartLine 14)
            New-TestApprovedFinding -Id 'F2' -Anchor (New-TestAnchor -Kind 'file' -Path 'old/gone.txt')
        )

        $unanchored = Remove-ApprovedFindingOutsideDiff -Document $document -Diff $script:Diff

        $unanchored.Count | Should -Be 0
        @($document['findings'] | ForEach-Object { $_['id'] }) | Should -Be @('F1', 'F2')
    }
}
