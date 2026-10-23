BeforeAll {
    $lib = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review\scripts\lib'
    . (Join-Path $lib 'JsonValue.ps1')
    . (Join-Path $lib 'PublicationFailure.ps1')
    . (Join-Path $lib 'PullRequestTarget.ps1')
    . (Join-Path $lib 'PullRequestModel.ps1')
    . (Join-Path $lib 'PullRequestTransportHost.ps1')
    . (Join-Path $lib 'GitHubCliTransport.ps1')
    . (Join-Path $lib 'AdoRestTransport.ps1')
    . (Join-Path $lib 'AdoPullRequestApi.ps1')
    . (Join-Path $lib 'ReviewHtml.ps1')
    . (Join-Path $lib 'OverviewPublication.ps1')
    . (Join-Path $lib 'OverviewPublicationApi.ps1')
}

Describe 'GitHub overview adapter' {
    BeforeEach {
        $script:Text = "# Overview`n`nText $([char]0x2014) $([char]0x00e9)."
        $script:Path = Join-Path $TestDrive 'overview.md'
        Write-PullRequestOutputFile -Path $script:Path -Content $script:Text
        $script:Snapshot = @{
            markdownPath = $script:Path
            markdownHash = Get-ReviewReportHash -Bytes ([IO.File]::ReadAllBytes($script:Path))
            runId = 'run'
        }
        $script:Target = @{ organization = 'owner'; repository = 'repo'; pullRequestId = 7 }
        $script:Revision = 'a' * 40
        $script:Gist = @{
            public = $false
            html_url = 'https://gist.github.com/owner/abc123'
            files = @{ 'overview.md' = @{ content = $script:Text; truncated = $false } }
            history = @(@{ version = $script:Revision })
        }
        $script:ReadRevision = { throw 'Full Git retrieval was not expected' }
    }

    It 'creates an unlisted Gist without retrying a create operation' {
        Mock Invoke-GitHubRestRequest { @{ id = 'abc123'; html_url = 'https://gist.github.com/owner/abc123' } }
        $r = Invoke-GitHubOverviewOperation -Operation upload -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -ReadRevision $script:ReadRevision
        $r.id | Should -Be abc123
        Should -Invoke Invoke-GitHubRestRequest -Exactly 1 -ParameterFilter {
            $Path -eq 'gists' -and $Method -eq 'POST' -and $NoRetry -and $Body.public -eq $false -and
            $Body.files['overview.md'].content -ceq $script:Text
        }
    }

    It 'verifies complete content and links to the rendered revision' {
        Mock Invoke-GitHubRestRequest { $script:Gist }
        $r = Invoke-GitHubOverviewOperation -Operation verifyArtifact -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ id = 'abc123' } -ReadRevision $script:ReadRevision
        $r.url | Should -Be "https://gist.github.com/owner/abc123/$script:Revision"
    }

    It 'retrieves the complete revision when API content is truncated' {
        $script:Gist.files['overview.md'].truncated = $true
        $script:Gist.files['overview.md'].content = 'partial'
        Mock Invoke-GitHubRestRequest { $script:Gist }
        $script:ReadArgs = $null
        $r = Invoke-GitHubOverviewOperation -Operation verifyArtifact -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ id = 'abc123' } -ReadRevision {
                param($id, $revision)
                $script:ReadArgs = @($id, $revision)
                $script:Text
            }
        $script:ReadArgs[0] | Should -Be abc123
        $script:ReadArgs[1] | Should -Be $script:Revision
        $r.revision | Should -Be $script:Revision
    }

    It 'refuses a content mismatch or unexpectedly public Gist' {
        Mock Invoke-GitHubRestRequest { $script:Gist }
        $script:Gist.files['overview.md'].content = 'wrong'
        { Invoke-GitHubOverviewOperation -Operation verifyArtifact -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ id = 'abc123' } -ReadRevision $script:ReadRevision
        } | Should -Throw '*does not match*'
        $script:Gist.public = $true
        { Invoke-GitHubOverviewOperation -Operation verifyArtifact -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ id = 'abc123' } -ReadRevision $script:ReadRevision
        } | Should -Throw '*not unlisted*'
    }

    It 'posts to the PR conversation rather than creating a review' {
        Mock Invoke-GitHubRestRequest { @{ id = 10; html_url = 'https://example.invalid/comment' } }
        Invoke-GitHubOverviewOperation -Operation comment -Transport @{} -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ body = '[Copilot speaking] report' } -ReadRevision $script:ReadRevision
        Should -Invoke Invoke-GitHubRestRequest -Exactly 1 -ParameterFilter {
            $Path -eq 'repos/owner/repo/issues/7/comments' -and $Method -eq 'POST' -and $NoRetry
        }
    }
}

Describe 'ADO overview adapter and binary transport' {
    BeforeEach {
        $script:Target = @{
            project = 'Project'
            repository = 'repo'
            pullRequestId = 7
            collectionUrl = 'https://dev.azure.com/owner'
        }
        $script:Html = "<!DOCTYPE html><html>Report $([char]0x00e9)</html>"
        $script:Path = Join-Path $TestDrive 'report.html'
        Write-PullRequestOutputFile -Path $script:Path -Content $script:Html
        $script:Bytes = [IO.File]::ReadAllBytes($script:Path)
        $script:Snapshot = @{ htmlPath = $script:Path; htmlHash = Get-ReviewReportHash -Bytes $script:Bytes; runId = 'run' }
        $archive = Write-OverviewArchive -HtmlPath $script:Path -ArchivePath (Join-Path $TestDrive 'report.zip')
        $script:Snapshot.archivePath = $archive.archivePath
        $script:Snapshot.archiveHash = $archive.archiveHash
        $script:Attachment = @{ id = 3; fileName = 'report.zip'; url = 'https://dev.azure.com/owner/attachment' }
        $script:Transport = @{ collectionUrl = 'https://dev.azure.com/owner' }
    }

    It 'uploads the exact ZIP container with the attachment media type' {
        Mock Invoke-AdoRestRequest { $script:Attachment }
        Invoke-AdoOverviewOperation -Operation upload -Transport $script:Transport -Target $script:Target -Snapshot $script:Snapshot
        Should -Invoke Invoke-AdoRestRequest -Exactly 1 -ParameterFilter {
            $Method -eq 'POST' -and $NoRetry -and $ContentType -eq 'application/octet-stream' -and
            $Body -is [byte[]] -and (Get-ReviewReportHash -Bytes $Body) -ceq $script:Snapshot.archiveHash -and $Path -match '/attachments/.*\.zip$'
        }
    }

    It 'verifies attachment bytes instead of attempting to parse its HTML as JSON' {
        $script:Bytes = [IO.File]::ReadAllBytes($script:Snapshot.archivePath)
        Mock Invoke-AdoRestRequest { , $script:Bytes }
        $r = Invoke-AdoOverviewOperation -Operation verifyArtifact -Transport $script:Transport -Target $script:Target `
            -Snapshot $script:Snapshot -Payload $script:Attachment
        $r.id | Should -Be 3
        Should -Invoke Invoke-AdoRestRequest -Exactly 1 -ParameterFilter { $AsBytes }
        $script:Bytes = [Text.Encoding]::UTF8.GetBytes('<html>Sign in</html>')
        { Invoke-AdoOverviewOperation -Operation verifyArtifact -Transport $script:Transport -Target $script:Target `
            -Snapshot $script:Snapshot -Payload $script:Attachment } | Should -Throw '*does not match*'
    }

    It 'creates a resolved top-level thread with no anchor or vote' {
        Mock Invoke-AdoRestRequest { @{ id = 5 } }
        Invoke-AdoOverviewOperation -Operation comment -Transport $script:Transport -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ body = '[Copilot speaking] overview' }
        Should -Invoke Invoke-AdoRestRequest -Exactly 1 -ParameterFilter {
            $Method -eq 'POST' -and $NoRetry -and $Body.status -eq 'fixed' -and
            $Body.comments[0].parentCommentId -eq 0 -and $Body.comments[0].commentType -eq 'text' -and
            -not $Body.ContainsKey('threadContext') -and $Path.EndsWith('/threads')
        }
    }

    It 'reports an unresolved thread instead of reposting it' {
        Mock Invoke-AdoRestRequest { @{ comments = @(@{ content = 'body' }); status = 'active' } }
        { Invoke-AdoOverviewOperation -Operation verifyComment -Transport $script:Transport -Target $script:Target `
            -Snapshot $script:Snapshot -Payload @{ body = 'body'; comment = @{ id = 5 } }
        } | Should -Throw '*not resolved*'
        Should -Invoke Invoke-AdoRestRequest -Exactly 1 -ParameterFilter { $Method -ne 'POST' }
    }

    It 'uses ByteArrayContent without adding a text charset' {
        $message = New-HttpRequestMessage -Method POST -Url 'https://example.invalid/upload' `
            -Headers @{ 'Content-Type' = 'application/octet-stream' } -Body ([byte[]]@(0, 128, 255))
        try {
            $message.Content.Headers.ContentType.CharSet | Should -BeNullOrEmpty
            ($message.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult() -join ',') | Should -Be '0,128,255'
        } finally { $message.Dispose() }
    }

    It 'preserves binary requests and permits HTML only in the explicit bytes response mode' {
        $script:SeenBody = $null
        $transport = New-AdoRestTransport -CollectionUrl 'https://example.invalid/org' -GetAccessToken { 'fake-token' } -SendRequest {
            param($Method, $Url, $Headers, $Body)
            $script:SeenBody = $Body
            @{ statusCode = 200; headers = @{}; contentType = 'text/html'; contentBytes = $script:Bytes }
        }
        $bytes = Invoke-AdoRestRequest -Transport $transport -Path 'attachment' -Method POST -Body $script:Bytes -AsBytes
        ($script:SeenBody -is [byte[]]) | Should -BeTrue
        (Get-ReviewReportHash -Bytes $bytes) | Should -Be $script:Snapshot.htmlHash
        { Invoke-AdoRestRequest -Transport $transport -Path 'json' } | Should -Throw '*HTML document rather than JSON*'
    }

    It 'rejects an ADO sign-in interception status on a binary read' {
        $transport = New-AdoRestTransport -CollectionUrl 'https://example.invalid/org' -GetAccessToken { 'fake-token' } -SendRequest {
            @{ statusCode = 203; headers = @{}; contentType = 'text/html'; contentBytes = $script:Bytes }
        }
        { Invoke-AdoRestRequest -Transport $transport -Path 'attachment' -AsBytes } | Should -Throw '*HTTP 203*'
    }
}
