#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Root = Split-Path -Parent $PSScriptRoot
    $script:Skill = Join-Path $script:Root 'skills\ss-test-audit'
}

Describe 'ss-test-audit skill contract' {
    It 'documents the complete native workflow and named rate semantics' {
        $skill = Get-Content -LiteralPath (Join-Path $script:Skill 'SKILL.md') -Raw
        $skill | Should -Match 'ss-ai test-audit initialize'
        $skill | Should -Match 'next-batch'
        $skill | Should -Match 'record-batch'
        $skill | Should -Match 'No explicitly documented promise = implicit-only \+ incidental'
        $skill | Should -Match 'No public-contract check = incidental only'
        $skill | Should -Match 'Classified C of T assertions'
        $skill | Should -Match 'not a completion, checkpoint, pause, handoff, request for input, or permission to end the turn'
        $skill | Should -Match 'Never stop after a progress message'
        $skill | Should -Match 'If `R` is greater than zero, invoke `next-batch`'
        $skill | Should -Match 'If `R` is zero, continue to stage 4 and run the final `report`'
        $skill | Should -Match 'classification-batch\.schema\.json'
        $skill | Should -Match 'direct AST-visible free-function'
        $skill | Should -Match 'assert_panics'
    }

    It 'keeps executable behavior in native command recipes' {
        $commands = Get-Content -LiteralPath (Join-Path $script:Skill 'commands.md') -Raw
        foreach ($command in @('initialize', 'search', 'next-batch', 'record-batch', 'report')) {
            $commands | Should -Match ([regex]::Escape("ss-ai test-audit $command"))
        }
        $commands | Should -Not -Match '(?im)^\s*pwsh\b[^\r\n]*\s-File\b'
        $commands | Should -Match 'Do not treat a successful record or `remaining: 0` alone as completion'
        $commands | Should -Match 'The command performs the authoritative exact-coverage validation'
        Test-Path -LiteralPath (Join-Path $script:Skill 'scripts') | Should -BeFalse
    }

    It 'ships a strict classification batch schema' {
        $schemaPath = Join-Path $script:Skill 'schemas\classification-batch.schema.json'
        $schema = Get-Content -LiteralPath $schemaPath -Raw | ConvertFrom-Json
        $schema.properties.schemaVersion.const | Should -Be 1
        $schema.properties.assertions.maxItems | Should -Be 25
        @($schema.properties.assertions.items.properties.classification.enum) |
            Should -Be @('Documented', 'Implicit', 'Mixed', 'Incidental')
    }
}
