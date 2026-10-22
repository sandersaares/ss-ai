#Requires -Version 7.6

BeforeAll {
    Set-StrictMode -Version Latest
    $ErrorActionPreference = 'Stop'
    $PSNativeCommandUseErrorActionPreference = $true
}

Describe 'Automatic reapproval repository scope instructions' {
    BeforeAll {
        $skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-autoreapprove'
        $script:Prompt = Get-Content -LiteralPath (Join-Path $skill 'SKILL.md') -Raw
        $script:Recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
    }

    It 'defaults to the invocation repository while honoring explicit invocation and schedule scope' {
        $script:Prompt | Should -Match 'An explicit repository in the invocation or user-authored schedule prompt\s+takes precedence'
        $script:Prompt | Should -Match "default to the Git repository containing the\s+invocation's working directory, including a linked worktree or a subdirectory\s+of a checkout"
        $script:Prompt | Should -Match 'Use the repository identity supplied by the session when\s+available'
        $script:Prompt | Should -Match "Do not use the skill's installation directory or a temporary\s+checkout"
        $script:Prompt | Should -Match 'Keep the resolved URL fixed throughout the run'
        $script:Prompt | Should -Not -Match 'Require an explicit HTTPS repository browser URL|Do not infer one from'
        $script:Recipes | Should -Match 'REPOSITORY.*resolved in skill stage 1 from the explicit repository or invocation checkout'
    }

    It 'resolves supported fetch remotes without requesting confirmation for a resolved repository' {
        $script:Prompt | Should -Match 'git -C "\{\{INVOCATION_DIRECTORY\}\}" remote -v'
        $script:Prompt | Should -Match 'Use the fetch URL of `origin`, or the sole remote if there is no `origin`'
        $script:Prompt | Should -Match 'remove GitHub''s clone\s+suffix `\.git`'
        $script:Prompt | Should -Match 'git@github\.com:OWNER/REPO\.git'
        $script:Prompt | Should -Match 'git@ssh\.dev\.azure\.com:v3/ORG/PROJECT/REPO'
        $script:Prompt | Should -Match 'corresponding `ssh://` forms'
        $script:Prompt | Should -Match 'https://ORG\.visualstudio\.com/PROJECT/_git/REPO'
        $script:Prompt | Should -Match 'Do not include credentials in the resolved URL'
        $script:Prompt | Should -Match 'Do not ask the user to supply or confirm the repository URL when it resolves\s+from the invocation''s Git repository'
    }

    It 'asks only for unresolved scope and never falls back from an invalid explicit repository' {
        $script:Prompt | Should -Match 'Ask for a repository URL only when no\s+supported repository can be resolved'
        $script:Prompt | Should -Match 'outside a Git checkout, after\s+a failed remote inspection, or with ambiguous remotes'
        $script:Prompt | Should -Match 'Do not silently replace\s+an invalid explicit repository with the current checkout'
        $script:Prompt | Should -Match 'schedule running in a Git repository can simply invoke\s+`ss-autoreapprove`'
        $script:Prompt | Should -Match 'Report the resolved repository and whether it was explicit or defaulted from\s+the invocation checkout'
    }
}

Describe 'Automatic reapproval provider fixtures' {
    It 'validates the GitHub contract projection and its explicit provenance' {
        $fixture = Join-Path $PSScriptRoot '..\packages\ss-skills-util\tests\fixtures\github-approval.json'
        $schema = Join-Path $PSScriptRoot '..\dev\ss-autoreapprove\github-approval-fixture.schema.json'
        Test-Json -LiteralPath $fixture -SchemaFile $schema | Should -BeTrue
    }

    It 'installs one canonical policy and native-only lifecycle recipes' {
        $root = Split-Path -Parent $PSScriptRoot
        $skill = Join-Path $root 'skills\ss-autoreapprove'
        Test-Path -LiteralPath (Join-Path $skill 'SKILL.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $skill 'significance.md') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $root 'dev\ss-autoreapprove\significance.md') | Should -BeFalse
        $recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
        $help = & ss-skills-util autoreapprove --help | Out-String
        $LASTEXITCODE | Should -Be 0
        foreach ($command in @('discover', 'results', 'inspect', 'prepare', 'assessment', 'read', 'judge', 'complete', 'publish')) {
            $recipes | Should -Match "(?m)^ss-skills-util autoreapprove $command "
            $help | Should -Match "(?m)^\s+$command\s"
        }
        $recipes | Should -Not -Match 'pwsh .* -File|gh api|az devops|ConvertTo-Json'
    }

    It 'routes ADO publication and recovery through the guarded native transaction' {
        $skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-autoreapprove'
        $prompt = Get-Content -LiteralPath (Join-Path $skill 'SKILL.md') -Raw
        $recipes = Get-Content -LiteralPath (Join-Path $skill 'commands.md') -Raw
        $prompt | Should -Not -Match 'ADO publication.*not supported|ADO inspection and assessment are read-only'
        $recipes | Should -Not -Match 'ADO supports read-only|unsupported publication transaction'
        $recipes | Should -Match '(?m)^ss-skills-util autoreapprove publish .*--tenant "\{\{TENANT\}\}"'
        $recipes | Should -Match 'unique new structured vote on the assessed iteration'
        $prompt | Should -Match 'never issue a direct reviewer vote'
        $prompt | Should -Match 'never repeats an uncertain vote or audit'
        $help = & ss-skills-util autoreapprove publish --help | Out-String
        $LASTEXITCODE | Should -Be 0
        $help | Should -Match '\-\-tenant'
    }
}
