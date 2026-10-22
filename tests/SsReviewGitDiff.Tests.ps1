BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'GitDiff.ps1')

    # Every assertion here runs against recorded Git output. Nothing shells out to a real repository,
    # so no test depends on what any clone on this machine happens to contain, and none depends on
    # elapsed time.

    # Characters are built from code points rather than typed literally, so the fixtures do not
    # depend on how this file is encoded on disk and the assertions compare code points rather than
    # rendered glyphs.
    $script:LatinSmallEWithAcute = [string] [char] 0x00E9
    $script:LatinSmallIWithDiaeresis = [string] [char] 0x00EF
    $script:RobotFace = [char]::ConvertFromUtf32(0x1F916)

    # Builds a Git invoker that answers from recorded output.
    #
    # The recorded calls are held at script scope rather than in the closure so an assertion can read
    # what was run; one invoker is built per test, so a single recorder is enough.
    function New-FakeGitInvoker {
        param(
            [string] $DiffText = '',

            [AllowNull()]
            [byte[]] $DiffBytes,

            # The commits the repository holds. Anything else is reported as missing.
            [string[]] $PresentCommit = @(),

            [switch] $NoRepository,

            [int] $DiffExitCode = 0,

            [string] $DiffStandardError = ''
        )

        $script:GitCalls = [System.Collections.Generic.List[object]]::new()

        $calls = $script:GitCalls
        $bytes = if ($null -ne $DiffBytes) { $DiffBytes } else { [Text.Encoding]::UTF8.GetBytes($DiffText) }
        $present = [string[]] @($PresentCommit)
        $hasRepository = -not $NoRepository
        $diffExitCode = $DiffExitCode
        $diffStandardError = $DiffStandardError

        {
            param(
                [Parameter(Mandatory)]
                [string[]] $ArgumentList
            )

            $arguments = @($ArgumentList)
            $calls.Add($arguments)

            # The first two arguments name the repository, so the verb is the third.
            $verb = $arguments[2]

            if ($verb -eq 'rev-parse') {
                if ($hasRepository) {
                    return [ordered]@{ exitCode = 0; standardOutputBytes = [Text.Encoding]::UTF8.GetBytes(".git`n"); standardErrorText = '' }
                }
                return [ordered]@{
                    exitCode            = 128
                    standardOutputBytes = [byte[]]::new(0)
                    standardErrorText   = "fatal: not a git repository: '$($arguments[1])'"
                }
            }

            if ($verb -eq 'cat-file') {
                $commit = $arguments[$arguments.Count - 1] -replace '\^\{commit\}$', ''
                if ($present -contains $commit) {
                    return [ordered]@{ exitCode = 0; standardOutputBytes = [byte[]]::new(0); standardErrorText = '' }
                }
                return [ordered]@{
                    exitCode            = 1
                    standardOutputBytes = [byte[]]::new(0)
                    standardErrorText   = "fatal: Not a valid object name $commit"
                }
            }

            if ($verb -eq 'diff') {
                return [ordered]@{
                    exitCode            = $diffExitCode
                    standardOutputBytes = $bytes
                    standardErrorText   = $diffStandardError
                }
            }

            throw "The fake Git invoker has no answer for 'git $($arguments -join ' ')'."
        }.GetNewClosure()
    }

    function Get-CodePointText {
        param(
            [Parameter(Mandatory)]
            [AllowEmptyString()]
            [string] $Value
        )

        $points = [System.Collections.Generic.List[string]]::new()
        for ($index = 0; $index -lt $Value.Length; $index++) {
            $points.Add(('U+{0:X4}' -f [char]::ConvertToUtf32($Value, $index)))
            if ([char]::IsHighSurrogate($Value[$index])) { $index++ }
        }
        $points -join ' '
    }

    $script:MergeBase = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    $script:Source = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'

    $script:ModifiedDiff = @'
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
@@ -40,6 +41,6 @@ fn other() {
 alpha
 beta
 gamma
-delta
+DELTA
 epsilon
 zeta
'@

    $script:AddedDiff = @'
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,3 @@
+first
+second
+third
'@

    $script:DeletedDiff = @'
diff --git a/old/gone.txt b/old/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/old/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-alpha
-beta
'@

    $script:RenamedDiff = @'
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
'@

    # A rename with no content change carries no hunks and no content headers at all, and the paths
    # here begin with the very letters Git uses to label the two sides of the comparison.
    $script:PureRenameDiff = @'
diff --git a/a/one.txt b/b/two.txt
similarity index 100%
rename from a/one.txt
rename to b/two.txt
'@

    $script:ModeOnlyWithSpacesDiff = @'
diff --git a/docs/my file.md b/docs/my file.md
old mode 100644
new mode 100755
'@

    $script:PureRenameWithSpacesDiff = @'
diff --git a/docs/old name.md b/docs/new name.md
similarity index 100%
rename from docs/old name.md
rename to docs/new name.md
'@

    $script:OmittedCountDiff = @'
diff --git a/tiny.txt b/tiny.txt
index 7777777..8888888 100644
--- a/tiny.txt
+++ b/tiny.txt
@@ -1 +1 @@
-one
+one and a half
\ No newline at end of file
'@

    # The removed and added lines of the first file are themselves the text of diff headers, which
    # only the hunk's stated line counts distinguish from the real thing.
    $script:HeaderShapedBodyDiff = @'
diff --git a/tricky.txt b/tricky.txt
index 9999999..aaaaaaa 100644
--- a/tricky.txt
+++ b/tricky.txt
@@ -1,2 +1,2 @@
--- a/not-a-file.txt
+++ b/also-not-a-file.txt
 tail
diff --git a/after.txt b/after.txt
index bbbbbbb..ccccccc 100644
--- a/after.txt
+++ b/after.txt
@@ -5,1 +5,1 @@
-before
+after
'@
}

Describe 'ss-review Git diff parsing' {
    It 'reports an added file with the hunk that introduces it' {
        $files = ConvertFrom-GitDiffText -Text $script:AddedDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'docs/new.md'
        $files[0]['status'] | Should -Be 'added'
        $files[0]['previousPath'] | Should -BeNullOrEmpty
        $files[0]['hunks'].Count | Should -Be 1
        $files[0]['hunks'][0]['oldStart'] | Should -Be 0
        $files[0]['hunks'][0]['oldCount'] | Should -Be 0
        $files[0]['hunks'][0]['newStart'] | Should -Be 1
        $files[0]['hunks'][0]['newCount'] | Should -Be 3
    }

    It 'reports a modified file with every hunk it carries' {
        $files = ConvertFrom-GitDiffText -Text $script:ModifiedDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'src/lib.rs'
        $files[0]['status'] | Should -Be 'modified'
        $files[0]['hunks'].Count | Should -Be 2
        $files[0]['hunks'][0]['oldStart'] | Should -Be 10
        $files[0]['hunks'][0]['oldCount'] | Should -Be 7
        $files[0]['hunks'][0]['newStart'] | Should -Be 10
        $files[0]['hunks'][0]['newCount'] | Should -Be 8
        $files[0]['hunks'][1]['oldStart'] | Should -Be 40
        $files[0]['hunks'][1]['newStart'] | Should -Be 41
        $files[0]['hunks'][1]['newCount'] | Should -Be 6
    }

    It 'names a deleted file by the path it had' {
        $files = ConvertFrom-GitDiffText -Text $script:DeletedDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'old/gone.txt'
        $files[0]['status'] | Should -Be 'deleted'
        $files[0]['hunks'][0]['newCount'] | Should -Be 0
    }

    It 'reports a renamed file under its new path and remembers the old one' {
        $files = ConvertFrom-GitDiffText -Text $script:RenamedDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'src/new-name.rs'
        $files[0]['previousPath'] | Should -Be 'src/old-name.rs'
        $files[0]['status'] | Should -Be 'renamed'
        $files[0]['hunks'].Count | Should -Be 1
    }

    It 'reports a rename that changed nothing, without stripping a directory named after a diff side' {
        $files = ConvertFrom-GitDiffText -Text $script:PureRenameDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'b/two.txt'
        $files[0]['previousPath'] | Should -Be 'a/one.txt'
        $files[0]['status'] | Should -Be 'renamed'
        $files[0]['hunks'].Count | Should -Be 0
    }

    It 'reports a mode-only change whose path contains spaces' {
        $files = ConvertFrom-GitDiffText -Text $script:ModeOnlyWithSpacesDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'docs/my file.md'
        $files[0]['previousPath'] | Should -BeNullOrEmpty
        $files[0]['status'] | Should -Be 'modified'
        $files[0]['hunks'].Count | Should -Be 0
    }

    It 'reads a hunk header that omits its counts as one line on each side' {
        $files = ConvertFrom-GitDiffText -Text $script:OmittedCountDiff

        $files[0]['hunks'].Count | Should -Be 1
        $files[0]['hunks'][0]['oldStart'] | Should -Be 1
        $files[0]['hunks'][0]['oldCount'] | Should -Be 1
        $files[0]['hunks'][0]['newStart'] | Should -Be 1
        $files[0]['hunks'][0]['newCount'] | Should -Be 1
    }

    It 'does not mistake diff header text inside a hunk body for a header' {
        $files = ConvertFrom-GitDiffText -Text $script:HeaderShapedBodyDiff

        $files.Count | Should -Be 2
        $files[0]['path'] | Should -Be 'tricky.txt'
        $files[0]['previousPath'] | Should -BeNullOrEmpty
        $files[1]['path'] | Should -Be 'after.txt'
        $files[1]['hunks'][0]['newStart'] | Should -Be 5
    }

    It 'reports no files for a comparison that changed nothing' {
        $files = ConvertFrom-GitDiffText -Text ''

        $files.Count | Should -Be 0
    }

    It 'restores a path Git quoted with octal escapes' {
        $diff = @'
diff --git "a/src/caf\303\251.rs" "b/src/caf\303\251.rs"
index 1111111..2222222 100644
--- "a/src/caf\303\251.rs"
+++ "b/src/caf\303\251.rs"
@@ -1 +1 @@
-alpha
+beta
'@

        $files = ConvertFrom-GitDiffText -Text $diff

        $files[0]['path'] | Should -Be "src/caf$($script:LatinSmallEWithAcute).rs"
        Get-CodePointText -Value $files[0]['path'] | Should -Be (Get-CodePointText -Value "src/caf$($script:LatinSmallEWithAcute).rs")
    }

    It 'restores a path whose escapes span a character outside the basic plane' {
        $diff = @'
diff --git "a/art/\360\237\244\226.svg" "b/art/\360\237\244\226.svg"
index 1111111..2222222 100644
--- "a/art/\360\237\244\226.svg"
+++ "b/art/\360\237\244\226.svg"
@@ -1 +1 @@
-alpha
+beta
'@

        $files = ConvertFrom-GitDiffText -Text $diff

        Get-CodePointText -Value $files[0]['path'] |
            Should -Be (Get-CodePointText -Value "art/$($script:RobotFace).svg")
    }

    It 'restores a path carrying an escaped quote and an escaped tab' {
        $diff = @'
diff --git "a/src/we\"ird\tname.rs" "b/src/we\"ird\tname.rs"
index 1111111..2222222 100644
--- "a/src/we\"ird\tname.rs"
+++ "b/src/we\"ird\tname.rs"
@@ -1 +1 @@
-alpha
+beta
'@

        $files = ConvertFrom-GitDiffText -Text $diff

        $files[0]['path'] | Should -Be "src/we`"ird`tname.rs"
    }

    It 'restores an escaped backslash as file-name text' {
        $diff = @'
diff --git "a/src/name\\with-backslash.rs" "b/src/name\\with-backslash.rs"
index 1111111..2222222 100644
--- "a/src/name\\with-backslash.rs"
+++ "b/src/name\\with-backslash.rs"
@@ -1 +1 @@
-alpha
+beta
'@

        $files = ConvertFrom-GitDiffText -Text $diff

        $files[0]['path'] | Should -Be 'src/name\with-backslash.rs'
    }

    It 'reports a rename whose paths contain spaces from its rename headers' {
        # A rename command header cannot distinguish spaces that separate paths from spaces inside
        # paths, so the rename headers are what a rename is read from.
        $files = ConvertFrom-GitDiffText -Text $script:PureRenameWithSpacesDiff

        $files.Count | Should -Be 1
        $files[0]['path'] | Should -Be 'docs/new name.md'
        $files[0]['previousPath'] | Should -Be 'docs/old name.md'
        $files[0]['status'] | Should -Be 'renamed'
        $files[0]['hunks'].Count | Should -Be 0
    }

    It 'keeps no previous path for a file that was only modified' {
        $files = ConvertFrom-GitDiffText -Text $script:ModifiedDiff

        $files[0]['previousPath'] | Should -BeNullOrEmpty
    }
}

Describe 'ss-review Git diff reading' {
    It 'maps exact native endpoints into the existing review result shape' {
        $files = ConvertFrom-GitDiffText -Text $script:ModifiedDiff
        Mock Invoke-SkillsGit {
            @{ oldCommit = $script:MergeBase; newCommit = $script:Source; files = $files }
        }
        $diff = Get-GitDiff -RepositoryPath 'C:\clone' -MergeBaseCommit $script:MergeBase -SourceCommit $script:Source

        $diff['mergeBaseCommit'] | Should -Be $script:MergeBase
        $diff['sourceCommit'] | Should -Be $script:Source
        $diff['files'].Count | Should -Be 1
        Should -Invoke Invoke-SkillsGit -Exactly 1 -ParameterFilter {
            ($ArgumentList -join '|') -eq "diff|--repository-path|C:\clone|--old-commit|$script:MergeBase|--new-commit|$script:Source"
        }
    }

    It 'decodes diff output as UTF-8 rather than as whatever the console would use' {
        # A path Git printed unquoted, carrying multi-byte UTF-8. Decoded through a single-byte code
        # page it would come back as two characters per byte, which is what this rejects.
        $path = "src/na$($script:LatinSmallIWithDiaeresis)ve.rs"
        $text = @"
diff --git a/$path b/$path
index 1111111..2222222 100644
--- a/$path
+++ b/$path
@@ -1 +1 @@
-alpha $($script:LatinSmallEWithAcute)
+beta $($script:RobotFace)
"@

        $files = ConvertFrom-GitDiffText -Text $text
        Get-CodePointText -Value $files[0]['path'] | Should -Be (Get-CodePointText -Value $path)
    }

    It 'reports a commit present in the repository and one that is not' {
        $invoker = New-FakeGitInvoker -PresentCommit @($script:MergeBase)

        Test-GitCommitPresent -Invoker $invoker -RepositoryPath 'C:\clone' -Commit $script:MergeBase | Should -BeTrue
        Test-GitCommitPresent -Invoker $invoker -RepositoryPath 'C:\clone' -Commit $script:Source | Should -BeFalse

        # The commit is asked for as a commit, so a tag or a tree of the same name is not mistaken
        # for one.
        $script:GitCalls[0][-1] | Should -Be "$($script:MergeBase)^{commit}"
    }

    It 'preserves a failing native comparison diagnostic' {
        Mock Invoke-SkillsGit { throw 'Git comparison failed: fatal: bad object' }
        { Get-GitDiff -RepositoryPath 'C:\clone' -MergeBaseCommit $script:MergeBase -SourceCommit $script:Source } |
            Should -Throw -ExpectedMessage '*Git comparison failed*bad object*'
    }

    It 'refuses an invoker that answers with something other than a dictionary' {
        $invoker = { param([string[]] $ArgumentList) 'git output' }

        { Test-GitCommitPresent -Invoker $invoker -RepositoryPath 'C:\clone' -Commit $script:Source } |
            Should -Throw -ExpectedMessage '*must return a dictionary*'
    }

    It 'refuses an invoker that omits a value the caller reads' {
        $invoker = { param([string[]] $ArgumentList) [ordered]@{ exitCode = 0; standardOutputBytes = [byte[]]::new(0) } }

        { Test-GitCommitPresent -Invoker $invoker -RepositoryPath 'C:\clone' -Commit $script:Source } |
            Should -Throw -ExpectedMessage "*no 'standardErrorText' value*"
    }

    It 'finds a changed file by its path and nothing by a path the diff does not carry' {
        $diff = @{ files = ConvertFrom-GitDiffText -Text $script:RenamedDiff }

        (Get-GitDiffFile -Diff $diff -Path 'src/new-name.rs')['status'] | Should -Be 'renamed'
        Get-GitDiffFile -Diff $diff -Path 'src/old-name.rs' | Should -BeNullOrEmpty
        Get-GitDiffFile -Diff $diff -Path '.\src\new-name.rs' | Should -BeNullOrEmpty
    }
}

Describe 'ss-review Git diff line membership' {
    BeforeAll {
        $script:ModifiedFile = (ConvertFrom-GitDiffText -Text $script:ModifiedDiff)[0]
        $script:AddedFile = (ConvertFrom-GitDiffText -Text $script:AddedDiff)[0]
        $script:DeletedFile = (ConvertFrom-GitDiffText -Text $script:DeletedDiff)[0]
    }

    It 'accepts a line inside a hunk on the post-change side' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 12 -Side 'right' | Should -BeTrue
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 17 -Side 'right' | Should -BeTrue
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 41 -Side 'right' | Should -BeTrue
    }

    It 'rejects a line outside every hunk on the post-change side' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 18 -Side 'right' | Should -BeFalse
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 9 -Side 'right' | Should -BeFalse
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 47 -Side 'right' | Should -BeFalse
    }

    It 'measures the pre-change side against the pre-change positions' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 16 -Side 'left' | Should -BeTrue
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 17 -Side 'left' | Should -BeFalse
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 40 -Side 'left' | Should -BeTrue
    }

    It 'takes the post-change side when none is named' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 41 | Should -BeTrue
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 40 | Should -BeFalse
    }

    It 'accepts a range that lies wholly within one hunk' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 11 -EndLine 15 -Side 'right' | Should -BeTrue
    }

    It 'rejects a range that runs past the end of the hunk it starts in' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 15 -EndLine 20 -Side 'right' | Should -BeFalse
    }

    It 'rejects a range that spans two hunks, because the text between them is not commentable' {
        Test-GitDiffLineInHunk -File $script:ModifiedFile -StartLine 12 -EndLine 44 -Side 'right' | Should -BeFalse
    }

    It 'reports nothing on the pre-change side of a file the pull request adds' {
        Test-GitDiffLineInHunk -File $script:AddedFile -StartLine 1 -Side 'left' | Should -BeFalse
        Test-GitDiffLineInHunk -File $script:AddedFile -StartLine 1 -Side 'right' | Should -BeTrue
    }

    It 'reports nothing on the post-change side of a file the pull request deletes' {
        Test-GitDiffLineInHunk -File $script:DeletedFile -StartLine 1 -Side 'right' | Should -BeFalse
        Test-GitDiffLineInHunk -File $script:DeletedFile -StartLine 2 -Side 'left' | Should -BeTrue
    }

    It 'reports nothing for a file whose change carried no hunks' {
        $file = (ConvertFrom-GitDiffText -Text $script:PureRenameDiff)[0]

        Test-GitDiffLineInHunk -File $file -StartLine 1 -Side 'right' | Should -BeFalse
    }
}
