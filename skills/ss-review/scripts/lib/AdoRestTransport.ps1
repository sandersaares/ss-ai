#Requires -Version 7.6

# Azure DevOps REST transport for the ss-review pull request capture
# (docs/implementation.md, "Azure DevOps transport").
#
# Every Azure DevOps read and write in this skill goes through here, and nothing goes through the
# Azure CLI's `az devops` / `az repos` commands. Those commands drop non-ASCII characters from their
# output: on a measured pull request the REST service returned ninety non-ASCII characters and the
# CLI returned none, silently deleting em dashes, curly quotes and emoji from the comment bodies
# that carried them. A review that reads mangled comment text draws conclusions about text nobody
# wrote, so the bytes are fetched here and decoded as UTF-8 explicitly. The only Azure CLI use that
# remains anywhere in this skill is access token acquisition, whose output is a base64 JWT and
# therefore pure ASCII.
#
# This file deliberately knows nothing about pull requests. It is the transport for any Azure DevOps
# REST resource - work items and builds included - and the pull request vocabulary lives one layer
# up in AdoPullRequestApi.ps1.
#
# The HTTP call itself and the wall-clock wait are injected rather than hard-coded, so the offline
# test suite can drive every branch below - retry, HTML sign-in document, truncated result - without
# a network and without elapsed time. The production implementations are supplied by the executable
# composition root.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls ConvertFrom-JsonVerbatim, ConvertTo-JsonText,
# Get-JsonProperty, Get-JsonArrayProperty, Set-JsonProperty and Test-JsonProperty from JsonValue.ps1,
# and New-PullRequestServiceRefusalException from PublicationFailure.ps1, which the composition
# root loads alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Azure DevOps signals a truncated result set with this response header. The pull request endpoints
# this skill reads have never returned it, so no paging is implemented; the header is checked so
# that a service change surfaces as a loud failure instead of a capture that silently omits threads.
$script:AdoRestContinuationTokenHeader = 'x-ms-continuationtoken'

# Azure DevOps asks a throttled client to wait through this header, in seconds.
$script:AdoRestRetryAfterHeader = 'retry-after'

# First backoff step when the service gives no Retry-After guidance. Each further wait doubles, so
# the total spent in backoff across the default attempt count absorbs the brief throttling bursts the
# service applies to a burst of reads without making an outage look like a hang.
$script:AdoRestBaseRetryDelayMilliseconds = 500

# Cap locally chosen backoff so indefinite throttling recovery remains responsive. Throttling
# honours Retry-After without shortening the service's recovery window.
$script:AdoRestMaximumRetryDelayMilliseconds = 30000

# How much of a failing response body is quoted back in the error. Enough to carry an Azure DevOps
# error payload, which states the reason in its first sentence, and short enough that a stack of
# nested errors stays readable.
$script:AdoRestErrorExcerptLength = 512

# How far to follow InnerException when classifying a failure raised by the injected sender. The
# depth reaches past the wrappers PowerShell and HttpClient add - MethodInvocationException around
# HttpRequestException around SocketException - and the bound keeps a self-referencing chain from
# spinning.
$script:AdoRestExceptionChainDepth = 4

# The status that says the presented token is no longer accepted. It is handled apart from the
# retryable statuses because repeating the request unchanged would fail identically; only a freshly
# acquired token changes the outcome.
$script:AdoRestUnauthorizedStatus = 401

# The generally available version across the Git resources this skill reads. Resources that are
# still in preview reject it and are called with their own version by the caller.
$script:AdoRestDefaultApiVersion = '7.1'

# The media type a request body carries by default. Every Azure DevOps resource this skill reads and
# every pull request write it makes takes a plain JSON document.
$script:AdoRestDefaultContentType = 'application/json; charset=utf-8'

# The media type a JSON Patch document carries. Azure DevOps refuses a work item write sent as plain
# JSON, because the body is a list of operations rather than a representation of the item.
$script:AdoRestJsonPatchContentType = 'application/json-patch+json'

# The two properties an Azure DevOps list envelope carries: the collection itself, and the service's
# own statement of how many items it holds.
$script:AdoRestListValueProperty = 'value'
$script:AdoRestListCountProperty = 'count'

# Builds the configuration every request in a run shares. MaxAttempts bounds non-throttling
# transient retries; explicit rate-limit refusals are retried until success.
#
# SendRequest and GetAccessToken are mandatory and have no defaults. A default network
# implementation here would let a test that forgot to inject a fake reach the live service and pass,
# which would break the contract that this repository's test suite is entirely offline; a missing
# injection has to fail loudly instead.
#
# SendRequest is invoked as
#     & $SendRequest -Method <string> -Url <string> -Headers <hashtable> -Body <string or $null>
# and returns
#     [ordered]@{ statusCode = [int]; headers = <hashtable, lower-cased keys>; contentType = [string]; contentBytes = [byte[]] }
# It returns the response bytes undecoded, because choosing the encoding is this library's
# responsibility and not the caller's.
#
# GetAccessToken is invoked as `& $GetAccessToken` and returns the bearer token string. It is called
# at most once per transport under normal operation; the acquired token is held on the returned
# dictionary and reused by every request made through it.
#
# Wait is invoked as `& $Wait -Milliseconds <int>` and produces nothing. Its output is discarded at
# every call site rather than trusted, because an implementation that emits a value - a recorder
# built on ArrayList.Add, which returns the index it inserted at - would otherwise have that value
# spliced ahead of the request result, and a materialized payload arriving as an array of two would
# read as a record with no properties at all.
function New-AdoRestTransport {
    param(
        [Parameter(Mandatory)]
        [string] $CollectionUrl,

        [Parameter(Mandatory)]
        [scriptblock] $SendRequest,

        [Parameter(Mandatory)]
        [scriptblock] $GetAccessToken,

        [scriptblock] $Wait = { param([int] $Milliseconds) Start-Sleep -Milliseconds $Milliseconds },

        [ValidateRange(1, 100)]
        [int] $MaxAttempts = 5
    )

    if ([string]::IsNullOrWhiteSpace($CollectionUrl)) {
        throw 'An Azure DevOps transport requires a collection URL.'
    }

    [ordered]@{
        collectionUrl  = $CollectionUrl.TrimEnd('/')
        sendRequest    = $SendRequest
        getAccessToken = $GetAccessToken
        wait           = $Wait
        maxAttempts    = $MaxAttempts
        accessToken    = $null
    }
}

# Returns the bearer token for this transport, acquiring one only when there is none to reuse.
#
# Acquiring a token shells out to the Azure CLI and costs on the order of a second. A capture makes
# five or more requests, so acquiring per request would add several seconds to a phase a reviewer is
# waiting on, for a credential that does not change between those requests. The token is therefore
# held on the transport for the life of the process.
#
# Refresh discards the held token and acquires another. That is what a mid-capture rejection calls
# for: the run has outlived the token it started with, and only a new one can change the outcome.
function Get-AdoRestAccessToken {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [switch] $Refresh
    )

    if (-not $Refresh) {
        $cached = [string] (Get-JsonProperty -Node $Transport -Name 'accessToken')
        if (-not [string]::IsNullOrWhiteSpace($cached)) { return $cached }
    }

    $token = [string] (& $Transport['getAccessToken'])
    if ([string]::IsNullOrWhiteSpace($token)) {
        throw 'Azure DevOps access token acquisition produced no token.'
    }

    Set-JsonProperty -Node $Transport -Name 'accessToken' -Value $token
    return $token
}

# Assembles the request headers.
#
# The bearer token is written here and nowhere else, which is what keeps it out of every diagnostic
# path in this file.
#
# The content type is stated by the caller rather than assumed, because Azure DevOps decides what a
# body means from it: the same work item endpoint reads a plain JSON body as a malformed request and
# a JSON Patch body as the list of operations it is.
function Get-AdoRestRequestHeader {
    param(
        [Parameter(Mandatory)]
        [string] $Token,

        [string] $ContentType = $script:AdoRestDefaultContentType,

        [string] $Accept = 'application/json',

        [switch] $HasBody
    )

    $headers = @{
        'Authorization' = "Bearer $Token"
        'Accept'        = $Accept
    }
    if ($HasBody) {
        $headers['Content-Type'] = $ContentType
    }
    return $headers
}

# Assembles the request URL.
#
# The api-version parameter is always appended because Azure DevOps rejects a request without one,
# and query values are sorted by name so that two runs against the same resource produce byte
# identical URLs and therefore comparable diagnostics. The sort is ordinal rather than through
# Sort-Object: a culture-sensitive comparison would order the same parameters differently on a host
# configured for a different culture, and Sort-Object additionally returns its input wrapped in
# PSObject.
function Get-AdoRestRequestUrl {
    param(
        [Parameter(Mandatory)]
        [string] $CollectionUrl,

        [Parameter(Mandatory)]
        [string] $Path,

        [AllowNull()]
        [hashtable] $Query,

        [Parameter(Mandatory)]
        [string] $ApiVersion
    )

    $parts = [System.Collections.Generic.List[string]]::new()
    if ($null -ne $Query) {
        $names = [string[]] @($Query.Keys)
        [array]::Sort($names, [StringComparer]::Ordinal)
        foreach ($name in $names) {
            $value = $Query[$name]
            if ($null -eq $value) { continue }
            $parts.Add("$([uri]::EscapeDataString($name))=$([uri]::EscapeDataString([string] $value))")
        }
    }
    $parts.Add("api-version=$([uri]::EscapeDataString($ApiVersion))")

    "$($CollectionUrl.TrimEnd('/'))/$($Path.TrimStart('/'))?$($parts -join '&')"
}

# Finds a response header irrespective of the casing the service used.
function Get-AdoRestResponseHeader {
    param(
        [AllowNull()]
        [System.Collections.IDictionary] $Headers,

        [Parameter(Mandatory)]
        [string] $Name
    )

    if ($null -eq $Headers) { return $null }
    foreach ($key in $Headers.Keys) {
        if ([string]::Equals([string] $key, $Name, [StringComparison]::OrdinalIgnoreCase)) {
            return $Headers[$key]
        }
    }
    return $null
}

# Reduces a response body to a bounded single-line excerpt for an error message.
function Get-AdoRestBodyExcerpt {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Body
    )

    if ([string]::IsNullOrEmpty($Body)) { return '(empty body)' }

    $flattened = ($Body -replace '\s+', ' ').Trim()
    if ($flattened.Length -le $script:AdoRestErrorExcerptLength) { return $flattened }
    "$($flattened.Substring(0, $script:AdoRestErrorExcerptLength))..."
}

# Reports whether a response carries an HTML document rather than the requested JSON.
#
# Azure DevOps answers an unauthenticated or wrong-tenant request with a success status and a
# sign-in page rather than with 401: the measured response to an anonymous read of a pull request's
# threads is 203 with text/html. Parsing that would yield a success-shaped capture containing no
# threads at all, which is the exact silent failure this helper exists to prevent, so both the
# content type and the document prefix are inspected.
function Test-AdoRestHtmlResponse {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $ContentType,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Body
    )

    if (-not [string]::IsNullOrEmpty($ContentType) -and $ContentType -match '(?i)text/html') {
        return $true
    }
    if ([string]::IsNullOrEmpty($Body)) { return $false }

    $leading = $Body.TrimStart()
    if ($leading.StartsWith('<!DOCTYPE', [StringComparison]::OrdinalIgnoreCase)) { return $true }
    if ($leading.StartsWith('<html', [StringComparison]::OrdinalIgnoreCase)) { return $true }
    return $false
}

# Decides how long to wait before repeating a throttled or failed request.
#
# Retry-After is honoured when the service supplies it, because the service knows its own recovery
# window better than any local heuristic; otherwise the wait doubles per attempt. Only local
# backoff is capped for throttling, so the client never retries before the service permits it.
function Get-AdoRestRetryDelayMilliseconds {
    param(
        [AllowNull()]
        $RetryAfter,

        [Parameter(Mandatory)]
        [int] $Attempt,

        [switch] $RateLimit
    )

    $seconds = 0
    if ($null -ne $RetryAfter -and [int]::TryParse([string] $RetryAfter, [ref] $seconds) -and $seconds -gt 0) {
        $maximumDelay = if ($RateLimit) { [int]::MaxValue } else { $script:AdoRestMaximumRetryDelayMilliseconds }
        return [int] [Math]::Min([long] $seconds * 1000, $maximumDelay)
    }

    # Saturate before exponentiation because a rate-limit refusal sequence has no attempt limit.
    $maximumExponent = [Math]::Ceiling([Math]::Log2($script:AdoRestMaximumRetryDelayMilliseconds / $script:AdoRestBaseRetryDelayMilliseconds))
    $delay = $script:AdoRestBaseRetryDelayMilliseconds * [Math]::Pow(2, [Math]::Min($Attempt - 1, $maximumExponent))
    [int] [Math]::Min($delay, $script:AdoRestMaximumRetryDelayMilliseconds)
}

# Reports whether a status justifies repeating the request.
#
# Throttling and server-side faults are the two transient classes: both resolve without any change
# to the request. Every other status reflects the request itself and repeating it would only repeat
# the failure.
function Test-AdoRestRetryableStatus {
    param(
        [Parameter(Mandatory)]
        [int] $StatusCode
    )

    $StatusCode -eq 429 -or ($StatusCode -ge 500 -and $StatusCode -le 599)
}

# Reports whether a failure raised by the injected sender justifies repeating the request.
#
# A connection reset, a DNS lookup failure, a TLS handshake failure and a client-side timeout all
# occur before any status code exists, and all of them clear on their own. Ending a capture on one
# would throw away every read already completed over a momentary blip. Anything else - a malformed
# request, a fault in the sender itself - would repeat identically and is rethrown at once.
#
# The chain of inner exceptions is walked rather than only the outermost type, because PowerShell
# wraps a failure raised inside a .NET method call in a MethodInvocationException, so the type that
# identifies the failure is usually not the one thrown.
function Test-AdoRestRetryableException {
    param(
        [Parameter(Mandatory)]
        [System.Management.Automation.ErrorRecord] $ErrorRecord
    )

    $exception = $ErrorRecord.Exception
    for ($depth = 0; $null -ne $exception -and $depth -lt $script:AdoRestExceptionChainDepth; $depth++) {
        if ($exception -is [System.Net.Http.HttpRequestException]) { return $true }

        # TaskCanceledException derives from this, and a request that timed out client-side arrives
        # as one.
        if ($exception -is [System.OperationCanceledException]) { return $true }

        $exception = $exception.InnerException
    }
    return $false
}

# Performs one Azure DevOps REST call and returns its materialized JSON body.
#
# Failure is always terminating and never partial: a capture that quietly returns fewer threads than
# the service holds is worse than one that stops, because the review that follows would treat the
# gap as evidence of absence.
function Invoke-AdoRestRequest {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Path,

        [hashtable] $Query,

        [string] $Method = 'GET',

        $Body,

        [string] $ApiVersion = $script:AdoRestDefaultApiVersion,

        # The media type the body is sent as. It defaults to plain JSON, which every resource this
        # skill reads and every pull request write it makes expects; a work item write states the
        # JSON Patch type instead, because the service reads the same body differently under each.
        [string] $ContentType = $script:AdoRestDefaultContentType,

        # Suppresses uncertain retries and token refresh. Explicit HTTP 429 refusals are safe to
        # repeat because the service has declined the write.
        [switch] $NoRetry,

        # Paging callers consume the token themselves; ordinary captures still reject truncation.
        [switch] $AsPage,

        # Comment deletion returns no document. Other operations still require a JSON response.
        [switch] $AllowEmptyResponse,

        # Attachment reads return the original bytes, not JSON. Callers verify the frozen hash.
        [switch] $AsBytes
    )

    if ($AsBytes -and ($AsPage -or $AllowEmptyResponse)) {
        throw 'A binary attachment read cannot also request a JSON page or an empty response.'
    }
    $url = Get-AdoRestRequestUrl -CollectionUrl $Transport['collectionUrl'] -Path $Path -Query $Query -ApiVersion $ApiVersion

    $bodyText = $null
    if ($null -ne $Body) {
        $bodyText = if ($Body -is [byte[]]) { , $Body } elseif ($Body -is [string]) { $Body } else { ConvertTo-JsonText -Value $Body }
    }

    $token = Get-AdoRestAccessToken -Transport $Transport
    $accept = if ($AsBytes) { 'application/octet-stream' } else { 'application/json' }
    $headers = Get-AdoRestRequestHeader -Token $token -ContentType $ContentType -Accept $accept -HasBody:($null -ne $bodyText)

    $maxAttempts = $Transport['maxAttempts']
    $response = $null
    $attempt = 0
    $rateLimitAttempt = 0
    $tokenRefreshed = $false

    while ($true) {
        $attempt++

        $transportFailure = $null
        try {
            $response = & $Transport['sendRequest'] -Method $Method -Url $url -Headers $headers -Body $bodyText
        } catch {
            if ($NoRetry -or -not (Test-AdoRestRetryableException -ErrorRecord $_) -or $attempt -ge $maxAttempts) { throw }
            $transportFailure = $_
        }

        if ($null -ne $transportFailure) {
            # The connection never produced a response, so there is no Retry-After to honour and the
            # backoff schedule is the only guidance available.
            & $Transport['wait'] -Milliseconds (Get-AdoRestRetryDelayMilliseconds -RetryAfter $null -Attempt $attempt) | Out-Null
            continue
        }

        if ($null -eq $response) {
            throw "The Azure DevOps request to '$url' produced no response."
        }

        $statusCode = [int] (Get-JsonProperty -Node $response -Name 'statusCode' -Default 0)

        if ($statusCode -eq 429) {
            $rateLimitAttempt++
            # Throttling does not spend the independent budget for server and connection faults.
            $attempt--
            $retryAfter = Get-AdoRestResponseHeader -Headers (Get-JsonProperty -Node $response -Name 'headers') -Name $script:AdoRestRetryAfterHeader
            $delay = Get-AdoRestRetryDelayMilliseconds -RetryAfter $retryAfter -Attempt $rateLimitAttempt -RateLimit
            Write-Warning "Azure DevOps refused the request due to a rate limit. Waiting $delay ms before retrying (rate-limit refusal $rateLimitAttempt)."
            try {
                & $Transport['wait'] -Milliseconds $delay | Out-Null
            } catch {
                throw (New-PullRequestServiceRefusalException -Message "Azure DevOps rate-limit recovery was interrupted: $($_.Exception.Message)")
            }
            continue
        }

        # A rejected token is retried once with a freshly acquired one. This costs one attempt beyond
        # the throttling budget on purpose: it is not a repeat of the same request but a different
        # one, and refusing it would fail any capture that outlives the token it started with. The
        # flag bounds it to a single refresh, so a token the service will never accept surfaces as
        # the rejection it is rather than as a loop. A caller that has forbidden repeats forgoes
        # this too, because the refreshed attempt sends the same body a second time.
        if ($statusCode -eq $script:AdoRestUnauthorizedStatus -and -not $tokenRefreshed -and -not $NoRetry) {
            $tokenRefreshed = $true
            $token = Get-AdoRestAccessToken -Transport $Transport -Refresh
            $headers = Get-AdoRestRequestHeader -Token $token -ContentType $ContentType -Accept $accept -HasBody:($null -ne $bodyText)
            continue
        }

        if ($NoRetry -or -not (Test-AdoRestRetryableStatus -StatusCode $statusCode) -or $attempt -ge $maxAttempts) {
            break
        }

        $retryAfter = Get-AdoRestResponseHeader -Headers (Get-JsonProperty -Node $response -Name 'headers') -Name $script:AdoRestRetryAfterHeader
        & $Transport['wait'] -Milliseconds (Get-AdoRestRetryDelayMilliseconds -RetryAfter $retryAfter -Attempt $attempt) | Out-Null
    }

    $statusCode = [int] (Get-JsonProperty -Node $response -Name 'statusCode' -Default 0)
    $contentBytes = Get-JsonProperty -Node $response -Name 'contentBytes'
    $text = if ($null -eq $contentBytes) { '' } else { [System.Text.Encoding]::UTF8.GetString($contentBytes) }
    $contentType = [string] (Get-JsonProperty -Node $response -Name 'contentType' -Default '')

    # Error text quotes the URL and the response body only. Request headers carry the bearer token,
    # and a diagnostic that leaks it would put a live credential into logs and transcripts.
    if ($statusCode -lt 200 -or $statusCode -gt 299) {
        $message = "Azure DevOps returned HTTP $statusCode for '$url': $(Get-AdoRestBodyExcerpt -Body $text)"
        if ($statusCode -ge 400 -and $statusCode -le 499) {
            throw (New-PullRequestServiceRefusalException -Message $message)
        }
        throw $message
    }

    if ($AsBytes) {
        # A sign-in document is sometimes returned with 203. HTML itself is a valid report, so
        # byte verification, not the JSON path's HTML sniffing, establishes attachment identity.
        if ($statusCode -ne 200 -or $contentType -notmatch '^(application/octet-stream|application/zip|application/x-zip-compressed|text/html|text/plain)(;|$)') {
            throw "Azure DevOps returned an unexpected attachment response: HTTP $statusCode, '$contentType'."
        }
        if ($null -eq $contentBytes -or $contentBytes.Length -eq 0) { throw 'Azure DevOps returned an empty attachment.' }
        return , ([byte[]] $contentBytes)
    }

    if (Test-AdoRestHtmlResponse -ContentType $contentType -Body $text) {
        throw "Azure DevOps returned an HTML document rather than JSON for '$url' with HTTP $statusCode, which is how it serves a sign-in page to a caller it does not accept. The likely cause is an expired access token or a token issued for the wrong tenant. Re-authenticate and supply the tenant that owns the collection."
    }

    $continuation = Get-AdoRestResponseHeader -Headers (Get-JsonProperty -Node $response -Name 'headers') -Name $script:AdoRestContinuationTokenHeader
    if (-not $AsPage -and -not [string]::IsNullOrWhiteSpace([string] $continuation)) {
        throw "Azure DevOps returned a continuation token for '$url', so the result is truncated. This helper does not page and would otherwise report an incomplete capture as complete."
    }

    $leading = $text.TrimStart()
    if ($AllowEmptyResponse -and $leading.Length -eq 0) {
        return
    }
    if ($leading.Length -eq 0 -or -not ($leading.StartsWith('{') -or $leading.StartsWith('['))) {
        throw "Azure DevOps returned a body that is not a JSON object or array for '$url': $(Get-AdoRestBodyExcerpt -Body $text)"
    }

    $payload = ConvertFrom-JsonVerbatim -Text $text
    if ($AsPage) {
        return [ordered]@{ payload = $payload; continuationToken = $continuation }
    }
    if ($payload -is [object[]]) { return , $payload }
    return $payload
}

# Reads the items out of an Azure DevOps list envelope, refusing anything that is not one.
#
# Every Azure DevOps collection resource answers with a 'value' array and a 'count'. Reading 'value'
# with an empty default instead would turn a payload that is not a list envelope - an error object
# served with a success status, or an envelope whose shape the service has changed - into a capture
# that succeeds and reports nothing. That renders as a pull request nobody has commented on, which
# is indistinguishable from the truth and therefore the most expensive failure this capture has. An
# absent collection is consequently a terminating failure, while a collection that is present and
# empty is an ordinary empty result and is returned as one.
#
# The stated count is checked against the items because it is the service's own count, so a
# disagreement is truncation that the continuation header did not signal. Nothing else in the
# response can reveal that loss: a short list looks exactly like a complete one.
function Get-AdoRestListItem {
    param(
        [Parameter(Mandatory)]
        [AllowNull()]
        $Payload,

        # Names the resource in any failure, because a capture reads several collections in a row
        # and the payload itself carries nothing that identifies which one it came from.
        [Parameter(Mandatory)]
        [string] $Source
    )

    if (-not (Test-JsonProperty -Node $Payload -Name $script:AdoRestListValueProperty)) {
        throw "Azure DevOps returned no '$script:AdoRestListValueProperty' collection for '$Source', so the response is not the list this read requires: $(Get-AdoRestBodyExcerpt -Body (ConvertTo-JsonText -Value $Payload))"
    }

    $items = Get-JsonArrayProperty -Node $Payload -Name $script:AdoRestListValueProperty

    if (Test-JsonProperty -Node $Payload -Name $script:AdoRestListCountProperty) {
        $statedCount = 0
        $rawCount = Get-JsonProperty -Node $Payload -Name $script:AdoRestListCountProperty
        if (-not [int]::TryParse([string] $rawCount, [ref] $statedCount)) {
            throw "Azure DevOps reported a non-numeric item count '$rawCount' for '$Source'."
        }
        if ($statedCount -ne $items.Count) {
            throw "Azure DevOps reported $statedCount items for '$Source' but sent $($items.Count), so the result is incomplete."
        }
    }

    return , $items
}

# Performs one Azure DevOps REST call that answers with a collection, and returns its items.
#
# Every list read goes through here rather than reaching into the payload at the call site, so the
# envelope check applies to all of them and cannot be omitted from one and lost there.
function Invoke-AdoRestListRequest {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [string] $Path,

        [hashtable] $Query,

        [string] $ApiVersion = $script:AdoRestDefaultApiVersion,

        # Suppresses uncertain retries through the list form, but not explicit throttling refusals.
        [switch] $NoRetry
    )

    $payload = Invoke-AdoRestRequest -Transport $Transport -Path $Path -Query $Query -ApiVersion $ApiVersion -NoRetry:$NoRetry
    $items = Get-AdoRestListItem -Payload $payload -Source $Path
    return , $items
}
