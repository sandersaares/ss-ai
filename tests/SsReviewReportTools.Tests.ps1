BeforeAll {
    $script:SkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    $lib = Join-Path $script:SkillRoot 'scripts\lib'
    . (Join-Path $lib 'PullRequestModel.ps1')
    . (Join-Path $lib 'PullRequestTransportHost.ps1')
    . (Join-Path $lib 'ReportTools.ps1')
    $script:RealProcessInvoker = ${function:Get-ReportProcessInvoker}
    $script:ShellPath = Get-Command sh -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty Source

    # Build only the layout needed by each discovery or invocation, without installing tools.
    function New-ReportToolTestFile {
        param([string] $Path, [string] $Content = '')
        [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
        [IO.File]::WriteAllText($Path, $Content)
        $Path
    }

    # Minimal package entry points let setup and discovery share realistic fixtures offline.
    function New-ReportMermaidTestPackages {
        param([string] $Directory)
        foreach ($relativePath in @(
            'node_modules\@mermaid-js\mermaid-cli\src\cli.js',
            'node_modules\puppeteer\install.mjs',
            'node_modules\puppeteer\lib\puppeteer\puppeteer.js'
        )) {
            $null = New-ReportToolTestFile -Path (Join-Path $Directory $relativePath)
        }
        $null = New-ReportToolTestFile -Path (Join-Path $Directory 'node_modules\puppeteer\package.json') `
            -Content '{"main":"./lib/puppeteer/puppeteer.js"}'
    }
}

Describe 'Node.js discovery for report tools' {
    BeforeEach {
        $script:OriginalIsWindows = $IsWindows
        $script:SavedNvmEnvironment = @{}
        foreach ($name in @('NVM_DIR', 'NVM_BIN', 'NVM_SYMLINK', 'XDG_CONFIG_HOME')) {
            $script:SavedNvmEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
            [Environment]::SetEnvironmentVariable($name, $null)
        }
        $script:OriginalPath = $env:PATH
        $script:NvmDirectory = Join-Path $TestDrive "nvm root 'quoted' `$literal"
        $script:NodePath = New-ReportToolTestFile -Path (Join-Path $script:NvmDirectory 'versions\node\v24.1.0\bin\node')
        $null = New-ReportToolTestFile -Path (Join-Path $script:NvmDirectory 'nvm.sh')
        $env:NVM_DIR = $script:NvmDirectory
        $script:NvmOutput = "$script:NodePath`n"
        $script:NvmExitCode = 0
        $script:NvmError = ''
        $script:NvmCalls = [Collections.Generic.List[object]]::new()
        Mock Get-Command { $null } -ParameterFilter { $Name -eq 'node' }
        Mock Get-Command { [pscustomobject]@{ Source = 'test-sh' } } -ParameterFilter { $Name -eq 'sh' }
        Mock Get-ReportProcessInvoker {
            {
                param($FilePath, $ArgumentList, $StandardInput)
                $script:NvmCalls.Add(@{ file = $FilePath; arguments = $ArgumentList })
                @{
                    exitCode = $script:NvmExitCode
                    standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($script:NvmOutput)
                    standardErrorText = $script:NvmError
                }
            }
        }
    }

    AfterEach {
        # AllScope platform variables remain shared even when changed inside a child scope.
        Set-Variable -Name IsWindows -Value $script:OriginalIsWindows -Force
        foreach ($name in $script:SavedNvmEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($name, $script:SavedNvmEnvironment[$name])
        }
        $env:PATH | Should -BeExactly $script:OriginalPath
    }

    It 'preserves the runtime already on PATH' {
        Mock Get-Command { [pscustomobject]@{ Source = $script:NodePath } } -ParameterFilter { $Name -eq 'node' }
        Get-ReportNodePath | Should -BeExactly $script:NodePath
        $script:NvmCalls.Count | Should -Be 0
    }

    It 'finds the active nvm location on <Platform> without PATH' -ForEach @(
        @{ Platform = 'Windows'; Windows = $true; Variable = 'NVM_SYMLINK'; Executable = 'node.exe' }
        @{ Platform = 'POSIX'; Windows = $false; Variable = 'NVM_BIN'; Executable = 'node' }
    ) {
        $active = Join-Path $TestDrive 'active node'
        $expected = New-ReportToolTestFile -Path (Join-Path $active $Executable)
        [Environment]::SetEnvironmentVariable($Variable, $active)
        $actual = & {
            Set-Variable -Name IsWindows -Value $Windows -Scope Local -Force
            Get-ReportNodePath
        }
        $actual | Should -BeExactly $expected
        $script:NvmCalls.Count | Should -Be 0
    }

    It 'resolves the POSIX default when the active location is stale' {
        $env:NVM_BIN = Join-Path $TestDrive 'removed version'
        $actual = & {
            Set-Variable -Name IsWindows -Value $false -Scope Local -Force
            Get-ReportNodePath
        }
        $actual | Should -BeExactly $script:NodePath
        $script:NvmCalls.Count | Should -Be 1
    }

    It 'reports no Windows runtime when the nvm symlink is stale' {
        $env:NVM_SYMLINK = Join-Path $TestDrive 'removed symlink'
        $actual = & {
            Set-Variable -Name IsWindows -Value $true -Scope Local -Force
            Get-ReportNodePath
        }
        $actual | Should -BeNullOrEmpty
        $script:NvmCalls.Count | Should -Be 0
    }

    It 'delegates alias resolution to nvm without interpolating paths or activating a version' {
        Get-ReportNvmNodePath | Should -BeExactly $script:NodePath
        $script:NvmCalls.Count | Should -Be 1
        $call = $script:NvmCalls[0]
        $call.file | Should -BeExactly 'test-sh'
        $call.arguments | Should -Be @(
            '-c', 'export NVM_DIR="$1"; set -- --no-use; . "$NVM_DIR/nvm.sh" >&2 && nvm which default',
            'ss-review-node', $script:NvmDirectory
        )
    }

    It 'loads nvm with automatic activation disabled in a real POSIX shell' -Skip:(-not (Get-Command sh -CommandType Application -ErrorAction SilentlyContinue)) {
        $null = New-ReportToolTestFile -Path (Join-Path $script:NvmDirectory 'nvm.sh') -Content @'
if [ "$1" != '--no-use' ]; then
    printf 'Automatic activation was not disabled\n' >&2
    return 1
fi
nvm() {
    if [ "$#" -ne 2 ] || [ "$1" != 'which' ] || [ "$2" != 'default' ]; then
        printf 'Unexpected nvm command\n' >&2
        return 1
    fi
    printf '%s\n' "$NVM_DIR/versions/node/v24.1.0/bin/node"
}
'@
        Mock Get-Command { [pscustomobject]@{ Source = $script:ShellPath } } -ParameterFilter { $Name -eq 'sh' }
        Mock Get-ReportProcessInvoker { & $script:RealProcessInvoker }
        [IO.Path]::GetFullPath((Get-ReportNvmNodePath)) | Should -BeExactly $script:NodePath
    }

    It 'uses the standard XDG nvm directory when NVM_DIR is absent' {
        $env:NVM_DIR = $null
        $env:XDG_CONFIG_HOME = Join-Path $TestDrive 'config'
        $expected = Join-Path $env:XDG_CONFIG_HOME 'nvm'
        $null = New-ReportToolTestFile -Path (Join-Path $expected 'nvm.sh')
        Get-ReportNvmNodePath | Should -BeExactly $script:NodePath
        $script:NvmCalls[0].arguments[-1] | Should -BeExactly $expected
    }

    It 'uses the standard home nvm directory without environment setup' {
        $env:NVM_DIR = $null
        Mock Join-Path { $script:NvmDirectory } -ParameterFilter { $Path -eq $HOME -and $ChildPath -eq '.nvm' }
        Get-ReportNvmNodePath | Should -BeExactly $script:NodePath
        Should -Invoke Join-Path -Times 1 -Exactly -ParameterFilter { $Path -eq $HOME -and $ChildPath -eq '.nvm' }
    }

    It 'does not search another installation when an explicit nvm directory is absent' {
        $env:NVM_DIR = Join-Path $TestDrive 'not installed'
        Get-ReportNvmNodePath | Should -BeNullOrEmpty
        $script:NvmCalls.Count | Should -Be 0
    }

    It 'reports a missing POSIX shell explicitly' {
        Mock Get-Command { $null } -ParameterFilter { $Name -eq 'sh' }
        { Get-ReportNvmNodePath } | Should -Throw '*POSIX shell*'
    }

    It 'preserves nvm failure diagnostics rather than selecting an arbitrary version' {
        # nvm uses this exit code when the requested version is not installed.
        $script:NvmExitCode = 3
        $script:NvmError = 'N/A: version "default" is not yet installed.'
        { Get-ReportNvmNodePath } | Should -Throw '*version "default" is not yet installed*'
    }

    It 'rejects a successful nvm response that is not an installed absolute path: <Output>' -ForEach @(
        @{ Output = '' }
        @{ Output = 'N/A' }
        @{ Output = 'relative-node' }
    ) {
        $script:NvmOutput = $Output
        { Get-ReportNvmNodePath } | Should -Throw '*did not return an installed Node.js executable*'
    }

    It 'rejects an absolute path whose executable is missing' {
        $script:NvmOutput = Join-Path $TestDrive 'missing-node'
        { Get-ReportNvmNodePath } | Should -Throw '*did not return an installed Node.js executable*'
    }
}

Describe 'User-wide report tool storage' {
    BeforeEach {
        $script:StorageRoot = Join-Path $TestDrive 'shared tools'
        Mock Get-ReportToolsDirectory { $script:StorageRoot }
        $script:FirstSkill = Join-Path $TestDrive 'first checkout\ss-review'
        $script:SecondSkill = Join-Path $TestDrive 'second checkout\ss-review'
        foreach ($root in @($script:FirstSkill, $script:SecondSkill)) {
            $null = New-ReportToolTestFile -Path (Join-Path $root 'tools\package-lock.json') -Content '{"version":1}'
        }
    }

    It 'shares one installation across skill copies with the same lockfile' {
        $first = Get-ReportMermaidDirectory -SkillRoot $script:FirstSkill
        $first | Should -BeExactly (Get-ReportMermaidDirectory -SkillRoot $script:SecondSkill)
        Split-Path -Parent $first | Should -BeExactly $script:StorageRoot
    }

    It 'isolates different dependency sets without changing the shared lock or Pandoc location' {
        $first = Get-ReportMermaidDirectory -SkillRoot $script:FirstSkill
        Set-Content -LiteralPath (Join-Path $script:SecondSkill 'tools\package-lock.json') -Value '{"version":2}'
        Get-ReportMermaidDirectory -SkillRoot $script:SecondSkill | Should -Not -BeExactly $first
        Get-ReportToolsLockPath | Should -BeExactly (Join-Path $script:StorageRoot 'installation')
        Split-Path -Parent (Get-ReportPandocDirectory) | Should -BeExactly $script:StorageRoot
    }

    It 'does not treat a missing lockfile as an installed dependency set' {
        Remove-Item -LiteralPath (Join-Path $script:FirstSkill 'tools\package-lock.json')
        { Get-ReportMermaidDirectory -SkillRoot $script:FirstSkill } | Should -Throw
    }
}

Describe 'Report discovery uses the resolved runtime' {
    BeforeEach {
        $script:ReportRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:NodePath = New-ReportToolTestFile -Path (Join-Path $script:ReportRoot 'runtime\node')
        $script:BrowserPath = New-ReportToolTestFile -Path (Join-Path $script:ReportRoot 'runtime\browser')
        $script:BrowserExitCode = 0
        $script:BrowserError = ''
        $script:BrowserArguments = $null
        $script:PandocPath = New-ReportToolTestFile -Path (Join-Path $script:ReportRoot 'pandoc')
        Mock Get-ReportToolsDirectory { Join-Path $script:ReportRoot 'shared tools' }
        $null = New-ReportToolTestFile -Path (Join-Path $script:ReportRoot 'tools\package-lock.json') -Content '{}'
        $script:MermaidDirectory = Get-ReportMermaidDirectory -SkillRoot $script:ReportRoot
        New-ReportMermaidTestPackages -Directory $script:MermaidDirectory
        Mock Get-Command { [pscustomobject]@{ Source = $script:PandocPath } } -ParameterFilter { $Name -eq 'pandoc' }
        Mock Get-ReportNodePath { $script:NodePath }
        Mock Get-ReportToolsLockPath { Join-Path $TestDrive 'report-tools' }
        Mock Get-ReportProcessInvoker {
            {
                param($FilePath, $ArgumentList, $StandardInput)
                $FilePath | Should -BeExactly $script:NodePath
                $script:BrowserArguments = $ArgumentList
                @{
                    exitCode = $script:BrowserExitCode
                    standardOutputBytes = [Text.Encoding]::UTF8.GetBytes($script:BrowserPath)
                    standardErrorText = $script:BrowserError
                }
            }
        }
    }

    It 'probes Puppeteer with the discovered executable and preserves the tool record shape' {
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        @($tools.Keys) | Should -Be @('pandoc', 'node', 'mermaid', 'browser', 'missing', 'diagnostics')
        $tools.node | Should -BeExactly $script:NodePath
        $tools.browser | Should -BeExactly $script:BrowserPath
        $tools.missing | Should -BeNullOrEmpty
        $tools.diagnostics | Should -BeNullOrEmpty
        $script:BrowserArguments[0] | Should -BeExactly '-e'
        $script:BrowserArguments[-1] | Should -BeExactly (Join-Path $script:MermaidDirectory 'node_modules\puppeteer')
    }

    It 'finds the same installed tools from a fresh skill checkout without local node_modules' {
        $otherSkill = Join-Path $script:ReportRoot 'fresh checkout'
        $null = New-ReportToolTestFile -Path (Join-Path $otherSkill 'tools\package-lock.json') -Content '{}'
        $first = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $second = Get-ReviewReportTools -SkillRoot $otherSkill
        $second.mermaid | Should -BeExactly $first.mermaid
        $second.browser | Should -BeExactly $first.browser
        $second.missing | Should -BeNullOrEmpty
        Test-Path -LiteralPath (Join-Path $otherSkill 'tools\node_modules') | Should -BeFalse
    }

    It 'reports an uninstalled shared dependency set even when a checkout has local packages' {
        Remove-Item -LiteralPath (Join-Path $script:MermaidDirectory 'node_modules\@mermaid-js\mermaid-cli\src\cli.js')
        $null = New-ReportToolTestFile -Path (Join-Path $script:ReportRoot 'tools\node_modules\@mermaid-js\mermaid-cli\src\cli.js')
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $tools.missing | Should -Be @('mermaid-cli')
        $script:BrowserArguments | Should -BeNullOrEmpty
    }

    It 'retains missing-node reporting when no runtime is installed' {
        Mock Get-ReportNodePath { $null }
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $tools.node | Should -BeNullOrEmpty
        $tools.missing | Should -Be @('node')
        $script:BrowserArguments | Should -BeNullOrEmpty
    }

    It 'reports incomplete Puppeteer packages as requiring package installation: <Missing>' -ForEach @(
        @{ Missing = 'node_modules\puppeteer\install.mjs' }
        @{ Missing = 'node_modules\puppeteer\package.json' }
        @{ Missing = 'node_modules\puppeteer\lib\puppeteer\puppeteer.js' }
    ) {
        Remove-Item -LiteralPath (Join-Path $script:MermaidDirectory $Missing)
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $tools.missing | Should -Be @('mermaid-cli')
        $script:BrowserArguments | Should -BeNullOrEmpty
    }

    It 'preserves browser discovery failures instead of reporting only a missing executable' {
        $script:BrowserExitCode = 1
        $script:BrowserError = 'Cannot load Puppeteer with this Node.js runtime.'
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $tools.browser | Should -BeNullOrEmpty
        $tools.missing | Should -Be @('mermaid-browser')
        $tools.diagnostics | Should -Be @("Browser discovery failed (exit 1): $script:BrowserError")
        { Assert-ReviewReportTools -Tools $tools } | Should -Throw '*Cannot load Puppeteer*'
    }

    It 'reports the resolved path when the browser download is absent' {
        Remove-Item -LiteralPath $script:BrowserPath
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        $tools.missing | Should -Be @('mermaid-browser')
        $tools.diagnostics | Should -Be @("Puppeteer's browser executable is absent: '$script:BrowserPath'.")
    }

    It 'requests installation permission during discovery but not after an approved attempt' {
        Remove-Item -LiteralPath $script:BrowserPath
        $tools = Get-ReviewReportTools -SkillRoot $script:ReportRoot
        { Assert-ReviewReportTools -Tools $tools } | Should -Throw '*Ask the user for installation permission*'
        $caught = $null
        try { Assert-ReviewReportTools -Tools $tools -InstallationAttempted } catch { $caught = $_.Exception }
        $caught | Should -Not -BeNullOrEmpty
        $caught.Message | Should -Not -Match 'Ask the user for installation permission'
        $caught.Message | Should -Match 'Incomplete browser cache recovery'
        $caught.Message | Should -Match 'separate approval'
        $caught.Message | Should -Match ([regex]::Escape($script:BrowserPath))
        $caught.Data['missingTools'] | Should -Be @('mermaid-browser')
    }
}

Describe 'Report installation uses the discovered runtime' {
    BeforeAll {
        # A local command lets these tests mock npm even on hosts without Node.js installed.
        function npm { throw 'Unexpected npm invocation.' }
    }

    BeforeEach {
        $script:InstallRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:NodePath = New-ReportToolTestFile -Path (Join-Path $script:InstallRoot 'runtime with spaces\node.ps1') -Content @'
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'arguments.json'), (ConvertTo-Json -InputObject @($args) -Compress))
[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'working-directory'), (Get-Location).Path)
exit ([int] (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'exit-code')))
'@
        $script:NodeDirectory = Split-Path -Parent $script:NodePath
        $script:NodeExitFile = New-ReportToolTestFile -Path (Join-Path $script:NodeDirectory 'exit-code') -Content '0'
        $script:InstallSkill = Join-Path $script:InstallRoot 'skill'
        $null = New-ReportToolTestFile -Path (Join-Path $script:InstallSkill 'tools\package.json') -Content '{"private":true}'
        $null = New-ReportToolTestFile -Path (Join-Path $script:InstallSkill 'tools\package-lock.json') -Content '{}'
        Mock Get-ReportToolsDirectory { Join-Path $script:InstallRoot 'shared tools' }
        $script:ToolsDirectory = Get-ReportMermaidDirectory -SkillRoot $script:InstallSkill
        $script:NpmExitCode = 0
        $script:NpmArguments = $null
        $script:NpmDirectory = $null
        $script:ExistingInstallation = $false
        $script:OmittedPackage = $null
        $script:OriginalPath = $env:PATH
        $script:OriginalLocation = (Get-Location).Path
        Mock npm {
            $script:NpmArguments = @($args)
            $env:PATH.Split([IO.Path]::PathSeparator)[0] | Should -BeExactly $script:NodeDirectory
            $script:NpmDirectory = (Get-Location).Path
            Split-Path -Parent $script:NpmDirectory | Should -BeExactly (Get-ReportToolsDirectory)
            Get-Content -LiteralPath (Join-Path $script:NpmDirectory 'package.json') -Raw | Should -BeExactly '{"private":true}'
            Get-Content -LiteralPath (Join-Path $script:NpmDirectory 'package-lock.json') -Raw | Should -BeExactly '{}'
            Test-Path -LiteralPath $script:ToolsDirectory | Should -Be $script:ExistingInstallation
            New-ReportMermaidTestPackages -Directory $script:NpmDirectory
            if ($script:OmittedPackage) { Remove-Item -LiteralPath (Join-Path $script:NpmDirectory $script:OmittedPackage) }
            $global:LASTEXITCODE = $script:NpmExitCode
        }
    }

    AfterEach {
        $env:PATH | Should -BeExactly $script:OriginalPath
        (Get-Location).Path | Should -BeExactly $script:OriginalLocation
    }

    It 'exposes the selected runtime to npm and invokes that executable for browser setup' {
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        $script:NpmArguments | Should -Be @('ci', '--ignore-scripts', '--engine-strict', '--no-audit', '--no-fund')
        $arguments = Get-Content -LiteralPath (Join-Path $script:NodeDirectory 'arguments.json') -Raw | ConvertFrom-Json
        $arguments | Should -Be @((Join-Path $script:NpmDirectory 'node_modules\puppeteer\install.mjs'))
        Get-Content -LiteralPath (Join-Path $script:NodeDirectory 'working-directory') -Raw | Should -BeExactly $script:OriginalLocation
        Test-Path -LiteralPath (Join-Path $script:ToolsDirectory 'package-lock.json') | Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:ToolsDirectory 'node_modules\@mermaid-js\mermaid-cli\src\cli.js') | Should -BeTrue
        Test-Path -LiteralPath $script:NpmDirectory | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:InstallSkill 'tools\node_modules') | Should -BeFalse
    }

    It 'reuses the published dependency tree while allowing browser cache repair' {
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        $otherSkill = Join-Path $script:InstallRoot 'other checkout'
        foreach ($name in @('package.json', 'package-lock.json')) {
            $null = New-ReportToolTestFile -Path (Join-Path $otherSkill "tools\$name") `
                -Content (Get-Content -LiteralPath (Join-Path $script:InstallSkill "tools\$name") -Raw)
        }
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $otherSkill
        Should -Invoke npm -Times 1 -Exactly
        $arguments = Get-Content -LiteralPath (Join-Path $script:NodeDirectory 'arguments.json') -Raw | ConvertFrom-Json
        $arguments | Should -Be @((Join-Path $script:ToolsDirectory 'node_modules\puppeteer\install.mjs'))
        Get-Content -LiteralPath (Join-Path $script:NodeDirectory 'working-directory') -Raw | Should -BeExactly $script:OriginalLocation
    }

    It 'rebuilds incomplete shared packages without discarding the old tree during npm: <Missing>' -ForEach @(
        @{ Missing = 'node_modules\@mermaid-js\mermaid-cli\src\cli.js' }
        @{ Missing = 'node_modules\puppeteer\install.mjs' }
        @{ Missing = 'node_modules\puppeteer\lib\puppeteer\puppeteer.js' }
    ) {
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        $script:ExistingInstallation = $true
        Remove-Item -LiteralPath (Join-Path $script:ToolsDirectory $Missing)
        $marker = New-ReportToolTestFile -Path (Join-Path $script:ToolsDirectory 'old-tree')
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        Should -Invoke npm -Times 2 -Exactly
        Test-ReportMermaidInstallation -Directory $script:ToolsDirectory | Should -BeTrue
        Test-Path -LiteralPath $marker | Should -BeFalse
        @(Get-ChildItem -LiteralPath (Get-ReportToolsDirectory) -Force).Count | Should -Be 1
    }

    It 'preserves an incomplete published tree when rebuilding fails during <Failure>' -ForEach @(
        @{ Failure = 'npm' }
        @{ Failure = 'browser' }
    ) {
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        $script:ExistingInstallation = $true
        Remove-Item -LiteralPath (Join-Path $script:ToolsDirectory 'node_modules\@mermaid-js\mermaid-cli\src\cli.js')
        $marker = New-ReportToolTestFile -Path (Join-Path $script:ToolsDirectory 'old-tree')
        if ($Failure -eq 'npm') { $script:NpmExitCode = 1 }
        else { Set-Content -LiteralPath $script:NodeExitFile -Value '1' }
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } | Should -Throw '*Installing*failed*'
        Test-Path -LiteralPath $marker | Should -BeTrue
        @(Get-ChildItem -LiteralPath (Get-ReportToolsDirectory) -Force).Count | Should -Be 1
    }

    It 'rejects a checkout lockfile change between selecting the directory and copying manifests' {
        Mock Copy-Item {
            param($LiteralPath, $Destination)
            # Updating during the first copy deterministically reproduces a checkout change.
            if ((Split-Path -Leaf $LiteralPath) -eq 'package.json') {
                [IO.File]::WriteAllText((Join-Path $script:InstallSkill 'tools\package-lock.json'), '{"changed":true}')
            }
            [IO.File]::Copy($LiteralPath, $Destination)
        }
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } |
            Should -Throw '*lockfile changed during setup*'
        Should -Invoke npm -Times 0 -Exactly
        Test-Path -LiteralPath (Join-Path $script:NodeDirectory 'arguments.json') | Should -BeFalse
        Test-Path -LiteralPath $script:ToolsDirectory | Should -BeFalse
        @(Get-ChildItem -LiteralPath (Get-ReportToolsDirectory) -Force).Count | Should -Be 0
    }

    It 'does not publish an npm success with missing required packages' {
        $script:OmittedPackage = 'node_modules\puppeteer\lib\puppeteer\puppeteer.js'
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } |
            Should -Throw '*did not provide the required Mermaid and Puppeteer package files*'
        Test-Path -LiteralPath $script:ToolsDirectory | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:NodeDirectory 'arguments.json') | Should -BeFalse
    }

    It 'restores PATH and location after npm fails without running browser setup' {
        $script:NpmExitCode = 1
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } |
            Should -Throw '*Installing Mermaid CLI failed*'
        Test-Path -LiteralPath (Join-Path $script:NodeDirectory 'arguments.json') | Should -BeFalse
        Test-Path -LiteralPath $script:ToolsDirectory | Should -BeFalse
        @(Get-ChildItem -LiteralPath (Get-ReportToolsDirectory) -Force).Count | Should -Be 0
    }

    It 'restores PATH and location after browser setup fails' {
        Set-Content -LiteralPath $script:NodeExitFile -Value '1'
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } |
            Should -Throw "*Installing Mermaid's browser failed*Incomplete browser cache recovery*separate approval*"
        Test-Path -LiteralPath $script:ToolsDirectory | Should -BeFalse
        @(Get-ChildItem -LiteralPath (Get-ReportToolsDirectory) -Force).Count | Should -Be 0
    }

    It 'preserves published packages when a later browser repair fails' {
        Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill
        Set-Content -LiteralPath $script:NodeExitFile -Value '1'
        { Install-ReviewMermaidTools -NodePath $script:NodePath -SkillRoot $script:InstallSkill } |
            Should -Throw "*Installing Mermaid's browser failed*"
        Should -Invoke npm -Times 1 -Exactly
        Test-Path -LiteralPath (Join-Path $script:ToolsDirectory 'node_modules\@mermaid-js\mermaid-cli\src\cli.js') | Should -BeTrue
    }
}

Describe 'Ubuntu browser system dependencies' {
    BeforeEach {
        $script:OriginalIsLinux = $IsLinux
        Set-Variable -Name IsLinux -Value $true -Force
        $script:BrowserPath = New-ReportToolTestFile -Path (Join-Path $TestDrive 'chrome\chrome')
        # Preserve alternatives and version constraints; apt selects the distro's ABI packages.
        $script:DependencyPath = New-ReportToolTestFile -Path (Join-Path $TestDrive 'chrome\deb.deps') `
            -Content "libnss3 (>= 3.35)`nlibnspr4 (>= 4.9.5)`nlibasound2 (>= 1.0.17) | libasound2t64`n"
        $script:AptCalls = [Collections.Generic.List[object]]::new()
        $script:FailedOperation = ''
        Mock Get-Command { [pscustomobject]@{ Source = 'test-apt-get' } } -ParameterFilter { $Name -eq 'apt-get' }
        Mock Get-Command { [pscustomobject]@{ Source = 'test-sudo' } } -ParameterFilter { $Name -eq 'sudo' }
        Mock Get-ReportProcessInvoker {
            {
                param($FilePath, $ArgumentList, $StandardInput)
                $FilePath | Should -BeExactly 'test-sudo'
                $ArgumentList[0] | Should -BeExactly '-n'
                $ArgumentList[1] | Should -BeExactly 'test-apt-get'
                $script:AptCalls.Add($ArgumentList)
                @{
                    exitCode = if ($ArgumentList[2] -eq $script:FailedOperation) { 1 } else { 0 }
                    standardOutputBytes = [byte[]]@()
                    standardErrorText = if ($script:FailedOperation) { 'System package installation refused.' } else { '' }
                }
            }
        }
    }

    AfterEach {
        Set-Variable -Name IsLinux -Value $script:OriginalIsLinux -Force
    }

    It 'uses the downloaded browser dependency relations and adds diagram fonts' {
        Install-ReviewBrowserDependencies -BrowserPath $script:BrowserPath
        $script:AptCalls.Count | Should -Be 3
        $script:AptCalls[0] | Should -Be @('-n', 'test-apt-get', 'update')
        $script:AptCalls[1] | Should -Be @('-n', 'test-apt-get', 'satisfy', '--yes', '--no-install-recommends',
            'libnss3 (>= 3.35), libnspr4 (>= 4.9.5), libasound2 (>= 1.0.17) | libasound2t64')
        $script:AptCalls[2] | Should -Be @('-n', 'test-apt-get', 'install', '--yes', '--no-install-recommends', 'fonts-liberation')
    }

    It 'does not use Linux package installation on another platform' {
        Set-Variable -Name IsLinux -Value $false -Force
        { Install-ReviewBrowserDependencies -BrowserPath $script:BrowserPath } | Should -Throw '*Ubuntu or Debian*'
        $script:AptCalls.Count | Should -Be 0
    }

    It 'refuses missing or empty browser metadata: <Metadata>' -ForEach @(
        @{ Metadata = 'missing' }
        @{ Metadata = 'empty' }
    ) {
        if ($Metadata -eq 'missing') { Remove-Item -LiteralPath $script:DependencyPath }
        else { Set-Content -LiteralPath $script:DependencyPath -Value " `n" }
        { Install-ReviewBrowserDependencies -BrowserPath $script:BrowserPath } | Should -Throw "*metadata is $Metadata*"
        $script:AptCalls.Count | Should -Be 0
    }

    It 'reports unavailable elevation instead of prompting' {
        Mock Get-Command { $null } -ParameterFilter { $Name -eq 'sudo' }
        { Install-ReviewBrowserDependencies -BrowserPath $script:BrowserPath } | Should -Throw '*noninteractive sudo access*'
        $script:AptCalls.Count | Should -Be 0
    }

    It 'stops on an apt failure without claiming complete installation: <Operation>' -ForEach @(
        @{ Operation = 'update'; Calls = 1 }
        @{ Operation = 'satisfy'; Calls = 2 }
        @{ Operation = 'install'; Calls = 3 }
    ) {
        $script:FailedOperation = $Operation
        { Install-ReviewBrowserDependencies -BrowserPath $script:BrowserPath } | Should -Throw '*System package installation refused*'
        $script:AptCalls.Count | Should -Be $Calls
    }
}
