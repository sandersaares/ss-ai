#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Publisher = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../skills/ss-review/scripts/Publish-ReviewTrackerItems.ps1'))
    $script:PowerShell = (Get-Process -Id $PID).Path
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:Root = Join-Path $PSScriptRoot ('.native-tracker-' + [guid]::NewGuid().ToString('N'))
    $script:Bin = Join-Path $script:Root 'provider bin'
    [void] [IO.Directory]::CreateDirectory($script:Bin)

    if (-not $IsWindows) {
        # The provider records requests locally and never delegates to a real CLI.
        $fake = @'
#Requires -Version 7.6
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
[IO.File]::WriteAllText($env:TRACKER_PROVIDER_MARKER, 'called')
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json -AsHashtable
$query = $request.query
if ($query.Contains('createIssue(')) {
    $journal = [IO.File]::ReadAllText($env:TRACKER_JOURNAL) | ConvertFrom-Json -AsHashtable
    if ($journal.status -cne 'inProgress' -or $journal.operations[0].outcome -cne 'ambiguous') { throw 'Missing durable creation intent' }
    $item = @{id='node'; number=42; url='https://github.com/acme/tracker/issues/42'; title=$request.variables.title; body=$request.variables.body}
    [IO.File]::WriteAllText($env:TRACKER_ITEM, (ConvertTo-Json -InputObject $item -Compress), [Text.UTF8Encoding]::new($false))
    $response = @{data=@{createIssue=@{issue=$item}}}
} elseif ($query.Contains('node(id:')) {
    $journal = [IO.File]::ReadAllText($env:TRACKER_JOURNAL) | ConvertFrom-Json -AsHashtable
    if ($journal.operations[0].outcome -cne 'succeeded') { throw 'Missing durable receipt' }
    $item = [IO.File]::ReadAllText($env:TRACKER_ITEM) | ConvertFrom-Json -AsHashtable
    if ($env:TRACKER_MISMATCH -ceq '1') { $item.body = 'different' }
    $response = @{data=@{node=$item}}
} else {
    $response = @{data=@{repository=@{id='repo'; nameWithOwner='acme/tracker'; hasIssuesEnabled=$true; isArchived=$false; viewerPermission='WRITE'}}}
}
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::Write("HTTP/2 200`n`n" + (ConvertTo-Json -InputObject $response -Depth 20 -Compress))
'@
        $path = Join-Path $script:Bin 'gh'
        [IO.File]::WriteAllText($path, "#!$script:PowerShell`n$fake", [Text.UTF8Encoding]::new($false))
        [IO.File]::SetUnixFileMode($path, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
        $az = Join-Path $script:Bin 'az'
        [IO.File]::WriteAllText($az, "#!$script:PowerShell`n$fake", [Text.UTF8Encoding]::new($false))
        [IO.File]::SetUnixFileMode($az, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserWrite -bor [IO.UnixFileMode]::UserExecute)
    }

    # A child process exercises adapter discovery, streams, exit status, and cwd.
    function Invoke-TrackerAdapter {
        param([string] $Directory, [switch] $Mismatch, [switch] $Malformed)

        [void] [IO.Directory]::CreateDirectory($Directory)
        $finding = @{schemaVersion=1; id='F'; sourceFindingIds=@('F'); origin='reviewer'; title='Finding 雪 😀';
            description='Problem café 雪'; reasoning='Reason'; consequence='Impact'; recommendedAction='Fix';
            anchor=@{kind='file'; path='src/lib.rs'}}
        $approved = @{schemaVersion=2; target=$null; reviewedSourceCommit=$null; reviewedMergeBaseCommit=$null;
            reviewedContextHash=$null; findings=@($finding)}
        $approvedPath = Join-Path $Directory 'approved 雪.json'
        [IO.File]::WriteAllText($approvedPath, $(if ($Malformed) { '{' } else { ConvertTo-Json -InputObject $approved -Depth 20 -Compress }), [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText((Join-Path $Directory 'diagnostics 雪.json'), '{}', [Text.UTF8Encoding]::new($false))
        $start = [Diagnostics.ProcessStartInfo]::new($script:PowerShell)
        $start.WorkingDirectory = $Directory
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false, $true)
        $start.Environment['PATH'] = $script:Bin + [IO.Path]::PathSeparator + (Split-Path -Parent $script:Native)
        $start.Environment['TRACKER_ITEM'] = Join-Path $Directory 'created.json'
        $start.Environment['TRACKER_PROVIDER_MARKER'] = Join-Path $Directory 'provider-called'
        $start.Environment['TRACKER_JOURNAL'] = Join-Path $Directory 'output 雪/tracker-publication-result.json'
        $start.Environment['TRACKER_MISMATCH'] = if ($Mismatch) { '1' } else { '0' }
        foreach ($arg in @('-NoProfile', '-NonInteractive', '-File', $script:Publisher,
            '-Url', 'https://github.com/acme/tracker', '-ApprovedFindingsPath', 'approved 雪.json',
            '-DiagnosticsPath', 'diagnostics 雪.json', '-OutputDirectory', 'output 雪')) {
            $start.ArgumentList.Add($arg)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            return @{ exitCode=$process.ExitCode; output=$stdout.GetAwaiter().GetResult(); error=$stderr.GetAwaiter().GetResult() }
        } finally { $process.Dispose() }
    }
}

AfterAll {
    if ([IO.Directory]::Exists($script:Root)) { [IO.Directory]::Delete($script:Root, $true) }
}

# Windows cannot execute the script-based fake gh directly as a native executable.
Describe 'Real native tracker publication adapter' -Skip:$IsWindows {
    It 'publishes and reads back UTF-8 through the actual native transaction' {
        $work = Join-Path $script:Root 'success 雪'
        $result = Invoke-TrackerAdapter -Directory $work
        $result.exitCode | Should -Be 0 -Because $result.error
        $outcome = ConvertFrom-Json -InputObject $result.output -AsHashtable
        $outcome.status | Should -BeExactly 'succeeded'
        $outcome.publishedItemCount | Should -Be 1
        $item = [IO.File]::ReadAllText((Join-Path $work 'created.json')) | ConvertFrom-Json -AsHashtable
        $item.title | Should -Match '雪 😀'
        $item.body | Should -Match 'café 雪'
        $journal = [IO.File]::ReadAllText((Join-Path $work 'output 雪/tracker-publication-result.json')) | ConvertFrom-Json -AsHashtable
        $journal.status | Should -BeExactly 'succeeded'
        $journal.verification[0].itemId | Should -BeExactly '42'
    }

    It 'emits a failed outcome with durable creation evidence before exiting nonzero' {
        $work = Join-Path $script:Root 'failure 雪'
        $result = Invoke-TrackerAdapter -Directory $work -Mismatch
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Match 'exit code'
        $outcome = ConvertFrom-Json -InputObject $result.output -AsHashtable
        $outcome.status | Should -BeExactly 'failed'
        $outcome.publishedItemCount | Should -Be 1
        $journal = [IO.File]::ReadAllText((Join-Path $work 'output 雪/tracker-publication-result.json')) | ConvertFrom-Json -AsHashtable
        $journal.status | Should -BeExactly 'failed'
        $journal.operations[0].outcome | Should -BeExactly 'succeeded'
        $journal.operations[1].outcome | Should -BeExactly 'failed'
    }

    It 'rejects malformed local input before contacting a provider' {
        $work = Join-Path $script:Root 'invalid 雪'
        $result = Invoke-TrackerAdapter -Directory $work -Malformed
        $result.exitCode | Should -Not -Be 0
        $result.output | Should -BeNullOrEmpty
        [IO.File]::Exists((Join-Path $work 'provider-called')) | Should -BeFalse
        [IO.File]::Exists((Join-Path $work 'created.json')) | Should -BeFalse
    }
}
