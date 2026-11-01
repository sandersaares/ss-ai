#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
}

Describe 'Report presentation instructions' {
    It 'uses the native renderer with a title file and preserves the Markdown fallback' {
        $instructions = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'pwsh-calls/render-report-html.md') -Raw
        $instructions | Should -Match 'ss-ai workflow render-review-html'
        $instructions | Should -Match '--title-path'
        $instructions | Should -Match 'title file in session state'
        $instructions | Should -Match 'Markdown fallback'
        $instructions | Should -Match 'Never present a pre-existing HTML file as current after a failed render'
    }

    It 'treats partial overview publication as non-blocking and forbids blind replay' {
        $instructions = Get-Content -LiteralPath (Join-Path $script:SkillRoot 'pwsh-calls/publish-content-overview.md') -Raw
        $instructions | Should -Match 'ss-ai review publish-overview'
        $instructions | Should -Match 'continue the review'
        $instructions | Should -Match 'Never blindly repeat a write'
        $instructions | Should -Match 'needsReconciliation'
        $instructions | Should -Match 'Resuming a successful run reuses its existing receipt'
    }
}
