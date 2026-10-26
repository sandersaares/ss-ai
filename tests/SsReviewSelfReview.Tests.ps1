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
        $script:Recipe | Should -Match '\-Path "\{\{SESSION_STATE\}\}/findings.json"'
        $command = [regex]::Match($script:Recipe, '(?s)```powershell(.*?)```').Groups[1].Value
        $command | Should -Not -Match '\-FindingId|\-ContextPath|approved-findings'
        $command | Should -Match '\-NonInteractive'
        $command | Should -Match '\-CombinedPath'
    }

    It 'delivers exact filesystem paths and distinguishes empty approval from empty review' {
        $script:Delivery | Should -Match 'absolute Markdown filesystem paths'
        $script:Delivery | Should -Match 'each file exists and contains its rendered finding'
        $script:Delivery | Should -Match 'When there are no findings'
        $script:Delivery | Should -Match 'when all findings require audit'
        $script:Delivery | Should -Match 'without merge approval, publication, or learning'
    }
}
