#Requires -Version 7.0

# Production implementations of the primitives the transports depend on
# (docs/implementation.md, "Transport boundaries").
#
# The transports take the HTTP call, the token acquisition and the `gh` invocation as injected
# script blocks so that the whole test suite runs offline. That injection needs something real to
# inject in production, and both executables need the same real thing, so it lives here rather than
# being written twice in two composition roots.
#
# Everything here is about bytes and process plumbing. Nothing here knows what a pull request is.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The Azure DevOps resource identifier that an access token must be scoped to. This is a fixed
# well-known application identifier, identical in every tenant.
$script:AzureDevOpsResourceId = '499b84ac-1321-427f-aa17-267ca6975798'

# A capture reads a handful of resources from an interactive review, so a request that has not
# answered within this many seconds is better reported than waited on. This restates HttpClient's
# own default rather than tuning it: the value belongs beside the client it configures, where a
# later reader can weigh it, instead of being an invisible property of the framework.
$script:HttpRequestTimeoutSeconds = 100

# One client is shared for the life of the process. A client per request exhausts sockets under
# retry, which is the situation this code is most likely to be in when it matters.
$script:SharedHttpClient = $null

function Get-SharedHttpClient {
    if ($null -eq $script:SharedHttpClient) {
        $client = [System.Net.Http.HttpClient]::new()
        $client.Timeout = [TimeSpan]::FromSeconds($script:HttpRequestTimeoutSeconds)
        $script:SharedHttpClient = $client
    }
    $script:SharedHttpClient
}

# The media type a body carries when the caller named none. Every resource this skill reads and
# every pull request write it makes takes a plain JSON document.
$script:HttpRequestDefaultMediaType = 'application/json'

# Builds the message one request is sent as.
#
# Separate from the sender so that the media type and the header handling can be exercised without a
# service. .NET holds the media type on the content rather than on the request, so it cannot be
# copied across with the other headers, and a body that reaches a service under the wrong one is
# refused by it rather than by anything here.
function New-HttpRequestMessage {
    param(
        [Parameter(Mandatory)]
        [string] $Method,

        [Parameter(Mandatory)]
        [string] $Url,

        [AllowNull()]
        [hashtable] $Headers,

        [AllowNull()]
        $Body
    )

    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method), $Url)
    try {
        # Emptiness rather than nullness, because a caller that names no body reaches a [string]
        # parameter as the empty string: PowerShell converts $null on the way in, so a null check
        # here would attach an empty content to every read request the transport makes.
        if ($Body -is [byte[]] -or -not [string]::IsNullOrEmpty([string] $Body)) {
            $mediaType = ''
            if ($null -ne $Headers) {
                foreach ($name in $Headers.Keys) {
                    if (([string] $name) -ieq 'Content-Type') { $mediaType = [string] $Headers[$name] }
                }
            }
            if ([string]::IsNullOrWhiteSpace($mediaType)) { $mediaType = $script:HttpRequestDefaultMediaType }

            $contentType = [System.Net.Http.Headers.MediaTypeHeaderValue]::Parse($mediaType)
            # The charset is stated from the encoding the bytes were produced with rather than
            # carried over from the caller, so the declared charset and the bytes on the wire cannot
            # disagree.
            if ($Body -is [byte[]]) {
                $request.Content = [System.Net.Http.ByteArrayContent]::new($Body)
            } else {
                $request.Content = [System.Net.Http.StringContent]::new([string] $Body, [System.Text.UTF8Encoding]::new($false))
                $contentType.CharSet = 'utf-8'
            }
            $request.Content.Headers.ContentType = $contentType
        }

        if ($null -ne $Headers) {
            foreach ($name in $Headers.Keys) {
                # A content header belongs to the content and is refused by the request's own
                # collection. The one this skill sends is applied above, from the same source.
                if (([string] $name).StartsWith('Content-', [StringComparison]::OrdinalIgnoreCase)) { continue }
                $request.Headers.TryAddWithoutValidation([string] $name, [string] $Headers[$name]) | Out-Null
            }
        }

        $request
    } catch {
        $request.Dispose()
        throw
    }
}

# Produces the HTTP primitive the Azure DevOps transport expects.
#
# Response bytes are returned undecoded. Decoding is the transport's decision, and letting the
# console code page decide instead is exactly how non-ASCII comment text gets destroyed.
function Get-HttpRequestSender {
    {
        param(
            [Parameter(Mandatory)]
            [string] $Method,

            [Parameter(Mandatory)]
            [string] $Url,

            [AllowNull()]
            [hashtable] $Headers,

            [AllowNull()]
            $Body
        )

        $request = New-HttpRequestMessage -Method $Method -Url $Url -Headers $Headers -Body $Body
        try {
            $response = (Get-SharedHttpClient).SendAsync($request).GetAwaiter().GetResult()
            try {
                $bytes = $response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()

                $responseHeaders = @{}
                foreach ($header in $response.Headers) {
                    $responseHeaders[$header.Key.ToLowerInvariant()] = ($header.Value -join ', ')
                }
                foreach ($header in $response.Content.Headers) {
                    $responseHeaders[$header.Key.ToLowerInvariant()] = ($header.Value -join ', ')
                }

                $contentType = if ($null -eq $response.Content.Headers.ContentType) { '' } else { $response.Content.Headers.ContentType.MediaType }

                [ordered]@{
                    statusCode   = [int] $response.StatusCode
                    headers      = $responseHeaders
                    contentType  = $contentType
                    contentBytes = $bytes
                }
            } finally {
                $response.Dispose()
            }
        } finally {
            $request.Dispose()
        }
    }
}

# Produces the token primitive the Azure DevOps transport expects.
#
# The Azure CLI is used for this and only this. It deletes non-ASCII characters from its output,
# which makes it unusable for reading review content, but an access token is a base64 JWT and
# therefore pure ASCII.
function Get-AzureDevOpsTokenProvider {
    param(
        # The token is issued for the current subscription's tenant, which is usually but not always
        # the tenant that owns the organization.
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Tenant
    )

    $resource = $script:AzureDevOpsResourceId
    $tenantValue = $Tenant

    {
        $arguments = @('account', 'get-access-token', '--resource', $resource, '--query', 'accessToken', '--output', 'tsv')
        if (-not [string]::IsNullOrWhiteSpace($tenantValue)) {
            $arguments += @('--tenant', $tenantValue)
        }

        $token = (& az @arguments) -join ''
        if ($LASTEXITCODE -ne 0) {
            throw "Acquiring an Azure DevOps access token failed with exit code $LASTEXITCODE. Sign in with 'az login' and try again."
        }

        $token.Trim()
    }.GetNewClosure()
}

# Produces the `gh` primitive the GitHub transport expects.
#
# GitHub is reached through `gh` rather than through direct HTTP so that the credential stays inside
# the CLI's own keychain handling and never enters this process. Standard output is read as raw
# bytes for the same reason the Azure DevOps path does: the console code page must not get a vote on
# what a comment body says.
function Get-GitHubCliInvoker {
    param(
        # The program to run. Named rather than written into the start info so that the draining,
        # the exit code handling and the single-result contract of this invoker can be exercised
        # against a stand-in program instead of against a real GitHub account.
        [ValidateNotNullOrEmpty()]
        [string] $FileName = 'gh'
    )

    $program = $FileName

    {
        param(
            [Parameter(Mandatory)]
            [string[]] $ArgumentList,

            [AllowNull()]
            [string] $StandardInput
        )

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $program
        foreach ($argument in $ArgumentList) {
            $startInfo.ArgumentList.Add($argument)
        }
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.RedirectStandardInput = $true
        $startInfo.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)
        # A pager would block forever on a non-interactive caller.
        $startInfo.Environment['GH_PAGER'] = ''
        # Report publication must notify rather than block on interactive credential acquisition.
        $startInfo.Environment['GH_PROMPT_DISABLED'] = '1'
        $startInfo.Environment['GIT_TERMINAL_PROMPT'] = '0'

        $process = [System.Diagnostics.Process]::Start($startInfo)
        $outputBuffer = [System.IO.MemoryStream]::new()
        try {
            # Output and error are drained concurrently with the write to standard input. Doing any
            # of the three in sequence deadlocks as soon as a response outgrows a pipe buffer, which
            # a pull request discussion reliably does.
            $outputTask = $process.StandardOutput.BaseStream.CopyToAsync($outputBuffer)
            $errorTask = $process.StandardError.ReadToEndAsync()

            if (-not [string]::IsNullOrEmpty($StandardInput)) {
                $inputBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($StandardInput)
                $process.StandardInput.BaseStream.Write($inputBytes, 0, $inputBytes.Length)
                $process.StandardInput.BaseStream.Flush()
            }
            $process.StandardInput.Close()

            # Awaiting a non-generic Task yields a VoidTaskResult that PowerShell would otherwise
            # emit onto the pipeline, so the invoker would return an array of two objects rather
            # than the single result dictionary its callers require.
            $outputTask.GetAwaiter().GetResult() | Out-Null
            $standardErrorText = $errorTask.GetAwaiter().GetResult()
            $process.WaitForExit()

            [ordered]@{
                exitCode           = $process.ExitCode
                standardOutputBytes = $outputBuffer.ToArray()
                standardErrorText  = $standardErrorText
            }
        } finally {
            $outputBuffer.Dispose()
            $process.Dispose()
        }
    }.GetNewClosure()
}

# Produces the Git primitive GitDiff.ps1 expects.
#
# Diff hunks are read from the local repository on both platforms, so this invoker is what every
# review that publishes a line-anchored finding runs. Standard output is handed back as raw bytes
# for the same reason as the other invokers here: a diff carries whatever the source files carry,
# and the console code page must not get to replace it.
function Get-GitInvoker {
    param(
        # The program to run. Named so that the draining, the exit code handling and the byte
        # fidelity of this invoker can be exercised against a stand-in program.
        [ValidateNotNullOrEmpty()]
        [string] $FileName = 'git'
    )

    $program = $FileName

    {
        param(
            [Parameter(Mandatory)]
            [string[]] $ArgumentList
        )

        $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $program
        foreach ($argument in $ArgumentList) {
            $startInfo.ArgumentList.Add($argument)
        }
        $startInfo.UseShellExecute = $false
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $startInfo.RedirectStandardInput = $true
        $startInfo.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)
        # A pager and a credential prompt each block forever on a non-interactive caller, and a
        # review runs inside one.
        $startInfo.Environment['GIT_PAGER'] = ''
        $startInfo.Environment['GIT_TERMINAL_PROMPT'] = '0'

        $process = [System.Diagnostics.Process]::Start($startInfo)
        $outputBuffer = [System.IO.MemoryStream]::new()
        try {
            # Output and error are drained concurrently. Reading either to completion before the
            # other deadlocks as soon as the unread one fills its pipe buffer, which the diff of a
            # sizeable pull request does immediately.
            $outputTask = $process.StandardOutput.BaseStream.CopyToAsync($outputBuffer)
            $errorTask = $process.StandardError.ReadToEndAsync()

            $process.StandardInput.Close()

            # Awaiting a non-generic Task yields a VoidTaskResult that PowerShell would otherwise
            # emit onto the pipeline, so the invoker would return an array rather than the single
            # result dictionary its callers require.
            $outputTask.GetAwaiter().GetResult() | Out-Null
            $standardErrorText = $errorTask.GetAwaiter().GetResult()
            $process.WaitForExit()

            [ordered]@{
                exitCode            = $process.ExitCode
                standardOutputBytes = $outputBuffer.ToArray()
                standardErrorText   = $standardErrorText
            }
        } finally {
            $outputBuffer.Dispose()
            $process.Dispose()
        }
    }.GetNewClosure()
}
