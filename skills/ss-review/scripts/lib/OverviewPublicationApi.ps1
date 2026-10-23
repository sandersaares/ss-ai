#Requires -Version 7.6

# Platform-specific operations for the initial overview, separate from finding review submissions.
# Ref: docs/implementation.md, "Initial overview publication".
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-GitHubOverviewRevision {
    param(
        [Parameter(Mandatory)][string] $GistId,
        [Parameter(Mandatory)][string] $Revision,
        [Parameter(Mandatory)][scriptblock] $InvokeProcess
    )
    if ($GistId -notmatch '^[0-9a-f]+$' -or $Revision -notmatch '^[0-9a-f]{40,64}$') {
        throw 'GitHub returned an invalid Gist or revision identifier.'
    }
    $scratch = Join-Path ([IO.Path]::GetTempPath()) "ss-review-gist-$([guid]::NewGuid().ToString('N'))"
    try {
        # Git reads complete revision content for every truncated API response, including files
        # beyond raw_url's documented range. No credential is sent to a returned arbitrary URL.
        Invoke-ReportProcess -InvokeProcess $InvokeProcess -FilePath 'git' -ArgumentList @(
            '-c', 'credential.helper=', 'clone', '--bare', '--quiet', "https://gist.github.com/$GistId.git", $scratch
        ) | Out-Null
        Invoke-ReportProcess -InvokeProcess $InvokeProcess -FilePath 'git' `
            -ArgumentList @('--git-dir', $scratch, 'show', "${Revision}:overview.md")
    } finally {
        if ([IO.Directory]::Exists($scratch)) { Remove-Item -LiteralPath $scratch -Recurse -Force }
    }
}

function Invoke-GitHubOverviewOperation {
    param(
        [string] $Operation,
        [System.Collections.IDictionary] $Transport,
        [System.Collections.IDictionary] $Target,
        [AllowNull()][System.Collections.IDictionary] $Snapshot,
        [AllowNull()][System.Collections.IDictionary] $Payload,
        [Parameter(Mandatory)][scriptblock] $ReadRevision
    )
    $repo = "repos/$([uri]::EscapeDataString($Target['organization']))/$([uri]::EscapeDataString($Target['repository']))"
    switch ($Operation) {
        'isOpen' {
            $pr = Invoke-GitHubRestRequest -Transport $Transport -Path "$repo/pulls/$($Target['pullRequestId'])"
            return (Get-JsonProperty -Node $pr -Name 'state') -eq 'open'
        }
        'upload' {
            $text = (Read-PullRequestFileText -Path ($Snapshot['markdownPath']) -Encoding ([Text.Encoding]::UTF8))
            $gist = Invoke-GitHubRestRequest -Transport $Transport -Path 'gists' -Method POST -NoRetry -Body @{
                public = $false
                description = "ss-review initial overview $($Snapshot['runId'])"
                files = @{ 'overview.md' = @{ content = $text } }
            }
            return [ordered]@{ id = [string] $gist['id']; url = [string] $gist['html_url'] }
        }
        'verifyArtifact' {
            $gist = Invoke-GitHubRestRequest -Transport $Transport -Path "gists/$($Payload['id'])"
            if ($gist['public'] -ne $false) { throw 'The report Gist is not unlisted.' }
            $file = $gist['files']['overview.md']
            $revision = [string] $gist['history'][0]['version']
            $text = if ($file['truncated']) { & $ReadRevision $Payload['id'] $revision } else { [string] $file['content'] }
            if ((Get-ReviewReportHash -Bytes ([Text.Encoding]::UTF8.GetBytes($text))) -cne $Snapshot['markdownHash']) {
                throw 'GitHub Gist read-back does not match the frozen overview.'
            }
            $url = [uri] $gist['html_url']
            if ($url.Scheme -cne 'https' -or $url.Host -cne 'gist.github.com') { throw 'GitHub returned an unexpected Gist URL.' }
            return [ordered]@{ id = $Payload['id']; revision = $revision; url = "$($url.AbsoluteUri.TrimEnd('/'))/$revision" }
        }
        'comment' {
            $comment = Invoke-GitHubRestRequest -Transport $Transport -Path "$repo/issues/$($Target['pullRequestId'])/comments" `
                -Method POST -NoRetry -Body @{ body = $Payload['body'] }
            return [ordered]@{ id = $comment['id']; url = $comment['html_url'] }
        }
        'verifyComment' {
            $comment = Invoke-GitHubRestRequest -Transport $Transport -Path "$repo/issues/comments/$($Payload['comment']['id'])"
            if ($comment['body'] -cne $Payload['body']) { throw 'GitHub did not retain the initial overview comment body.' }
            return
        }
        default { throw "Unknown overview operation '$Operation'." }
    }
}

function Invoke-AdoOverviewOperation {
    param(
        [string] $Operation,
        [System.Collections.IDictionary] $Transport,
        [System.Collections.IDictionary] $Target,
        [AllowNull()][System.Collections.IDictionary] $Snapshot,
        [AllowNull()][System.Collections.IDictionary] $Payload
    )
    $path = Get-AdoPullRequestResourcePath -Target $Target
    switch ($Operation) {
        'isOpen' {
            $pr = Invoke-AdoRestRequest -Transport $Transport -Path $path
            return (Get-JsonProperty -Node $pr -Name 'status') -eq 'active'
        }
        'upload' {
            $name = "ss-review-$($Snapshot['runId'])-$($Snapshot['archiveHash']).zip"
            $attachment = Invoke-AdoRestRequest -Transport $Transport -Path "$path/attachments/$name" `
                -Method POST -ContentType 'application/octet-stream' -Body ((Read-PullRequestFileBytes -Path ($Snapshot['archivePath']))) -NoRetry
            # Link the same tenant-scoped resource that read-back verifies, including its API
            # version, rather than depending on the service's choice of legacy-host aliases.
            $url = Get-AdoRestRequestUrl -CollectionUrl $Transport['collectionUrl'] -Path "$path/attachments/$name" -ApiVersion '7.1'
            return [ordered]@{ id = $attachment['id']; url = $url; fileName = $name }
        }
        'verifyArtifact' {
            $bytes = Invoke-AdoRestRequest -Transport $Transport -Path "$path/attachments/$($Payload['fileName'])" -AsBytes
            if ((Get-ReviewReportHash -Bytes $bytes) -cne $Snapshot['archiveHash']) {
                throw 'ADO attachment read-back does not match the frozen HTML archive.'
            }
            $url = [uri] $Payload['url']
            $collection = [uri] $Transport['collectionUrl']
            if ($url.Scheme -cne 'https' -or $url.Authority -cne $collection.Authority) {
                throw 'ADO returned an unexpected attachment URL.'
            }
            return $Payload
        }
        'comment' {
            $thread = Invoke-AdoRestRequest -Transport $Transport -Path "$path/threads" -Method POST -NoRetry -Body @{
                status = 'fixed'
                comments = @(@{ parentCommentId = 0; commentType = 'text'; content = $Payload['body'] })
            }
            return [ordered]@{ id = $thread['id']; url = Get-AdoPullRequestThreadUrl -Target $Target -ThreadId $thread['id'] }
        }
        'verifyComment' {
            $thread = Invoke-AdoRestRequest -Transport $Transport -Path "$path/threads/$($Payload['comment']['id'])"
            if ($thread['comments'][0]['content'] -cne $Payload['body']) { throw 'ADO did not retain the overview comment body.' }
            if ($thread['status'] -ne 'fixed' -and $thread['status'] -ne 2) {
                throw 'The ADO overview comment exists, but its thread is not resolved. Do not repost it.'
            }
            return
        }
        default { throw "Unknown overview operation '$Operation'." }
    }
}
