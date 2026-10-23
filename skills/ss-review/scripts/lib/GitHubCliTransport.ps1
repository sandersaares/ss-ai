#Requires -Version 7.6

# GitHub REST and GraphQL transport over the GitHub CLI (docs/implementation.md, "Verbatim
# fidelity").
#
# Role in the overall design: every GitHub read and write in this skill goes through here. The
# GitHub CLI is the transport of record because it reproduces the service payload byte for byte -
# measured across a full pull request review corpus against direct REST, with no body mismatch and
# emoji preserved identically - while keeping the credential inside the CLI process, where no
# diagnostic emitted by this skill can reach it. The credential is never read, passed or logged
# here.
#
# This library knows nothing about pull requests. It converts a request into CLI arguments, runs the
# CLI through an injected invoker, decodes the raw bytes as UTF-8 and materializes the JSON. Anything
# specific to a resource belongs in the adapter above it, so the transport stays reusable for any
# other GitHub work.
#
# Process execution is injected rather than performed here (New-GitHubCliTransport -InvokeGh). That
# keeps the whole retry, decode and pagination path under offline test, and the invoker is mandatory
# with no default so a test that forgets to supply a fake fails loudly instead of reaching the
# network.
#
# This library calls ConvertFrom-JsonVerbatim, Get-JsonProperty, Get-JsonArrayProperty,
# Set-JsonProperty and ConvertTo-JsonText from JsonValue.ps1, and
# New-PullRequestServiceRefusalException from PublicationFailure.ps1. Libraries never dot-source one
# another; the executable composition root dot-sources every library it needs before calling into
# this one.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The cap GitHub REST places on per_page, which is well above the size it assumes when the parameter
# is absent. Requesting the cap minimizes round trips, and it is only ever a lower bound on
# completeness because every paginated read follows the CLI's Link header traversal to the last page.
$script:GitHubRestPageSize = 100

# Backoff for bounded retries of server and connection faults.
$script:GitHubRetryBaseDelayMilliseconds = 1000

# GitHub requires this minimum pause when a rate-limit response provides no recovery time.
# Ref: https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#handle-rate-limit-errors-appropriately
$script:GitHubRateLimitBaseDelayMilliseconds = 60000

# Cap locally chosen exponential delays so indefinite throttling recovery remains responsive.
# Service-supplied recovery times are not shortened by this cap.
$script:GitHubRetryMaximumDelayMilliseconds = 120000

# Diagnostics quote the CLI output, which can be an entire error document. This bound keeps a thrown
# message readable while retaining enough of the service response to identify the failure.
$script:GitHubDiagnosticExcerptLength = 2000

# Failures worth another attempt because the same request may succeed unchanged: rate limiting,
# server-side faults, and connection-level faults. Matched case-insensitively against the CLI's own
# diagnostics.
$script:GitHubRateLimitPatterns = @(
    'rate limit'
    'ratelimit'
    'abuse detection'
    'HTTP 429'
    'too many requests'
)
$script:GitHubTransientPatterns = @(
    'HTTP 500'
    'HTTP 502'
    'HTTP 503'
    'HTTP 504'
    'bad gateway'
    'service unavailable'
    'gateway timeout'
    'connection reset'
    'connection refused'
    'i/o timeout'
    'TLS handshake'
    'unexpected EOF'
    'server error'
)

# Shapes that leak credential material if a future CLI release ever echoes a request header. The
# whole authorization header value is removed rather than a token shape, so a credential format this
# code has never seen is scrubbed as well. Matched against every diagnostic before it reaches an
# exception message.
$script:GitHubCredentialPatterns = @(
    '(?i)authorization\s*:[^\r\n]*'
    '(?i)\bbearer\s+\S+'
    '(?i)\bgh[pousr]_[A-Za-z0-9_]{8,}'
)

# Holds the transport configuration: how to run the CLI, how to wait between attempts, how the
# current instant is read, and how many attempts non-throttling transient failures get. Explicit
# rate-limit refusals are retried until success. Returned as an ordered dictionary so a capture that
# records its own configuration serializes it stably.
#
# The invoker is called as `& $InvokeGh -ArgumentList <string[]> -StandardInput <string-or-null>` and
# must return an ordered dictionary with exitCode, standardOutputBytes and standardErrorText. Raw
# bytes rather than text, because the decode has to be explicit: an ambient console encoding is
# exactly what corrupts non-ASCII content.
#
# The wait block is called as `& $Wait -Milliseconds <int>` and must emit nothing. Anything it writes
# to the success stream would join the output of the request that is being retried, turning a single
# materialized document into an array of unrelated values.
#
# The clock is a block rather than a fixed instant because a wait derived from an absolute rate limit
# reset has to be measured against the time of the attempt that hit the limit, not against the time
# the transport was constructed. Injecting it keeps that arithmetic under test with a fixed instant.
function New-GitHubCliTransport {
    param(
        [Parameter(Mandatory)]
        [scriptblock] $InvokeGh,

        [scriptblock] $Wait = { param([int] $Milliseconds) Start-Sleep -Milliseconds $Milliseconds },

        [scriptblock] $GetUtcNow = { [DateTimeOffset]::UtcNow },

        [ValidateRange(1, 100)]
        [int] $MaxAttempts = 5
    )

    [ordered]@{
        invokeGh    = $InvokeGh
        wait        = $Wait
        getUtcNow   = $GetUtcNow
        maxAttempts = $MaxAttempts
    }
}

# Removes anything credential-shaped from text destined for an exception message.
function Remove-GitHubCredentialMaterial {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Text
    )

    if ([string]::IsNullOrEmpty($Text)) {
        return ''
    }

    $result = $Text
    foreach ($pattern in $script:GitHubCredentialPatterns) {
        $result = [regex]::Replace($result, $pattern, '<redacted>')
    }
    return $result
}

# Produces the bounded, scrubbed excerpt quoted by every failure raised here.
#
# Standard error carries the CLI's own message and standard output carries the service error
# document; a failure is far easier to diagnose with both, and the CLI writes the response body to
# standard output even when it exits non-zero.
function Get-GitHubCliDiagnosticExcerpt {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $StandardErrorText,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $StandardOutputText
    )

    $parts = @()
    if (-not [string]::IsNullOrWhiteSpace($StandardErrorText)) { $parts += $StandardErrorText.Trim() }
    if (-not [string]::IsNullOrWhiteSpace($StandardOutputText)) { $parts += $StandardOutputText.Trim() }

    $combined = Remove-GitHubCredentialMaterial -Text ($parts -join ' | ')
    if ($combined.Length -gt $script:GitHubDiagnosticExcerptLength) {
        return $combined.Substring(0, $script:GitHubDiagnosticExcerptLength) + '...'
    }
    return $combined
}

function Test-GitHubTextMatchesAny {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Text,

        [Parameter(Mandatory)]
        [string[]] $Pattern
    )

    if ([string]::IsNullOrEmpty($Text)) {
        return $false
    }
    foreach ($candidate in $Pattern) {
        if ([regex]::IsMatch($Text, [regex]::Escape($candidate), [Text.RegularExpressions.RegexOptions]::IgnoreCase)) {
            return $true
        }
    }
    return $false
}

# Decides whether a failed attempt is worth repeating.
function Test-GitHubFailureIsRetryable {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Diagnostics
    )

    if (Test-GitHubTextMatchesAny -Text $Diagnostics -Pattern $script:GitHubRateLimitPatterns) {
        return $true
    }
    return (Test-GitHubTextMatchesAny -Text $Diagnostics -Pattern $script:GitHubTransientPatterns)
}

# Computes how long to wait before the next attempt.
#
# A service-supplied delay wins over backoff, because retrying earlier than the service asked is how
# a secondary rate limit turns into a longer block. Rate-limit waits honour Retry-After as elapsed
# seconds and x-ratelimit-reset as an absolute Unix time, without shortening service guidance.
#
# The instant a reset is measured against is supplied by the caller. Reading a clock here would make
# the only branch that depends on one unverifiable, and the transport already holds the clock the
# rest of the retry path uses.
function Get-GitHubRetryDelayMilliseconds {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Diagnostics,

        [Parameter(Mandatory)]
        [int] $Attempt,

        [Parameter(Mandatory)]
        [DateTimeOffset] $NowUtc,

        [switch] $RateLimit
    )

    $baseDelay = if ($RateLimit) { $script:GitHubRateLimitBaseDelayMilliseconds } else { $script:GitHubRetryBaseDelayMilliseconds }
    # Saturate the exponent before computing it: an unbounded refusal sequence must not overflow.
    $maximumExponent = [Math]::Ceiling([Math]::Log2($script:GitHubRetryMaximumDelayMilliseconds / $baseDelay))
    $backoff = [int] [Math]::Min(
        $baseDelay * [Math]::Pow(2, [Math]::Min($Attempt - 1, $maximumExponent)),
        $script:GitHubRetryMaximumDelayMilliseconds
    )
    $maximumDelay = if ($RateLimit) { [int]::MaxValue } else { $script:GitHubRetryMaximumDelayMilliseconds }
    if (-not [string]::IsNullOrEmpty($Diagnostics)) {
        $retryAfter = [regex]::Match($Diagnostics, '(?i)retry[-_]after"?\s*[:=]\s*"?(?<seconds>\d+)')
        if ($retryAfter.Success) {
            $requested = [long] $retryAfter.Groups['seconds'].Value * 1000
            if ($RateLimit -and $requested -eq 0) { return $backoff }
            return [int] [Math]::Min($requested, $maximumDelay)
        }

        $reset = [regex]::Match($Diagnostics, '(?i)x-ratelimit-reset"?\s*[:=]\s*"?(?<epoch>\d+)')
        # A secondary limit can occur while the primary quota still has requests remaining.
        $remainingQuota = [regex]::Match($Diagnostics, '(?i)x-ratelimit-remaining"?\s*[:=]\s*"?(?<remaining>\d+)')
        if ($reset.Success -and (-not $RateLimit -or -not $remainingQuota.Success -or $remainingQuota.Groups['remaining'].Value -eq '0')) {
            $resetInstant = [DateTimeOffset]::FromUnixTimeSeconds([long] $reset.Groups['epoch'].Value)
            $remaining = ($resetInstant - $NowUtc).TotalMilliseconds
            if ($remaining -le 0) {
                if ($RateLimit) { return $backoff }
                return 0
            }
            return [int] [Math]::Min($remaining, $maximumDelay)
        }
    }

    return $backoff
}

function Assert-GitHubCliResult {
    param(
        $Result
    )

    if ($null -eq $Result -or $Result -isnot [System.Collections.IDictionary]) {
        throw 'The GitHub CLI invoker must return a dictionary with exitCode, standardOutputBytes and standardErrorText.'
    }
    foreach ($key in @('exitCode', 'standardOutputBytes', 'standardErrorText')) {
        if (-not $Result.Contains($key)) {
            throw "The GitHub CLI invoker returned no '$key' value."
        }
    }
}

# Reports whether the CLI invocation is a GraphQL request.
#
# GraphQL refusals have a structured response body even when the CLI exits with a non-zero status,
# and only GraphQL requests give the transport enough structure to interpret that body as a service
# decision rather than as arbitrary process output.
function Test-GitHubCliGraphQlRequest {
    param(
        [Parameter(Mandatory)]
        [string[]] $ArgumentList
    )

    $ArgumentList.Count -ge 2 -and $ArgumentList[0] -ceq 'api' -and $ArgumentList[1] -ceq 'graphql'
}

# Reports whether a materialized GraphQL response proves that execution never began.
#
# Parsing, validation and variable coercion failures happen before a resolver can run. A response
# from that phase carries errors and omits the data entry entirely, so a write request could not have
# reached a mutation resolver. Once execution begins, the response carries a data entry even when the
# value is null or sibling fields report errors. A mutation resolver may have applied its side effect
# before an execution error is reported, so any response with a data entry keeps ambiguous recovery
# semantics.
function Test-GitHubGraphQlPreExecutionRefusalResponse {
    param(
        $Response
    )

    $dictionary = $Response -as [System.Collections.IDictionary]
    if ($null -eq $dictionary) { return $false }

    $errors = Get-JsonArrayProperty -Node $dictionary -Name 'errors'
    # Test-JsonProperty intentionally treats JSON null as absent for general platform reads. The
    # GraphQL execution boundary depends on property presence, and ConvertFrom-JsonVerbatim
    # materializes objects as dictionaries whose key lookup distinguishes an omitted entry from a
    # null value.
    $errors.Count -gt 0 -and -not $dictionary.Contains('data')
}

# Recognizes GraphQL throttling only when every error is a rate-limit refusal and no execution
# result exists. Partial data can describe an applied mutation and must never justify replay.
function Test-GitHubGraphQlRateLimitRefusalResponse {
    param($Response)

    $dictionary = $Response -as [System.Collections.IDictionary]
    if ($null -eq $dictionary -or $null -ne (Get-JsonProperty -Node $dictionary -Name 'data')) { return $false }
    $errors = Get-JsonArrayProperty -Node $dictionary -Name 'errors'
    if ($errors.Count -eq 0) { return $false }
    foreach ($error in $errors) {
        if ((Get-JsonProperty -Node $error -Name 'type') -ceq 'RATE_LIMITED') { continue }
        if (-not (Test-GitHubTextMatchesAny -Text ([string] (Get-JsonProperty -Node $error -Name 'message')) -Pattern $script:GitHubRateLimitPatterns)) {
            return $false
        }
    }
    return $true
}

# Runs one CLI invocation with retries and returns the decoded standard output.
#
# Decoding is pinned to UTF-8 rather than inherited from the console, which is the entire reason the
# invoker hands back bytes: an ambient code page silently replaces every character outside it, and
# the discussion this skill captures routinely contains punctuation and emoji outside ASCII.
function Invoke-GitHubCliText {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string[]] $ArgumentList,

        [AllowNull()]
        [string] $StandardInput,

        # Suppresses retries whose outcome is uncertain. Explicit rate-limit refusals are safe to
        # repeat because the service has declined the write.
        [switch] $NoRetry
    )

    # Headers provide real service recovery times, which gh does not normally print in diagnostics.
    # Paginated reads retain gh's slurped JSON output; without headers they use local backoff.
    if ($ArgumentList[0] -ceq 'api' -and $ArgumentList -cnotcontains '--paginate' -and $ArgumentList -cnotcontains '--include') {
        $ArgumentList = @($ArgumentList) + @('--include')
    }
    $attempt = 1
    $transientAttempt = 1
    $rateLimitAttempt = 0
    while ($true) {
        $result = & $Transport['invokeGh'] -ArgumentList $ArgumentList -StandardInput $StandardInput
        Assert-GitHubCliResult -Result $result

        $bytes = $result['standardOutputBytes']
        $standardOutputText = if ($null -eq $bytes -or $bytes.Length -eq 0) {
            ''
        } else {
            [Text.Encoding]::UTF8.GetString($bytes)
        }

        $headerText = ''
        $header = [regex]::Match($standardOutputText, '\AHTTP/\S+\s+\d{3}[^\r\n]*(?:\r?\n[^\r\n]+)*\r?\n\r?\n')
        if ($header.Success) {
            $headerText = $header.Value
            $standardOutputText = $standardOutputText.Substring($header.Length)
        }

        $graphQlResponse = $null
        if (Test-GitHubCliGraphQlRequest -ArgumentList $ArgumentList) {
            try {
                if (-not [string]::IsNullOrWhiteSpace($standardOutputText)) {
                    $graphQlResponse = ConvertFrom-JsonVerbatim -Text $standardOutputText
                }
            } catch {
                # Unusable output cannot attest refusal; the ordinary failure path retains it.
                $graphQlResponse = $null
            }
        }

        $statusText = $headerText + [string] $result['standardErrorText']
        $failureText = $statusText + "`n" + $standardOutputText
        # Ordinary permission refusals also carry X-RateLimit headers. Only the error message,
        # not a quota header's name, can establish that an HTTP 403 is throttling.
        $errorText = [string] $result['standardErrorText'] + "`n" + $standardOutputText
        $graphQlErrors = Get-JsonArrayProperty -Node $graphQlResponse -Name 'errors'
        $isRateLimit = [regex]::IsMatch($statusText, '(?i)HTTP(?:/\S+)?\s+429\b') -or
            ([regex]::IsMatch($statusText, '(?i)HTTP(?:/\S+)?\s+403\b') -and
                $graphQlErrors.Count -eq 0 -and
                (Test-GitHubTextMatchesAny -Text $errorText -Pattern $script:GitHubRateLimitPatterns)) -or
            (Test-GitHubGraphQlRateLimitRefusalResponse -Response $graphQlResponse)
        if ($null -ne (Get-JsonProperty -Node $graphQlResponse -Name 'data')) { $isRateLimit = $false }
        if ($isRateLimit) {
            $rateLimitAttempt++
            $delay = Get-GitHubRetryDelayMilliseconds -Diagnostics $failureText -Attempt $rateLimitAttempt `
                -NowUtc ([DateTimeOffset] (& $Transport['getUtcNow'])) -RateLimit
            Write-Warning "GitHub refused the request due to a rate limit. Waiting $delay ms before retrying (rate-limit refusal $rateLimitAttempt)."
            try {
                & $Transport['wait'] -Milliseconds $delay | Out-Null
            } catch {
                # No new request was sent during this wait, so interruption is still a definite
                # refusal rather than an unknown write.
                throw (New-PullRequestServiceRefusalException -Message "GitHub rate-limit recovery was interrupted: $($_.Exception.Message)")
            }
            $attempt++
            continue
        }

        if ([int] $result['exitCode'] -eq 0) {
            return $standardOutputText
        }

        $diagnostics = Get-GitHubCliDiagnosticExcerpt `
            -StandardErrorText ([string] $result['standardErrorText']) `
            -StandardOutputText $standardOutputText

        $isRetryable = -not $NoRetry -and (Test-GitHubFailureIsRetryable -Diagnostics $diagnostics)
        $isGivingUp = -not $isRetryable -or $transientAttempt -ge $Transport['maxAttempts']
        if ($isGivingUp) {
            if (Test-GitHubCliGraphQlRequest -ArgumentList $ArgumentList) {
                if (Test-GitHubGraphQlPreExecutionRefusalResponse -Response $graphQlResponse) {
                    throw (New-PullRequestServiceRefusalException -Message "The GitHub GraphQL request was refused by the service after $attempt attempt(s): $diagnostics")
                }
            }

            $command = Remove-GitHubCredentialMaterial -Text ($ArgumentList -join ' ')
            throw "The GitHub CLI failed with exit code $($result['exitCode']) for 'gh $command' after $attempt attempt(s): $diagnostics"
        }

        $delay = Get-GitHubRetryDelayMilliseconds `
            -Diagnostics $diagnostics `
            -Attempt $transientAttempt `
            -NowUtc ([DateTimeOffset] (& $Transport['getUtcNow']))
        & $Transport['wait'] -Milliseconds $delay | Out-Null
        $attempt++
        $transientAttempt++
    }
}

# Serializes a request body. A string is taken to be JSON already and is sent unchanged, so a caller
# that has an exact body to reproduce can supply it verbatim.
function ConvertTo-GitHubRequestBodyText {
    param(
        $Body
    )

    if ($Body -is [string]) {
        return $Body
    }
    return ConvertTo-JsonText -Value $Body
}

# Issues one REST request and returns its materialized body.
#
# With -Paginate the CLI walks the Link header to the last page and, with --slurp, presents the
# result as an array of pages. Flattening those pages here is what makes truncation impossible: a
# short page never terminates the walk, and a caller receives one array holding every item the
# service returned. Without --slurp the CLI splices array pages together, which is indistinguishable
# from a single page and hides how many were fetched, so the explicit form is used.
function Invoke-GitHubRestRequest {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Path,

        [string] $Method = 'GET',

        $Body,

        [switch] $Paginate,

        # Publication suppresses uncertain retries, but explicit throttling refusals remain safe.
        [switch] $NoRetry
    )

    $resolvedPath = $Path.TrimStart('/')
    if ($Paginate -and $resolvedPath -notmatch '(?i)[?&]per_page=') {
        $separator = if ($resolvedPath.Contains('?')) { '&' } else { '?' }
        $resolvedPath = "$resolvedPath$separator" + "per_page=$script:GitHubRestPageSize"
    }

    $arguments = @('api', $resolvedPath, '--method', $Method)
    if ($Paginate) { $arguments += @('--paginate', '--slurp') }

    $standardInput = $null
    if ($null -ne $Body) {
        $standardInput = ConvertTo-GitHubRequestBodyText -Body $Body
        $arguments += @('--input', '-')
    }

    $text = Invoke-GitHubCliText -Transport $Transport -ArgumentList $arguments -StandardInput $standardInput -NoRetry:$NoRetry
    if ([string]::IsNullOrWhiteSpace($text)) {
        return $null
    }

    $value = ConvertFrom-JsonVerbatim -Text $text
    if (-not $Paginate) {
        return $value
    }

    if ($value -isnot [object[]]) {
        throw "Paginated request '$resolvedPath' returned a single document where pages were expected."
    }

    $items = [System.Collections.Generic.List[object]]::new()
    foreach ($page in $value) {
        # A page from a list endpoint is an array of records; a page from an endpoint that answers
        # with a single document is that document. Both are flattened into one sequence, so a caller
        # receives records in either case.
        if ($page -is [object[]]) {
            foreach ($item in $page) { $items.Add($item) }
        } else {
            $items.Add($page)
        }
    }
    return , $items.ToArray()
}

# Issues one GraphQL query and returns its materialized body.
#
# The query and its variables travel as a JSON document on standard input rather than as CLI
# key-value flags, because the flag form stringifies every value and would send a null cursor or a
# numeric pull request identifier as text.
function Invoke-GitHubGraphQlQuery {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Query,

        [hashtable] $Variables,

        # Publication suppresses uncertain retries, but explicit throttling refusals remain safe.
        [switch] $NoRetry
    )

    $payload = [ordered]@{ query = $Query }
    if ($null -ne $Variables) { Set-JsonProperty -Node $payload -Name 'variables' -Value $Variables }

    $text = Invoke-GitHubCliText `
        -Transport $Transport `
        -ArgumentList @('api', 'graphql', '--input', '-') `
        -StandardInput (ConvertTo-JsonText -Value $payload) `
        -NoRetry:$NoRetry

    if ([string]::IsNullOrWhiteSpace($text)) {
        throw 'The GraphQL request returned an empty response.'
    }

    $response = ConvertFrom-JsonVerbatim -Text $text

    # Accepting a response that carries GraphQL errors would silently capture a truncated discussion,
    # so any error present fails the request. Only the pre-execution shape proves service refusal;
    # execution errors stay ambiguous because a resolver may have applied a side effect first.
    $errors = Get-JsonArrayProperty -Node $response -Name 'errors'
    if ($errors.Count -gt 0) {
        $messages = foreach ($item in $errors) { [string] (Get-JsonProperty -Node $item -Name 'message' -Default 'unspecified error') }
        $excerpt = Get-GitHubCliDiagnosticExcerpt -StandardErrorText '' -StandardOutputText ($messages -join ' | ')
        $message = "The GitHub GraphQL request returned service errors: $excerpt"
        if (Test-GitHubGraphQlPreExecutionRefusalResponse -Response $response) {
            throw (New-PullRequestServiceRefusalException -Message "The GitHub GraphQL request was refused by the service: $excerpt")
        }
        throw $message
    }

    return $response
}
