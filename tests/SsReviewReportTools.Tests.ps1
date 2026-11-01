#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:NativeReportCommand = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $script:ShellPath = Get-Command sh -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty Source
    $script:NativeSkillRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills/ss-review'

    # Minimal entry points permit offline discovery without executing package tools.
    function New-ReportToolTestFile {
        param([string] $Path, [string] $Content = '')
        [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
        [IO.File]::WriteAllText($Path, $Content)
        $Path
    }

    function New-ReportMermaidTestPackages {
        param([string] $Directory)
        foreach ($relativePath in @(
            'node_modules/@mermaid-js/mermaid-cli/src/cli.js',
            'node_modules/puppeteer/install.mjs',
            'node_modules/puppeteer/lib/puppeteer/puppeteer.js'
        )) {
            $null = New-ReportToolTestFile -Path (Join-Path $Directory $relativePath)
        }
        $null = New-ReportToolTestFile -Path (Join-Path $Directory 'node_modules/puppeteer/package.json') `
            -Content '{"main":"./lib/puppeteer/puppeteer.js"}'
    }

    # Isolate native lookup from the machine's installed runtimes and shared cache.
    function Invoke-NativeReportDiscovery {
        param(
            [string] $Skill,
            [string] $Tools,
            [string] $ActiveNodeDirectory = '',
            [string] $NvmDirectory = (Join-Path $script:NativeReportRoot 'missing-nvm'),
            [string] $XdgDirectory = '',
            [string] $HomeDirectory = '',
            [switch] $UseFallbackNvm
        )
        $start = [Diagnostics.ProcessStartInfo]::new($script:NativeReportCommand)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:NativeEmptyBin
        if ($UseFallbackNvm) { [void] $start.Environment.Remove('NVM_DIR') }
        else { $start.Environment['NVM_DIR'] = $NvmDirectory }
        if ($XdgDirectory) { $start.Environment['XDG_CONFIG_HOME'] = $XdgDirectory }
        else { [void] $start.Environment.Remove('XDG_CONFIG_HOME') }
        if ($HomeDirectory) { $start.Environment['HOME'] = $HomeDirectory }
        $start.Environment['NVM_BIN'] = if ($IsWindows) { '' } else { $ActiveNodeDirectory }
        $start.Environment['NVM_SYMLINK'] = if ($IsWindows) { $ActiveNodeDirectory } else { '' }
        foreach ($argument in @('workflow', 'test-review-report-tools',
            "--skill-root=$Skill", "--tools-directory=$Tools")) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            @{
                exitCode = $process.ExitCode
                output = $stdout.GetAwaiter().GetResult()
                error = $stderr.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
    }
}

Describe 'Native report discovery without installation' {
    BeforeEach {
        $script:NativeReportRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N') + ' native report')
        $script:NativeSkill = Join-Path $script:NativeReportRoot 'skill with spaces'
        $script:NativeTools = Join-Path $script:NativeReportRoot 'shared tools'
        $script:NativeEmptyBin = Join-Path $script:NativeReportRoot 'empty bin'
        [void] [IO.Directory]::CreateDirectory($script:NativeEmptyBin)
        $lockfile = New-ReportToolTestFile -Path (Join-Path $script:NativeSkill 'tools/package-lock.json') -Content '{}'
        $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([IO.File]::ReadAllBytes($lockfile))).ToLowerInvariant()
        $script:NativePackages = Join-Path $script:NativeTools "mermaid-$digest"
        New-ReportMermaidTestPackages -Directory $script:NativePackages
    }

    It 'shares matching lockfiles and isolates different lockfile bytes' {
        $other = Join-Path $script:NativeReportRoot 'other checkout'
        $lockfile = New-ReportToolTestFile -Path (Join-Path $other 'tools/package-lock.json') -Content '{}'
        $first = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools
        $second = Invoke-NativeReportDiscovery -Skill $other -Tools $script:NativeTools
        $first.exitCode | Should -Be 0 -Because $first.error
        $second.exitCode | Should -Be 0 -Because $second.error
        $firstRecord = $first.output | ConvertFrom-Json -AsHashtable
        $secondRecord = $second.output | ConvertFrom-Json -AsHashtable
        $firstRecord.mermaid | Should -BeExactly $secondRecord.mermaid
        $firstRecord.missing | Should -Not -Contain 'mermaid-cli'
        Test-Path -LiteralPath (Join-Path $other 'tools/node_modules') | Should -BeFalse

        [IO.File]::WriteAllText($lockfile, '{"changed":true}')
        $changed = Invoke-NativeReportDiscovery -Skill $other -Tools $script:NativeTools
        $changed.exitCode | Should -Be 0 -Because $changed.error
        $changedRecord = $changed.output | ConvertFrom-Json -AsHashtable
        $changedRecord.mermaid | Should -Not -BeExactly $firstRecord.mermaid
        $changedRecord.missing | Should -Contain 'mermaid-cli'
    }

    It 'reports missing required package file <Missing> without running a browser' -ForEach @(
        @{ Missing = 'node_modules/@mermaid-js/mermaid-cli/src/cli.js' }
        @{ Missing = 'node_modules/puppeteer/install.mjs' }
        @{ Missing = 'node_modules/puppeteer/package.json' }
        @{ Missing = 'node_modules/puppeteer/lib/puppeteer/puppeteer.js' }
    ) {
        Remove-Item -LiteralPath (Join-Path $script:NativePackages $Missing)
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools
        $result.exitCode | Should -Be 0 -Because $result.error
        $tools = $result.output | ConvertFrom-Json -AsHashtable
        $tools.missing | Should -Contain 'mermaid-cli'
        $tools.browser | Should -BeNullOrEmpty
    }

    It 'reports malformed Puppeteer manifest <Case> as requiring package installation' -ForEach @(
        @{ Case = 'empty'; Content = '' }
        @{ Case = 'whitespace'; Content = ' ' }
        @{ Case = 'truncated'; Content = '{"main":' }
        @{ Case = 'null'; Content = 'null' }
        @{ Case = 'array'; Content = '[{"main":"lib/main.js"}]' }
        @{ Case = 'string'; Content = '"text"' }
        @{ Case = 'number'; Content = '3' }
        @{ Case = 'boolean'; Content = 'false' }
        @{ Case = 'missing main'; Content = '{}' }
        @{ Case = 'non-string main'; Content = '{"main":["lib/main.js"]}' }
    ) {
        [IO.File]::WriteAllText((Join-Path $script:NativePackages 'node_modules/puppeteer/package.json'), $Content)
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools
        $result.exitCode | Should -Be 0 -Because $result.error
        $tools = $result.output | ConvertFrom-Json -AsHashtable
        $tools.missing | Should -Contain 'mermaid-cli'
        $tools.browser | Should -BeNullOrEmpty
    }

    It 'prefers a runtime on PATH to the active nvm runtime' {
        $name = if ($IsWindows) { 'node.exe' } else { 'node' }
        $pathNode = New-ReportToolTestFile -Path (Join-Path $script:NativeEmptyBin $name)
        if (-not $IsWindows) {
            [IO.File]::SetUnixFileMode($pathNode, [IO.UnixFileMode]::UserRead -bor [IO.UnixFileMode]::UserExecute)
        }
        $active = Join-Path $script:NativeReportRoot 'active node'
        $null = New-ReportToolTestFile -Path (Join-Path $active $name)
        Remove-Item -LiteralPath (Join-Path $script:NativePackages 'node_modules/puppeteer/install.mjs')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools -ActiveNodeDirectory $active
        $result.exitCode | Should -Be 0 -Because $result.error
        ($result.output | ConvertFrom-Json -AsHashtable).node | Should -BeExactly $pathNode
    }

    It 'uses the active nvm runtime and reports a stale location' {
        $active = Join-Path $script:NativeReportRoot 'active node'
        $name = if ($IsWindows) { 'node.exe' } else { 'node' }
        $node = New-ReportToolTestFile -Path (Join-Path $active $name)
        Remove-Item -LiteralPath (Join-Path $script:NativePackages 'node_modules/puppeteer/install.mjs')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools -ActiveNodeDirectory $active
        $result.exitCode | Should -Be 0 -Because $result.error
        $tools = $result.output | ConvertFrom-Json -AsHashtable
        $tools.node | Should -BeExactly $node
        $tools.missing | Should -Not -Contain 'node'

        Remove-Item -LiteralPath $node
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools -ActiveNodeDirectory $active
        $result.exitCode | Should -Be 0 -Because $result.error
        $tools = $result.output | ConvertFrom-Json -AsHashtable
        $tools.node | Should -BeNullOrEmpty
        $tools.missing | Should -Contain 'node'
    }

    It 'reports an absent POSIX shell when resolving the nvm default' -Skip:$IsWindows {
        $nvm = Join-Path $script:NativeReportRoot 'nvm'
        $null = New-ReportToolTestFile -Path (Join-Path $nvm 'nvm.sh')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools -NvmDirectory $nvm
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Match 'POSIX shell'
        $result.output | Should -BeNullOrEmpty
    }

    It 'resolves a quoted nvm root through a real shell when the active location is stale' -Skip:($IsWindows -or -not (Get-Command sh -CommandType Application -ErrorAction SilentlyContinue)) {
        $nvm = Join-Path $script:NativeReportRoot "nvm root 'quoted' `$literal"
        $node = New-ReportToolTestFile -Path (Join-Path $nvm 'versions/node/default/bin/node')
        $null = New-ReportToolTestFile -Path (Join-Path $nvm 'nvm.sh') -Content @'
if [ "$1" != '--no-use' ]; then return 1; fi
nvm() {
    if [ "$#" -ne 2 ] || [ "$1" != 'which' ] || [ "$2" != 'default' ]; then return 1; fi
    printf '%s\n' "$NVM_DIR/versions/node/default/bin/node"
}
'@
        New-Item -ItemType SymbolicLink -Path (Join-Path $script:NativeEmptyBin 'sh') -Target $script:ShellPath | Out-Null
        Remove-Item -LiteralPath (Join-Path $script:NativePackages 'node_modules/puppeteer/install.mjs')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools `
            -NvmDirectory $nvm -ActiveNodeDirectory (Join-Path $script:NativeReportRoot 'stale')
        $result.exitCode | Should -Be 0 -Because $result.error
        ($result.output | ConvertFrom-Json -AsHashtable).node | Should -BeExactly $node
    }

    It 'resolves the default from the <Location> nvm directory' -Skip:($IsWindows -or -not (Get-Command sh -CommandType Application -ErrorAction SilentlyContinue)) -ForEach @(
        @{ Location = 'XDG' }
        @{ Location = 'home' }
    ) {
        $homeDirectory = Join-Path $script:NativeReportRoot 'home'
        $xdg = if ($Location -eq 'XDG') { Join-Path $script:NativeReportRoot 'config' } else { '' }
        $nvm = if ($xdg) { Join-Path $xdg 'nvm' } else { Join-Path $homeDirectory '.nvm' }
        $node = New-ReportToolTestFile -Path (Join-Path $nvm 'versions/node/default/bin/node')
        $null = New-ReportToolTestFile -Path (Join-Path $nvm 'nvm.sh') -Content @'
nvm() { printf '%s\n' "$NVM_DIR/versions/node/default/bin/node"; }
'@
        New-Item -ItemType SymbolicLink -Path (Join-Path $script:NativeEmptyBin 'sh') -Target $script:ShellPath | Out-Null
        Remove-Item -LiteralPath (Join-Path $script:NativePackages 'node_modules/puppeteer/install.mjs')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools `
            -UseFallbackNvm -XdgDirectory $xdg -HomeDirectory $homeDirectory
        $result.exitCode | Should -Be 0 -Because $result.error
        ($result.output | ConvertFrom-Json -AsHashtable).node | Should -BeExactly $node
    }

    It 'does not search a fallback when an explicit nvm directory is missing' -Skip:$IsWindows {
        $homeDirectory = Join-Path $script:NativeReportRoot 'home'
        $null = New-ReportToolTestFile -Path (Join-Path $homeDirectory '.nvm/nvm.sh')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools `
            -HomeDirectory $homeDirectory -NvmDirectory (Join-Path $script:NativeReportRoot 'missing')
        $result.exitCode | Should -Be 0 -Because $result.error
        ($result.output | ConvertFrom-Json -AsHashtable).node | Should -BeNullOrEmpty
    }

    It 'rejects nvm output <Case> without selecting another runtime' -Skip:($IsWindows -or -not (Get-Command sh -CommandType Application -ErrorAction SilentlyContinue)) -ForEach @(
        @{ Case = 'empty'; Body = "nvm() { printf ''; }"; Diagnostic = 'did not return an installed' }
        @{ Case = 'N/A'; Body = "nvm() { printf 'N/A'; }"; Diagnostic = 'did not return an installed' }
        @{ Case = 'relative path'; Body = "nvm() { printf 'relative-node'; }"; Diagnostic = 'did not return an installed' }
        @{ Case = 'missing absolute path'; Body = 'nvm() { printf "%s" "$NVM_DIR/missing"; }'; Diagnostic = 'did not return an installed' }
        @{ Case = 'failure'; Body = "nvm() { printf 'version unavailable' >&2; return 3; }"; Diagnostic = 'version unavailable' }
    ) {
        $nvm = Join-Path $script:NativeReportRoot 'nvm'
        $null = New-ReportToolTestFile -Path (Join-Path $nvm 'nvm.sh') -Content $Body
        New-Item -ItemType SymbolicLink -Path (Join-Path $script:NativeEmptyBin 'sh') -Target $script:ShellPath | Out-Null
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools -NvmDirectory $nvm
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Match $Diagnostic
        $result.output | Should -BeNullOrEmpty
    }

    It 'rejects a missing dependency lockfile without installing anything' {
        Remove-Item -LiteralPath (Join-Path $script:NativeSkill 'tools/package-lock.json')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools
        $result.exitCode | Should -Not -Be 0
        $result.error | Should -Match 'lockfile'
        $result.output | Should -BeNullOrEmpty
    }

    It 'reports missing tools as one JSON record without installation' {
        Remove-Item -LiteralPath (Join-Path $script:NativePackages 'node_modules/puppeteer/install.mjs')
        $result = Invoke-NativeReportDiscovery -Skill $script:NativeSkill -Tools $script:NativeTools
        $result.exitCode | Should -Be 0 -Because $result.error
        $tools = ConvertFrom-Json -InputObject $result.output -AsHashtable
        $tools.missing | Should -Contain 'pandoc'
        $tools.missing | Should -Contain 'node'
        $tools.missing | Should -Contain 'mermaid-cli'
        $tools.diagnostics | Should -BeNullOrEmpty
    }

    It 'fails explicitly instead of installing without a runtime' {
        $start = [Diagnostics.ProcessStartInfo]::new($script:NativeReportCommand)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.Environment['PATH'] = $script:NativeEmptyBin
        $start.Environment['NVM_DIR'] = Join-Path $script:NativeReportRoot 'missing-nvm'
        $start.Environment['NVM_BIN'] = ''
        $start.Environment['NVM_SYMLINK'] = ''
        foreach ($argument in @('workflow', 'install-review-report-tools',
            "--skill-root=$script:NativeSkillRoot", "--tools-directory=$script:NativeTools")) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $output = $process.StandardOutput.ReadToEndAsync()
            $errors = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $process.ExitCode | Should -Not -Be 0
            $errors.GetAwaiter().GetResult() | Should -Match 'Node\.js'
            $output.GetAwaiter().GetResult() | Should -BeNullOrEmpty
        } finally { $process.Dispose() }
        @(Get-ChildItem -LiteralPath $script:NativeReportRoot -Filter node_modules -Directory -Recurse |
            Where-Object { $_.FullName -notlike "$script:NativePackages*" }).Count | Should -Be 0
    }
}
