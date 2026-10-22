BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:LibraryRoot = Join-Path $script:RepoRoot 'agents\.github\skills\ss-review\scripts\lib'

    . (Join-Path $script:LibraryRoot 'JsonValue.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestTarget.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestIdentity.ps1')
    . (Join-Path $script:LibraryRoot 'PullRequestModel.ps1')

    # Built from code points rather than written literally, so that a test file whose own encoding was
    # damaged fails instead of comparing two identically damaged strings.
    $script:Robot = [char]::ConvertFromUtf32(0x1F916)
    $script:EmDash = [string][char]0x2014
    $script:LeftQuote = [string][char]0x201C
    $script:RightQuote = [string][char]0x201D

    function New-TestIdentity {
        param(
            [string] $Id = '1',
            [string] $DisplayName = 'Test Person',
            [string] $UniqueName = 'person@example.com',
            [string] $Descriptor = 'aad.PERSON',
            [string] $Platform = 'azureDevOps'
        )

        New-PullRequestIdentity -Platform $Platform -Id $Id -DisplayName $DisplayName -UniqueName $UniqueName -Descriptor $Descriptor
    }

    function New-TestComment {
        param(
            [Parameter(Mandatory)] $Id,
            [Parameter(Mandatory)] [System.Collections.IDictionary] $Author,
            [AllowEmptyString()] [string] $Content = 'body',
            [string] $PublishedUtc = '2025-11-05T10:00:00Z',
            [AllowNull()] [AllowEmptyString()] [string] $LastEditedUtc = $null
        )

        $authorship = Get-PullRequestAuthorship -Identity $Author -Content $Content
        [ordered]@{
            id                 = $Id
            url                = "https://example.invalid/c/$Id"
            parentId           = $null
            author             = $Author
            content            = $Content
            publishedUtc       = (ConvertTo-CanonicalTimestamp -Value $PublishedUtc)
            lastEditedUtc      = (ConvertTo-CanonicalTimestamp -Value $LastEditedUtc)
            isEdited           = -not [string]::IsNullOrWhiteSpace($LastEditedUtc)
            authorship         = $authorship['authorship']
            authorshipEvidence = $authorship['evidence']
        }
    }

    function New-TestThread {
        param(
            [Parameter(Mandatory)] $Id,
            [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Comments,
            [string] $EventUtc = '2025-11-05T10:00:00Z',
            [string] $Resolution = 'active',
            [string] $Kind = 'text',
            $Context = $null,
            $Decision = $null,
            $OpeningCommentId = $null,
            [switch] $Outdated
        )

        # An adapter names the thread's opener structurally, so a fixture that states no opener
        # falls back to the first comment it was handed rather than leaving the field unset.
        if ($null -eq $OpeningCommentId -and $Comments.Count -gt 0) { $OpeningCommentId = $Comments[0]['id'] }

        [ordered]@{
            id               = $Id
            url              = "https://example.invalid/t/$Id"
            publishedUtc     = (ConvertTo-CanonicalTimestamp -Value $EventUtc)
            lastUpdatedUtc   = $null
            eventUtc         = (ConvertTo-CanonicalTimestamp -Value $EventUtc)
            rawStatus        = $Resolution
            resolution       = $Resolution
            kind             = $Kind
            isOutdated       = [bool] $Outdated
            context          = $Context
            anchorKey        = $null
            properties       = [ordered]@{}
            openingCommentId = $OpeningCommentId
            decision         = $Decision
            comments         = $Comments
        }
    }

    function New-TestContext {
        param(
            [Parameter(Mandatory)] [AllowEmptyCollection()] [object[]] $Threads,
            [System.Collections.IDictionary] $AuthenticatedIdentity,
            [AllowEmptyCollection()] [object[]] $Reviewers = @(),
            [System.Collections.IDictionary] $PullRequestAuthor
        )

        [ordered]@{
            target                = (Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/42')
            authenticatedIdentity = $AuthenticatedIdentity
            pullRequest           = [ordered]@{
                id                       = 42
                title                    = 'Test change'
                author                   = $PullRequestAuthor
                reviewedSourceCommit     = 'aaa111'
                reviewedMergeBaseCommit  = 'bbb222'
            }
            reviewers             = $Reviewers
            revisions             = @()
            threads               = $Threads
        }
    }
}

Describe 'JSON value handling' {
    It 'keeps a timestamp-shaped string value as a string' {
        # ConvertFrom-Json rewrites any date-shaped string value into a DateTime and back into
        # locale-shaped text, which silently corrupts a comment whose body is a timestamp.
        $parsed = ConvertFrom-JsonVerbatim -Text '{"content":"2026-08-04T10:00:00+03:00"}'

        $content = Get-JsonProperty -Node $parsed -Name 'content'
        $content | Should -BeOfType ([string])
        $content | Should -BeExactly '2026-08-04T10:00:00+03:00'
    }

    It 'reproduces non-ASCII content exactly' {
        $body = "$script:Robot $script:EmDash $script:LeftQuote quoted $script:RightQuote"
        $text = ConvertTo-JsonText -Value ([ordered]@{ content = $body })

        $parsed = ConvertFrom-JsonVerbatim -Text $text
        (Get-JsonProperty -Node $parsed -Name 'content') | Should -BeExactly $body
    }

    It 'reads an absent property as its default under strict mode' {
        $parsed = ConvertFrom-JsonVerbatim -Text '{"present":1}'

        Test-JsonProperty -Node $parsed -Name 'absent' | Should -BeFalse
        Get-JsonProperty -Node $parsed -Name 'absent' -Default 'fallback' | Should -Be 'fallback'
    }

    It 'treats an explicit JSON null as absent' {
        $parsed = ConvertFrom-JsonVerbatim -Text '{"line":null}'

        Test-JsonProperty -Node $parsed -Name 'line' | Should -BeFalse
        Get-JsonProperty -Node $parsed -Name 'line' -Default 7 | Should -Be 7
    }

    It 'yields an array for a list property regardless of length' {
        $parsed = ConvertFrom-JsonVerbatim -Text '{"empty":[],"one":[1],"many":[1,2,3]}'

        (Get-JsonArrayProperty -Node $parsed -Name 'empty').Count | Should -Be 0
        (Get-JsonArrayProperty -Node $parsed -Name 'one').Count | Should -Be 1
        (Get-JsonArrayProperty -Node $parsed -Name 'many').Count | Should -Be 3
        (Get-JsonArrayProperty -Node $parsed -Name 'missing').Count | Should -Be 0
    }

    It 'assigns a property on a dictionary that has been through the pipeline' {
        # An OrderedDictionary exposes both an integer and a key indexer, so a name-keyed write
        # through anything but an IDictionary reference fails as a type conversion.
        $node = @([ordered]@{ id = 1 } | Write-Output)[0]

        { Set-JsonProperty -Node $node -Name 'added' -Value 'value' } | Should -Not -Throw
        Get-JsonProperty -Node $node -Name 'added' | Should -Be 'value'
    }

    It 'reports the offending type when the assignment target is not a dictionary' {
        { Set-JsonProperty -Node @(1, 2) -Name 'x' -Value 1 } | Should -Throw '*System.Object`[`]*'
    }

    It 'serializes past the default depth without truncation' {
        $deep = [ordered]@{ a = [ordered]@{ b = [ordered]@{ c = [ordered]@{ d = 'leaf' } } } }

        ConvertTo-JsonText -Value $deep | Should -Match 'leaf'
    }

    It 'normalizes timestamps to round-trip UTC' {
        ConvertTo-CanonicalTimestamp -Value '2026-08-04T10:00:00+03:00' | Should -Be '2026-08-04T07:00:00.0000000Z'
        ConvertTo-CanonicalTimestamp -Value '' | Should -BeNullOrEmpty
        { ConvertTo-CanonicalTimestamp -Value 'not a time' } | Should -Throw
    }

    It 'reads a timestamp that carries no offset as UTC' {
        # A payload without an offset would otherwise be read as host local time, so the same capture
        # would canonicalize differently on two machines. These values are the sort keys for the
        # whole discussion, so the capture would not be reproducible across hosts.
        $sample = '2025-11-05T10:00:00'
        if ([TimeZoneInfo]::Local.GetUtcOffset([datetime]::Parse($sample, [cultureinfo]::InvariantCulture)) -eq [TimeSpan]::Zero) {
            # A local reading and a UTC reading coincide on a host at zero offset, so the assertion
            # could not fail there. The gap is reported rather than passed off as coverage.
            Set-ItResult -Skipped -Because 'the host reads local time at zero offset, where the two readings agree'
        }

        ConvertTo-CanonicalTimestamp -Value $sample | Should -BeExactly '2025-11-05T10:00:00.0000000Z'
        ConvertTo-CanonicalTimestamp -Value $sample | Should -BeExactly (ConvertTo-CanonicalTimestamp -Value "$($sample)Z")
    }

    It 'converts an explicit offset to UTC and keeps sub-second precision' {
        # The canonical timestamps are the sort keys for the whole discussion, so a truncated
        # fraction would reorder two comments posted within the same second.
        ConvertTo-CanonicalTimestamp -Value '2025-11-05T10:00:00.1234567+02:00' | Should -BeExactly '2025-11-05T08:00:00.1234567Z'
        ConvertTo-CanonicalTimestamp -Value '2025-11-05T10:00:00.0010000Z' | Should -BeExactly '2025-11-05T10:00:00.0010000Z'
    }

    It 'materializes an Int64 boundary value exactly' {
        # Every number the capture reads is an integer within Int64 range, and the parser holds
        # those exactly, including at the extremes where a Double fallback would round.
        $parsed = ConvertFrom-JsonVerbatim -Text '{"max":9223372036854775807,"min":-9223372036854775808}'

        $max = Get-JsonProperty -Node $parsed -Name 'max'
        $max | Should -BeOfType ([long])
        $max | Should -Be ([long]::MaxValue)

        $min = Get-JsonProperty -Node $parsed -Name 'min'
        $min | Should -BeOfType ([long])
        $min | Should -Be ([long]::MinValue)
    }

    It 'materializes a number outside Int64 as a double' {
        # The exactness promise covers strings and integers within Int64 range. Any other number
        # arrives as a Double, so its token is not reproduced digit for digit; this is asserted so
        # that a caller who needs the original digits knows to read them as a string instead.
        $parsed = ConvertFrom-JsonVerbatim -Text '{"precise":0.1234567890123456789,"huge":9223372036854775808}'

        $precise = Get-JsonProperty -Node $parsed -Name 'precise'
        $precise | Should -BeOfType ([double])
        $precise.ToString('R', [cultureinfo]::InvariantCulture) | Should -BeExactly '0.12345678901234568'

        (Get-JsonProperty -Node $parsed -Name 'huge') | Should -BeOfType ([double])
    }
}

Describe 'Pull request target resolution' {
    It 'resolves an Azure DevOps URL on dev.azure.com' {
        $target = Resolve-PullRequestTarget -Url 'https://dev.azure.com/example/Example%20Project/_git/example-repo/pullrequest/42'

        $target['platform'] | Should -Be 'azureDevOps'
        $target['organization'] | Should -Be 'example'
        $target['project'] | Should -Be 'Example Project'
        $target['repository'] | Should -Be 'example-repo'
        $target['pullRequestId'] | Should -Be 42
        $target['collectionUrl'] | Should -Be 'https://dev.azure.com/example'
    }

    It 'resolves the legacy per-organization host' {
        $target = Resolve-PullRequestTarget -Url 'https://contoso.visualstudio.com/Core/_git/widgets/pullrequest/7'

        $target['platform'] | Should -Be 'azureDevOps'
        $target['organization'] | Should -Be 'contoso'
        $target['project'] | Should -Be 'Core'
        $target['collectionUrl'] | Should -Be 'https://contoso.visualstudio.com'
    }

    It 'resolves a named collection on the legacy host' {
        $target = Resolve-PullRequestTarget -Url 'https://contoso.visualstudio.com/DefaultCollection/Core/_git/widgets/pullrequest/7'

        $target['collectionUrl'] | Should -Be 'https://contoso.visualstudio.com/DefaultCollection'
        $target['project'] | Should -Be 'Core'
    }

    It 'ignores a query string' {
        $target = Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7?_a=files&path=/src'

        $target['pullRequestId'] | Should -Be 7
        $target['repository'] | Should -Be 'widgets'
    }

    It 'resolves a GitHub URL' {
        $target = Resolve-PullRequestTarget -Url 'https://github.com/example/widgets/pull/42'

        $target['platform'] | Should -Be 'gitHub'
        $target['organization'] | Should -Be 'example'
        $target['repository'] | Should -Be 'widgets'
        $target['pullRequestId'] | Should -Be 42
        $target['webUrl'] | Should -Be 'https://github.com/example/widgets/pull/42'
    }

    It 'resolves explicit coordinates without a URL' {
        $target = Resolve-PullRequestTarget -Platform azureDevOps -Organization contoso -Project Core -Repository widgets -PullRequestId 7

        $target['collectionUrl'] | Should -Be 'https://dev.azure.com/contoso'
        $target['webUrl'] | Should -Be 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7'
    }

    It 'lets explicit coordinates correct the URL' {
        $target = Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7' -Repository 'gadgets'

        $target['repository'] | Should -Be 'gadgets'
    }

    It 'lets an explicitly supplied identifier correct the URL on either platform' {
        # Reviewer registration writes to whichever pull request the resolved target names, so an
        # identifier the caller supplied must reach both the stored coordinate and the derived link.
        $azureDevOps = Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/7' -PullRequestId 8

        $azureDevOps['pullRequestId'] | Should -Be 8
        $azureDevOps['webUrl'] | Should -BeExactly 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/8'

        $gitHub = Resolve-PullRequestTarget -Url 'https://github.com/octo/widget/pull/7' -PullRequestId 8

        $gitHub['pullRequestId'] | Should -Be 8
        $gitHub['webUrl'] | Should -BeExactly 'https://github.com/octo/widget/pull/8'
    }

    It 'rejects a supplied identifier below one' {
        # An unsupplied [int] arrives as zero, so only a bound value is checked; a URL-only call
        # keeps the identifier the URL carried.
        { Resolve-PullRequestTarget -Url 'https://github.com/octo/widget/pull/7' -PullRequestId 0 } | Should -Throw
        { Resolve-PullRequestTarget -Url 'https://github.com/octo/widget/pull/7' -PullRequestId (-3) } | Should -Throw
        (Resolve-PullRequestTarget -Url 'https://github.com/octo/widget/pull/7')['pullRequestId'] | Should -Be 7
    }

    It 'normalizes a trailing separator on an Azure DevOps collection URL' {
        # Consumers append their own leading separator to the collection URL, so the two spellings
        # must resolve to one canonical target or the same pull request captures differently
        # depending on how the caller spelled the collection.
        $trailing = Resolve-PullRequestTarget -Platform azureDevOps -Organization contoso -Project Core -Repository widgets -PullRequestId 7 -CollectionUrl 'https://dev.azure.com/contoso/'
        $plain = Resolve-PullRequestTarget -Platform azureDevOps -Organization contoso -Project Core -Repository widgets -PullRequestId 7 -CollectionUrl 'https://dev.azure.com/contoso'

        $trailing['collectionUrl'] | Should -BeExactly 'https://dev.azure.com/contoso'
        ConvertTo-JsonText -Value $trailing | Should -BeExactly (ConvertTo-JsonText -Value $plain)
    }

    It 'rejects input it cannot resolve' {
        { Resolve-PullRequestTarget -Url 'not a url' } | Should -Throw
        { Resolve-PullRequestTarget -Url 'https://example.invalid/a/b/pullrequest/1' } | Should -Throw
        { Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Core/_git/widgets/pullrequest/abc' } | Should -Throw
        { Resolve-PullRequestTarget -Url 'https://github.com/example/widgets/issues/1' } | Should -Throw
        { Resolve-PullRequestTarget -Platform gitHub -Organization example } | Should -Throw
    }

    It 'forwards only the coordinates a caller supplied' {
        # An unsupplied parameter arrives as an empty string, which a validated set rejects outright
        # and which would otherwise overwrite a coordinate parsed from the URL.
        $bound = [System.Collections.Generic.Dictionary[string, object]]::new()
        $bound['Url'] = 'https://github.com/example/widgets/pull/1'
        $bound['Verbose'] = $true

        $selected = Get-PullRequestTargetParameter -BoundParameter $bound

        @($selected.Keys) | Should -Be @('Url')
        { Resolve-PullRequestTarget @selected } | Should -Not -Throw
    }
}

Describe 'Identity classification' {
    It 'classifies Azure DevOps service descriptors as services' {
        foreach ($descriptor in @('aadsp.ABC', 's2s.DEF', 'svc.GHI')) {
            (New-PullRequestIdentity -Platform azureDevOps -Id '1' -DisplayName 'Automation' -Descriptor $descriptor)['kind'] |
                Should -Be 'service' -Because $descriptor
        }
    }

    It 'classifies Azure DevOps user descriptors as persons' {
        foreach ($descriptor in @('aad.ABC', 'msa.DEF')) {
            (New-PullRequestIdentity -Platform azureDevOps -Id '1' -DisplayName 'Person' -Descriptor $descriptor)['kind'] |
                Should -BeExactly 'person' -Because $descriptor
        }
    }

    # Identity kind and comment authorship are different vocabularies that answer different
    # questions: 'person' says whose account posted, 'human' says who composed the text. Sharing a
    # token between them would make one readable as the other.
    It 'keeps the identity kind vocabulary clear of the authorship vocabulary' {
        $identity = New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'Person' -AccountType 'User'

        $identity['kind'] | Should -BeExactly 'person'
        $identity['kind'] | Should -Not -BeExactly 'human'
    }

    It 'classifies GitHub accounts by type, node identifier and app URL' {
        (New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'App' -AccountType 'Bot')['kind'] | Should -Be 'service'
        (New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'App' -Descriptor 'BOT_example')['kind'] | Should -Be 'service'
        (New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'App' -Url 'https://github.com/apps/copilot-pull-request-reviewer')['kind'] | Should -Be 'service'
        (New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'Person' -AccountType 'User')['kind'] | Should -Be 'person'
    }

    It 'does not classify by the bot login suffix' {
        # The same GitHub account is reported with a bot-suffixed login by one endpoint and with a
        # plain display name by another, so the suffix answers differently for a single identity.
        $identity = New-PullRequestIdentity -Platform gitHub -Id '1' -DisplayName 'copilot-pull-request-reviewer[bot]' -UniqueName 'copilot-pull-request-reviewer[bot]'

        $identity['kind'] | Should -Be 'indeterminate'
        $identity['evidence'] | Should -BeNullOrEmpty
    }

    It 'reports unrecognized evidence as indeterminate' {
        (New-PullRequestIdentity -Platform azureDevOps -Id '1' -DisplayName 'Mystery')['kind'] | Should -Be 'indeterminate'
    }

    It 'retains the evidence behind a classification' {
        (New-PullRequestIdentity -Platform azureDevOps -Id '1' -DisplayName 'Automation' -Descriptor 'aadsp.ABC')['evidence'] |
            Should -Contain 'descriptor:aadsp'
    }

    It 'leaves a comment posted from a personal account indeterminate' {
        # A personal account proves who a comment was posted as, not who composed it: the same
        # account posts on behalf of an agent. Only Set-CommentAuthorship.ps1 may settle this as
        # human.
        $authorship = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content ''

        $authorship['authorship'] | Should -BeExactly 'indeterminate'
    }

    It 'never attributes a comment to a human without a review' {
        foreach ($descriptor in @('aad.ABC', 'msa.DEF', 'aadsp.GHI', 'svc.JKL')) {
            $identity = New-PullRequestIdentity -Platform azureDevOps -Id '1' -DisplayName 'Person' -Descriptor $descriptor
            (Get-PullRequestAuthorship -Identity $identity -Content 'text')['authorship'] |
                Should -Not -BeExactly 'human' -Because $descriptor
        }
    }

    It 'treats the agent marker as overriding a personal account' {
        $authorship = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content '[Copilot speaking] Addressed.'

        $authorship['authorship'] | Should -Be 'agent'
        $authorship['evidence'] | Should -Contain 'content:agentMarker'
    }

    It 'requires the agent marker to open the comment rather than merely appear in it' {
        # An agent begins what it writes with the marker, so the marker elsewhere in a body is a
        # person quoting or discussing an agent. Because only an indeterminate comment reaches the
        # classification pass, a mechanical agent verdict here would never be reconsidered
        # (docs/design.md, "Authorship classification").
        $quoting = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content "I disagree:`n`n> [Copilot speaking] Addressed."

        $quoting['authorship'] | Should -BeExactly 'indeterminate'
        $quoting['evidence'] | Should -Not -Contain 'content:agentMarker'
    }

    It 'reads the agent marker through the whitespace a comment may open with' {
        $padded = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content "`r`n  [Copilot speaking] Addressed."

        $padded['authorship'] | Should -BeExactly 'agent'
        $padded['evidence'] | Should -Contain 'content:agentMarker'
    }

    It 'holds the agent marker to the exact case agents write' {
        # The marker is fixed text that agents emit verbatim, so differing case is a person writing
        # about an agent rather than an agent writing. The comparison is ordinal for that reason, and
        # the mechanical verdict it produces is never revisited, so a relaxation to a
        # case-insensitive or culture-sensitive comparison has to fail here.
        $lowerCase = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content '[copilot speaking] addressed.'

        $lowerCase['authorship'] | Should -BeExactly 'indeterminate'
        $lowerCase['evidence'] | Should -Not -Contain 'content:agentMarker'
    }

    It 'treats an agent thread property as evidence of an agent' {
        $properties = [ordered]@{ 'Microsoft.Publisher.LLMModel' = 'gpt' }
        $authorship = Get-PullRequestAuthorship -Identity (New-TestIdentity) -Content 'text' -Properties $properties

        $authorship['authorship'] | Should -Be 'agent'
        $authorship['evidence'] | Should -Contain 'property:LLMModel'
    }

    It 'matches the authenticated Azure DevOps identity on descriptor' {
        $self = New-TestIdentity -Descriptor 'aad.SELF'
        $same = New-TestIdentity -Id '99' -DisplayName 'Renamed' -Descriptor 'aad.self'
        $other = New-TestIdentity -Id '1' -Descriptor 'aad.OTHER'

        Test-PullRequestIdentityIsSelf -Identity $same -AuthenticatedIdentity $self | Should -BeTrue
        Test-PullRequestIdentityIsSelf -Identity $other -AuthenticatedIdentity $self | Should -BeFalse
        Test-PullRequestIdentityIsSelf -Identity $null -AuthenticatedIdentity $self | Should -BeFalse
    }

    It 'matches the authenticated GitHub identity on account identifier' {
        $self = New-PullRequestIdentity -Platform gitHub -Id '5' -DisplayName 'old-login' -AccountType 'User'
        $renamed = New-PullRequestIdentity -Platform gitHub -Id '5' -DisplayName 'new-login' -AccountType 'User'

        Test-PullRequestIdentityIsSelf -Identity $renamed -AuthenticatedIdentity $self | Should -BeTrue
    }
}

Describe 'Document assembly' {
    BeforeAll {
        $script:Self = New-TestIdentity -Id '1' -DisplayName 'Reviewer' -UniqueName 'reviewer@example.com' -Descriptor 'aad.SELF'
        $script:Other = New-TestIdentity -Id '2' -DisplayName 'Author' -UniqueName 'author@example.com' -Descriptor 'aad.OTHER'
        $script:Bot = New-TestIdentity -Id '3' -DisplayName 'Automation' -UniqueName $null -Descriptor 'aadsp.BOT'
    }

    It 'orders threads and comments by their own time' {
        $threads = @(
            New-TestThread -Id 11 -EventUtc '2025-11-06T10:00:00Z' -Comments @(
                (New-TestComment -Id 111 -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
            New-TestThread -Id 10 -EventUtc '2025-11-05T10:00:00Z' -Comments @(
                (New-TestComment -Id 102 -Author $script:Other -PublishedUtc '2025-11-05T11:00:00Z')
                (New-TestComment -Id 101 -Author $script:Other -PublishedUtc '2025-11-05T10:00:00Z')
            )
        )
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads $threads -AuthenticatedIdentity $script:Self)

        @($document['threads'] | ForEach-Object { $_['id'] }) | Should -Be @(10, 11)
        @($document['threads'][0]['comments'] | ForEach-Object { $_['id'] }) | Should -Be @(101, 102)
    }

    It 'orders numeric identifiers numerically when timestamps tie' {
        $comments = @(
            (New-TestComment -Id 10 -Author $script:Other -PublishedUtc '2025-11-05T10:00:00Z')
            (New-TestComment -Id 9 -Author $script:Other -PublishedUtc '2025-11-05T10:00:00Z')
        )
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @((New-TestThread -Id 1 -Comments $comments)) -AuthenticatedIdentity $script:Self)

        @($document['threads'][0]['comments'] | ForEach-Object { $_['id'] }) | Should -Be @(9, 10)
    }

    It 'sorts an undated event last' {
        $threads = @(
            New-TestThread -Id 2 -EventUtc '2025-11-06T10:00:00Z' -Comments @()
            New-TestThread -Id 1 -Comments @()
        )
        $threads[1]['eventUtc'] = $null
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads $threads -AuthenticatedIdentity $script:Self)

        @($document['threads'] | ForEach-Object { $_['id'] }) | Should -Be @(2, 1)
    }

    It 'marks what the running identity wrote' {
        $threads = @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 1 -Author $script:Self)
                (New-TestComment -Id 2 -Author $script:Other -PublishedUtc '2025-11-05T11:00:00Z')
            )
            New-TestThread -Id 2 -EventUtc '2025-11-07T10:00:00Z' -Comments @(
                (New-TestComment -Id 3 -Author $script:Other -PublishedUtc '2025-11-07T10:00:00Z')
            )
        )
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads $threads -AuthenticatedIdentity $script:Self)

        $document['threads'][0]['isAuthoredBySelf'] | Should -BeTrue
        $document['threads'][0]['comments'][0]['isAuthoredBySelf'] | Should -BeTrue
        $document['threads'][0]['comments'][1]['isAuthoredBySelf'] | Should -BeFalse
        $document['threads'][1]['isAuthoredBySelf'] | Should -BeFalse
    }

    It 'marks the pull request as the running identity''s own when it authored it' {
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self -PullRequestAuthor $script:Self)

        $document['pullRequest']['isAuthoredBySelf'] | Should -BeTrue
    }

    It 'marks the pull request as another identity''s when someone else authored it' {
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self -PullRequestAuthor $script:Other)

        $document['pullRequest']['isAuthoredBySelf'] | Should -BeFalse
    }

    It 'marks the pull request as another identity''s when its author is unknown' {
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self)

        $document['pullRequest']['isAuthoredBySelf'] | Should -BeFalse
    }

    It 'attributes a thread to the comment that opened it rather than to the one presented first' {
        # Two comments sharing an instant are ordered by identifier, which can place a reply ahead
        # of the root. The opener the adapter named settles the attribution, so the thread stays
        # attributed to the person who started it (docs/implementation.md, "Canonical document").
        $thread = New-TestThread -Id 1 -OpeningCommentId 20 -Comments @(
            (New-TestComment -Id 20 -Author $script:Other -PublishedUtc '2025-11-05T10:00:00Z')
            (New-TestComment -Id 10 -Author $script:Self -PublishedUtc '2025-11-05T10:00:00Z')
        )
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @($thread) -AuthenticatedIdentity $script:Self)

        @($document['threads'][0]['comments'] | ForEach-Object { $_['id'] }) | Should -Be @(10, 20)
        $document['threads'][0]['isAuthoredBySelf'] | Should -BeFalse
    }

    It 'attributes a thread whose opener carries no timestamp to that opener' {
        # An undated comment sorts last, so a thread attributed from presentation order would be
        # handed to a reply whenever the root's instant is missing.
        $opener = New-TestComment -Id 10 -Author $script:Other -PublishedUtc '2025-11-05T10:00:00Z'
        $opener['publishedUtc'] = $null
        $thread = New-TestThread -Id 1 -OpeningCommentId 10 -Comments @(
            $opener
            (New-TestComment -Id 20 -Author $script:Self -PublishedUtc '2025-11-05T11:00:00Z')
        )
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @($thread) -AuthenticatedIdentity $script:Self)

        @($document['threads'][0]['comments'] | ForEach-Object { $_['id'] }) | Should -Be @(20, 10)
        $document['threads'][0]['isAuthoredBySelf'] | Should -BeFalse
    }

    It 'attributes a bodyless decision thread to the identity that decided' {
        $decision = [ordered]@{
            type       = 'refUpdate'
            value      = 'commit001'
            label      = 'force pushed'
            by         = $script:Self
            utc        = (ConvertTo-CanonicalTimestamp -Value '2025-11-20T10:00:00Z')
            authorship = 'human'
        }
        $thread = New-TestThread -Id 1 -Comments @() -Kind 'refUpdate' -Decision $decision -Resolution 'indeterminate'
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @($thread) -AuthenticatedIdentity $script:Self)

        $document['threads'][0]['isAuthoredBySelf'] | Should -BeTrue
        $document['threads'][0]['decision']['authorship'] | Should -Be 'human'
    }

    It 'derives the repository URL for both platforms' {
        $ado = Resolve-PullRequestTarget -Url 'https://dev.azure.com/contoso/Example%20Project/_git/widgets/pullrequest/7'
        $gitHub = Resolve-PullRequestTarget -Url 'https://github.com/example/widgets/pull/1'

        Get-RepositoryUrl -Target $ado | Should -Be 'https://dev.azure.com/contoso/Example%20Project/_git/widgets'
        Get-RepositoryUrl -Target $gitHub | Should -Be 'https://github.com/example/widgets'
    }
}

Describe 'Capture round trip' {
    BeforeAll {
        $script:Self = New-TestIdentity -Descriptor 'aad.SELF'
        $script:OutputRoot = Join-Path ([System.IO.Path]::GetTempPath()) "ss-review-tests-$([guid]::NewGuid())"
    }

    AfterAll {
        if (Test-Path -LiteralPath $script:OutputRoot) {
            Remove-Item -LiteralPath $script:OutputRoot -Recurse -Force
        }
    }

    It 'preserves a body through the document and back byte for byte' {
        $body = @"
Consider the $script:Robot path $script:EmDash see $script:LeftQuote Registered I/O $script:RightQuote below.

``````
pub fn x() {}
``````
"@
        $body = $body.Replace("`r`n", "`n")
        $thread = New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Self -Content $body))
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @($thread) -AuthenticatedIdentity $script:Self)

        $path = Join-Path $script:OutputRoot 'pr-context.json'
        Write-PullRequestOutputFile -Path $path -Content (ConvertTo-JsonText -Value $document)
        $reread = Read-PullRequestContextDocument -Path $path

        $reread['threads'][0]['comments'][0]['content'] | Should -BeExactly $body
    }

    It 'preserves a body that is nothing but a timestamp' {
        $body = '2026-08-04T10:00:00+03:00'
        $thread = New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Self -Content $body))
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @($thread) -AuthenticatedIdentity $script:Self)

        $path = Join-Path $script:OutputRoot 'timestamp.json'
        Write-PullRequestOutputFile -Path $path -Content (ConvertTo-JsonText -Value $document)
        $reread = Read-PullRequestContextDocument -Path $path

        $reread['threads'][0]['comments'][0]['content'] | Should -BeExactly $body
    }

    It 'writes UTF-8 without a byte order mark and without carriage returns' {
        $path = Join-Path $script:OutputRoot 'nested\encoding.md'
        Write-PullRequestOutputFile -Path $path -Content "first`r`nsecond $script:Robot"

        $bytes = [System.IO.File]::ReadAllBytes($path)
        $bytes[0] | Should -Not -Be 0xEF
        $bytes | Should -Not -Contain 13
        [System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8) | Should -BeExactly "first`nsecond $script:Robot"
    }
}

Describe 'Capture comparison' {
    BeforeAll {
        $script:Self = New-TestIdentity -Descriptor 'aad.SELF'
        $script:Other = New-TestIdentity -Id '2' -Descriptor 'aad.OTHER'

        function New-Document {
            param([object[]] $Threads, [string] $SourceCommit = 'aaa111', [string] $MergeBaseCommit = 'bbb222')
            $context = New-TestContext -Threads $Threads -AuthenticatedIdentity $script:Self
            $context['pullRequest']['reviewedSourceCommit'] = $SourceCommit
            $context['pullRequest']['reviewedMergeBaseCommit'] = $MergeBaseCommit
            New-PullRequestContextDocument -Context $context
        }
    }

    It 'reports no change between two identical captures' {
        $threads = { @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other)))) }
        $delta = Get-PullRequestContextDelta -Previous (New-Document -Threads (& $threads)) -Current (New-Document -Threads (& $threads))

        $delta['hasChanges'] | Should -BeFalse
    }

    It 'reports an added thread and an added comment' {
        $previous = New-Document -Threads @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $current = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 1 -Author $script:Other)
                (New-TestComment -Id 2 -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
            New-TestThread -Id 2 -EventUtc '2025-11-07T10:00:00Z' -Comments @(
                (New-TestComment -Id 3 -Author $script:Other -PublishedUtc '2025-11-07T10:00:00Z')
            )
        )
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['threadsAdded'] | Should -Be @('2')
        # Both the comment added to the existing thread and the comment that arrived with the new
        # thread count, so a reader sizing the new feedback by this number sees all of it.
        $delta['commentsAdded'].Count | Should -Be 2
        @($delta['commentsAdded'] | ForEach-Object { $_['commentId'] }) | Should -Be @('2', '3')
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'reports a comment that disappeared' {
        # GitHub leaves no tombstone for a deleted comment, so absence on the second look is the only
        # evidence there is.
        $previous = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 1 -Author $script:Other)
                (New-TestComment -Id 2 -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
        )
        $current = New-Document -Threads @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['commentsRemoved'].Count | Should -Be 1
        $delta['commentsRemoved'][0]['commentId'] | Should -Be '2'
    }

    It 'reports a thread that disappeared along with its comments' {
        $previous = New-Document -Threads @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $current = New-Document -Threads @()
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['threadsRemoved'] | Should -Be @('1')
        $delta['commentsRemoved'].Count | Should -Be 1
    }

    It 'reports a resolution change' {
        $previous = New-Document -Threads @((New-TestThread -Id 1 -Resolution 'active' -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $current = New-Document -Threads @((New-TestThread -Id 1 -Resolution 'resolved' -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['resolutionChanged'].Count | Should -Be 1
        $delta['resolutionChanged'][0]['from'] | Should -Be 'active'
        $delta['resolutionChanged'][0]['to'] | Should -Be 'resolved'
    }

    It 'reports a new reviewed source commit' {
        $threads = @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $delta = Get-PullRequestContextDelta -Previous (New-Document -Threads $threads -SourceCommit 'aaa111') -Current (New-Document -Threads $threads -SourceCommit 'ccc333')

        $delta['reviewedSourceCommitChanged'] | Should -BeTrue
        $delta['previousReviewedSourceCommit'] | Should -Be 'aaa111'
        $delta['reviewedSourceCommit'] | Should -Be 'ccc333'
        $delta['reviewedMergeBaseCommitChanged'] | Should -BeFalse
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'reports a moved merge base even when the source commit stands still' {
        # The target branch advancing moves the merge base and changes what the pull request
        # proposes, without touching the source commit. A comparison blind to it would report a
        # review of the earlier diff as still current (docs/design.md, "Freshness check").
        $threads = @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $delta = Get-PullRequestContextDelta `
            -Previous (New-Document -Threads $threads -MergeBaseCommit 'bbb222') `
            -Current (New-Document -Threads $threads -MergeBaseCommit 'ddd444')

        $delta['reviewedSourceCommitChanged'] | Should -BeFalse
        $delta['reviewedMergeBaseCommitChanged'] | Should -BeTrue
        $delta['previousReviewedMergeBaseCommit'] | Should -Be 'bbb222'
        $delta['reviewedMergeBaseCommit'] | Should -Be 'ddd444'
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'reports a comment edited since the earlier capture' {
        # An edit keeps the comment's identifier, so nothing else in the comparison notices it, while
        # the text the reviewer read has been replaced. The comparison consults the edit timestamp,
        # which is the only edit signal both platforms report.
        $previous = New-Document -Threads @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other))))
        $current = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 1 -Author $script:Other -Content 'rewritten' -LastEditedUtc '2025-11-06T10:00:00Z')
            )
        )
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['commentsEdited'].Count | Should -Be 1
        $delta['commentsEdited'][0]['threadId'] | Should -Be '1'
        $delta['commentsEdited'][0]['commentId'] | Should -Be '1'
        $delta['commentsAdded'].Count | Should -Be 0
        $delta['commentsRemoved'].Count | Should -Be 0
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'leaves a comment whose edit timestamp is unchanged out of the edited list' {
        $threads = { @((New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other -LastEditedUtc '2025-11-06T10:00:00Z')))) }
        $delta = Get-PullRequestContextDelta -Previous (New-Document -Threads (& $threads)) -Current (New-Document -Threads (& $threads))

        $delta['commentsEdited'].Count | Should -Be 0
        $delta['hasChanges'] | Should -BeFalse
    }

    It 'tracks comments whose identifiers differ only in case as two comments' {
        $threads = {
            @(
                New-TestThread -Id 1 -Comments @(
                    (New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other)
                    (New-TestComment -Id 'prrc_kwdoabc' -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
                )
            )
        }
        $delta = Get-PullRequestContextDelta -Previous (New-Document -Threads (& $threads)) -Current (New-Document -Threads (& $threads))

        $delta['commentsAdded'].Count | Should -Be 0
        $delta['commentsRemoved'].Count | Should -Be 0
        $delta['hasChanges'] | Should -BeFalse

        # Two captures that agree report nothing whether or not the comparison tells the pair apart,
        # so the same pair is edited on both sides to show that each identifier is tracked in its own
        # right rather than one standing in for both.
        $edited = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other -LastEditedUtc '2025-11-07T10:00:00Z')
                (New-TestComment -Id 'prrc_kwdoabc' -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z' -LastEditedUtc '2025-11-07T11:00:00Z')
            )
        )
        $editDelta = Get-PullRequestContextDelta -Previous (New-Document -Threads (& $threads)) -Current $edited

        $editDelta['commentsEdited'].Count | Should -Be 2
        @($editDelta['commentsEdited'] | ForEach-Object { $_['commentId'] }) | Should -BeExactly @('PRRC_kwDOAbc', 'prrc_kwdoabc')
    }

    It 'reports a comment whose identifier differs only in case from one already captured' {
        # A GitHub node identifier is a case-sensitive opaque string. A case-insensitive index would
        # hide the new comment behind the one already captured, and the freshness check that gates
        # publication would report that nothing arrived.
        $previous = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @((New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other))
        )
        $current = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other)
                (New-TestComment -Id 'prrc_kwdoabc' -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
        )
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['commentsAdded'].Count | Should -Be 1
        $delta['commentsAdded'][0]['commentId'] | Should -BeExactly 'prrc_kwdoabc'
        $delta['commentsRemoved'].Count | Should -Be 0
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'reports the disappearance of a comment whose identifier differs only in case from a survivor' {
        $previous = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @(
                (New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other)
                (New-TestComment -Id 'prrc_kwdoabc' -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
        )
        $current = New-Document -Threads @(
            New-TestThread -Id 1 -Comments @((New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Other))
        )
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['commentsRemoved'].Count | Should -Be 1
        $delta['commentsRemoved'][0]['commentId'] | Should -BeExactly 'prrc_kwdoabc'
        $delta['commentsAdded'].Count | Should -Be 0
        $delta['hasChanges'] | Should -BeTrue
    }

    It 'keeps threads whose identifiers differ only in case apart' {
        $previous = New-Document -Threads @(
            New-TestThread -Id 'PRT_kwDOAbc' -Comments @((New-TestComment -Id 'c1' -Author $script:Other))
        )
        $current = New-Document -Threads @(
            New-TestThread -Id 'PRT_kwDOAbc' -Comments @((New-TestComment -Id 'c1' -Author $script:Other))
            New-TestThread -Id 'prt_kwdoabc' -EventUtc '2025-11-06T10:00:00Z' -Comments @(
                (New-TestComment -Id 'c2' -Author $script:Other -PublishedUtc '2025-11-06T10:00:00Z')
            )
        )
        $delta = Get-PullRequestContextDelta -Previous $previous -Current $current

        $delta['threadsAdded'] | Should -Be @('prt_kwdoabc')
        $delta['threadsRemoved'].Count | Should -Be 0
        $delta['commentsAdded'].Count | Should -Be 1
        $delta['commentsAdded'][0]['commentId'] | Should -BeExactly 'c2'
    }
}

Describe 'New-PullRequestCaptureResult' {
    BeforeAll {
        $script:Self = New-TestIdentity -Id '1' -Descriptor 'aad.SELF'
        $script:Other = New-TestIdentity -Id '2' -Descriptor 'aad.OTHER'

        $script:Document = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id 1 -Comments @(
                    (New-TestComment -Id 1 -Author $script:Other)
                    (New-TestComment -Id 2 -Author $script:Self -PublishedUtc '2025-11-06T10:00:00Z')
                )
                New-TestThread -Id 2 -EventUtc '2025-11-07T10:00:00Z' -Comments @(
                    (New-TestComment -Id 3 -Author $script:Other -PublishedUtc '2025-11-07T10:00:00Z')
                )
            ) -AuthenticatedIdentity $script:Self -Reviewers @($script:Other))
    }

    It 'counts threads, comments and reviewers across the whole document' {
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'C:\out\pr-context.json' -Delta $null

        $result['threadCount'] | Should -Be 2
        $result['commentCount'] | Should -Be 3
        $result['reviewerCount'] | Should -Be 1
    }

    It 'carries the reviewed commit pair and the written path' {
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'C:\out\pr-context.json' -Delta $null

        $result['reviewedSourceCommit'] | Should -Be 'aaa111'
        $result['reviewedMergeBaseCommit'] | Should -Be 'bbb222'
        $result['contextPath'] | Should -Be 'C:\out\pr-context.json'
        $result['delta'] | Should -BeNullOrEmpty
    }

    It 'serializes to a single JSON object' {
        # The executable emits this on standard output as its entire result, so a stray value on the
        # pipeline would turn the object into an array and break every caller that parses it.
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'a' -Delta $null
        $json = $result | ConvertTo-Json -Depth 32 -Compress

        $json | Should -BeOfType ([string])
        $json.StartsWith('{', [StringComparison]::Ordinal) | Should -BeTrue
        (ConvertFrom-JsonVerbatim -Text $json)['threadCount'] | Should -Be 2
    }

    It 'reports an empty capture without failing' {
        $empty = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self)
        $result = New-PullRequestCaptureResult -Document $empty -ContextPath 'a' -Delta $null

        $result['threadCount'] | Should -Be 0
        $result['commentCount'] | Should -Be 0
        $result['reviewerCount'] | Should -Be 0
    }

    It 'counts comments needing classification without listing them' {
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'a' -Delta $null

        $result['indeterminateCommentCount'] | Should -Be 3
        $result.Contains('indeterminateComments') | Should -BeFalse
    }

    It 'reports no outstanding classification when every comment is settled' {
        $agent = New-TestIdentity -Id '9' -DisplayName 'Review Bot' -UniqueName 'bot@example.com' -Descriptor 'aadsp.BOT'
        $settled = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $agent))
            ) -AuthenticatedIdentity $script:Self)
        $result = New-PullRequestCaptureResult -Document $settled -ContextPath 'a' -Delta $null

        $result['commentCount'] | Should -Be 1
        $result['indeterminateCommentCount'] | Should -Be 0
    }

    It 'reports exactly the keys its caller parses' {
        # The calling agent reads this object as the entire result of the capture, so a key that
        # silently appears or disappears changes a published contract.
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'a' -Delta $null

        @($result.Keys) | Should -BeExactly @(
            'platform'
            'pullRequestUrl'
            'repositoryUrl'
            'contextPath'
            'fetchedUtc'
            'reviewedSourceCommit'
            'reviewedMergeBaseCommit'
            'threadCount'
            'commentCount'
            'reviewerCount'
            'indeterminateCommentCount'
            'reusedClassificationCount'
            'delta'
        )
    }

    It 'reports delta counts without emitting discussion-sized arrays' {
        $empty = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self)
        $delta = Get-PullRequestContextDelta -Previous $empty -Current $script:Document
        $result = New-PullRequestCaptureResult -Document $script:Document -ContextPath 'a' -Delta $delta

        $result['delta']['threadsAddedCount'] | Should -Be 2
        $result['delta']['commentsAddedCount'] | Should -Be 3
        $result['delta']['hasChanges'] | Should -BeTrue
        $result['delta']['reviewedSourceCommitChanged'] | Should -BeFalse
        @($result['delta'].Values | Where-Object { $_ -is [array] }) | Should -BeNullOrEmpty
    }
}



Describe 'Line ending fidelity' {
    BeforeAll {
        $script:Self = New-TestIdentity -Id '1' -Descriptor 'aad.SELF'
        $script:Other = New-TestIdentity -Id '2' -Descriptor 'aad.OTHER'
        $script:Lf = [string][char]10
        $script:Crlf = [string][char]13 + $script:Lf
        $script:Body = 'first' + $script:Crlf + 'second'
    }

    It 'keeps a comment body byte-exact in the context document' {
        # GitHub returns comment bodies with CRLF. The machine-readable record is the byte-exact
        # copy, so a consumer that needs the original bytes has somewhere to get them.
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other -Content $script:Body))
            ) -AuthenticatedIdentity $script:Self)

        $threads = Get-JsonArrayProperty -Node $document -Name 'threads'
        $comments = Get-JsonArrayProperty -Node $threads[0] -Name 'comments'
        (Get-JsonProperty -Node $comments[0] -Name 'content') | Should -Be $script:Body
    }

    It 'normalizes line endings when writing a capture' {
        # A capture written on one platform has to read the same as a capture written on the other,
        # so the written file carries one line ending throughout.
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id 1 -Comments @((New-TestComment -Id 1 -Author $script:Other -Content $script:Body))
            ) -AuthenticatedIdentity $script:Self)

        $path = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
        try {
            Write-PullRequestOutputFile -Path $path -Content (ConvertTo-JsonText -Value $document)
            $written = [System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8)

            $written.Contains([char]13) | Should -BeFalse

            # A comment body lives inside a JSON string with its own line endings escaped, so
            # normalizing the file leaves the recorded body byte-exact.
            $reread = ConvertFrom-JsonVerbatim -Text $written
            $threads = Get-JsonArrayProperty -Node $reread -Name 'threads'
            $comments = Get-JsonArrayProperty -Node $threads[0] -Name 'comments'
            (Get-JsonProperty -Node $comments[0] -Name 'content') | Should -BeExactly $script:Body
        } finally {
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        }
    }
}
Describe 'Identity comparison' {
    BeforeAll {
        function New-Candidate {
            param([string] $Id = '', [string] $UniqueName = '', [string] $Descriptor = '', [string] $Platform = 'azureDevOps')
            New-PullRequestIdentity -Platform $Platform -Id $Id -DisplayName 'Someone' -UniqueName $UniqueName -Descriptor $Descriptor
        }
    }

    It 'settles on the most authoritative field both records carry' {
        $left = New-Candidate -Id '1' -Descriptor 'aad.SAME'
        $right = New-Candidate -Id '2' -Descriptor 'aad.SAME'

        Test-PullRequestIdentityMatch -Left $left -Right $right | Should -BeTrue
    }

    It 'treats disagreement on the deciding field as two accounts' {
        # A weaker field agreeing cannot rescue a stronger field that disagrees, or two accounts
        # sharing an email-shaped name would be reported as one.
        $left = New-Candidate -Id '1' -UniqueName 'shared@example.com'
        $right = New-Candidate -Id '2' -UniqueName 'shared@example.com'

        Test-PullRequestIdentityMatch -Left $left -Right $right | Should -BeFalse
    }

    It 'falls through to a weaker field when the stronger one is absent on either side' {
        $left = New-Candidate -Id '1'
        $right = New-Candidate -UniqueName 'person@example.com'
        $matching = New-Candidate -Id '1' -UniqueName 'person@example.com'

        Test-PullRequestIdentityMatch -Left $left -Right $matching | Should -BeTrue
        Test-PullRequestIdentityMatch -Left $right -Right $matching | Should -BeTrue
    }

    It 'accepts any single agreeing field when asked whether two records might match' {
        # The permissive comparison guards a write that would overwrite the state already recorded
        # for an account, so it errs towards concluding the account is already present.
        $left = New-Candidate -Id '1' -UniqueName 'shared@example.com'
        $right = New-Candidate -Id '2' -UniqueName 'SHARED@example.com'

        Test-PullRequestIdentityMayMatch -Left $left -Right $right | Should -BeTrue
        Test-PullRequestIdentityMatch -Left $left -Right $right | Should -BeFalse
    }

    It 'reports no match when the records share no populated field' {
        $left = New-Candidate -Id '1'
        $right = New-Candidate -UniqueName 'person@example.com'

        Test-PullRequestIdentityMayMatch -Left $left -Right $right | Should -BeFalse
        Test-PullRequestIdentityMatch -Left $left -Right $right | Should -BeFalse
    }

    It 'reports no match against a missing record' {
        $left = New-Candidate -Id '1'

        Test-PullRequestIdentityMatch -Left $left -Right $null | Should -BeFalse
        Test-PullRequestIdentityMayMatch -Left $null -Right $left | Should -BeFalse
    }

    It 'matches a GitHub account on its identifier rather than its login' {
        $renamed = New-Candidate -Id '42' -UniqueName 'new-login' -Platform 'gitHub'
        $recorded = New-Candidate -Id '42' -UniqueName 'old-login' -Platform 'gitHub'

        Test-PullRequestIdentityMatch -Left $renamed -Right $recorded | Should -BeTrue
    }
}
Describe 'Comparison path guard' {
    BeforeAll {
        $script:Directory = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
        $script:ContextPath = Join-Path $script:Directory 'pr-context.json'
    }

    It 'refuses a comparison against a file the capture is about to write' {
        { Assert-PullRequestComparisonPath -ComparePath $script:ContextPath -OutputPath @($script:ContextPath) } |
            Should -Throw -ExpectedMessage '*distinct -OutputPrefix*'
    }

    It 'refuses regardless of how the path is spelled' {
        # The caller composes this path from prose, so the same file arrives spelled differently:
        # through a relative segment, or with the casing Windows does not distinguish.
        $indirect = Join-Path $script:Directory (Join-Path 'nested' '..\pr-context.json')
        { Assert-PullRequestComparisonPath -ComparePath $indirect -OutputPath @($script:ContextPath) } |
            Should -Throw -ExpectedMessage '*distinct -OutputPrefix*'
        { Assert-PullRequestComparisonPath -ComparePath $script:ContextPath.ToUpperInvariant() -OutputPath @($script:ContextPath) } |
            Should -Throw -ExpectedMessage '*distinct -OutputPrefix*'
    }

    It 'permits a comparison against a capture written under a different prefix' {
        $earlier = Join-Path $script:Directory 'pr-context.json'
        $refreshed = Join-Path $script:Directory 'refresh-pr-context.json'
        { Assert-PullRequestComparisonPath -ComparePath $earlier -OutputPath @($refreshed) } |
            Should -Not -Throw
    }
}

Describe 'Capture schema enforcement' {
    BeforeAll {
        $script:SchemaRoot = Join-Path ([System.IO.Path]::GetTempPath()) "ss-review-schema-$([guid]::NewGuid())"
    }

    AfterAll {
        if (Test-Path -LiteralPath $script:SchemaRoot) {
            Remove-Item -LiteralPath $script:SchemaRoot -Recurse -Force
        }
    }

    It 'refuses a capture that reports no schema version' {
        # A document shaped by a different helper does not fail the comparison that consumes it: it
        # reports every thread as removed and every thread as added, or reports no change at all, and
        # that verdict decides whether a review may be published.
        $path = Join-Path $script:SchemaRoot 'unversioned.json'
        Write-PullRequestOutputFile -Path $path -Content '{"threads":[]}'

        $failure = { Read-PullRequestContextDocument -Path $path } | Should -Throw -PassThru
        $failure.Exception.Message.Contains('no version', [StringComparison]::Ordinal) | Should -BeTrue
        $failure.Exception.Message.Contains('Capture the pull request again', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'refuses a capture written to a schema version this helper does not read' {
        $path = Join-Path $script:SchemaRoot 'future.json'
        Write-PullRequestOutputFile -Path $path -Content '{"schemaVersion":99,"threads":[]}'

        $failure = { Read-PullRequestContextDocument -Path $path } | Should -Throw -PassThru
        $failure.Exception.Message.Contains('version 99', [StringComparison]::Ordinal) | Should -BeTrue
        $failure.Exception.Message.Contains('Capture the pull request again', [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'accepts a capture written by this helper' {
        $self = New-TestIdentity -Descriptor 'aad.SELF'
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $self)
        $path = Join-Path $script:SchemaRoot 'current.json'
        Write-PullRequestOutputFile -Path $path -Content (ConvertTo-JsonText -Value $document)

        (Read-PullRequestContextDocument -Path $path)['schemaVersion'] | Should -Be $document['schemaVersion']
    }

    It 'states the schema version the capture shape currently carries' {
        # The version is asserted against a literal rather than against the library's own constant,
        # so that a change to the persisted shape cannot be made without also being declared here.
        $self = New-TestIdentity -Descriptor 'aad.SELF'
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $self)

        $document['schemaVersion'] | Should -Be 3
    }
}

Describe 'Relative path resolution' {
    BeforeAll {
        $script:Self = New-TestIdentity -Descriptor 'aad.SELF'
        $script:PathRoot = Join-Path ([System.IO.Path]::GetTempPath()) "ss-review-paths-$([guid]::NewGuid())"
        New-Item -ItemType Directory -Path $script:PathRoot | Out-Null

        # The two settings this fixture exists to tell apart are compared as normalized full paths,
        # because either can be spelled with a trailing separator or in a different case.
        function Get-ComparablePath {
            param([Parameter(Mandatory)] [string] $Path)
            [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
        }
    }

    AfterAll {
        if (Test-Path -LiteralPath $script:PathRoot) {
            Remove-Item -LiteralPath $script:PathRoot -Recurse -Force
        }
    }

    It 'writes a relative path under the caller location rather than the process working directory' {
        $location = Get-Location

        # The segment is unique to the run, so an artifact a failing run left behind cannot
        # pre-satisfy the assertion that nothing was written against the process working directory.
        $segment = Join-Path "relative-$([guid]::NewGuid())" 'pr-context.json'
        $strayPath = Join-Path ([Environment]::CurrentDirectory) $segment
        try {
            Set-Location -LiteralPath $script:PathRoot

            # Set-Location moves the PowerShell location only, so System.IO still resolves a relative
            # path against the process working directory. Asserted rather than assumed, because if
            # the two ever agreed here the test would pass while covering nothing.
            (Get-ComparablePath -Path ([Environment]::CurrentDirectory)) |
                Should -Not -Be (Get-ComparablePath -Path $script:PathRoot)

            Write-PullRequestOutputFile -Path $segment -Content '{"schemaVersion":2,"threads":[]}'

            [System.IO.File]::Exists((Join-Path $script:PathRoot $segment)) | Should -BeTrue
            [System.IO.File]::Exists($strayPath) | Should -BeFalse
        } finally {
            Set-Location -LiteralPath $location

            # A regression writes the capture against the process working directory, which is the
            # repository root for the documented way of running this suite. The artifact is removed
            # so that it neither pollutes the working tree nor survives into a later run.
            $strayDirectory = Split-Path -Parent $strayPath
            if (Test-Path -LiteralPath $strayDirectory) {
                Remove-Item -LiteralPath $strayDirectory -Recurse -Force
            }
        }
    }

    It 'reads a capture named by a relative path' {
        $location = Get-Location
        try {
            $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @() -AuthenticatedIdentity $script:Self)
            Write-PullRequestOutputFile -Path (Join-Path $script:PathRoot 'read\pr-context.json') -Content (ConvertTo-JsonText -Value $document)

            Set-Location -LiteralPath $script:PathRoot
            (Read-PullRequestContextDocument -Path 'read\pr-context.json')['schemaVersion'] | Should -Be $document['schemaVersion']
        } finally {
            Set-Location -LiteralPath $location
        }
    }

    It 'recognizes a relative comparison path as the file the capture is about to write' {
        $location = Get-Location
        try {
            Set-Location -LiteralPath $script:PathRoot

            { Assert-PullRequestComparisonPath -ComparePath 'guard\pr-context.json' -OutputPath @((Join-Path $script:PathRoot 'guard\pr-context.json')) } |
                Should -Throw -ExpectedMessage '*distinct -OutputPrefix*'
        } finally {
            Set-Location -LiteralPath $location
        }
    }
}

Describe 'Comment authorship classification' {
    BeforeAll {
        $script:Person = New-TestIdentity -Id '1' -Descriptor 'aad.PERSON'
        $script:Agent = New-TestIdentity -Id '2' -DisplayName 'Review Bot' -UniqueName 'bot@example.com' -Descriptor 'aadsp.BOT'

        # Two comments the capture left indeterminate and one it proved machine-authored, which is
        # the state every classification rule has to be exercised against.
        function New-ClassifiableDocument {
            New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                    New-TestThread -Id 1 -Comments @(
                        (New-TestComment -Id 1 -Author $script:Person)
                        (New-TestComment -Id 2 -Author $script:Agent -PublishedUtc '2025-11-05T11:00:00Z')
                    )
                    New-TestThread -Id 2 -EventUtc '2025-11-06T10:00:00Z' -Comments @(
                        (New-TestComment -Id 3 -Author $script:Person -PublishedUtc '2025-11-06T10:00:00Z')
                    )
                ) -AuthenticatedIdentity $script:Person)
        }

        function Get-DocumentComment {
            param(
                [Parameter(Mandatory)] [System.Collections.IDictionary] $Document,
                [Parameter(Mandatory)] [string] $ThreadId,
                [Parameter(Mandatory)] [string] $CommentId
            )

            # Assigned before use: these accessors comma-wrap their result so a single-element list
            # survives whole, and wrapping the call in @( ) would nest it one level deeper.
            $threads = Get-JsonArrayProperty -Node $Document -Name 'threads'
            foreach ($thread in $threads) {
                if ([string] (Get-JsonProperty -Node $thread -Name 'id') -cne $ThreadId) { continue }
                $comments = Get-JsonArrayProperty -Node $thread -Name 'comments'
                foreach ($comment in $comments) {
                    if ([string] (Get-JsonProperty -Node $comment -Name 'id') -ceq $CommentId) { return $comment }
                }
            }
            throw "The test document contains no comment '$CommentId' in thread '$ThreadId'."
        }

        function New-Classification {
            param(
                [Parameter(Mandatory)] [string] $ThreadId,
                [Parameter(Mandatory)] [string] $CommentId,
                [Parameter(Mandatory)] [AllowEmptyString()] [string] $Kind
            )

            [ordered]@{ threadId = $ThreadId; commentId = $CommentId; kind = $Kind }
        }
    }

    It 'settles an indeterminate comment as human' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))

        $summary['applied'] | Should -Be 1
        $summary['unchanged'] | Should -Be 0
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') |
            Should -BeExactly 'human'
    }

    It 'settles an indeterminate comment as agent' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent'))

        $summary['applied'] | Should -Be 1
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') |
            Should -BeExactly 'agent'
    }

    It 'records that a reader settled the comment without discarding the capture evidence' {
        # A later reader has to be able to see what an attribution rests on, so the evidence the
        # capture collected is added to rather than replaced.
        $document = New-ClassifiableDocument
        $before = Get-JsonArrayProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorshipEvidence'
        $before.Count | Should -BeGreaterThan 0

        Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')) | Out-Null

        $after = Get-JsonArrayProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorshipEvidence'
        $after -ccontains 'classification:reviewed' | Should -BeTrue
        foreach ($item in $before) {
            $after -ccontains $item | Should -BeTrue
        }
        $after.Count | Should -Be ($before.Count + 1)
    }

    It 'refuses to overwrite an authorship the capture proved' {
        # Evidence is not opinion. A misreading must not be able to discard the one classification
        # the capture is certain of.
        $document = New-ClassifiableDocument
        $failure = { Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '2' -Kind 'human')) } |
            Should -Throw -PassThru

        $message = $failure.Exception.Message
        $message.Contains("already classified as 'agent'", [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains('evidence the capture collected', [StringComparison]::Ordinal) | Should -BeTrue
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '2') -Name 'authorship') |
            Should -BeExactly 'agent'
    }

    It 'accepts a classification that restates what the capture already proved' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '2' -Kind 'agent'))

        $summary['applied'] | Should -Be 0
        $summary['unchanged'] | Should -Be 1

        # Restating changes nothing, so the comment does not collect a reader's evidence either.
        $evidence = Get-JsonArrayProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '2') -Name 'authorshipEvidence'
        $evidence -ccontains 'classification:reviewed' | Should -BeFalse
    }

    It 'refuses a classification naming a thread the capture does not contain' {
        # Silently ignoring it would leave the comment unclassified while reporting that the pass had
        # finished.
        $document = New-ClassifiableDocument
        $failure = { Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '99' -CommentId '1' -Kind 'human')) } |
            Should -Throw -PassThru

        $failure.Exception.Message.Contains("no thread '99'", [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'refuses a classification naming a comment the thread does not contain' {
        $document = New-ClassifiableDocument
        $failure = { Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '98' -Kind 'human')) } |
            Should -Throw -PassThru

        $message = $failure.Exception.Message
        $message.Contains("no comment '98'", [StringComparison]::Ordinal) | Should -BeTrue
        $message.Contains("thread '1'", [StringComparison]::Ordinal) | Should -BeTrue
    }

    It 'refuses an authorship outside the vocabulary' {
        $document = New-ClassifiableDocument
        $failure = { Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'maybe')) } |
            Should -Throw -PassThru

        $failure.Exception.Message.Contains("Use 'agent' or 'human'", [StringComparison]::Ordinal) | Should -BeTrue
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') |
            Should -BeExactly 'indeterminate'
    }

    It 'accepts a capitalized authorship and stores the vocabulary in lower case' {
        # The batch is written by a language model, which capitalizes a word as readily as not, and a
        # single rejected entry abandons the whole pass. The word is therefore read leniently and
        # stored in the one case every consumer branches on.
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'Human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'AGENT')
        )

        $summary['applied'] | Should -Be 2
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') |
            Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '2' -CommentId '3') -Name 'authorship') |
            Should -BeExactly 'agent'
    }

    It 'names the word the caller wrote when refusing it' {
        # The caller has to recognize its own input in the refusal, so the message quotes what was
        # supplied rather than the lower-case form the vocabulary is matched against.
        $document = New-ClassifiableDocument
        $failure = { Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'Robot')) } |
            Should -Throw -PassThru

        $failure.Exception.Message.Contains("authorship 'Robot'", [StringComparison]::Ordinal) | Should -BeTrue
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId '1') -Name 'authorship') |
            Should -BeExactly 'indeterminate'
    }

    It 'counts what remains unsettled across the whole document' {
        $document = New-ClassifiableDocument

        $first = Set-PullRequestCommentAuthorship -Document $document -Classification @()
        $first['indeterminate'] | Should -Be 2
        $first['isComplete'] | Should -BeFalse

        $second = Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human'))
        $second['indeterminate'] | Should -Be 1
        $second['isComplete'] | Should -BeFalse

        # The remaining comment lives in the other thread, so completeness is a property of the
        # document rather than of the thread the batch happened to touch.
        $third = Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '2' -CommentId '3' -Kind 'agent'))
        $third['indeterminate'] | Should -Be 0
        $third['isComplete'] | Should -BeTrue
    }

    It 'names the comments a batch left unsettled' {
        # The pass that has to visit exactly those comments would otherwise re-read the whole
        # document to find them, which is the cost the capture result already avoids.
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
        )

        $summary['indeterminateComments'].Count | Should -Be 1
        $summary['indeterminateComments'][0]['threadId'] | Should -Be '2'
        $summary['indeterminateComments'][0]['commentId'] | Should -Be '3'
        $summary['indeterminate'] | Should -Be $summary['indeterminateComments'].Count
    }

    It 'names every comment an empty batch left unsettled' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @()

        @($summary['indeterminateComments'] | ForEach-Object { "$($_['threadId'])/$($_['commentId'])" }) |
            Should -BeExactly @('1/1', '2/3')
    }

    It 'names nothing once every comment is settled' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
        )

        $summary['indeterminateComments'].Count | Should -Be 0
        $summary['isComplete'] | Should -BeTrue
    }

    It 'reports exactly the keys its caller parses' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @()

        @($summary.Keys) | Should -BeExactly @('applied', 'unchanged', 'indeterminate', 'indeterminateComments', 'isComplete')
    }

    It 'agrees with the capture count of unsettled comments' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @()
        $capture = New-PullRequestCaptureResult -Document $document -ContextPath 'context.json' -Delta $null

        $summary['indeterminateComments'].Count | Should -Be $capture['indeterminateCommentCount']
    }

    It 'applies a batch that spans threads' {
        $document = New-ClassifiableDocument
        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')
            (New-Classification -ThreadId '1' -CommentId '2' -Kind 'agent')
            (New-Classification -ThreadId '2' -CommentId '3' -Kind 'human')
        )

        $summary['applied'] | Should -Be 2
        $summary['unchanged'] | Should -Be 1
        $summary['isComplete'] | Should -BeTrue
    }

    It 'updates the document in place so serializing it persists the classification' {
        # The executable writes the same object it passed in, so a classification recorded on a copy
        # would be reported as applied and then silently dropped.
        $document = New-ClassifiableDocument
        Set-PullRequestCommentAuthorship -Document $document -Classification @((New-Classification -ThreadId '1' -CommentId '1' -Kind 'human')) | Out-Null

        $reread = ConvertFrom-JsonVerbatim -Text (ConvertTo-JsonText -Value $document)
        $threads = Get-JsonArrayProperty -Node $reread -Name 'threads'
        $comments = Get-JsonArrayProperty -Node $threads[0] -Name 'comments'

        (Get-JsonProperty -Node $comments[0] -Name 'authorship') | Should -BeExactly 'human'
        $evidence = Get-JsonArrayProperty -Node $comments[0] -Name 'authorshipEvidence'
        $evidence -ccontains 'classification:reviewed' | Should -BeTrue
    }

    It 'settles the comment whose identifier matches in case and no other' {
        # A GitHub node identifier is a case-sensitive opaque string, so two comments may differ only
        # in case. A case-insensitive index would collapse them onto one entry and write the reader's
        # verdict to whichever of the two happened to land there.
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id '1' -Comments @(
                    (New-TestComment -Id 'PRRC_kwDOAbc' -Author $script:Person)
                    (New-TestComment -Id 'prrc_kwdoabc' -Author $script:Person -PublishedUtc '2025-11-05T11:00:00Z')
                )
            ) -AuthenticatedIdentity $script:Person)

        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId '1' -CommentId 'PRRC_kwDOAbc' -Kind 'human')
        )

        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId 'PRRC_kwDOAbc') -Name 'authorship') |
            Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId '1' -CommentId 'prrc_kwdoabc') -Name 'authorship') |
            Should -BeExactly 'indeterminate'
        $summary['applied'] | Should -Be 1
        $summary['indeterminate'] | Should -Be 1
        $summary['isComplete'] | Should -BeFalse
    }

    It 'keeps threads whose identifiers differ only in case apart' {
        $document = New-PullRequestContextDocument -Context (New-TestContext -Threads @(
                New-TestThread -Id 'PRT_kwDOAbc' -Comments @((New-TestComment -Id 'c1' -Author $script:Person))
                New-TestThread -Id 'prt_kwdoabc' -EventUtc '2025-11-06T10:00:00Z' -Comments @(
                    (New-TestComment -Id 'c2' -Author $script:Person -PublishedUtc '2025-11-06T10:00:00Z')
                )
            ) -AuthenticatedIdentity $script:Person)

        $summary = Set-PullRequestCommentAuthorship -Document $document -Classification @(
            (New-Classification -ThreadId 'PRT_kwDOAbc' -CommentId 'c1' -Kind 'human')
        )

        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId 'PRT_kwDOAbc' -CommentId 'c1') -Name 'authorship') |
            Should -BeExactly 'human'
        (Get-JsonProperty -Node (Get-DocumentComment -Document $document -ThreadId 'prt_kwdoabc' -CommentId 'c2') -Name 'authorship') |
            Should -BeExactly 'indeterminate'
        $summary['applied'] | Should -Be 1
        $summary['indeterminate'] | Should -Be 1
    }
}
