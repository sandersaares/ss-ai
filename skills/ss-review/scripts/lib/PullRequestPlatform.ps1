#Requires -Version 7.6

# Routes an operation to the adapter that serves the target's platform
# (docs/implementation.md, "Platform dispatch").
#
# Both executables have to answer the same question before they can do anything: which platform is
# this target on, and which transport and adapter therefore serve it. Answering it here rather than
# in each composition root means the answer is given once and can be exercised offline. A dispatch
# written inline in an executable is the one decision in these helpers whose inversion produces a
# result that is wrong yet perfectly well formed, so it is also the decision that most needs a test.
#
# The production primitives are resolved lazily inside each branch rather than as parameter
# defaults, so that capturing an Azure DevOps pull request never constructs a GitHub invoker and
# vice versa, and so that a test can substitute either without the other being reached.
#
# Every dispatch names both platforms explicitly and refuses anything else. Serving an unrecognised
# platform from the Azure DevOps branch would reach for an Azure DevOps token on behalf of a pull
# request that has nothing to do with Azure DevOps, and the run would fail as an authentication
# error against a collection that was never involved — a symptom that points nowhere near the cause.
#
# The platform vocabulary is matched case-insensitively, as it is everywhere else in these helpers
# (lib\PullRequestTarget.ps1, lib\PullRequestModel.ps1, lib\PullRequestIdentity.ps1 all compare it
# with -eq, and the executables validate it with a case-insensitive ValidateSet).
#
# Functions from lib\AdoRestTransport.ps1, lib\AdoPullRequestApi.ps1, lib\GitHubCliTransport.ps1,
# lib\GitHubPullRequestApi.ps1 and lib\PullRequestTransportHost.ps1 are called directly; the
# executables dot-source all of them.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Describes a target the dispatch cannot serve.
#
# The message names the offending value so that a target assembled elsewhere can be traced back to
# whatever produced it, and lists the platforms that are served so the reader does not have to find
# the vocabulary in the source. An absent or empty platform renders as a word rather than as empty
# quotes, which would otherwise read as though the value were merely unprintable.
function Get-UnsupportedPullRequestPlatformMessage {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Platform
    )

    $rendered = if ([string]::IsNullOrEmpty($Platform)) { 'none' } else { "'$Platform'" }
    "The target names an unsupported platform: $rendered. The supported platforms are 'azureDevOps' and 'gitHub'."
}

# Builds the transport that serves a target.
#
# The caller supplies the platform primitives only when substituting them; production runs leave
# them unset and get the real implementations.
function New-PullRequestTransport {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Tenant,

        [AllowNull()]
        [scriptblock] $InvokeGh,

        [AllowNull()]
        [scriptblock] $SendRequest,

        [AllowNull()]
        [scriptblock] $GetAccessToken
    )

    switch ($Target['platform']) {
        'gitHub' {
            $invoker = if ($null -ne $InvokeGh) { $InvokeGh } else { Get-GitHubCliInvoker }
            return New-GitHubCliTransport -InvokeGh $invoker
        }
        'azureDevOps' {
            $sender = if ($null -ne $SendRequest) { $SendRequest } else { Get-HttpRequestSender }
            $tokenProvider = if ($null -ne $GetAccessToken) { $GetAccessToken } else { Get-AzureDevOpsTokenProvider -Tenant $Tenant }
            return New-AdoRestTransport `
                -CollectionUrl $Target['collectionUrl'] `
                -SendRequest $sender `
                -GetAccessToken $tokenProvider
        }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}

# Captures the pull request a target names.
function Get-PullRequestContextForTarget {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Transport,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    switch ($Target['platform']) {
        'gitHub' { return Get-GitHubPullRequestContext -Transport $Transport -Target $Target }
        'azureDevOps' { return Get-AdoPullRequestContext -Transport $Transport -Target $Target }
        default { throw (Get-UnsupportedPullRequestPlatformMessage -Platform $Target['platform']) }
    }
}
