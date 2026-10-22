BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:ExecutableRoot = Join-Path $script:RepoRoot 'skills\ss-review\scripts'
    $script:LibraryRoot = Join-Path $script:ExecutableRoot 'lib'

    # Libraries never dot-source one another, so every dependency is loaded here in dependency
    # order, the same order the executables use: value handling and failure signaling first, then
    # identity and target coordinates, each transport with the adapter that uses it, the shared
    # model, and finally the process primitives and the dispatch that selects between the two
    # platforms.
    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PublicationFailure.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'AdoRestTransport.ps1')
    . (Join-Path $script:LibraryRoot 'AdoPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubCliTransport.ps1')
    . (Join-Path $script:LibraryRoot 'GitHubPullRequestApi.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTransportHost.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestPlatform.ps1')

    # The host running this suite stands in for the programs the helpers launch. Naming the running
    # executable rather than the command 'pwsh' keeps the child on the same build as the parent and
    # removes any dependency on how the search path is configured.
    $script:PowerShellPath = (Get-Process -Id $PID).Path

    # --- Platform probe -----------------------------------------------------------------------

    # Records which platform primitive a dispatch reached, and answers from a route table.
    #
    # One probe watches both platforms, because a dispatch is judged on what it touched rather than
    # on what it produced: the branch that was not taken has to leave no trace, and only a probe
    # watching both branches can show that it left none.
    function New-PlatformProbe {
        [ordered]@{
            gitHubRequests           = [System.Collections.Generic.List[string]]::new()
            azureDevOpsRequests      = [System.Collections.Generic.List[string]]::new()
            azureDevOpsTokenRequests = [System.Collections.Generic.List[string]]::new()
            gitHubReplies            = [System.Collections.Generic.List[object]]::new()
            azureDevOpsReplies       = [System.Collections.Generic.List[object]]::new()
        }
    }

    # Registers a canned reply for every request whose command line, standard input or URL contains
    # the key. Matching is ordinal, so a path segment differing only in casing from another is a
    # different key; the Azure DevOps routes rely on that to tell the organization-level pull
    # request resource apart from the project-level one.
    function Add-ProbeReply {
        param(
            [Parameter(Mandatory)]
            [System.Collections.IDictionary] $Probe,

            [Parameter(Mandatory)]
            [ValidateSet('gitHub', 'azureDevOps')]
            [string] $Platform,

            [Parameter(Mandatory)]
            [string] $Key,

            [Parameter(Mandatory)]
            [string] $Body
        )

        $name = if ($Platform -eq 'gitHub') { 'gitHubReplies' } else { 'azureDevOpsReplies' }
        $Probe[$name].Add([ordered]@{ key = $Key; body = $Body })
    }

    # Builds one transport that satisfies both adapters.
    #
    # A dispatch test has to be able to answer either adapter, because the question it asks is which
    # adapter ran. Handing the untaken branch a transport it cannot read would let that branch fail
    # on the transport shape instead, and the test would then pass for a reason that has nothing to
    # do with the dispatch. Both constructors are used rather than a hand-written dictionary, so the
    # probe keeps whatever shape each transport requires. The keys the two share - the wait and the
    # attempt budget - carry the same meaning on both, so either value serves.
    function New-ProbeTransport {
        param(
            [Parameter(Mandatory)]
            [System.Collections.IDictionary] $Probe,

            [string] $CollectionUrl = 'https://dev.azure.com/contoso'
        )

        $invokeGh = {
            param(
                [string[]] $ArgumentList,
                [string] $StandardInput
            )

            $composite = ($ArgumentList -join ' ') + ' ' + [string] $StandardInput
            $Probe['gitHubRequests'].Add($composite)

            # Later registrations win, which is what lets a request for a sub-resource be told apart
            # from the request for the resource whose path it extends.
            for ($index = $Probe['gitHubReplies'].Count - 1; $index -ge 0; $index--) {
                $reply = $Probe['gitHubReplies'][$index]
                if ($composite.Contains([string] $reply['key'], [StringComparison]::Ordinal)) {
                    return [ordered]@{
                        exitCode            = 0
                        standardOutputBytes = [System.Text.UTF8Encoding]::new($false).GetBytes([string] $reply['body'])
                        standardErrorText   = ''
                    }
                }
            }

            throw "The GitHub stand-in has no reply for 'gh $($ArgumentList -join ' ')'."
        }.GetNewClosure()

        $sendRequest = {
            param(
                [string] $Method,
                [string] $Url,
                [hashtable] $Headers,
                $Body
            )

            $Probe['azureDevOpsRequests'].Add("$Method $Url")

            for ($index = $Probe['azureDevOpsReplies'].Count - 1; $index -ge 0; $index--) {
                $reply = $Probe['azureDevOpsReplies'][$index]
                if ($Url.Contains([string] $reply['key'], [StringComparison]::Ordinal)) {
                    return [ordered]@{
                        statusCode   = 200
                        headers      = @{}
                        contentType  = 'application/json; charset=utf-8'
                        contentBytes = [System.Text.UTF8Encoding]::new($false).GetBytes([string] $reply['body'])
                    }
                }
            }

            throw "The Azure DevOps stand-in has no reply for '$Url'."
        }.GetNewClosure()

        $getAccessToken = {
            $Probe['azureDevOpsTokenRequests'].Add('acquire')
            'stand-in-token'
        }.GetNewClosure()

        # A single attempt means no reply can provoke a retry, so no test can reach a wait. The wait
        # is injected regardless, so that a fixture which somehow did reach one would return at once
        # rather than put the suite on a real clock.
        $noWait = {
            param([int] $Milliseconds)
        }

        $gitHub = New-GitHubCliTransport -InvokeGh $invokeGh -Wait $noWait -MaxAttempts 1
        $azureDevOps = New-AdoRestTransport `
            -CollectionUrl $CollectionUrl `
            -SendRequest $sendRequest `
            -GetAccessToken $getAccessToken `
            -Wait $noWait `
            -MaxAttempts 1

        $transport = [ordered]@{}
        foreach ($key in $gitHub.Keys) { $transport[$key] = $gitHub[$key] }
        foreach ($key in $azureDevOps.Keys) { $transport[$key] = $azureDevOps[$key] }
        $transport
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

            [string[]] $ArgumentList = @()
        )

        $executablePath = Join-Path $script:ExecutableRoot $Name

        # A path pwsh cannot open produces a usage banner on stdout and exit 64, which reads exactly
        # like an executable that failed to keep stdout clean. Naming the missing file instead
        # attributes the failure to a renamed or moved executable in one step.
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

            # Both streams are drained concurrently, for the same reason the production invoker
            # does it: a refusal writes a formatted error record, and reading one stream to its end
            # before starting on the other stalls as soon as either outgrows a pipe buffer.
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
    # the message is long - and these messages quote a full path, so they always are. The view
    # breaks only between words, so removing the decoration and collapsing runs of whitespace
    # restores the message as it was written.
    function Get-SingleLineErrorText {
        param(
            [AllowEmptyString()]
            [AllowNull()]
            [string] $Text
        )

        (((([string] $Text) -replace '\x1b\[[0-9;]*m', '') -replace '(?m)^\s*\|', ' ') -replace '\s+', ' ').Trim()
    }
}

Describe 'Platform dispatch' {
    BeforeAll {
        $script:GitHubTarget = [ordered]@{
            platform      = 'gitHub'
            collectionUrl = $null
            organization  = 'octo'
            project       = $null
            repository    = 'widget'
            pullRequestId = 7
            webUrl        = $null
        }

        # The project and the repository are deliberately absent. A caller may name nothing but the
        # collection and the identifier, and the Azure DevOps adapter is the only thing that can
        # resolve the rest during capture.
        $script:AzureDevOpsTarget = [ordered]@{
            platform      = 'azureDevOps'
            collectionUrl = 'https://dev.azure.com/contoso'
            organization  = 'contoso'
            project       = $null
            repository    = $null
            pullRequestId = 42
            webUrl        = $null
        }

    }

    Context 'capture' {
        # The probe answers nothing, so the capture stops at its first read. Which primitive that
        # read reached is the entire question, and the probe has already recorded it by then, so a
        # complete fixture for either adapter would add nothing this assertion does not already
        # make. The sibling platform's counters are asserted to be untouched, which is what fails
        # when the branch is inverted.
        It 'reads a GitHub pull request through the GitHub CLI and nothing else' {
            $probe = New-PlatformProbe
            $transport = New-ProbeTransport -Probe $probe

            { Get-PullRequestContextForTarget -Transport $transport -Target $script:GitHubTarget } | Should -Throw

            $probe['gitHubRequests'].Count | Should -BeGreaterThan 0
            $probe['azureDevOpsRequests'].Count | Should -Be 0
            $probe['azureDevOpsTokenRequests'].Count | Should -Be 0
        }

        It 'reads an Azure DevOps pull request over REST and nothing else' {
            $probe = New-PlatformProbe
            $transport = New-ProbeTransport -Probe $probe

            { Get-PullRequestContextForTarget -Transport $transport -Target $script:AzureDevOpsTarget } | Should -Throw

            $probe['azureDevOpsRequests'].Count | Should -BeGreaterThan 0
            $probe['gitHubRequests'].Count | Should -Be 0
        }

        It 'names the GitHub pull request the target identifies' {
            $probe = New-PlatformProbe
            $transport = New-ProbeTransport -Probe $probe

            { Get-PullRequestContextForTarget -Transport $transport -Target $script:GitHubTarget } | Should -Throw

            # Lowercased before matching so that a path segment restyled by the adapter cannot fail
            # a test that is about the coordinates the read carries, not about their spelling. The
            # identifier is anchored against a following digit, so a read of a neighbouring pull
            # request cannot satisfy the assertion.
            $requests = ($probe['gitHubRequests'] -join "`n").ToLowerInvariant()
            $requests | Should -Match 'repos/octo/widget/pulls/7(?![0-9])'
        }

        It 'names the Azure DevOps pull request the target identifies' {
            $probe = New-PlatformProbe
            $transport = New-ProbeTransport -Probe $probe

            { Get-PullRequestContextForTarget -Transport $transport -Target $script:AzureDevOpsTarget } | Should -Throw

            $requests = ($probe['azureDevOpsRequests'] -join "`n").ToLowerInvariant()
            $requests | Should -Match 'https://dev\.azure\.com/contoso/'
            $requests | Should -Match 'pullrequests/42(?![0-9])'
        }
    }

    Context 'transport construction' {
        BeforeAll {
            $script:StandInInvoker = {
                param(
                    [string[]] $ArgumentList,
                    [string] $StandardInput
                )
            }

            $script:StandInSender = {
                param(
                    [string] $Method,
                    [string] $Url,
                    [hashtable] $Headers,
                    $Body
                )
            }

            $script:StandInTokenProvider = { 'stand-in-token' }
        }

        It 'builds a GitHub CLI transport for a GitHub target' {
            $transport = New-PullRequestTransport -Target $script:GitHubTarget -InvokeGh $script:StandInInvoker

            $transport.Contains('invokeGh') | Should -BeTrue
            $transport.Contains('sendRequest') | Should -BeFalse
            [object]::ReferenceEquals($transport['invokeGh'], $script:StandInInvoker) | Should -BeTrue
        }

        It 'builds an Azure DevOps REST transport for an Azure DevOps target' {
            $transport = New-PullRequestTransport `
                -Target $script:AzureDevOpsTarget `
                -SendRequest $script:StandInSender `
                -GetAccessToken $script:StandInTokenProvider

            $transport.Contains('sendRequest') | Should -BeTrue
            $transport.Contains('invokeGh') | Should -BeFalse
            $transport['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/contoso'
            [object]::ReferenceEquals($transport['sendRequest'], $script:StandInSender) | Should -BeTrue
            [object]::ReferenceEquals($transport['getAccessToken'], $script:StandInTokenProvider) | Should -BeTrue
        }

        # Each branch resolves its own primitives and only when it has not been given them, so that
        # capturing a GitHub pull request never reaches the Azure CLI and capturing an Azure DevOps
        # pull request never reaches the GitHub CLI. Neither factory has an effect a result can be
        # read for, so the property is stated as the only thing it can be: that the factory on the
        # untaken branch is never called.
        It 'resolves no Azure DevOps primitive for a GitHub target' {
            Mock Get-GitHubCliInvoker { $script:StandInInvoker }
            Mock Get-HttpRequestSender { $script:StandInSender }
            Mock Get-AzureDevOpsTokenProvider { $script:StandInTokenProvider }

            New-PullRequestTransport -Target $script:GitHubTarget | Out-Null

            Should -Invoke Get-GitHubCliInvoker -Times 1 -Exactly
            Should -Invoke Get-HttpRequestSender -Times 0 -Exactly
            Should -Invoke Get-AzureDevOpsTokenProvider -Times 0 -Exactly
        }

        It 'resolves no GitHub primitive for an Azure DevOps target' {
            Mock Get-GitHubCliInvoker { $script:StandInInvoker }
            Mock Get-HttpRequestSender { $script:StandInSender }
            Mock Get-AzureDevOpsTokenProvider { $script:StandInTokenProvider }

            New-PullRequestTransport -Target $script:AzureDevOpsTarget | Out-Null

            Should -Invoke Get-HttpRequestSender -Times 1 -Exactly
            Should -Invoke Get-AzureDevOpsTokenProvider -Times 1 -Exactly
            Should -Invoke Get-GitHubCliInvoker -Times 0 -Exactly
        }

        It 'resolves nothing at all when every primitive is supplied' {
            Mock Get-GitHubCliInvoker { $script:StandInInvoker }
            Mock Get-HttpRequestSender { $script:StandInSender }
            Mock Get-AzureDevOpsTokenProvider { $script:StandInTokenProvider }

            New-PullRequestTransport -Target $script:GitHubTarget -InvokeGh $script:StandInInvoker | Out-Null
            New-PullRequestTransport `
                -Target $script:AzureDevOpsTarget `
                -SendRequest $script:StandInSender `
                -GetAccessToken $script:StandInTokenProvider | Out-Null

            Should -Invoke Get-GitHubCliInvoker -Times 0 -Exactly
            Should -Invoke Get-HttpRequestSender -Times 0 -Exactly
            Should -Invoke Get-AzureDevOpsTokenProvider -Times 0 -Exactly
        }
    }

    # A platform outside the vocabulary is refused rather than served by whichever adapter the
    # dispatch happens to fall through to. Served silently, a misspelling would be answered by the
    # Azure DevOps adapter and reported as an authentication failure against a collection the caller
    # never named, which describes neither the cause nor where to look for it.
    Context 'unsupported platform' {
        BeforeAll {
            $script:UnsupportedTarget = [ordered]@{
                platform      = 'githup'
                collectionUrl = $null
                project       = $null
                repository    = 'octo/widget'
                pullRequestId = 7
            }

            # A target carrying no platform at all reaches the same refusal, because indexing a
            # dictionary for an absent key yields null rather than failing under strict mode.
            $script:PlatformlessTarget = [ordered]@{
                repository    = 'octo/widget'
                pullRequestId = 7
            }
        }

        It 'refuses to build a transport for an unrecognised platform' {
            $thrown = { New-PullRequestTransport -Target $script:UnsupportedTarget } | Should -Throw -PassThru

            $thrown.Exception.Message.Contains('githup', [StringComparison]::Ordinal) | Should -BeTrue
            $thrown.Exception.Message.Contains('azureDevOps', [StringComparison]::Ordinal) | Should -BeTrue
            $thrown.Exception.Message.Contains('gitHub', [StringComparison]::Ordinal) | Should -BeTrue
        }

        # The refusal is what keeps an unrecognised platform from reaching for an Azure DevOps
        # token, which is the failure the guard exists to prevent rather than merely report.
        It 'resolves no platform primitive for an unrecognised platform' {
            Mock Get-GitHubCliInvoker { $script:StandInInvoker }
            Mock Get-HttpRequestSender { $script:StandInSender }
            Mock Get-AzureDevOpsTokenProvider { $script:StandInTokenProvider }

            { New-PullRequestTransport -Target $script:UnsupportedTarget } | Should -Throw

            Should -Invoke Get-GitHubCliInvoker -Times 0 -Exactly
            Should -Invoke Get-HttpRequestSender -Times 0 -Exactly
            Should -Invoke Get-AzureDevOpsTokenProvider -Times 0 -Exactly
        }

        It 'refuses to capture a pull request on an unrecognised platform' {
            $probe = New-PlatformProbe
            $transport = New-ProbeTransport -Probe $probe

            $thrown = { Get-PullRequestContextForTarget -Transport $transport -Target $script:UnsupportedTarget } |
                Should -Throw -PassThru

            $thrown.Exception.Message.Contains('githup', [StringComparison]::Ordinal) | Should -BeTrue
            $probe['gitHubRequests'].Count | Should -Be 0
            $probe['azureDevOpsRequests'].Count | Should -Be 0
        }

        It 'refuses a target carrying no platform at all' {
            $thrown = { New-PullRequestTransport -Target $script:PlatformlessTarget } | Should -Throw -PassThru

            $thrown.Exception.Message.Contains('none', [StringComparison]::Ordinal) | Should -BeTrue
            $thrown.Exception.Message.Contains('azureDevOps', [StringComparison]::Ordinal) | Should -BeTrue
        }
    }
}

Describe 'GitHub CLI process plumbing' {
    BeforeAll {
        $script:Invoker = Get-GitHubCliInvoker -FileName $script:PowerShellPath

        # Windows gives an anonymous pipe a buffer of a few kilobytes, and the stream layered over
        # it adds one of its own. This payload exceeds both by orders of magnitude, so a response of
        # this size cannot be produced by an implementation that finishes writing standard input
        # before it starts draining what the child has already written.
        $script:PipeOverflowByteCount = 512 * 1024

        # Writes the overflow payload to both output streams before reading anything, so the child
        # fills the pipes while the caller is still writing to it. The stream fill bytes render as
        # 'o' and 'e', which tells the two streams apart in a failure, and both are plain ASCII, so
        # the decoded error text is exactly as long as the byte count written.
        $script:OverflowProgram = @'
$chunkLength = 1024
$outputChunk = [byte[]]::new($chunkLength)
$errorChunk = [byte[]]::new($chunkLength)
for ($index = 0; $index -lt $chunkLength; $index++) {
    $outputChunk[$index] = 111
    $errorChunk[$index] = 101
}
$standardOutput = [Console]::OpenStandardOutput()
$standardError = [Console]::OpenStandardError()
for ($written = 0; $written -lt __BYTE_COUNT__; $written += $chunkLength) {
    $standardOutput.Write($outputChunk, 0, $chunkLength)
    $standardError.Write($errorChunk, 0, $chunkLength)
}
$standardOutput.Flush()
$standardError.Flush()
[Console]::In.ReadToEnd().Length | Out-Null
'@

        # Emits a two-byte, a three-byte and a four-byte UTF-8 sequence. The characters are built
        # from their code points here as well as in the assertion, so a file whose own encoding was
        # damaged fails rather than comparing two identically damaged strings, and the bytes are
        # written to the raw standard output stream so the child's console encoding cannot alter
        # them.
        $script:NonAsciiProgram = @'
$text = [char]::ConvertFromUtf32(0x00E4) + [char]::ConvertFromUtf32(0x2014) + [char]::ConvertFromUtf32(0x1F916)
$bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($text)
$standardOutput = [Console]::OpenStandardOutput()
$standardOutput.Write($bytes, 0, $bytes.Length)
$standardOutput.Flush()
'@

        $script:EchoProgram = @'
$text = [Console]::In.ReadToEnd()
$bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($text)
$standardOutput = [Console]::OpenStandardOutput()
$standardOutput.Write($bytes, 0, $bytes.Length)
$standardOutput.Flush()
'@

        function Invoke-StandInProgram {
            param(
                [Parameter(Mandatory)]
                [string] $Program,

                [AllowNull()]
                [string] $StandardInput
            )

            & $script:Invoker -ArgumentList @('-NoProfile', '-NonInteractive', '-Command', $Program) -StandardInput $StandardInput
        }
    }

    It 'reports the exit code the program ended with' {
        # A code that is neither success nor the value an unhandled failure produces, so neither can
        # be mistaken for it.
        $result = Invoke-StandInProgram -Program 'exit 3'

        $result['exitCode'] | Should -Be 3
    }

    It 'captures what the program wrote to standard error' {
        $result = Invoke-StandInProgram -Program '[Console]::Error.Write("stand-in diagnostics")'

        $result['standardErrorText'] | Should -BeExactly 'stand-in diagnostics'
    }

    It 'delivers standard input to the program' {
        $result = Invoke-StandInProgram -Program $script:EchoProgram -StandardInput 'payload-from-the-caller'

        [System.Text.Encoding]::UTF8.GetString($result['standardOutputBytes']) | Should -BeExactly 'payload-from-the-caller'
    }

    # A regression here does not produce a wrong answer, it produces no answer: both processes stop
    # on a full pipe and neither can proceed. The payload is therefore far above the buffer size
    # rather than near it, so the outcome cannot depend on how a particular host sizes its pipes.
    It 'returns output and error larger than a pipe buffer while writing standard input' {
        $program = $script:OverflowProgram.Replace('__BYTE_COUNT__', [string] $script:PipeOverflowByteCount)
        $result = Invoke-StandInProgram -Program $program -StandardInput ('i' * $script:PipeOverflowByteCount)

        $result['standardOutputBytes'].Length | Should -Be $script:PipeOverflowByteCount
        $result['standardErrorText'].Length | Should -Be $script:PipeOverflowByteCount
    }

    # Awaiting a non-generic task yields a result value that would land on the pipeline beside the
    # dictionary, and the callers of this invoker index into what it returns. An array of two would
    # not fail there; it would report an absent exit code and an empty response.
    It 'returns exactly one dictionary' {
        $result = Invoke-StandInProgram -Program 'exit 0'

        $result -is [object[]] | Should -BeFalse
        $result -is [System.Collections.IDictionary] | Should -BeTrue
        @($result).Count | Should -Be 1
    }

    It 'returns standard output bytes undecoded' {
        $expected = [char]::ConvertFromUtf32(0x00E4) + [char]::ConvertFromUtf32(0x2014) + [char]::ConvertFromUtf32(0x1F916)
        $expectedBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($expected)

        $result = Invoke-StandInProgram -Program $script:NonAsciiProgram

        # Compared as base64 so that a failure names two ASCII strings, which no console encoding
        # between here and the report can alter.
        [Convert]::ToBase64String($result['standardOutputBytes']) |
            Should -BeExactly ([Convert]::ToBase64String($expectedBytes))
        [System.Text.Encoding]::UTF8.GetString($result['standardOutputBytes']) | Should -BeExactly $expected
    }
}

Describe 'Executable refusals' {
    BeforeAll {
        $script:WorkRoot = Join-Path ([System.IO.Path]::GetTempPath()) "ss-review-executables-$([guid]::NewGuid())"
        New-Item -ItemType Directory -Path $script:WorkRoot | Out-Null

        # A GitHub pull request URL resolves to a complete target by parsing alone, so a run can
        # reach the checks that precede the transport without any account being reachable.
        $script:TargetUrl = 'https://github.com/octo/widget/pull/7'
    }

    AfterAll {
        if (Test-Path -LiteralPath $script:WorkRoot) {
            Remove-Item -LiteralPath $script:WorkRoot -Recurse -Force
        }
    }

    BeforeEach {
        $script:OutputDirectory = Join-Path $script:WorkRoot ([guid]::NewGuid().ToString())
        New-Item -ItemType Directory -Path $script:OutputDirectory | Out-Null
    }

    Context 'comparison baseline' {
        It 'refuses a comparison against the file the run is about to write' {
            $contextPath = Join-Path $script:OutputDirectory 'pr-context.json'

            $run = Invoke-SkillExecutable -Name 'Get-PullRequestContext.ps1' -ArgumentList @(
                '-Url', $script:TargetUrl,
                '-OutputDirectory', $script:OutputDirectory,
                '-ComparePath', $contextPath)

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains('distinct -OutputPrefix', [StringComparison]::Ordinal) |
                Should -BeTrue
            Get-ChildItem -LiteralPath $script:OutputDirectory -Force | Should -BeNullOrEmpty
        }

        It 'refuses a comparison against a capture of another schema version' {
            $stalePath = Join-Path $script:WorkRoot "stale-$([guid]::NewGuid()).json"
            # A version this helper does not read, stated as a literal so the refusal can be
            # recognized by the version it names rather than by prose that may be reworded.
            [System.IO.File]::WriteAllText($stalePath, '{"schemaVersion":99,"threads":[]}', [System.Text.UTF8Encoding]::new($false))

            $run = Invoke-SkillExecutable -Name 'Get-PullRequestContext.ps1' -ArgumentList @(
                '-Url', $script:TargetUrl,
                '-OutputDirectory', $script:OutputDirectory,
                '-ComparePath', $stalePath)

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains('version 99', [StringComparison]::Ordinal) |
                Should -BeTrue
            Get-ChildItem -LiteralPath $script:OutputDirectory -Force | Should -BeNullOrEmpty
        }
    }

    Context 'missing arguments' {
        # The executables the review phases invoke. The focus area cache helpers that share
        # this directory answer to a different design and are covered by their own suite.
        #
        # Named rather than enumerated, so that an executable renamed out from under this suite
        # fails the run instead of quietly dropping out of the enumeration and leaving a passing
        # suite that covers one command fewer than it claims.
        #
        # Each expects the refusal its own missing argument produces. Accepting any message at all
        # would also accept a run that got as far as reaching for an account and failed there, which
        # is the failure this assertion exists to distinguish from a clean refusal.
        It 'refuses <name> invoked with no arguments' -ForEach @(
            @{ name = 'Get-PullRequestContext.ps1'; refusal = 'missing mandatory parameters: OutputDirectory' }
            @{ name = 'Set-CommentAuthorship.ps1'; refusal = 'missing mandatory parameters: Path' }
        ) {
            $run = Invoke-SkillExecutable -Name $name

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains($refusal, [StringComparison]::Ordinal) |
                Should -BeTrue
        }

        It 'refuses a classification supplied both inline and by file' {
            $capturePath = Join-Path $script:OutputDirectory 'pr-context.json'

            $run = Invoke-SkillExecutable -Name 'Set-CommentAuthorship.ps1' -ArgumentList @(
                '-Path', $capturePath,
                '-SnapshotId', 'unused-before-payload-validation',
                '-Classification', '[]',
                '-ClassificationPath', (Join-Path $script:OutputDirectory 'classification.json'))

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains('not both', [StringComparison]::Ordinal) |
                Should -BeTrue
        }

        It 'refuses a classification supplied neither inline nor by file' {
            $capturePath = Join-Path $script:OutputDirectory 'pr-context.json'

            $run = Invoke-SkillExecutable -Name 'Set-CommentAuthorship.ps1' -ArgumentList @('-Path', $capturePath, '-SnapshotId', 'unused-before-payload-validation')

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains('-Classification or -ClassificationPath', [StringComparison]::Ordinal) |
                Should -BeTrue
        }

        # A payload that parses but is not an array is refused before the capture named by -Path is
        # opened, so this argument check needs neither a capture on disk nor a network. The happy
        # path of this executable belongs to its own suite; only the refusal is claimed here.
        It 'refuses a classification payload that is not a JSON array' {
            $capturePath = Join-Path $script:OutputDirectory 'pr-context.json'

            $run = Invoke-SkillExecutable -Name 'Set-CommentAuthorship.ps1' -ArgumentList @(
                '-Path', $capturePath,
                '-SnapshotId', 'unused-before-payload-validation',
                '-Classification', '{"threadId":1}')

            $run['exitCode'] | Should -Not -Be 0
            $run['standardOutput'] | Should -BeExactly ''
            (Get-SingleLineErrorText -Text $run['standardError']).Contains('must be a JSON array', [StringComparison]::Ordinal) |
                Should -BeTrue

            # The refusal precedes every write, so the capture the run was pointed at stays absent.
            Test-Path -LiteralPath $capturePath | Should -BeFalse
        }
    }
}
