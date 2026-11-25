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
        $skill | Should -Match 'Never emit any final assistant response while the latest transition has'
        $skill | Should -Match 'Never emit any final assistant response while the latest transition has `nextAction: "continue"`'
        $skill | Should -Match 'Saving state, writing SQL/checkpoint records, reading the next batch display, beginning the next batch, or reaching a round batch number does not remove this gate'
        $skill | Should -Match 'A normal user turn boundary, checkpoint write, progress message, or self-authored statement that work is lengthy is not a runtime interruption'
        $skill | Should -Match 'submit` is the normal transition'
        $skill | Should -Match 'read `nextBatch.displayPath`'
        $skill | Should -Match 'Report completion only from a `report` result'
        $skill | Should -Match 'An initial empty batch is still a transition to the report'
        $skill | Should -Match 'If `nextAction` is `report`, continue to stage 4'
        $skill | Should -Match 'compact-submission\.schema\.json'
        $skill | Should -Match 'direct AST-visible free-function'
        $skill | Should -Match 'assert_panics'
        $skill | Should -Match 'assert_eq_size'
        $skill | Should -Match 'fallibleValueCheck'
        $skill | Should -Match 'fallibleValueCheckCount'
        $skill | Should -Match 'one `search` command by repeating `--query`'
        $skill | Should -Match 'compact submission'
        $skill | Should -Match 'flat: each item is an assertion directly'
        $skill | Should -Match 'details\.json'
        $skill | Should -Match 'details\.md'
        $skill | Should -Match 'summary\.json'
        $skill | Should -Match 'summary\.md'
        $skill | Should -Match 'fixed order'
        $skill | Should -Match 'noCountedAssertionCount'
        $skill | Should -Match '\$LASTEXITCODE'
        $skill | Should -Match '@\(\.\.\.\)'
        $skill | Should -Match '\.Contains\(\)'
    }

    It 'keeps executable behavior in native command recipes' {
        $commands = Get-Content -LiteralPath (Join-Path $script:Skill 'commands.md') -Raw
        foreach ($command in @('initialize', 'search', 'next-batch', 'record-batch', 'advance', 'submit', 'report')) {
            $commands | Should -Match ([regex]::Escape("ss-ai test-audit $command"))
        }
        $commands | Should -Not -Match '(?im)^\s*pwsh\b[^\r\n]*\s-File\b'
        $commands | Should -Match 'Do not treat a successful record or `remaining: 0` alone as completion'
        $commands | Should -Match 'The command performs the authoritative exact-coverage validation'
        $commands | Should -Match 'compact-submission\.schema\.json'
        $commands | Should -Match 'evidenceHandle'
        $commands | Should -Match 'displayPath'
        $commands | Should -Match 'summaryJsonPath'
        $commands | Should -Match 'detailsJsonPath'
        $commands | Should -Match 'finalResponseAllowed` is always false'
        $commands | Should -Match 'nextAction: "complete"'
        $commands | Should -Match 'finalResponseAllowed: true'
        $commands | Should -Match 'sole final-response gate'
        $commands | Should -Match 'nextAction: "continue"'
        $commands | Should -Match 'nextAction: "report"'
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

    It 'ships a strict compact submission schema' {
        $schemaPath = Join-Path $script:Skill 'schemas\compact-submission.schema.json'
        $schema = Get-Content -LiteralPath $schemaPath -Raw | ConvertFrom-Json
        $schema.properties.schemaVersion.const | Should -Be 1
        $schema.properties.rows.maxItems | Should -Be 25
        @($schema.required) | Should -Contain 'batchFingerprint'
        $schema.properties.rows.items.oneOf.Count | Should -Be 2
        $reuse = $schema.properties.rows.items.oneOf |
            Where-Object { $_.required -contains 'reuseFrom' }
        @($reuse.required) | Should -Contain 'equivalenceConfirmed'
        @($reuse.required) | Should -Contain 'citationHandles'
        @($reuse.required) | Should -Contain 'callingTestIds'
    }

    It 'ships strict final report schemas' {
        foreach ($name in @('summary.schema.json', 'details.schema.json')) {
            $schemaPath = Join-Path $script:Skill "schemas/$name"
            $schema = Get-Content -LiteralPath $schemaPath -Raw | ConvertFrom-Json
            $schema.properties.schemaVersion.const | Should -Be 1
        }
        $summary = Get-Content -LiteralPath (Join-Path $script:Skill 'schemas\summary.schema.json') -Raw |
            ConvertFrom-Json
        @($summary.required) | Should -Contain 'scopePaths'
        @($summary.required) | Should -Contain 'sourceLanguages'
        @($summary.required) | Should -Contain 'modules'
        $details = Get-Content -LiteralPath (Join-Path $script:Skill 'schemas\details.schema.json') -Raw |
            ConvertFrom-Json
        @($details.required) | Should -Contain 'assertionTrace'
        @($details.required) | Should -Contain 'testInventory'
        @($details.required) | Should -Contain 'diagnostics'
    }
}
