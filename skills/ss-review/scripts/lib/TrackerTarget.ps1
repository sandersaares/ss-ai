#Requires -Version 7.0

# Resolves which tracker a review is filed into (docs/implementation.md, "Tracker target
# resolution").
#
# A tracker is where work is tracked rather than where code is discussed: a GitHub repository's
# issues, or an Azure DevOps project's boards. It is named separately from the pull request, because
# the two need not agree - a review of an Azure DevOps pull request may legitimately be filed as
# GitHub issues - and because a review of content that is no pull request at all has nothing else to
# name it by.
#
# Resolution is pure. It parses and validates and never reaches the network, so every refusal it is
# capable of is reached before anything is created.
#
# The accepted URL shapes are enumerated rather than parsed by position. Resolve-PullRequestTarget
# refuses everything that is not a pull request URL, so nothing here can be borrowed from it, and a
# tracker URL is the one a user pastes out of a browser: it may name the issue list, the board, the
# repository itself or a pull request inside it. Guessing from segment positions would accept a URL
# that names something else entirely and file a whole review into it.
#
# This is a library file: it is dot-sourced rather than executed, defines only functions and
# constants, and dot-sources nothing itself. It calls Get-JsonProperty from JsonValue.ps1 and
# Get-PullRequestPlatformFromUrl from PullRequestTarget.ps1, which the composition root loads
# alongside this file.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The Azure DevOps service area prefix. Every service area a URL can name - `_git`, `_boards`,
# `_workitems`, `_settings` and the rest - begins with it, and Azure DevOps forbids a project name
# from beginning with it, so the first segment carrying it is the boundary between the project and
# whatever the URL went on to name. Anchoring on the prefix rather than on a list of known areas is
# what lets a URL from a part of the service this skill has never heard of still resolve its project.
$script:TrackerAdoServiceAreaPrefix = '_'

# The Azure DevOps area path separator. Azure DevOps states an area path as a backslash-separated
# path rooted at the project name, in the one place in this skill where a backslash is meaningful
# rather than a mistake.
$script:TrackerAreaPathSeparator = '\'

# Produces the resolved tracker from a URL, with explicit coordinates overriding what it carried.
#
# A caller that has no URL supplies the platform and the coordinates directly, which is what a
# review of content that is not a pull request does.
function Resolve-TrackerTarget {
    param(
        [string] $Url,

        # Empty is accepted, because a composition root passes its own unbound parameter straight
        # through rather than assembling a splat.
        [AllowEmptyString()]
        [ValidateSet('azureDevOps', 'gitHub', '')]
        [string] $Platform,

        [string] $Organization,

        [string] $Project,

        [string] $Repository,

        [string] $CollectionUrl
    )

    $target = $null
    if ($Url) {
        $parsed = $null
        if (-not [uri]::TryCreate($Url, [UriKind]::Absolute, [ref] $parsed)) {
            throw "'$Url' is not an absolute URL."
        }

        $resolvedPlatform = if ($Platform) { $Platform } else { Get-PullRequestPlatformFromUrl -Url $parsed }
        $target = if ($resolvedPlatform -ceq 'gitHub') {
            Resolve-GitHubTrackerFromUrl -Url $parsed -Repository $Repository
        } else {
            Resolve-AdoTrackerFromUrl -Url $parsed -Project $Project
        }
    } else {
        if (-not $Platform) {
            throw 'Supply either -Url or -Platform with explicit coordinates for the tracker.'
        }
        if (-not $Organization) {
            throw 'Supply -Organization when resolving a tracker without a URL.'
        }

        $collection = $CollectionUrl
        if ($Platform -ceq 'azureDevOps' -and -not $collection) {
            $collection = "https://dev.azure.com/$([uri]::EscapeDataString($Organization))"
        }

        $target = [ordered]@{
            platform      = $Platform
            collectionUrl = if ($Platform -ceq 'gitHub') { $null } else { $collection }
            organization  = $Organization
            project       = if ($Platform -ceq 'gitHub') { $null } else { $Project }
            repository    = if ($Platform -ceq 'gitHub') { $Repository } else { $null }
        }
    }

    # Explicit coordinates win over the URL, so a caller can correct a URL that names the wrong part
    # of a service or omits part of the tracker.
    if ($Organization) { $target['organization'] = $Organization }
    if ($Project -and $target['platform'] -ceq 'azureDevOps') { $target['project'] = $Project }
    if ($Repository -and $target['platform'] -ceq 'gitHub') { $target['repository'] = $Repository }
    if ($CollectionUrl -and $target['platform'] -ceq 'azureDevOps') { $target['collectionUrl'] = $CollectionUrl }

    # A GitHub clone URL carries a '.git' suffix the repository name does not, and the fallback this
    # resolution serves reads the repository's own 'origin' remote, so the suffix arrives routinely
    # by whichever route the name came. GitHub forbids a repository name ending in it, so trimming it
    # can shorten no real name.
    if ($target['platform'] -ceq 'gitHub' -and $target['repository']) {
        $repository = [string] $target['repository']
        if ($repository.EndsWith('.git', [StringComparison]::OrdinalIgnoreCase)) {
            $target['repository'] = $repository.Substring(0, $repository.Length - 4)
        }
    }

    # Normalized once here, for the reason a pull request target normalizes it: every consumer
    # supplies its own leading separator, and a surviving trailing one reaches the recorded URLs as a
    # doubled separator while the request destinations trim it, so the same tracker would be recorded
    # differently depending on how the caller spelled the collection.
    if ($target['platform'] -ceq 'azureDevOps' -and $target['collectionUrl']) {
        $target['collectionUrl'] = ([string] $target['collectionUrl']).TrimEnd('/')
    }

    if ($target['platform'] -ceq 'gitHub' -and -not $target['repository']) {
        throw 'A GitHub tracker requires a repository. Name it in the URL as github.com/{owner}/{repository}, or supply -Repository.'
    }
    if ($target['platform'] -ceq 'azureDevOps' -and -not $target['project']) {
        throw 'An Azure DevOps tracker requires a project. Name it in the URL as dev.azure.com/{organization}/{project}, or supply -Project.'
    }

    $target['webUrl'] = Get-TrackerWebUrl -Target $target
    return $target
}

# Reads a GitHub tracker out of a URL.
#
# The owner and repository are the first two segments of every github.com URL that names anything
# inside a repository, so the issue list, the repository root and a pull request URL all resolve to
# the same tracker. A URL naming only the owner serves when the repository is named on the command
# line instead; anything shallower names the service itself and is refused.
function Resolve-GitHubTrackerFromUrl {
    param(
        [Parameter(Mandatory)]
        [uri] $Url,

        [string] $Repository
    )

    $segments = @($Url.AbsolutePath.Split('/', [StringSplitOptions]::RemoveEmptyEntries))
    $named = $segments.Count -ge 2
    if (-not $named -and ($segments.Count -lt 1 -or [string]::IsNullOrWhiteSpace($Repository))) {
        throw "'$Url' names no GitHub repository. A tracker URL names one as github.com/{owner}/{repository}, or name the repository with -Repository."
    }

    $repositoryName = if ($named) { [uri]::UnescapeDataString($segments[1]) } else { $Repository }

    [ordered]@{
        platform      = 'gitHub'
        collectionUrl = $null
        organization  = [uri]::UnescapeDataString($segments[0])
        project       = $null
        repository    = $repositoryName
    }
}

# Reads an Azure DevOps tracker out of a URL.
#
# The project is anchored on the first service area segment, which is what makes a board URL, a work
# item URL and a pull request URL resolve to the same project. A URL that stops at the project
# carries no such segment, so its last segment is the project. Everything ahead of the project is the
# collection, which is the organization on dev.azure.com and an optional collection name on the older
# per-organization hosts.
#
# A URL naming a service area where the project would be carries no project of its own: Azure DevOps
# writes one when the repository and the project share a name, and reading the repository out of it
# would resolve every other such URL to something that is not a project at all. Such a URL still
# serves when the project is named on the command line instead.
function Resolve-AdoTrackerFromUrl {
    param(
        [Parameter(Mandatory)]
        [uri] $Url,

        [string] $Project
    )

    $segments = @($Url.AbsolutePath.Split('/', [StringSplitOptions]::RemoveEmptyEntries))
    $isDevAzureHost = $Url.Host -ieq 'dev.azure.com' -or $Url.Host -ieq 'www.dev.azure.com'

    $boundary = $segments.Count
    for ($index = 0; $index -lt $segments.Count; $index++) {
        if ($segments[$index].StartsWith($script:TrackerAdoServiceAreaPrefix)) {
            $boundary = $index
            break
        }
    }

    # On dev.azure.com the organization is the first segment, so a URL carrying nothing ahead of the
    # service area names no organization and no explicit project can rescue it.
    $minimumProjectIndex = if ($isDevAzureHost) { 1 } else { 0 }
    if ($isDevAzureHost -and $boundary -lt 1) {
        throw "'$Url' names no Azure DevOps organization. A tracker URL names one as dev.azure.com/{organization}/{project}."
    }

    $namesProject = $boundary -gt $minimumProjectIndex
    if (-not $namesProject -and [string]::IsNullOrWhiteSpace($Project)) {
        $expected = if ($isDevAzureHost) { 'dev.azure.com/{organization}/{project}' } else { '{organization}.visualstudio.com/{project}' }
        throw "'$Url' names no Azure DevOps project. A tracker URL names one as $expected, or name the project with -Project."
    }

    $organization = if ($isDevAzureHost) {
        [uri]::UnescapeDataString($segments[0])
    } else {
        $Url.Host.Split('.')[0]
    }

    # Everything ahead of the project is the collection, and when the URL names no project that is
    # everything ahead of the service area instead.
    $collectionDepth = if ($namesProject) { $boundary - 1 } else { $boundary }
    $collectionSegments = @()
    if ($collectionDepth -ge 1) {
        $collectionSegments = @($segments[0..($collectionDepth - 1)])
    }

    $collectionUrl = "$($Url.Scheme)://$($Url.Authority)"
    if ($collectionSegments.Count -gt 0) {
        $collectionUrl = "$collectionUrl/$($collectionSegments -join '/')"
    }

    [ordered]@{
        platform      = 'azureDevOps'
        collectionUrl = $collectionUrl
        organization  = $organization
        project       = if ($namesProject) { [uri]::UnescapeDataString($segments[$boundary - 1]) } else { $Project }
        repository    = $null
    }
}

# Builds the browser URL of the tracker itself, which the run records so a reader can open what it
# filed into.
function Get-TrackerWebUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target
    )

    if ($Target['platform'] -ceq 'gitHub') {
        $owner = [uri]::EscapeDataString([string] $Target['organization'])
        $repository = [uri]::EscapeDataString([string] $Target['repository'])
        return "https://github.com/$owner/$repository/issues"
    }

    $collectionUrl = ([string] $Target['collectionUrl']).TrimEnd('/')
    $project = [uri]::EscapeDataString([string] $Target['project'])
    "$collectionUrl/$project/_workitems"
}

# Builds the browser URL of one created item.
#
# Azure DevOps answers a create request with API links rather than a browser one, so the browser URL
# has to be composed for it or a reader would be handed an API address. GitHub answers with the
# browser URL itself and is recorded from the answer, which is the better source where it exists:
# a URL the service stated is a URL the service stands behind.
function Get-TrackerItemWebUrl {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $ItemId
    )

    if ($Target['platform'] -ceq 'gitHub') {
        $owner = [uri]::EscapeDataString([string] $Target['organization'])
        $repository = [uri]::EscapeDataString([string] $Target['repository'])
        return "https://github.com/$owner/$repository/issues/$ItemId"
    }

    $collectionUrl = ([string] $Target['collectionUrl']).TrimEnd('/')
    $project = [uri]::EscapeDataString([string] $Target['project'])
    "$collectionUrl/$project/_workitems/edit/$ItemId"
}

# Reports whether two resolved trackers name the same place.
#
# Organization, project and repository names are compared case-insensitively, because both services
# treat them that way and a record spelled differently from the command line still describes the run
# it came from.
function Test-TrackerTargetsMatch {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Left,

        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Right
    )

    if ([string] $Left['platform'] -cne [string] $Right['platform']) { return $false }

    foreach ($name in @('organization', 'project', 'repository', 'collectionUrl')) {
        $leftValue = [string] (Get-JsonProperty -Node $Left -Name $name)
        $rightValue = [string] (Get-JsonProperty -Node $Right -Name $name)
        if (-not [string]::Equals($leftValue, $rightValue, [StringComparison]::OrdinalIgnoreCase)) { return $false }
    }

    $true
}

# Refuses the arguments that do not belong to the tracker's platform, before anything is read.
#
# An Azure DevOps argument passed against a GitHub tracker is refused rather than ignored: it states
# an intention the run cannot carry out, and silently dropping it would file a whole review without
# the categorisation the user asked for. The Azure DevOps requirements are checked here for the same
# reason - a work item cannot be created without a type, and an area path left to the service's
# default files the review wherever the project happens to point.
function Assert-TrackerArgument {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [string] $WorkItemType,

        [string] $AreaPath,

        [string] $DescriptionField,

        [int] $ParentWorkItemId
    )

    if ($Target['platform'] -ceq 'gitHub') {
        foreach ($pair in @(
                @{ name = '-WorkItemType'; value = $WorkItemType }
                @{ name = '-AreaPath'; value = $AreaPath }
                @{ name = '-DescriptionField'; value = $DescriptionField }
            )) {
            if (-not [string]::IsNullOrWhiteSpace([string] $pair['value'])) {
                throw "$($pair['name']) was supplied for a GitHub tracker, which files issues and has no such field. Remove it, or name an Azure DevOps project as the tracker."
            }
        }

        if ($ParentWorkItemId -gt 0) {
            throw '-ParentWorkItemId was supplied for a GitHub tracker, which files issues and links no parent. Remove it, or name an Azure DevOps project as the tracker.'
        }

        return
    }

    if ([string]::IsNullOrWhiteSpace($WorkItemType)) {
        throw 'An Azure DevOps tracker requires -WorkItemType, because a work item is created as a type of its own and the service assumes none.'
    }

    if ([string]::IsNullOrWhiteSpace($AreaPath)) {
        throw "An Azure DevOps tracker requires -AreaPath, because a work item left to the project's default area is filed wherever that happens to point."
    }

    Assert-TrackerAreaPath -Target $Target -AreaPath $AreaPath
}

# Refuses an area path that does not sit under the project the tracker names.
#
# Azure DevOps roots every area path at the project name, and accepts a path that names another
# project's root only to reject it later or, worse, to resolve it against this project and file the
# review somewhere nobody expects. Stating the rule here refuses it while the argument is still on
# the command line.
function Assert-TrackerAreaPath {
    param(
        [Parameter(Mandatory)]
        [System.Collections.IDictionary] $Target,

        [Parameter(Mandatory)]
        [string] $AreaPath
    )

    $project = [string] $Target['project']
    $root = $AreaPath.Split($script:TrackerAreaPathSeparator, [StringSplitOptions]::RemoveEmptyEntries) | Select-Object -First 1
    if ([string]::IsNullOrWhiteSpace([string] $root)) {
        throw "'$AreaPath' is not an area path. An area path is rooted at the project name, as '$project' or '$project$($script:TrackerAreaPathSeparator)Area'."
    }

    if (-not [string]::Equals([string] $root, $project, [StringComparison]::OrdinalIgnoreCase)) {
        throw "The area path '$AreaPath' is rooted at '$root', but the tracker names project '$project'. An Azure DevOps area path is rooted at the project it belongs to."
    }
}
