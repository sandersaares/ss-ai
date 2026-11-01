#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

# The mode is orchestrator policy, so these contracts guard its routing and reuse of
# the renderer rather than introducing a second execution engine for review phases.
BeforeAll {
    $root = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills/ss-review'
    $script:Skill = Get-Content -LiteralPath (Join-Path $root 'SKILL.md') -Raw
    $script:Delivery = Get-Content -LiteralPath (Join-Path $root 'self-review.md') -Raw
    $script:Recipe = Get-Content -LiteralPath (Join-Path $root 'pwsh-calls/render-self-review.md') -Raw
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}

Describe 'Self-review workflow contract' {
    It 'requires explicit selection and bypasses human and remote phases' {
        $script:Skill | Should -Match 'explicitly requests `self-review`'
        $script:Skill | Should -Match 'does not execute phases 7 through 10'
        $script:Skill | Should -Match 'Do not perform any remote writes'
        $script:Skill | Should -Match 'Ask no questions'
        $script:Skill | Should -Match 'primary-session ownership rule still applies'
    }

    It 'guards preparation side effects without skipping inventory preparation' {
        $script:Skill | Should -Match '1\. Unless this is self-review,.*register yourself'
        $script:Skill | Should -Match '4\. Unless this is self-review, check report-rendering'
        $script:Skill | Should -Match '7\. Unless this is self-review,.*publish the initial overview'
        $script:Skill | Should -Match 'still prepare the complete file inventory'
    }

    It 'renders all consolidated findings without manufacturing approval' {
        $script:Delivery | Should -Match 'require `isComplete: true`'
        $script:Delivery | Should -Match 'Do not build an approved set'
        $script:Delivery | Should -Match 'Audit skipped \(not approved\)'
        $command = [regex]::Match($script:Recipe, '(?s)```text(.*?)```').Groups[1].Value
        $command | Should -Match 'ss-ai workflow prepare-markdown'
        $command | Should -Match '--path "\{\{SESSION_STATE\}\}/findings\.json"'
        $command | Should -Not -Match '--finding-id|--context-path|approved-findings'
        $command | Should -Match '--destination tracker'
        $command | Should -Match '--combined-path "\{\{SESSION_STATE\}\}/self-review-findings\.md"'
    }

    It 'delivers exact filesystem paths and distinguishes empty approval from empty review' {
        $script:Delivery | Should -Match 'absolute Markdown filesystem paths'
        $script:Delivery | Should -Match 'each file exists and contains its rendered finding'
        $script:Delivery | Should -Match 'When there are no findings'
        $script:Delivery | Should -Match 'when all findings require audit'
        $script:Delivery | Should -Match 'without merge approval, publication, or learning'
    }

    It 'renders <Case> findings locally through the native self-review command' -ForEach @(
        @{ Case = 'all'; Empty = $false }
        @{ Case = 'no'; Empty = $true }
    ) {
        $directory = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' 雪')
        [void] [IO.Directory]::CreateDirectory($directory)
        $finding = @{
            schemaVersion = 1; id = 'F1'; origin = 'reviewer'; sourceFindingIds = @()
            title = 'Review finding 雪'; description = 'A problem'; reasoning = 'Reason'
            consequence = 'Impact'; recommendedAction = 'Fix'
            references = @(); impactedLocations = @(@{ path = 'src/example.rs' })
            anchor = @{ kind = 'file'; path = 'src/example.rs' }
        }
        $records = @()
        if (-not $Empty) {
            $second = $finding.Clone()
            $second.id = 'F2'
            $second.title = 'Second finding'
            $records = @($finding, $second)
        }
        $artifact = @{ schemaVersion = 1; kind = 'consolidated'; findings = @($records) }
        $artifactPath = Join-Path $directory 'findings.json'
        [IO.File]::WriteAllText($artifactPath, (ConvertTo-Json -InputObject $artifact -Depth 20 -Compress), [Text.UTF8Encoding]::new($false))
        $output = Join-Path $directory 'rendered'
        $combined = Join-Path $directory 'self-review-findings.md'

        $json = & $script:Native workflow prepare-markdown "--path=$artifactPath" `
            "--output-directory=$output" '--destination=tracker' "--combined-path=$combined"
        $result = ConvertFrom-Json -InputObject $json -AsHashtable
        $result.combinedPath | Should -BeExactly $combined
        [IO.File]::Exists($combined) | Should -BeTrue
        if ($Empty) {
            $result.findings.Count | Should -Be 0
        } else {
            $result.findings.Count | Should -Be 2
            @($result.findings.findingId) | Should -Contain 'F1'
            @($result.findings.findingId) | Should -Contain 'F2'
            foreach ($item in $result.findings) {
                [IO.Path]::IsPathFullyQualified($item.path) | Should -BeTrue
                [IO.File]::Exists($item.path) | Should -BeTrue
            }
            [IO.File]::ReadAllText($combined) | Should -Match 'Review finding 雪'
            [IO.File]::ReadAllText($combined) | Should -Match 'Second finding'
        }
    }
}
