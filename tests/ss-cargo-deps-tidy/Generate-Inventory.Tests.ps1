#Requires -Version 7
#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.5.0' }

Set-StrictMode -Version Latest

BeforeAll {
    $script:ScriptPath = (Resolve-Path (Join-Path $PSScriptRoot '..\..\skills\ss-cargo-deps-tidy\Generate-Inventory.ps1')).ProviderPath
    $script:FixturesRoot = Join-Path $PSScriptRoot 'fixtures'
    . $script:ScriptPath
}

Describe 'Remove-TomlLineComment' {
    It 'returns empty for blank line' {
        Remove-TomlLineComment -Line '' | Should -Be ''
    }
    It 'strips a pure comment line' {
        Remove-TomlLineComment -Line '# only a comment' | Should -Be ''
    }
    It 'strips a trailing comment after a value' {
        Remove-TomlLineComment -Line 'name = "1.0" # version pin' | Should -Be 'name = "1.0"'
    }
    It 'preserves # inside a double-quoted string' {
        Remove-TomlLineComment -Line 'name = "hash#inside"' | Should -Be 'name = "hash#inside"'
    }
    It 'preserves # inside a single-quoted string' {
        Remove-TomlLineComment -Line "name = 'hash#inside'" | Should -Be "name = 'hash#inside'"
    }
    It 'handles escaped quote then # outside string' {
        Remove-TomlLineComment -Line 'name = "a\"b" # tail' | Should -Be 'name = "a\"b"'
    }
    It 'trims trailing whitespace even without a comment' {
        Remove-TomlLineComment -Line 'name = "1.0"   ' | Should -Be 'name = "1.0"'
    }
}

Describe 'Remove-Quotes' {
    It 'returns unquoted text unchanged' {
        Remove-Quotes -Text 'foo' | Should -Be 'foo'
    }
    It 'strips double quotes' {
        Remove-Quotes -Text '"foo"' | Should -Be 'foo'
    }
    It 'strips single quotes' {
        Remove-Quotes -Text "'foo'" | Should -Be 'foo'
    }
    It 'leaves mismatched quotes alone' {
        Remove-Quotes -Text '"foo' | Should -Be '"foo'
    }
    It 'unwraps empty double quotes to empty string' {
        Remove-Quotes -Text '""' | Should -Be ''
    }
}

Describe 'Split-TomlKeyPath' {
    It 'returns a single token when there are no dots' {
        $r = Split-TomlKeyPath -Header 'workspace'
        $r | Should -HaveCount 1
        $r[0] | Should -Be 'workspace'
    }
    It 'splits on dots between bare tokens' {
        $r = Split-TomlKeyPath -Header 'workspace.dependencies'
        $r | Should -HaveCount 2
        $r[0] | Should -Be 'workspace'
        $r[1] | Should -Be 'dependencies'
    }
    It 'preserves single-quoted segments verbatim' {
        $r = Split-TomlKeyPath -Header "target.'cfg(unix)'.dependencies"
        $r | Should -HaveCount 3
        $r[0] | Should -Be 'target'
        $r[1] | Should -Be 'cfg(unix)'
        $r[2] | Should -Be 'dependencies'
    }
    It 'preserves dots inside double-quoted segments' {
        $r = Split-TomlKeyPath -Header 'target.''cfg(target_os = "linux")''.dependencies'
        $r | Should -HaveCount 3
        $r[0] | Should -Be 'target'
        $r[1] | Should -Be 'cfg(target_os = "linux")'
        $r[2] | Should -Be 'dependencies'
    }
}

Describe 'Get-SectionInfo' {
    It 'classifies [dependencies]' {
        $s = Get-SectionInfo -HeaderText 'dependencies'
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'normal'
        $s.Target | Should -BeNullOrEmpty
        $s.SubName | Should -BeNullOrEmpty
    }
    It 'classifies [dev-dependencies]' {
        $s = Get-SectionInfo -HeaderText 'dev-dependencies'
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'dev'
    }
    It 'classifies [workspace.dependencies]' {
        $s = Get-SectionInfo -HeaderText 'workspace.dependencies'
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'workspace'
    }
    It 'classifies a sub-table [dependencies.foo]' {
        $s = Get-SectionInfo -HeaderText 'dependencies.foo'
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'normal'
        $s.SubName | Should -Be 'foo'
    }
    It 'classifies a sub-table [workspace.dependencies.foo]' {
        $s = Get-SectionInfo -HeaderText 'workspace.dependencies.foo'
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'workspace'
        $s.SubName | Should -Be 'foo'
    }
    It 'classifies target-conditional dependencies' {
        $s = Get-SectionInfo -HeaderText "target.'cfg(windows)'.dependencies"
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'normal'
        $s.Target | Should -Be 'cfg(windows)'
    }
    It 'classifies target-conditional dev sub-table' {
        $s = Get-SectionInfo -HeaderText "target.'cfg(unix)'.dev-dependencies.foo"
        $s.Kind | Should -Be 'deps'
        $s.Variant | Should -Be 'dev'
        $s.Target | Should -Be 'cfg(unix)'
        $s.SubName | Should -Be 'foo'
    }
    It 'classifies [package]' {
        (Get-SectionInfo -HeaderText 'package').Kind | Should -Be 'package'
    }
    It 'classifies [workspace]' {
        (Get-SectionInfo -HeaderText 'workspace').Kind | Should -Be 'workspace'
    }
    It 'classifies [workspace.lints.rust] as other' {
        (Get-SectionInfo -HeaderText 'workspace.lints.rust').Kind | Should -Be 'other'
    }
    It 'classifies [profile.dist] as other' {
        (Get-SectionInfo -HeaderText 'profile.dist').Kind | Should -Be 'other'
    }
}

Describe 'Split-TopLevelEntries' {
    It 'returns empty array for empty input' {
        $r = Split-TopLevelEntries -Text ''
        $r | Should -HaveCount 0
    }
    It 'splits a flat comma list' {
        $r = Split-TopLevelEntries -Text 'a, b, c'
        $r | Should -HaveCount 3
    }
    It 'respects nested array depth' {
        $r = Split-TopLevelEntries -Text 'a, [b, c], d'
        $r | Should -HaveCount 3
        $r[1].Trim() | Should -Be '[b, c]'
    }
    It 'respects nested inline-table depth' {
        $r = Split-TopLevelEntries -Text 'a, { x = "1,2" }, c'
        $r | Should -HaveCount 3
    }
    It 'respects strings containing commas' {
        $r = Split-TopLevelEntries -Text '"a, b", "c, d"'
        $r | Should -HaveCount 2
    }
}

Describe 'ConvertFrom-TomlValue' {
    It 'returns null for empty text' {
        ConvertFrom-TomlValue -Text '' | Should -BeNullOrEmpty
    }
    It 'parses a basic string' {
        ConvertFrom-TomlValue -Text '"foo"' | Should -Be 'foo'
    }
    It 'parses a literal string' {
        ConvertFrom-TomlValue -Text "'foo'" | Should -Be 'foo'
    }
    It 'parses true' {
        ConvertFrom-TomlValue -Text 'true' | Should -BeTrue
    }
    It 'parses false' {
        ConvertFrom-TomlValue -Text 'false' | Should -BeFalse
    }
    It 'parses an empty array' {
        $r = ConvertFrom-TomlValue -Text '[]'
        $r | Should -HaveCount 0
    }
    It 'parses an array of strings' {
        $r = ConvertFrom-TomlValue -Text '["a", "b", "c"]'
        $r | Should -HaveCount 3
        $r[0] | Should -Be 'a'
        $r[2] | Should -Be 'c'
    }
    It 'parses a multi-line array' {
        $r = ConvertFrom-TomlValue -Text "[`n  `"a`",`n  `"b`",`n]"
        $r | Should -HaveCount 2
        $r[0] | Should -Be 'a'
        $r[1] | Should -Be 'b'
    }
    It 'unescapes embedded quotes' {
        ConvertFrom-TomlValue -Text '"a\"b"' | Should -Be 'a"b'
    }
}

Describe 'ConvertFrom-InlineTable' {
    It 'parses a single-attr inline table' {
        $r = ConvertFrom-InlineTable -Text '{ version = "1.0" }'
        $r['version'] | Should -Be '1.0'
    }
    It 'parses multiple attrs' {
        $r = ConvertFrom-InlineTable -Text '{ version = "1.0", default-features = false }'
        $r['version'] | Should -Be '1.0'
        $r['default-features'] | Should -BeFalse
    }
    It 'parses a features array' {
        $r = ConvertFrom-InlineTable -Text '{ features = ["a", "b"] }'
        $r['features'] | Should -HaveCount 2
        $r['features'][0] | Should -Be 'a'
    }
    It 'parses an empty inline table' {
        $r = ConvertFrom-InlineTable -Text '{ }'
        $r.Count | Should -Be 0
    }
    It 'accepts a quoted attribute name' {
        $r = ConvertFrom-InlineTable -Text '{ "default-features" = false }'
        $r['default-features'] | Should -BeFalse
    }
}

Describe 'Read-ValueSpan' {
    It 'reads a single-line bare-string value' {
        $lines = @('x = "1.0"')
        $r = Read-ValueSpan -CleanLines $lines -StartIndex 0 -StartCol 4
        $r.Text | Should -Be '"1.0"'
        $r.EndIndex | Should -Be 0
    }
    It 'reads a single-line inline table' {
        $lines = @('x = { version = "1.0" }')
        $r = Read-ValueSpan -CleanLines $lines -StartIndex 0 -StartCol 4
        $r.Text | Should -Be '{ version = "1.0" }'
        $r.EndIndex | Should -Be 0
    }
    It 'reads a multi-line inline table' {
        $lines = @(
            'x = {',
            '  version = "1.0",',
            '}',
            'y = 5'
        )
        $r = Read-ValueSpan -CleanLines $lines -StartIndex 0 -StartCol 4
        $r.EndIndex | Should -Be 2
        $r.Text | Should -Match '^\{'
        $r.Text | Should -Match '\}$'
    }
    It 'reads a multi-line array' {
        $lines = @(
            'x = [',
            '  "a",',
            '  "b",',
            ']'
        )
        $r = Read-ValueSpan -CleanLines $lines -StartIndex 0 -StartCol 4
        $r.EndIndex | Should -Be 3
        $r.Text | Should -Match '^\['
        $r.Text | Should -Match '\]$'
    }
    It 'does not terminate on a closing brace inside a string' {
        $lines = @(
            'x = { name = "} not the end", version = "1.0" }'
        )
        $r = Read-ValueSpan -CleanLines $lines -StartIndex 0 -StartCol 4
        $r.EndIndex | Should -Be 0
        $r.Text | Should -Match 'version = "1.0"'
    }
}

Describe 'ConvertTo-WorkspacePath' {
    It 'produces a forward-slashed relative path' {
        $root = $TestDrive
        New-Item -ItemType Directory -Path (Join-Path $root 'a\b') -Force | Out-Null
        $file = Join-Path $root 'a\b\Cargo.toml'
        Set-Content -LiteralPath $file -Value '# placeholder'
        ConvertTo-WorkspacePath -AbsolutePath $file -WorkspaceRoot $root | Should -Be 'a/b/Cargo.toml'
    }
    It 'normalizes the workspace-root file itself' {
        $root = $TestDrive
        $file = Join-Path $root 'Cargo.toml'
        Set-Content -LiteralPath $file -Value '# placeholder'
        ConvertTo-WorkspacePath -AbsolutePath $file -WorkspaceRoot $root | Should -Be 'Cargo.toml'
    }
}

Describe 'Get-PathKey' {
    BeforeAll {
        $script:PkRoot = Join-Path $TestDrive 'PathKey'
        $script:PkLeaf = Join-Path $script:PkRoot 'FooBar'
        New-Item -ItemType Directory -Path $script:PkLeaf -Force | Out-Null
    }
    It 'strips trailing separators' {
        $sep = [System.IO.Path]::DirectorySeparatorChar
        $a = Get-PathKey -Path ($script:PkLeaf + $sep)
        $b = Get-PathKey -Path $script:PkLeaf
        $a | Should -Be $b
    }
    It 'folds case on Windows and macOS, preserves case on Linux' {
        $upper = Get-PathKey -Path (Join-Path $script:PkRoot 'FooBar')
        $lower = Get-PathKey -Path (Join-Path $script:PkRoot 'foobar')
        if ($IsWindows -or $IsMacOS) {
            $upper | Should -BeExactly $lower
        } else {
            $upper | Should -Not -BeExactly $lower
        }
    }
}

Describe 'Expand-MemberGlob' {
    BeforeAll {
        $script:GlobRoot = Join-Path $TestDrive 'glob-test'
        New-Item -ItemType Directory -Path (Join-Path $script:GlobRoot 'alpha') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:GlobRoot 'beta') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:GlobRoot 'gamma') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:GlobRoot 'nested\deep1') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:GlobRoot 'nested\deep2') -Force | Out-Null
    }
    It 'resolves a literal segment' {
        $r = Expand-MemberGlob -Root $script:GlobRoot -Pattern 'alpha'
        $r | Should -HaveCount 1
        (Get-PathKey -Path $r[0]) | Should -Be (Get-PathKey -Path (Join-Path $script:GlobRoot 'alpha'))
    }
    It 'expands a star to all direct children' {
        $r = Expand-MemberGlob -Root $script:GlobRoot -Pattern '*'
        $r | Should -HaveCount 4
    }
    It 'expands a star to a sub-directory pattern' {
        $r = Expand-MemberGlob -Root $script:GlobRoot -Pattern 'nested/*'
        $r | Should -HaveCount 2
    }
    It 'expands a recursive ** glob' {
        $r = Expand-MemberGlob -Root $script:GlobRoot -Pattern '**/deep1'
        $r | Should -HaveCount 1
    }
    It 'returns no matches for a missing literal' {
        $r = Expand-MemberGlob -Root $script:GlobRoot -Pattern 'does-not-exist'
        $r | Should -HaveCount 0
    }
}

Describe 'Read-CargoManifest' {
    BeforeEach {
        $script:ManifestPath = Join-Path $TestDrive 'Cargo.toml'
        $script:WorkspaceRoot = $TestDrive
    }

    It 'returns empty results for an empty manifest' {
        Set-Content -LiteralPath $script:ManifestPath -Value '' -NoNewline
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Package | Should -BeNullOrEmpty
        $r.HasWorkspaceTable | Should -BeFalse
        $r.HasPackageTable | Should -BeFalse
        $r.Dependencies.Count | Should -Be 0
    }

    It 'captures the package name from [package]' {
        Set-Content -LiteralPath $script:ManifestPath -Value "[package]`nname = `"my-crate`"`n"
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Package | Should -Be 'my-crate'
        $r.HasPackageTable | Should -BeTrue
    }

    It 'captures workspace members and exclude' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[workspace]
members = ["packages/*", "crates/foo"]
exclude = ["legacy"]
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.HasWorkspaceTable | Should -BeTrue
        $r.Members | Should -HaveCount 2
        $r.Members[0] | Should -Be 'packages/*'
        $r.Members[1] | Should -Be 'crates/foo'
        $r.Exclude | Should -HaveCount 1
        $r.Exclude[0] | Should -Be 'legacy'
    }

    It 'captures an inline-table workspace dependency' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[workspace.dependencies]
serde = { version = "1.0", default-features = false, features = ["derive"] }
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Name | Should -Be 'serde'
        $d.Style | Should -Be 'inline-table'
        $d.Variant | Should -Be 'workspace'
        $d.Attrs['version'] | Should -Be '1.0'
        $d.Attrs['default-features'] | Should -BeFalse
        $d.Attrs['features'] | Should -HaveCount 1
        $d.Attrs['features'][0] | Should -Be 'derive'
    }

    It 'captures a bare-string dependency' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies]
anyhow = "1.0.86"
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Name | Should -Be 'anyhow'
        $d.Style | Should -Be 'bare-string'
        $d.Variant | Should -Be 'normal'
        $d.Attrs['version'] | Should -Be '1.0.86'
    }

    It 'merges consecutive dotted-key lines for the same dep' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dev-dependencies]
windows.workspace = true
windows.features = ["Win32_Foundation"]
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Name | Should -Be 'windows'
        $d.Style | Should -Be 'dotted-key'
        $d.Variant | Should -Be 'dev'
        $d.Attrs['workspace'] | Should -BeTrue
        $d.Attrs['features'] | Should -HaveCount 1
        $d.Attrs['features'][0] | Should -Be 'Win32_Foundation'
    }

    It 'reads a sub-table dependency' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies.serde]
version = "1.0.209"
features = ["derive"]
default-features = false
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Name | Should -Be 'serde'
        $d.Style | Should -Be 'sub-table'
        $d.Attrs['version'] | Should -Be '1.0.209'
        $d.Attrs['features'] | Should -HaveCount 1
        $d.Attrs['default-features'] | Should -BeFalse
    }

    It 'preserves the full key=value text in raw for sub-table entries' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies.serde]
version = "1.0.209"
features = ["derive"]
default-features = false
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $d = $r.Dependencies[0]
        $d.Raw | Should -Match 'version\s*=\s*"1\.0\.209"'
        $d.Raw | Should -Match 'features\s*=\s*\["derive"\]'
        $d.Raw | Should -Match 'default-features\s*=\s*false'
    }

    It 'preserves multi-line values in raw for sub-table entries' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies.serde]
version = "1.0.209"
features = [
    "derive",
    "rc",
]
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $d = $r.Dependencies[0]
        $d.Raw | Should -Match 'features\s*=\s*\['
        $d.Raw | Should -Match '"derive"'
        $d.Raw | Should -Match '"rc"'
    }

    It 'tags target-conditional dependencies with the target cfg' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[target.'cfg(windows)'.dependencies]
windows = { version = "0.62" }
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Name | Should -Be 'windows'
        $d.Target | Should -Be 'cfg(windows)'
        $d.Variant | Should -Be 'normal'
    }

    It 'reads a multi-line inline table' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies]
clap = { version = "4.5", features = [
    "derive",
    "env",
] }
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $d = $r.Dependencies[0]
        $d.Style | Should -Be 'inline-table'
        $d.Attrs['version'] | Should -Be '4.5'
        $d.Attrs['features'] | Should -HaveCount 2
    }

    It 'ignores trailing comments after declarations' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[dependencies]
anyhow = "1.0" # version pin
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 1
        $r.Dependencies[0].Attrs['version'] | Should -Be '1.0'
    }

    It 'does not emit records for ignored sections' {
        Set-Content -LiteralPath $script:ManifestPath -Value @"
[workspace.lints.rust]
missing_docs = "warn"

[profile.release]
opt-level = 3

[features]
default = []
extra = ["a", "b"]
"@
        $r = Read-CargoManifest -ManifestPath $script:ManifestPath -WorkspaceRoot $script:WorkspaceRoot
        $r.Dependencies.Count | Should -Be 0
    }
}

Describe 'End-to-end script' {

    BeforeAll {
        function Invoke-InventoryScript {
            param(
                [Parameter(Mandatory)] [string]$WorkspaceManifest,
                [string]$OutputPath
            )
            if ($OutputPath) {
                & $script:ScriptPath -WorkspaceManifest $WorkspaceManifest -OutputPath $OutputPath
                return (Get-Content -LiteralPath $OutputPath -Raw | ConvertFrom-Json)
            }
            else {
                $json = & $script:ScriptPath -WorkspaceManifest $WorkspaceManifest
                return ($json | ConvertFrom-Json)
            }
        }
    }

    Context 'virtual-workspace fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'virtual-workspace\Cargo.toml')
        }
        It 'is recognized as a virtual workspace' {
            $script:Inv.workspaceManifest.isVirtualWorkspace | Should -BeTrue
            $script:Inv.workspaceManifest.package | Should -BeNullOrEmpty
        }
        It 'discovers both members' {
            $script:Inv.memberManifests | Should -HaveCount 2
            ($script:Inv.memberManifests.package | Sort-Object) -join ',' | Should -Be 'alpha,beta'
        }
        It 'records all dependencies from workspace and members' {
            ($script:Inv.dependencies | Where-Object section -eq 'workspace.dependencies').Count | Should -Be 4
            ($script:Inv.dependencies | Where-Object section -eq 'dependencies').Count | Should -Be 2
            ($script:Inv.dependencies | Where-Object section -eq 'dev-dependencies').Count | Should -Be 1
        }
        It 'flags same-workspace path deps in workspace.dependencies' {
            $sw = $script:Inv.dependencies | Where-Object { $_.sameWorkspace -and $_.section -eq 'workspace.dependencies' }
            $sw | Should -HaveCount 2
            ($sw.name | Sort-Object) -join ',' | Should -Be 'alpha,beta'
        }
        It 'flags the path-only dev-dependency as same-workspace' {
            $devSame = $script:Inv.dependencies | Where-Object { $_.section -eq 'dev-dependencies' -and $_.sameWorkspace }
            $devSame | Should -HaveCount 1
            $devSame.name | Should -Be 'beta'
            $devSame.resolvedManifest | Should -Be 'packages/beta/Cargo.toml'
        }
    }

    Context 'non-virtual-workspace fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'non-virtual-workspace\Cargo.toml')
        }
        It 'is recognized as a non-virtual workspace' {
            $script:Inv.workspaceManifest.isVirtualWorkspace | Should -BeFalse
            $script:Inv.workspaceManifest.package | Should -Be 'non-virtual-root'
        }
        It 'includes the root in memberManifests' {
            ($script:Inv.memberManifests.path) | Should -Contain 'Cargo.toml'
            ($script:Inv.memberManifests.path) | Should -Contain 'crates/gamma/Cargo.toml'
        }
        It 'records the root [dependencies] as section=dependencies on the root manifest' {
            $rootDeps = $script:Inv.dependencies | Where-Object { $_.manifest -eq 'Cargo.toml' -and $_.section -eq 'dependencies' }
            $rootDeps | Should -HaveCount 1
            $rootDeps[0].name | Should -Be 'serde'
        }
    }

    Context 'single-crate fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'single-crate\Cargo.toml')
        }
        It 'treats the single crate as a one-member workspace' {
            $script:Inv.memberManifests | Should -HaveCount 1
            $script:Inv.memberManifests[0].package | Should -Be 'lonely'
        }
        It 'captures mixed bare-string and inline-table styles' {
            $styles = $script:Inv.dependencies.style | Sort-Object -Unique
            $styles | Should -Contain 'bare-string'
            $styles | Should -Contain 'inline-table'
        }
    }

    Context 'with-exclude fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'with-exclude\Cargo.toml')
        }
        It 'excludes the listed member directory' {
            $script:Inv.memberManifests | Should -HaveCount 1
            $script:Inv.memberManifests[0].package | Should -Be 'included'
        }
    }

    Context 'glob-recursive fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'glob-recursive\Cargo.toml')
        }
        It 'discovers both shallow and deeply nested members' {
            $script:Inv.memberManifests.package | Sort-Object | Should -Be @('deep', 'shallow')
        }
    }

    Context 'target-and-edge-cases fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'target-and-edge-cases\Cargo.toml')
        }
        It 'records target-conditional deps with their cfg expression' {
            $tgtDeps = $script:Inv.dependencies | Where-Object { $_.target }
            $tgtDeps.target | Sort-Object -Unique | Should -Be @('cfg(unix)', 'cfg(windows)')
        }
        It 'records the sub-table style for [dependencies.serde]' {
            $serde = $script:Inv.dependencies | Where-Object { $_.name -eq 'serde' -and $_.section -eq 'dependencies' }
            $serde | Should -HaveCount 1
            $serde.style | Should -Be 'sub-table'
            $serde.version | Should -Be '1.0'
            $serde.defaultFeatures | Should -BeFalse
            $serde.features | Should -Contain 'derive'
        }
        It 'merges dotted-key entries into one record' {
            $win = $script:Inv.dependencies | Where-Object { $_.name -eq 'windows' -and $_.section -eq 'dev-dependencies' -and -not $_.target }
            $win | Should -HaveCount 1
            $win.style | Should -Be 'dotted-key'
            $win.workspace | Should -BeTrue
            $win.features | Should -Contain 'Win32_System_JobObjects'
        }
        It 'parses multi-line inline-table features' {
            $nix = $script:Inv.dependencies | Where-Object { $_.name -eq 'nix' -and $_.target -eq 'cfg(unix)' }
            $nix | Should -HaveCount 1
            $nix.features | Should -HaveCount 2
        }
    }

    Context 'path-deps-mixed fixture' {
        BeforeAll {
            $script:Inv = Invoke-InventoryScript -WorkspaceManifest (Join-Path $script:FixturesRoot 'path-deps-mixed\Cargo.toml')
        }
        It 'flags the in-workspace path dep as sameWorkspace=true' {
            $inside = $script:Inv.dependencies | Where-Object { $_.name -eq 'inside' -and $_.section -eq 'workspace.dependencies' }
            $inside | Should -HaveCount 1
            $inside.sameWorkspace | Should -BeTrue
            $inside.resolvedManifest | Should -Be 'packages/host/Cargo.toml'
        }
        It 'flags the out-of-workspace path dep as sameWorkspace=false' {
            $outside = $script:Inv.dependencies | Where-Object { $_.name -eq 'outside' -and $_.section -eq 'workspace.dependencies' }
            $outside | Should -HaveCount 1
            $outside.sameWorkspace | Should -BeFalse
            $outside.resolvedManifest | Should -BeNullOrEmpty
        }
        It 'flags a member dev-dependency pointing outside as not same-workspace' {
            $devOutside = $script:Inv.dependencies | Where-Object { $_.name -eq 'outside' -and $_.section -eq 'dev-dependencies' }
            $devOutside | Should -HaveCount 1
            $devOutside.sameWorkspace | Should -BeFalse
        }
    }

    Context 'output mechanics' {
        It 'writes a file when -OutputPath is supplied and emits no stdout' {
            $outFile = Join-Path $TestDrive 'inventory.json'
            $stdout = & $script:ScriptPath -WorkspaceManifest (Join-Path $script:FixturesRoot 'single-crate\Cargo.toml') -OutputPath $outFile
            $stdout | Should -BeNullOrEmpty
            Test-Path -LiteralPath $outFile | Should -BeTrue
            $bytes = [System.IO.File]::ReadAllBytes($outFile)
            $bytes[0] | Should -Not -Be 239   # no UTF-8 BOM
        }
        It 'emits JSON on stdout when -OutputPath is omitted' {
            $stdout = & $script:ScriptPath -WorkspaceManifest (Join-Path $script:FixturesRoot 'single-crate\Cargo.toml')
            $stdout | Should -Not -BeNullOrEmpty
            { $stdout | ConvertFrom-Json } | Should -Not -Throw
        }
        It 'throws when the manifest file does not exist' {
            { & $script:ScriptPath -WorkspaceManifest (Join-Path $TestDrive 'does-not-exist.toml') } | Should -Throw '*not found*'
        }
    }
}
