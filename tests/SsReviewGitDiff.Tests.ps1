#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source

    # Pass bytes to the executable directly so console encodings cannot change the diff.
    function Invoke-NativeGitParse {
        param([byte[]] $Bytes)

        $start = [Diagnostics.ProcessStartInfo]::new($script:Native)
        $start.UseShellExecute = $false
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        foreach ($argument in @('git', 'parse')) { $start.ArgumentList.Add($argument) }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.StandardInput.BaseStream.Write($Bytes, 0, $Bytes.Length)
            $process.StandardInput.Close()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }

    function Invoke-NativeGitText {
        param([AllowEmptyString()] [string] $Text)

        $result = Invoke-NativeGitParse -Bytes ([Text.UTF8Encoding]::new($false).GetBytes($Text))
        $result.exitCode | Should -Be 0 -Because $result.error
        ConvertFrom-Json -InputObject $result.output -AsHashtable
    }
}

Describe 'Native Git diff parsing' {
    It 'retains added, deleted, modified and renamed file evidence' {
        $diff = @'
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,2 +10,3 @@
 context
-old
+new
+extra
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+new
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-old
diff --git a/docs/old name.md b/docs/new name.md
similarity index 100%
rename from docs/old name.md
rename to docs/new name.md
'@
        $files = (Invoke-NativeGitText -Text $diff).files
        $files.Count | Should -Be 4
        $files[0].path | Should -BeExactly 'src/lib.rs'
        $files[0].status | Should -BeExactly 'modified'
        $files[0].previousPath | Should -BeNullOrEmpty
        $files[0].hunks[0].oldStart | Should -Be 10
        $files[0].hunks[0].oldCount | Should -Be 2
        $files[0].hunks[0].newCount | Should -Be 3
        $files[1].status | Should -BeExactly 'added'
        $files[1].hunks[0].oldCount | Should -Be 0
        $files[2].status | Should -BeExactly 'deleted'
        $files[2].hunks[0].newCount | Should -Be 0
        $files[3].path | Should -BeExactly 'docs/new name.md'
        $files[3].previousPath | Should -BeExactly 'docs/old name.md'
        $files[3].status | Should -BeExactly 'renamed'
        $files[3].hunks.Count | Should -Be 0
    }

    It 'preserves Git-quoted UTF-8 bytes and escaped path characters' -ForEach @(
        @{ Encoded = '"src/caf\303\251.rs"'; Expected = 'src/café.rs' }
        @{ Encoded = '"art/\360\237\244\226.svg"'; Expected = 'art/🤖.svg' }
        @{ Encoded = '"src/we\"ird\tname.rs"'; Expected = "src/we`"ird`tname.rs" }
        @{ Encoded = '"src/name\\with-backslash.rs"'; Expected = 'src/name\with-backslash.rs' }
    ) {
        $old = '"a/' + $Encoded.Substring(1)
        $new = '"b/' + $Encoded.Substring(1)
        $diff = "diff --git $old $new`n--- $old`n+++ $new`n@@ -1 +1 @@`n-before`n+after`n"
        $files = (Invoke-NativeGitText -Text $diff).files
        $files.Count | Should -Be 1
        $files[0].path | Should -BeExactly $Expected
    }

    It 'reads unquoted UTF-8 paths and supplementary-plane content without console conversion' {
        $path = 'src/naïve🤖.rs'
        $diff = "diff --git a/$path b/$path`n--- a/$path`n+++ b/$path`n@@ -1 +1 @@`n-café`n+🤖`n"
        $files = (Invoke-NativeGitText -Text $diff).files
        $files[0].path | Should -BeExactly $path
        $files[0].hunks[0].oldCount | Should -Be 1
        $files[0].hunks[0].newCount | Should -Be 1
    }

    It 'does not interpret header-shaped lines inside a hunk as file headers' {
        $diff = "diff --git a/file b/file`n--- a/file`n+++ b/file`n@@ -1 +1 @@`n--- a/not-a-file`n+++ b/not-a-file`n"
        $files = (Invoke-NativeGitText -Text $diff).files
        $files.Count | Should -Be 1
        $files[0].path | Should -BeExactly 'file'
    }

    It 'returns no files for an empty comparison' {
        $files = (Invoke-NativeGitText -Text '').files
        $files.Count | Should -Be 0
    }

    It 'rejects malformed UTF-8 without reporting successful output' {
        $result = Invoke-NativeGitParse -Bytes ([byte[]] @(0xff))
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        $result.error | Should -Not -BeNullOrEmpty
    }
}
