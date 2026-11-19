#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $root = Split-Path -Parent $PSScriptRoot
    $script:ProactorSkill = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\SKILL.md')
    )
    $script:ProactorDesign = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-review-proactor\docs\design.md')
    )
    $script:AutoreapproveSkill = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-autoreapprove\SKILL.md')
    )
    $script:AutoreapproveCommands = [IO.File]::ReadAllText(
        (Join-Path $root 'skills\ss-autoreapprove\commands.md')
    )
}

Describe 'Multi-repository skill contracts' {
    It 'accepts an ordered explicit repository list in both skills' {
        foreach ($content in @($script:ProactorSkill, $script:AutoreapproveSkill)) {
            $content | Should -Match 'One or more explicit repositories'
            $content | Should -Match 'preserve the\s+user''s\s+(?:repository\s+)?order'
            $content | Should -Match 'Resolve (?:all of them|every explicitly\s+supplied repository) before (?:remote work|starting selection)'
        }
    }

    It 'keeps proactor reviews in one repository until it is exhausted' {
        $script:ProactorSkill |
            Should -Match 'continues reviewing the current repository until selection reports no candidate'
        $script:ProactorDesign |
            Should -Match 'A completed review returns to the same active\s+repository\.'
        $script:ProactorSkill |
            Should -Match 'On `noCandidate`, switch to the next repository'
        $script:ProactorSkill |
            Should -Match 'Do not cycle back to an earlier repository during the same invocation'
    }

    It 'isolates sequential autoreapproval runs by repository' {
        $script:AutoreapproveSkill |
            Should -Match 'For each repository, create a fresh absolute run path'
        $script:AutoreapproveSkill |
            Should -Match 'Do not overlap repository runs'
        $script:AutoreapproveCommands |
            Should -Match 'The native commands accept one repository per run'
        $script:AutoreapproveCommands |
            Should -Match 'Use a separate path for every repository'
    }

    It 'aggregates autoreapproval results across repository runs' {
        $script:AutoreapproveSkill |
            Should -Match 'from every repository'
        $script:AutoreapproveSkill |
            Should -Match 'Repository`, `PR`, `Decision`, and `Required detail'
    }
}
