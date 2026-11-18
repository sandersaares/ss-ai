#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

Describe 'Automatic reapproval provider contracts' {
    It 'validates the GitHub contract projection and its explicit provenance' {
        $fixture = Join-Path $PSScriptRoot '..\packages\ss-ai-review\tests\fixtures\github-approval.json'
        $schema = Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\github-approval-fixture.schema.json'
        Test-Json -LiteralPath $fixture -SchemaFile $schema | Should -BeTrue
    }

    It 'installs one canonical policy' {
        $root = Split-Path -Parent $PSScriptRoot
        $skill = Join-Path $root 'skills\ss-autoreapprove'
        Test-Path -LiteralPath (Join-Path $skill 'SKILL.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $skill 'significance.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $root 'dev\ss-autoreapprove\significance.md') | Should -BeFalse
    }

    It 'exposes the native lifecycle command surface' {
        $help = & ss-ai autoreapprove --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($command in @(
            'discover', 'results', 'inspect', 'refresh', 'prepare', 'start-worker', 'assessment',
            'read-next', 'read', 'read-batch', 'source-context', 'units', 'checkpoint', 'conclude',
            'synthesize', 'complete-partial', 'publish', 'notify'
        )) {
            $help | Should -Match "(?m)^\s+$command\s"
        }
    }

    It 'exposes bounded source reads and tenant-scoped publication through the CLI' {
        $sourceHelp = & ss-ai autoreapprove source-context --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($argument in @('change-id', 'side', 'start-line', 'line-count', 'limit', 'snapshot-digest')) {
            $sourceHelp | Should -Match "\-\-$argument"
        }

        $publishHelp = & ss-ai autoreapprove publish --help | Out-String
        $LASTEXITCODE | Should -Be 0
        $publishHelp | Should -Match '\-\-tenant'
    }
}
