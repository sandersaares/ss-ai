BeforeAll {
    $script:RepoRoot = Split-Path -Parent $PSScriptRoot
    $script:Scripts = Join-Path $script:RepoRoot 'scripts'

    # Skills ship their own executable scripts and libraries and follow the same layout, so they are
    # held to the same conventions as the repository-level scripts directory.
    $script:SkillScriptRoots = @(
        Get-ChildItem -LiteralPath (Join-Path $script:RepoRoot 'skills') -Directory |
            ForEach-Object { Join-Path $_.FullName 'scripts' } |
            Where-Object { Test-Path -LiteralPath $_ -PathType Container }
    )
    # Development tools use skill-specific subfolders rather than executable skill installations.
    # Include each executable directory so relocation cannot bypass the script conventions.
    $developmentRoot = Join-Path $script:RepoRoot 'dev'
    $developmentScriptRoots = @($developmentRoot) + @(
        Get-ChildItem -LiteralPath $developmentRoot -Recurse -Filter '*.ps1' -File |
            Where-Object { $_.Directory.Name -ne 'lib' } |
            Select-Object -ExpandProperty DirectoryName -Unique
    )
    # Root Install.ps1 is an executable too; its sibling documentation is not a library.
    $script:ScriptRoots = @($script:RepoRoot, $script:Scripts) + $script:SkillScriptRoots + $developmentScriptRoots

    # Reports the parameters an executable declares a fixed position for. PowerShell honours an
    # explicit position even under PositionalBinding = $false, so the declaration has to be read
    # rather than inferred from the CmdletBinding attribute. The parameter block is taken from the
    # parsed script rather than matched as text, because an attribute may be spelled across lines
    # and in any argument order.
    function Get-PositionalParameterName {
        param(
            [Parameter(Mandatory)]
            [string] $Path
        )

        $ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref] $null, [ref] $null)
        if ($null -eq $ast.ParamBlock) { return @() }

        foreach ($parameter in $ast.ParamBlock.Parameters) {
            foreach ($attribute in $parameter.Attributes) {
                if ($attribute -isnot [System.Management.Automation.Language.AttributeAst]) { continue }
                if ($attribute.TypeName.GetReflectionType() -ne [Parameter]) { continue }
                foreach ($argument in $attribute.NamedArguments) {
                    if ($argument.ArgumentName -eq 'Position') {
                        $parameter.Name.VariablePath.UserPath
                    }
                }
            }
        }
    }
}

Describe 'PowerShell library layout' {
    It 'keeps shared selection and registration outside the skill directories' {
        $skills = Join-Path $script:RepoRoot 'skills'
        Test-Path (Join-Path $skills 'ss-review-proactor\scripts\Select-ReviewPullRequest.ps1') | Should -BeFalse
        Test-Path (Join-Path $skills 'ss-review\scripts\Register-PullRequestReviewer.ps1') | Should -BeFalse
        foreach ($recipe in @(
            'ss-review-proactor\tool-calls\select-pull-request.md'
            'ss-review\tool-calls\register-reviewer.md'
        )) {
            $content = Get-Content -LiteralPath (Join-Path $skills $recipe) -Raw
            $content | Should -Match '(?m)^ss-ai '
            $content | Should -Not -Match 'pwsh .* -File'
        }
        $proactorScripts = Join-Path $skills 'ss-review-proactor\scripts'
        if (Test-Path $proactorScripts) {
            @(Get-ChildItem $proactorScripts -Recurse -File -Filter '*.ps1') | Should -BeNullOrEmpty
        }
    }

    It 'has every dot-sourced library referenced by executable scripts' {
        foreach ($scriptRoot in $script:ScriptRoots) {
            foreach ($scriptFile in (Get-ChildItem -LiteralPath $scriptRoot -Filter '*.ps1' -File)) {
                $content = Get-Content -LiteralPath $scriptFile.FullName -Raw
                foreach ($match in [regex]::Matches($content, '\.\s+"\$PSScriptRoot\\(lib\\[^"]+\.ps1)"')) {
                    Test-Path -LiteralPath (Join-Path $scriptRoot $match.Groups[1].Value) | Should -BeTrue
                }
            }
        }
    }

    It 'assigns no parameter position automatically' {
        # A bare CmdletBinding gives every parameter a position in declaration order, so a
        # multi-value parameter spelled as separate arguments binds its second value to whichever
        # parameter is declared next, producing a partial result and reporting success.
        foreach ($scriptRoot in $script:ScriptRoots) {
            foreach ($scriptFile in (Get-ChildItem -LiteralPath $scriptRoot -Filter '*.ps1' -File)) {
                $content = Get-Content -LiteralPath $scriptFile.FullName -Raw
                if ($content -notmatch '(?m)^\[CmdletBinding') { continue }
                $content | Should -Match 'PositionalBinding\s*=\s*\$false' -Because "$($scriptFile.Name) must not assign parameter positions automatically"
            }
        }
    }

    It 'declares entry point versions at least as new as their directly loaded libraries' {
        Set-StrictMode -Version Latest
        foreach ($scriptRoot in $script:ScriptRoots) {
            foreach ($scriptFile in (Get-ChildItem -LiteralPath $scriptRoot -Filter '*.ps1' -File)) {
                $ast = [System.Management.Automation.Language.Parser]::ParseFile($scriptFile.FullName, [ref] $null, [ref] $null)
                $content = Get-Content -LiteralPath $scriptFile.FullName -Raw
                foreach ($match in [regex]::Matches($content, '\.\s+"\$PSScriptRoot\\(lib\\[^"]+\.ps1)"')) {
                    $library = Join-Path $scriptRoot $match.Groups[1].Value
                    $libraryAst = [System.Management.Automation.Language.Parser]::ParseFile($library, [ref] $null, [ref] $null)
                    if ($null -eq $libraryAst.ScriptRequirements) { continue }
                    $libraryVersion = $libraryAst.ScriptRequirements.RequiredPSVersion
                    if ($null -eq $libraryVersion) { continue }
                    $ast.ScriptRequirements | Should -Not -BeNullOrEmpty `
                        -Because "$($scriptFile.Name) must declare the requirements of $($match.Groups[1].Value)"
                    $ast.ScriptRequirements.RequiredPSVersion | Should -BeGreaterOrEqual $libraryVersion `
                        -Because "$($scriptFile.Name) loads $($match.Groups[1].Value)"
                }
            }
        }
    }

    It 'declares no parameter position in a skill executable' {
        # Skill executables are invoked only by an agent following written instructions, never by a
        # person at a prompt, so an unnamed argument is always a mistake and never a convenience.
        # Accepting one lets a misspelled invocation bind a value to the wrong parameter and succeed.
        # Repository-level scripts are run by hand and may declare a position deliberately, which is
        # why this holds the skills to a stricter rule than the repository.
        foreach ($scriptRoot in $script:SkillScriptRoots) {
            foreach ($scriptFile in (Get-ChildItem -LiteralPath $scriptRoot -Filter '*.ps1' -File)) {
                $positional = @(Get-PositionalParameterName -Path $scriptFile.FullName)
                $positional | Should -BeNullOrEmpty -Because "$($scriptFile.Name) must accept every argument by name"
            }
        }
    }

    It 'loads each library independently under strict mode' {
        foreach ($scriptRoot in $script:ScriptRoots) {
            $libraryRoot = Join-Path $scriptRoot 'lib'
            if (-not (Test-Path -LiteralPath $libraryRoot -PathType Container)) { continue }
            foreach ($library in (Get-ChildItem -LiteralPath $libraryRoot -Filter '*.ps1' -File)) {
                $escaped = $library.FullName.Replace("'", "''")
                & pwsh -NoProfile -Command "Set-StrictMode -Version Latest; . '$escaped'"
                $LASTEXITCODE | Should -Be 0 -Because $library.Name
            }
        }
    }

    It 'contains no common-library references or nested library dot-sourcing' {
        foreach ($scriptRoot in $script:ScriptRoots) {
            $allScripts = Get-ChildItem -LiteralPath $scriptRoot -Recurse:($scriptRoot -ne $script:RepoRoot) -Filter '*.ps1' -File
            ($allScripts | Select-String -SimpleMatch '_Common.ps1') | Should -BeNullOrEmpty
            foreach ($library in ($allScripts | Where-Object DirectoryName -EQ (Join-Path $scriptRoot 'lib'))) {
                (Get-Content -LiteralPath $library.FullName -Raw) | Should -Not -Match '(?m)^\s*\.\s+'
            }
        }
    }

    # The assertions above enumerate lib/, so moving a library out of it would make them pass
    # vacuously. This one fixes the convention from the opposite direction: whatever an executable
    # script dot-sources must live under lib/.
    It 'keeps every dot-sourced library under lib' {
        foreach ($scriptRoot in $script:ScriptRoots) {
            foreach ($scriptFile in (Get-ChildItem -LiteralPath $scriptRoot -Recurse:($scriptRoot -ne $script:RepoRoot) -Filter '*.ps1' -File)) {
                $content = Get-Content -LiteralPath $scriptFile.FullName -Raw
                foreach ($match in [regex]::Matches($content, '(?m)^\s*\.\s+(?<target>\S[^\r\n]*?\.ps1["'']?)(\s+[^\r\n]*)?$')) {
                    $target = $match.Groups['target'].Value
                    $target | Should -Match '(^|[\\/])lib[\\/]' -Because "$($scriptFile.Name) dot-sources $target"
                }
            }
        }
    }
}
