#Requires -Version 7.0

# Normalizes who said something, and decides whether that was a person or a machine
# (docs/implementation.md, "Authorship classification").
#
# Both entry points need the same notion of identity: the capture attributes every comment, and the
# registration has to recognize the authenticated account among the reviewers already on the
# pull request. Neither can use display names for this, because they are not unique.
#
# Classification is deliberately two-stage. The account answers who acted, but it does not settle
# authorship on its own: an agent posting under a person's credentials is the normal case in this
# repository, so a per-comment marker overrides the account. This is pure and never reaches the
# network.
#
# Calls Get-JsonProperty from JsonValue.ps1, because strict mode throws on a missing key and an
# identity record carries different subsets of its fields depending on the resource it came from.
# Libraries never dot-source one another; the executable composition root dot-sources every library
# it needs before calling into this one.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Azure DevOps encodes the subject class in the descriptor prefix. Service identities are the
# classes that post automated review feedback; the remaining classes belong to people.
$script:AzureDevOpsPersonDescriptorPrefixes = @('aad.', 'msa.')
$script:AzureDevOpsServiceDescriptorPrefixes = @('aadsp.', 's2s.', 'svc.')

# Azure DevOps thread property bags carry these keys when an automated reviewer produced the thread.
# Matching is on the final dot-separated segment because publishers namespace their keys.
$script:AzureDevOpsAgentPropertyMarkers = @(
    'LLMModel'
    'Model'
    'Focus_Area'
    'Intent'
    'PromptVersionQualifier'
    'SeverityThreshold'
    'GitOpsUserAgent'
    'ExtensionName'
)

# Every agent in this repository begins what it writes with this marker, so a comment starting with
# it is machine-authored regardless of which account posted it (global.instructions.md,
# "Communication policy").
$script:AgentContentMarker = '[Copilot speaking]'

# GitHub apps live under this path, which is a reliable signal even where the account type is absent.
$script:GitHubAppUrlPrefix = 'https://github.com/apps/'

function Test-StringStartsWithAny {
    param(
        [AllowNull()]
        [AllowEmptyString()]
        [string] $Value,

        [Parameter(Mandatory)]
        [string[]] $Prefix
    )

    if ([string]::IsNullOrEmpty($Value)) {
        return $false
    }
    foreach ($candidate in $Prefix) {
        if ($Value.StartsWith($candidate, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

# Produces the one identity shape used everywhere, together with the evidence for its classification.
#
# The evidence is retained rather than discarded because a reader who disagrees with a classification
# must be able to see what it was based on without re-fetching the pull request.
#
# The account-type vocabulary is `person`, `service` and `indeterminate`. It is deliberately distinct
# from the authorship vocabulary `agent`, `human` and `indeterminate`: the two answer different
# questions, and a single word shared between them would read as though an account that belongs to a
# person settled who composed the comment, which it does not.
function New-PullRequestIdentity {
    param(
        [Parameter(Mandatory)]
        [ValidateSet('azureDevOps', 'gitHub')]
        [string] $Platform,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Id,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $DisplayName,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $UniqueName,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Descriptor,

        # GitHub reports "User", "Bot", "Organization" or "Mannequin"; the GraphQL __typename carries
        # the same values.
        [AllowNull()]
        [AllowEmptyString()]
        [string] $AccountType,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Url
    )

    $kind = 'indeterminate'
    $evidence = [System.Collections.Generic.List[string]]::new()

    if ($Platform -eq 'azureDevOps') {
        if (Test-StringStartsWithAny -Value $Descriptor -Prefix $script:AzureDevOpsServiceDescriptorPrefixes) {
            $kind = 'service'
            $evidence.Add("descriptor:$($Descriptor.Split('.')[0])")
        } elseif (Test-StringStartsWithAny -Value $Descriptor -Prefix $script:AzureDevOpsPersonDescriptorPrefixes) {
            $kind = 'person'
            $evidence.Add("descriptor:$($Descriptor.Split('.')[0])")
        }
    } else {
        # The trailing "[bot]" on a login is deliberately not consulted: the same account is returned
        # as "copilot-pull-request-reviewer[bot]" by one endpoint and as "Copilot" by another, so the
        # suffix reports different answers for one identity.
        if ($AccountType -ieq 'Bot') {
            $kind = 'service'
            $evidence.Add('accountType:Bot')
        } elseif (Test-StringStartsWithAny -Value $Descriptor -Prefix @('BOT_')) {
            $kind = 'service'
            $evidence.Add('nodeId:BOT_')
        } elseif (Test-StringStartsWithAny -Value $Url -Prefix @($script:GitHubAppUrlPrefix)) {
            $kind = 'service'
            $evidence.Add('url:apps')
        } elseif ($AccountType -ieq 'User') {
            $kind = 'person'
            $evidence.Add('accountType:User')
        }
    }

    $resolvedName = $DisplayName
    if ([string]::IsNullOrWhiteSpace($resolvedName)) { $resolvedName = $UniqueName }
    if ([string]::IsNullOrWhiteSpace($resolvedName)) { $resolvedName = 'unknown' }

    [ordered]@{
        platform    = $Platform
        id          = if ([string]::IsNullOrWhiteSpace($Id)) { $null } else { $Id }
        displayName = $resolvedName
        uniqueName  = if ([string]::IsNullOrWhiteSpace($UniqueName)) { $null } else { $UniqueName }
        descriptor  = if ([string]::IsNullOrWhiteSpace($Descriptor)) { $null } else { $Descriptor }
        accountType = if ([string]::IsNullOrWhiteSpace($AccountType)) { $null } else { $AccountType }
        kind        = $kind
        evidence    = @($evidence)
    }
}

# Decides whether a comment was written by a machine, or leaves the question open.
#
# Only two things prove machine authorship: an account that cannot belong to a person, and a marker
# the agent itself wrote. Everything else is left indeterminate, including anything posted from a
# person's account, because such an account is routinely driven by an agent and therefore proves who
# the comment was posted as rather than who composed it. Claiming such a comment as human-written
# would be a guess presented as a fact, and the review deduplicates its findings against this
# classification.
#
# The value `human` is never produced here. It is settled by the classification pass
# (docs/design.md, "Authorship classification"), which reads the comment and can weigh the agent
# conventions that no fixed rule captures. The authorship vocabulary `agent`, `human` and
# `indeterminate` is separate from the identity account-type vocabulary `person`, `service` and
# `indeterminate`, and `service` is never an authorship value.
#
# A system-generated comment type is not part of this decision. Azure DevOps records a person
# approving a pull request as a system comment, and treating the comment type as the signal would
# report that approval as machine-generated.
function Get-PullRequestAuthorship {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Identity,

        [AllowNull()]
        [AllowEmptyString()]
        [string] $Content,

        [AllowNull()]
        [System.Collections.IDictionary] $Properties
    )

    $evidence = [System.Collections.Generic.List[string]]::new()
    foreach ($item in $Identity['evidence']) {
        $evidence.Add("identity:$item")
    }

    $authorship = if ($Identity['kind'] -ceq 'service') { 'agent' } else { 'indeterminate' }

    if ($null -ne $Properties) {
        foreach ($key in $Properties.Keys) {
            $leaf = ([string] $key).Split('.')[-1]
            if ($script:AzureDevOpsAgentPropertyMarkers -contains $leaf) {
                $authorship = 'agent'
                $evidence.Add("property:$leaf")
            }
        }
    }

    # The marker is required at the start of the body rather than anywhere within it. Agents place it
    # first in what they write (global.instructions.md, "Communication policy"), so a marker further
    # in is a person quoting or discussing an agent comment. Matching that would produce a
    # mechanical 'agent' verdict, and only an indeterminate comment reaches the classification pass
    # (docs/design.md, "Authorship classification"), so the misattribution would never be revisited.
    # Leading whitespace is tolerated because a body may be indented or start with a blank line, and
    # the comparison is ordinal so that the marker matches exactly the text agents emit.
    if (-not [string]::IsNullOrEmpty($Content) -and $Content.TrimStart().StartsWith($script:AgentContentMarker, [StringComparison]::Ordinal)) {
        $authorship = 'agent'
        $evidence.Add('content:agentMarker')
    }

    [ordered]@{
        authorship = $authorship
        evidence   = @($evidence)
    }
}

# Identifier fields that name an account, in the order they settle a comparison.
#
# A service reports several identifier forms for one account and populates different subsets of them
# on different resources, so a comparison that insists on one field reports two views of the same
# account as two accounts. The order is by authority: an Azure DevOps subject descriptor appears on
# comments in exactly the form its connection data returns, an account identifier is stable while a
# login is not, and a login-shaped name is the last resort.
$script:PullRequestIdentityMatchKeys = @('descriptor', 'id', 'uniqueName')

# Reports whether two identity records name the same account.
#
# The comparison settles on the most authoritative field both records carry and does not consult the
# weaker ones afterwards: two records that both carry an account identifier and disagree on it are
# different accounts, however similar their remaining fields look.
function Test-PullRequestIdentityMatch {
    param(
        [AllowNull()]
        [System.Collections.IDictionary] $Left,

        [AllowNull()]
        [System.Collections.IDictionary] $Right
    )

    if ($null -eq $Left -or $null -eq $Right) {
        return $false
    }

    foreach ($name in $script:PullRequestIdentityMatchKeys) {
        $leftValue = [string] (Get-JsonProperty -Node $Left -Name $name)
        $rightValue = [string] (Get-JsonProperty -Node $Right -Name $name)
        if ([string]::IsNullOrWhiteSpace($leftValue) -or [string]::IsNullOrWhiteSpace($rightValue)) { continue }
        return [string]::Equals($leftValue, $rightValue, [StringComparison]::OrdinalIgnoreCase)
    }

    return $false
}

# Reports whether two identity records might name the same account.
#
# Any single agreeing field is enough. This is the comparison to use before a write that would be
# destructive if it went ahead against an account already present, because the two failure modes are
# not symmetric: treating two accounts as one merely skips a write that was not needed, while
# treating one account as two performs a write that overwrites the state already recorded for it.
function Test-PullRequestIdentityMayMatch {
    param(
        [AllowNull()]
        [System.Collections.IDictionary] $Left,

        [AllowNull()]
        [System.Collections.IDictionary] $Right
    )

    if ($null -eq $Left -or $null -eq $Right) {
        return $false
    }

    foreach ($name in $script:PullRequestIdentityMatchKeys) {
        $leftValue = [string] (Get-JsonProperty -Node $Left -Name $name)
        $rightValue = [string] (Get-JsonProperty -Node $Right -Name $name)
        if ([string]::IsNullOrWhiteSpace($leftValue) -or [string]::IsNullOrWhiteSpace($rightValue)) { continue }
        if ([string]::Equals($leftValue, $rightValue, [StringComparison]::OrdinalIgnoreCase)) { return $true }
    }

    return $false
}

# Reports whether an identity is the account the run authenticated as.
function Test-PullRequestIdentityIsSelf {
    param(
        [AllowNull()]
        [System.Collections.IDictionary] $Identity,

        [AllowNull()]
        [System.Collections.IDictionary] $AuthenticatedIdentity
    )

    Test-PullRequestIdentityMatch -Left $Identity -Right $AuthenticatedIdentity
}
