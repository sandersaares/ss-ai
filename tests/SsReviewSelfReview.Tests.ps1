#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
}

Describe 'Self-review workflow contract' {
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
