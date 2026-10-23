#Requires -Version 7.6
#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.5.0' }

Set-StrictMode -Version Latest

BeforeDiscovery {
    # Discovery-time setup: values referenced in -Skip expressions on individual
    # tests must be available before BeforeAll runs.
    $skillsDir       = (Resolve-Path (Join-Path $PSScriptRoot '..\skills')).ProviderPath
    $ExpectedSkills  = @(Get-ChildItem -LiteralPath $skillsDir -Directory | Select-Object -ExpandProperty Name)
    $ExpectedSkillCount = $ExpectedSkills.Count

    $probeDir = Join-Path ([System.IO.Path]::GetTempPath()) ("ss-ai-symlink-probe-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $probeDir -Force | Out-Null
    $probeLink = Join-Path $probeDir 'link'
    try {
        New-Item -ItemType SymbolicLink -Path $probeLink -Target $probeDir -ErrorAction Stop | Out-Null
        $CanCreateSymlink = $true
    }
    catch { $CanCreateSymlink = $false }
    finally { Remove-Item -Recurse -Force -LiteralPath $probeDir -ErrorAction SilentlyContinue }
}

BeforeAll {
    $script:ScriptPath       = (Resolve-Path (Join-Path $PSScriptRoot '..\Install.ps1')).ProviderPath
    . $script:ScriptPath
    $script:SkillsDir        = (Resolve-Path (Join-Path $PSScriptRoot '..\skills')).ProviderPath
    $script:ExpectedSkills   = @(Get-ChildItem -LiteralPath $script:SkillsDir -Directory | Select-Object -ExpandProperty Name)
    $script:ExpectedLinkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }

    function New-TempWorkspace {
        $path = Join-Path ([System.IO.Path]::GetTempPath()) ("ss-ai-installer-test-" + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $path -Force | Out-Null
        $path
    }

    function Remove-WorkspaceSafe {
        param([Parameter(Mandatory)] [string]$Path)
        if (-not (Test-Path -LiteralPath $Path)) { return }
        # Delete reparse points explicitly first so Remove-Item -Recurse never follows
        # a link into the real skills dir under the repo.
        $links = Get-ChildItem -LiteralPath $Path -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue
        foreach ($link in $links) {
            try { [System.IO.Directory]::Delete($link.FullName, $false) }
            catch { try { [System.IO.File]::Delete($link.FullName) } catch { } }
        }
        Remove-Item -Recurse -Force -LiteralPath $Path -ErrorAction SilentlyContinue
    }

    function Invoke-Installer {
        param([Parameter(Mandatory)] [string]$WorkspacePath)
        Install-CopilotSkills -TargetRoot (Get-SkillsTargetDir -WorkspacePath $WorkspacePath) *>&1
    }

    function Get-SkillsTargetDir {
        param([Parameter(Mandatory)] [string]$WorkspacePath)
        Join-Path $WorkspacePath '.copilot\skills'
    }
}

Describe 'Installed skill links' {
    BeforeEach {
        $script:Workspace = New-TempWorkspace
    }
    AfterEach {
        if ($script:Workspace) { Remove-WorkspaceSafe -Path $script:Workspace }
    }

    Context 'fresh install' {
        It 'creates a link for every skill found under skills/' {
            Invoke-Installer -WorkspacePath $script:Workspace | Out-Null
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            $names = @(Get-ChildItem -Force -LiteralPath $target | Select-Object -ExpandProperty Name)
            $names | Sort-Object | Should -Be ($script:ExpectedSkills | Sort-Object)
        }

        It 'creates the user skills target directory if missing' {
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            Test-Path -LiteralPath $target | Should -BeFalse
            Invoke-Installer -WorkspacePath $script:Workspace | Out-Null
            Test-Path -LiteralPath $target -PathType Container | Should -BeTrue
        }

        It 'creates links that resolve to the source skill folders' {
            Invoke-Installer -WorkspacePath $script:Workspace | Out-Null
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            foreach ($skill in $script:ExpectedSkills) {
                $link = Get-Item -Force -LiteralPath (Join-Path $target $skill)
                $link.Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) | Should -BeTrue
                $expected = (Get-Item -LiteralPath (Join-Path $script:SkillsDir $skill)).FullName
                @($link.Target) | Should -Contain $expected
            }
        }

        It 'uses the platform-appropriate link type' {
            Invoke-Installer -WorkspacePath $script:Workspace | Out-Null
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            foreach ($skill in $script:ExpectedSkills) {
                (Get-Item -Force -LiteralPath (Join-Path $target $skill)).LinkType | Should -Be $script:ExpectedLinkType
            }
        }

        It 'reports each linked skill in its output' {
            $output = Invoke-Installer -WorkspacePath $script:Workspace
            $text   = $output | Out-String
            foreach ($skill in $script:ExpectedSkills) {
                $text | Should -Match ([regex]::Escape("Linked $skill"))
            }
        }
    }

    Context 'overwrite of existing links' {
        It 're-running over an existing junction replaces it cleanly' {
            Invoke-Installer -WorkspacePath $script:Workspace | Out-Null
            $output = Invoke-Installer -WorkspacePath $script:Workspace
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            foreach ($skill in $script:ExpectedSkills) {
                $link = Get-Item -Force -LiteralPath (Join-Path $target $skill)
                $link.LinkType | Should -Be $script:ExpectedLinkType
                $expected = (Get-Item -LiteralPath (Join-Path $script:SkillsDir $skill)).FullName
                @($link.Target) | Should -Contain $expected
            }
            ($output | Out-String) | Should -Match 'Linked'
        }

        It 'replaces a pre-existing manual symbolic link pointing somewhere else' -Skip:(-not $CanCreateSymlink) {
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            New-Item -ItemType Directory -Path $target -Force | Out-Null
            $skill    = $script:ExpectedSkills[0]
            $linkPath = Join-Path $target $skill

            $bogus = New-TempWorkspace
            try {
                $manual = New-Item -ItemType SymbolicLink -Path $linkPath -Target $bogus
                $manual.LinkType | Should -Be 'SymbolicLink'

                Invoke-Installer -WorkspacePath $script:Workspace | Out-Null

                $post = Get-Item -Force -LiteralPath $linkPath
                $post.Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) | Should -BeTrue
                $post.LinkType | Should -Be $script:ExpectedLinkType
                $expected = (Get-Item -LiteralPath (Join-Path $script:SkillsDir $skill)).FullName
                @($post.Target) | Should -Contain $expected
            }
            finally {
                Remove-WorkspaceSafe -Path $bogus
            }
        }
    }

    Context 'safety for non-link contents' {
        It 'reports a pre-existing plain directory and preserves its contents' {
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            New-Item -ItemType Directory -Path $target -Force | Out-Null
            $skill    = $script:ExpectedSkills[0]
            $plain    = Join-Path $target $skill
            New-Item -ItemType Directory -Path $plain | Out-Null
            $sentinel = Join-Path $plain 'keep.txt'
            'do not delete' | Out-File -LiteralPath $sentinel

            { Invoke-Installer -WorkspacePath $script:Workspace } | Should -Throw '*collision*'

            $entry = Get-Item -Force -LiteralPath $plain
            $entry.Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) | Should -BeFalse
            (Get-Content -LiteralPath $sentinel) | Should -Be 'do not delete'
        }

        It 'reports a pre-existing plain file and preserves its contents' {
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            New-Item -ItemType Directory -Path $target -Force | Out-Null
            $skill    = $script:ExpectedSkills[0]
            $filePath = Join-Path $target $skill
            'do not delete' | Out-File -LiteralPath $filePath

            { Invoke-Installer -WorkspacePath $script:Workspace } | Should -Throw '*collision*'

            (Get-Item -LiteralPath $filePath).Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) | Should -BeFalse
            (Get-Content -LiteralPath $filePath) | Should -Be 'do not delete'
        }

        It 'does not report complete installation when a skill collides' -Skip:($ExpectedSkillCount -lt 2) {
            $target = Get-SkillsTargetDir -WorkspacePath $script:Workspace
            New-Item -ItemType Directory -Path $target -Force | Out-Null
            $blocked = $script:ExpectedSkills[0]
            $other   = $script:ExpectedSkills[1]
            New-Item -ItemType Directory -Path (Join-Path $target $blocked) | Out-Null

            { Invoke-Installer -WorkspacePath $script:Workspace } | Should -Throw '*collision*'

            (Get-Item -Force -LiteralPath (Join-Path $target $blocked)).Attributes.HasFlag([System.IO.FileAttributes]::ReparsePoint) | Should -BeFalse
            # Other entries may precede the collision; no unrelated content is removed.
        }
    }
}
