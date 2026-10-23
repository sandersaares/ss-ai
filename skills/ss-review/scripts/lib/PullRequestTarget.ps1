#Requires -Version 7.0

# Resolves which pull request a run is about (docs/implementation.md, "Target resolution").
#
# Both the context capture and the reviewer registration need the same answer -- which platform, which
# organization, project, repository and pull request -- before they can do anything else, and neither
# has any business inferring it separately. Keeping resolution here also keeps it pure: it parses and
# validates, and never reaches the network.
#
# A target may be under-specified on purpose. Azure DevOps can resolve a pull request from its
# identifier alone, so project and repository are allowed to stay empty here and be filled in by the
# platform adapter.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Azure DevOps places the repository under a fixed path segment, which is the only reliable anchor
# in a pull request URL: the project name precedes it and the repository name follows it, and both
# may contain characters that make positional parsing from the front unsafe.
$script:AzureDevOpsRepositorySegment = '_git'

function Split-UrlPathSegment {
    param(
        [Parameter(Mandatory)]
        [uri] $Url
    )

    $segments = $Url.AbsolutePath.Split('/', [StringSplitOptions]::RemoveEmptyEntries)
    return , @($segments)
}

function Get-PullRequestPlatformFromUrl {
    param(
        [Parameter(Mandatory)]
        [uri] $Url
    )

    $hostName = $Url.Host.ToLowerInvariant()
    if ($hostName -eq 'github.com' -or $hostName -eq 'www.github.com') {
        return 'gitHub'
    }
    if ($hostName -eq 'dev.azure.com' -or $hostName.EndsWith('.visualstudio.com')) {
        return 'azureDevOps'
    }

    throw "Cannot determine the pull request platform from host '$($Url.Host)'. Supply -Platform explicitly."
}

function Resolve-AzureDevOpsTargetFromUrl {
    param(
        [Parameter(Mandatory)]
        [uri] $Url
    )

    $segments = Split-UrlPathSegment -Url $Url
    $repositoryIndex = -1
    for ($index = 0; $index -lt $segments.Count; $index++) {
        if ($segments[$index] -ieq $script:AzureDevOpsRepositorySegment) {
            $repositoryIndex = $index
            break
        }
    }

    if ($repositoryIndex -lt 1 -or $segments.Count -le $repositoryIndex + 3) {
        throw "'$Url' is not an Azure DevOps pull request URL."
    }
    if ($segments[$repositoryIndex + 2] -inotmatch '^pullrequest$') {
        throw "'$Url' is not an Azure DevOps pull request URL."
    }

    $pullRequestId = 0
    if (-not [int]::TryParse($segments[$repositoryIndex + 3], [ref] $pullRequestId)) {
        throw "'$Url' does not contain a numeric pull request identifier."
    }

    $isDevAzureHost = $Url.Host -ieq 'dev.azure.com' -or $Url.Host -ieq 'www.dev.azure.com'
    if ($isDevAzureHost -and $repositoryIndex -lt 2) {
        throw "'$Url' is missing the organization or project segment."
    }

    # Everything ahead of the project segment is the collection, which is the organization on
    # dev.azure.com and an optional collection name on the older per-organization hosts.
    $collectionSegments = @()
    if ($repositoryIndex -ge 2) {
        $collectionSegments = @($segments[0..($repositoryIndex - 2)])
    }

    $organization = if ($isDevAzureHost) {
        [uri]::UnescapeDataString($segments[0])
    } else {
        $Url.Host.Split('.')[0]
    }

    $collectionUrl = "$($Url.Scheme)://$($Url.Authority)"
    if ($collectionSegments.Count -gt 0) {
        $collectionUrl = "$collectionUrl/$($collectionSegments -join '/')"
    }

    return [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = $collectionUrl
        organization  = $organization
        project       = [uri]::UnescapeDataString($segments[$repositoryIndex - 1])
        repository    = [uri]::UnescapeDataString($segments[$repositoryIndex + 1])
        pullRequestId = $pullRequestId
    }
}

function Resolve-GitHubTargetFromUrl {
    param(
        [Parameter(Mandatory)]
        [uri] $Url
    )

    $segments = Split-UrlPathSegment -Url $Url
    if ($segments.Count -lt 4 -or $segments[2] -inotmatch '^pull$') {
        throw "'$Url' is not a GitHub pull request URL."
    }

    $pullRequestId = 0
    if (-not [int]::TryParse($segments[3], [ref] $pullRequestId)) {
        throw "'$Url' does not contain a numeric pull request number."
    }

    return [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = [uri]::UnescapeDataString($segments[0])
        project       = $null
        repository    = [uri]::UnescapeDataString($segments[1])
        pullRequestId = $pullRequestId
    }
}

# Builds the browser URL for a pull request, which the capture records so that a reader can open the
# pull request the capture describes.
#
# This is the single derivation of a pull request URL. Both platform adapters call it rather than
# assembling their own, because a second form drifts silently: the divergence shows only as a
# reported URL that does not open the pull request a reviewer was sent to.
#
# An Azure DevOps target may still be missing its project and repository here. The adapter resolves
# those from the pull request identifier alone, and this function runs before that, so it renders
# what it can rather than refusing; an adapter that requires the resolved coordinates guards for
# them at its own call site.
function Get-PullRequestWebUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    if ($Target['platform'] -eq 'gitHub') {
        $owner = [uri]::EscapeDataString($Target['organization'])
        $repository = [uri]::EscapeDataString($Target['repository'])
        return "https://github.com/$owner/$repository/pull/$($Target['pullRequestId'])"
    }

    # Resolve-PullRequestTarget normalizes the collection URL, so a resolved target never carries a
    # trailing separator. The trim is kept for callers that assemble a target directly: every
    # segment below supplies its own leading separator, and Azure DevOps serves the doubled form, so
    # the defect would surface only as a URL that looks wrong to whoever it was published to.
    $collectionUrl = ([string] $Target['collectionUrl']).TrimEnd('/')

    if (-not $Target['project'] -or -not $Target['repository']) {
        return "$collectionUrl/_git/pullrequest/$($Target['pullRequestId'])"
    }

    $project = [uri]::EscapeDataString($Target['project'])
    $repository = [uri]::EscapeDataString($Target['repository'])
    "$collectionUrl/$project/_git/$repository/pullrequest/$($Target['pullRequestId'])"
}

# Selects the target coordinates a caller actually supplied.
#
# An unsupplied parameter must not be forwarded as an empty string. A validated set rejects the
# empty string outright, and an empty repository would override the one parsed from the URL rather
# than leaving it alone.
function Get-PullRequestTargetParameter {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $BoundParameter
    )

    $selected = @{}
    foreach ($name in @('Url', 'Platform', 'Organization', 'Project', 'Repository', 'PullRequestId', 'CollectionUrl')) {
        if ($BoundParameter.Keys -contains $name) {
            $selected[$name] = $BoundParameter[$name]
        }
    }
    $selected
}

# Produces the resolved target from either a pull request URL or explicit coordinates.
function Resolve-PullRequestTarget {    param(
        [string] $Url,

        [ValidateSet('azureDevOps', 'gitHub')]
        [string] $Platform,

        [string] $Organization,

        [string] $Project,

        [string] $Repository,

        [int] $PullRequestId,

        [string] $CollectionUrl
    )

    $target = $null
    if ($Url) {
        $parsed = $null
        if (-not [uri]::TryCreate($Url, [UriKind]::Absolute, [ref] $parsed)) {
            throw "'$Url' is not an absolute URL."
        }

        $resolvedPlatform = if ($Platform) { $Platform } else { Get-PullRequestPlatformFromUrl -Url $parsed }
        $target = if ($resolvedPlatform -eq 'gitHub') {
            Resolve-GitHubTargetFromUrl -Url $parsed
        } else {
            Resolve-AzureDevOpsTargetFromUrl -Url $parsed
        }
    } else {
        if (-not $Platform) {
            throw 'Supply either -Url or -Platform with explicit coordinates.'
        }
        if (-not $Organization) {
            throw 'Supply -Organization when resolving a target without a URL.'
        }
        if ($PullRequestId -le 0) {
            throw 'Supply a positive -PullRequestId when resolving a target without a URL.'
        }

        $collection = $CollectionUrl
        if ($Platform -eq 'azureDevOps' -and -not $collection) {
            $collection = "https://dev.azure.com/$([uri]::EscapeDataString($Organization))"
        }

        $target = [ordered]@{
            platform      = $Platform
            collectionUrl = if ($Platform -eq 'gitHub') { $null } else { $collection }
            organization  = $Organization
            project       = if ($Platform -eq 'gitHub') { $null } else { $Project }
            repository    = $Repository
            pullRequestId = $PullRequestId
        }
    }

    # Explicit coordinates win over the URL so a caller can correct a URL that omits or misstates
    # part of the target, which Azure DevOps short links routinely do.
    if ($Project -and $target['platform'] -eq 'azureDevOps') { $target['project'] = $Project }
    if ($Repository) { $target['repository'] = $Repository }
    if ($CollectionUrl -and $target['platform'] -eq 'azureDevOps') { $target['collectionUrl'] = $CollectionUrl }

    # An unsupplied [int] arrives as zero, which is indistinguishable by value from a caller who
    # typed zero, so the bound-parameter set is what separates a correction from a default. The
    # override matters most for reviewer registration, which writes to whichever pull request the
    # target names: silently keeping the URL's identifier would post to the wrong one.
    if ($PSBoundParameters.ContainsKey('PullRequestId')) {
        if ($PullRequestId -lt 1) {
            throw "'$PullRequestId' is not a pull request identifier. Supply a positive -PullRequestId."
        }
        $target['pullRequestId'] = $PullRequestId
    }

    # The collection URL is normalized once here so that the resolved target carries the single
    # authoritative spelling. Every consumer appends its own leading separator, so a surviving
    # trailing separator would reach the repository link and the capture as a doubled separator
    # while the request destinations, which trim defensively, used a single one -- the same pull
    # request would then capture differently depending on how the caller spelled the collection.
    if ($target['platform'] -eq 'azureDevOps' -and $target['collectionUrl']) {
        $target['collectionUrl'] = ([string] $target['collectionUrl']).TrimEnd('/')
    }

    if ($target['platform'] -eq 'gitHub' -and -not $target['repository']) {
        throw 'A GitHub target requires a repository.'
    }

    $target['webUrl'] = Get-PullRequestWebUrl -Target $target
    return $target
}
