#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    . "$script:Skill\scripts\lib\JsonValue.ps1"
    . "$script:Skill\scripts\lib\PullRequestModel.ps1"
    . "$script:Skill\scripts\lib\ReviewWork.ps1"

    # Fixture spans use real line positions so validation and copying exercise the same contract.
    function New-TestSpan {
        param([string] $Path, [int] $Start, [int] $End, [string] $Root = 'instruction-set-raw')
        [ordered]@{ root = $Root; path = $Path; startLine = $Start; endLine = $End }
    }

    # Provides complete explicit defaults, including the empty arrays implicated in the report.
    function New-TestApplicability {
        [ordered]@{
            effectiveScope = 'No explicit restriction; interpret the guideline in context.'
            declarations = @()
            origins = @()
            uncertainties = @()
        }
    }

    # Keeps test chapters independent while assigning original-source attribution mechanically.
    function New-TestChapter {
        param([string] $Id, [string] $Category, [string] $Path, [int] $Start, [int] $End)
        [ordered]@{
            id = $Id
            categoryId = $Category
            content = New-TestSpan $Path $Start $End
            sources = @((New-TestSpan $Path $Start $End 'instruction-set-source'))
            applicability = @((New-TestApplicability))
        }
    }

    # Serializes the mutable fixture through the same verbatim JSON helpers used by the CLI.
    function Save-TestFocusMap {
        [IO.File]::WriteAllText($script:MapPath, (ConvertTo-JsonText -Value $script:Map))
    }

    # Runs public entry points with a real native argument boundary and captures failures without
    # depending on console formatting, interactive prompts, or wall-clock timeouts.
    function Start-FocusPreparationCommand {
        param([string] $Name, [string[]] $Arguments)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', "$script:Skill\scripts\$Name") + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        [Diagnostics.Process]::Start($start)
    }

    function Receive-FocusPreparationCommand {
        param([Diagnostics.Process] $Process)
        try {
            $stdout = $Process.StandardOutput.ReadToEndAsync()
            $stderr = $Process.StandardError.ReadToEndAsync()
            $Process.WaitForExit()
            [pscustomobject]@{
                ExitCode = $Process.ExitCode
                Output = $stdout.GetAwaiter().GetResult()
                Error = $stderr.GetAwaiter().GetResult()
            }
        } finally { $Process.Dispose() }
    }

    function Invoke-FocusPreparationCommand {
        param([string] $Name, [string[]] $Arguments)
        Receive-FocusPreparationCommand (Start-FocusPreparationCommand $Name $Arguments)
    }

    # Generation has native entrypoints rather than PowerShell wrappers.
    function Invoke-TestFocusNative {
        param([string[]] $Arguments)
        $native = (Get-Command ss-ai -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $start = [Diagnostics.ProcessStartInfo]::new($native)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('workflow') + $Arguments) { $start.ArgumentList.Add($argument) }
        Receive-FocusPreparationCommand ([Diagnostics.Process]::Start($start))
    }

    # The executable owns validation and rendering; inspect its published files rather than internals.
    function Invoke-TestFocusMap {
        param([string] $Path = $script:MapPath, [switch] $Complete)
        $name = if ($Complete) { 'New-FocusAreaArtifacts.ps1' } else { 'Test-FocusAreaMap.ps1' }
        $result = Invoke-FocusPreparationCommand $name @('-SessionStateDirectory', $script:Session, '-MapPath', $Path)
        if ($result.ExitCode -ne 0) { throw $result.Error }
        $result.Output | ConvertFrom-Json
    }

    function Get-TestFocusDocuments {
        $null = Invoke-TestFocusMap -Complete
        $documents = @{}
        foreach ($category in $script:Map.categories) {
            $path = "focus-areas\focus-$($category.id).md"
            $documents[$path] = [IO.File]::ReadAllText((Join-Path $script:Session $path))
        }
        $documents['instruction-set-categories.md'] = [IO.File]::ReadAllText((Join-Path $script:Session 'instruction-set-categories.md'))
        $documents
    }

    function Invoke-TestExpansion {
        param([string] $Operation, $InputRecord, [string] $ReferenceId)
        $arguments = @('-SessionStateDirectory', $script:Session, '-Operation', $Operation)
        if ($null -ne $InputRecord) {
            $inputPath = Join-Path $script:Session 'expansion-input.json'
            [IO.File]::WriteAllText($inputPath, (ConvertTo-JsonText -Value $InputRecord))
            $arguments += @('-InputPath', $inputPath)
        }
        if ($ReferenceId) { $arguments += @('-ReferenceId', $ReferenceId) }
        $result = Invoke-FocusPreparationCommand 'Update-FocusAreaExpansion.ps1' $arguments
        if ($result.ExitCode -ne 0) { throw $result.Error }
        $result.Output | ConvertFrom-Json
    }
}

Describe 'ss-review focus-area preparation' {
    BeforeEach {
        $script:Session = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:MapPath = Join-Path $script:Session 'focus-area-map.json'
        $script:ScopedText = @(
            '---'
            'applyTo: "**/*.ps1,**/*.psm1"'
            '---'
            'Scope: this file applies only to agent workflows.'
            ''
            '# Named arguments'
            ''
            'Require named arguments.'
            ''
            '## Fuzzing exception'
            ''
            'Permit bounded fuzzing.'
            ''
            '## Fuzzing exception'
            ''
            'Retain sibling exception context.'
        ) -join "`n"
        foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
            $directory = Join-Path $script:Session $root
            New-Item -ItemType Directory -Path $directory -Force | Out-Null
            # Equivalent declarations arrive with different line endings in the reported failure.
            $text = if ($root -eq 'instruction-set-raw') { $script:ScopedText.Replace("`n", "`r`n") } else { $script:ScopedText }
            [IO.File]::WriteAllText((Join-Path $directory 'scoped.md'), $text)
            [IO.File]::WriteAllText((Join-Path $directory 'general.md'), "# General`n`nKeep names clear.`n")
        }
        $script:Map = [ordered]@{
            schemaVersion = 1
            categories = @(
                [ordered]@{ id = 'scripts'; title = 'Script contracts' }
                [ordered]@{ id = 'general'; title = 'General guidance' }
            )
            chapters = @(
                (New-TestChapter 'named-arguments' 'scripts' 'scoped.md' 6 8)
                (New-TestChapter 'fuzzing' 'scripts' 'scoped.md' 10 12)
                (New-TestChapter 'sibling' 'scripts' 'scoped.md' 14 16)
                (New-TestChapter 'general' 'general' 'general.md' 1 3)
            )
        }
        foreach ($chapter in $script:Map.chapters[0..2]) {
            $chapter.applicability[0].effectiveScope = 'Agent workflows matching either **/*.ps1 or **/*.psm1.'
            $chapter.applicability[0].declarations = @((New-TestSpan 'scoped.md' 1 4))
        }
        Save-TestFocusMap
        [IO.File]::WriteAllText((Join-Path $script:Session 'focus-area-expansion-status.json'), '{"schemaVersion":1,"state":"Complete","references":[]}')
    }

    It 'allows a shared PowerShell owner while <CommandName> reads' -ForEach @(
        @{ CommandName = 'Test-FocusAreaMap.ps1' }
        @{ CommandName = 'Update-FocusAreaMap.ps1' }
        @{ CommandName = 'New-FocusAreaArtifacts.ps1' }
        @{ CommandName = 'Update-FocusAreaExpansion.ps1' }
    ) {
        # A separate process owns the shared descriptor so readers cannot inherit it.
        $holderPath = Join-Path $script:Session 'publisher.ps1'
        [IO.File]::WriteAllText($holderPath, @'
#Requires -Version 7.6
param([string] $Library, [string] $Session)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
. $Library
$lock = Open-PullRequestFileLock -Path (Join-Path $Session 'focus-area-cache-operation') -Shared
try {
    [Console]::Out.WriteLine('ready')
    [Console]::Out.Flush()
    $null = [Console]::In.ReadLine()
} finally {
    $lock.Dispose()
}
'@)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardInput = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($arg in @('-NoProfile', '-NonInteractive', '-File', $holderPath, '-Library', "$script:Skill\scripts\lib\PullRequestModel.ps1", '-Session', $script:Session)) {
            $start.ArgumentList.Add($arg)
        }
        $holder = [Diagnostics.Process]::Start($start)
        $arguments = @('-SessionStateDirectory', $script:Session)
        switch ($CommandName) {
            'Update-FocusAreaExpansion.ps1' { $arguments += @('-Operation', 'Read') }
            'Update-FocusAreaMap.ps1' {
                $arguments += @('-Path', (Join-Path $script:Session 'merged-map.json'), '-Operation', 'Append', '-InputPath', $script:MapPath)
            }
            default { $arguments += @('-MapPath', $script:MapPath) }
        }
        try {
            $holder.StandardOutput.ReadLine() | Should -BeExactly 'ready'
            {
                $probe = [IO.File]::Open((Join-Path $script:Session 'focus-area-cache-operation.lock'), 'Open', 'ReadWrite', 'None')
                $probe.Dispose()
            } | Should -Throw
            $result = Invoke-FocusPreparationCommand $CommandName $arguments
            $result.ExitCode | Should -Be 0 -Because $result.Error
            $holder.StandardInput.WriteLine('release')
            $holder.StandardInput.Flush()
            $holder.WaitForExit()
            $holder.ExitCode | Should -Be 0 -Because $holder.StandardError.ReadToEnd()
        } finally {
            if (-not $holder.HasExited) {
                $holder.StandardInput.Close()
                $holder.WaitForExit()
            }
            $holder.Dispose()
        }
    }

    It 'renders exact headings, source lines, frontmatter and independent scopes with LF endings' {
        $documents = Get-TestFocusDocuments
        $scripts = $documents['focus-areas\focus-scripts.md']
        $scripts | Should -Match '(?m)^# Named arguments$'
        ([regex]::Matches($scripts, '(?m)^## Fuzzing exception$')).Count | Should -Be 4 # Content and quoted source headings.
        $scripts | Should -Match 'Require named arguments\.'
        $scripts | Should -Match 'Retain sibling exception context\.'
        $scripts | Should -Match ([regex]::Escape(($script:ScopedText -split "`n")[0..3] -join "`n"))
        $scripts | Should -Match 'Source: instruction-set-source\\scoped\.md:10-12'
        $scripts | Should -Not -Match "`r"
        $documents['focus-areas\focus-general.md'] | Should -Match 'Effective scope: No explicit restriction; interpret the guideline in context\.'
        $documents['focus-areas\focus-general.md'] | Should -Not -Match 'agent workflows'
        $documents['instruction-set-categories.md'] | Should -Match 'Content: instruction-set-raw\\scoped\.md:14-16'
    }

    It 'keeps reference alternatives, original exception context and uncertainty evidence explicit' {
        $context = $script:Map.chapters[0].applicability[0]
        $context.declarations += New-TestSpan 'scoped.md' 14 16 'instruction-set-source'
        $context.origins = @('https://example.invalid/referrer', 'https://example.invalid/guidance')
        $alternative = New-TestApplicability
        $alternative.effectiveScope = 'Published scripts; applicability outside publication is uncertain.'
        $alternative.declarations = @((New-TestSpan 'general.md' 1 3 'instruction-set-source'))
        $alternative.origins = @('instruction-set-source\general.md', 'https://example.invalid/guidance')
        $alternative.uncertainties = @([ordered]@{
            reason = 'The referring contexts differ.'
            evidence = @((New-TestSpan 'scoped.md' 4 4 'instruction-set-source'))
        })
        $script:Map.chapters[0].applicability += $alternative
        Save-TestFocusMap
        $metadata = (Get-TestFocusDocuments)['focus-areas\focus-scripts.md']
        $metadata | Should -Match 'Applicability alternative 2:'
        $metadata | Should -Match 'Scope uncertainty: The referring contexts differ\.'
        $metadata | Should -Match 'Scope: this file applies only to agent workflows\.'
        $metadata | Should -Match 'Retain sibling exception context\.'
        $metadata | Should -Match "https://example.invalid/referrer`nhttps://example.invalid/guidance"
        $metadata | Should -Match 'Source: instruction-set-source\\scoped.md:6-8'
    }

    It 'retains mixed scopes, leading source whitespace, and Unicode without newline-dependent comparisons' {
        $script:Map.chapters[3].categoryId = 'scripts'
        $script:Map.categories = @($script:Map.categories[0])
        $script:Map.chapters[0].sources[0].startLine = 5
        # Generate non-ASCII input without tying the test file's encoding to the fixture's encoding.
        $text = "# General`r`n`r`nName: $([char] 0x00E4)."
        foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
            [IO.File]::WriteAllText((Join-Path $script:Session "$root\general.md"), $text)
        }
        $script:Map.chapters[0].applicability[0].effectiveScope += "`r`nRetain the declared alternatives."
        Save-TestFocusMap
        $documents = Get-TestFocusDocuments
        $output = $documents['focus-areas\focus-scripts.md']
        $output | Should -Match ([regex]::Escape("Name: $([char] 0x00E4)."))
        $output | Should -Match 'Effective scope: Agent workflows matching either'
        $output | Should -Match 'Effective scope: No explicit restriction; interpret the guideline in context\.'
        $output | Should -Not -Match "`r"
    }

    It 'rejects schema drift before rendering: <Case>' -ForEach @(
        @{ Case = 'missing uncertainty'; Change = { $script:Map.chapters[0].applicability[0].Remove('uncertainties') } }
        @{ Case = 'null uncertainty'; Change = { $script:Map.chapters[0].applicability[0].uncertainties = $null } }
        @{ Case = 'range alias'; Change = { $script:Map.chapters[0].content['sourceLineRange'] = @(6, 8) } }
        @{ Case = 'copied text alias'; Change = { $script:Map.chapters[0].applicability[0]['declarationContextVerbatim'] = 'Scope' } }
        @{ Case = 'unknown version'; Change = { $script:Map.schemaVersion = 2 } }
        @{ Case = 'scalar sources'; Change = { $script:Map.chapters[0].sources = $script:Map.chapters[0].sources[0] } }
        @{ Case = 'missing evidence'; Change = { $script:Map.chapters[0].applicability[0].uncertainties = @(@{ reason = 'Unclear'; evidence = @() }) } }
        @{ Case = 'fractional line'; Change = { $script:Map.chapters[0].content.startLine = 6.5 } }
    ) {
        & $Change
        Save-TestFocusMap
        { Invoke-TestFocusMap } | Should -Throw
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas') | Should -BeFalse
    }

    It 'rejects invalid source or assignment data: <Case>' -ForEach @(
        @{ Case = 'duplicate chapter'; Change = { $script:Map.chapters[1].id = 'named-arguments' }; Error = '*duplicate chapter*' }
        @{ Case = 'duplicate category'; Change = { $script:Map.categories[1].id = 'scripts' }; Error = '*duplicate category*' }
        @{ Case = 'blank title'; Change = { $script:Map.categories[0].title = ' ' }; Error = '*category*' }
        @{ Case = 'unknown category'; Change = { $script:Map.chapters[0].categoryId = 'missing' }; Error = '*category*' }
        @{ Case = 'overlap'; Change = { $script:Map.chapters[1].content.startLine = 8 }; Error = '*Overlapping*chapter*' }
        @{ Case = 'past EOF'; Change = { $script:Map.chapters[0].content.endLine = 99 }; Error = '*line*' }
        @{ Case = 'reversed range'; Change = { $script:Map.chapters[0].content.endLine = 5 }; Error = '*line*' }
        @{ Case = 'wrong root'; Change = { $script:Map.chapters[0].content.root = 'instruction-set-source' }; Error = '*raw source*' }
        @{ Case = 'traversal'; Change = { $script:Map.chapters[0].content.path = '..\scoped.md' }; Error = '*path*' }
        @{ Case = 'absolute path'; Change = { $script:Map.chapters[0].content.path = 'C:\scoped.md' }; Error = '*path*' }
        @{ Case = 'missing file'; Change = { $script:Map.chapters[0].content.path = 'missing.md' }; Error = '*not found*' }
        @{ Case = 'incorrect spelling'; Change = { $script:Map.chapters[0].content.path = 'SCOPED.md' }; Error = '*exact spelling*' }
    ) {
        & $Change
        Save-TestFocusMap
        $result = Invoke-FocusPreparationCommand 'Test-FocusAreaMap.ps1' @('-SessionStateDirectory', $script:Session, '-MapPath', $script:MapPath)
        $result.ExitCode | Should -Not -Be 0
        # PowerShell can wrap native diagnostics and insert a gutter between words.
        $pattern = [regex]::Escape($Error).Replace('\*', '(?s:.*?)').Replace('\ ', '(?s:.*?)')
        $result.Error | Should -Match $pattern
    }

    It 'accepts partial worker maps but rejects incomplete merged coverage and empty categories' {
        $script:Map.chapters = @($script:Map.chapters[0])
        Save-TestFocusMap
        { Invoke-TestFocusMap } | Should -Throw '*has no chapters*'
        $script:Map.categories = @($script:Map.categories[0])
        Save-TestFocusMap
        $partial = Invoke-TestFocusMap
        $partial.chapterCount | Should -Be 1
        $result = Invoke-FocusPreparationCommand 'New-FocusAreaArtifacts.ps1' @('-SessionStateDirectory', $script:Session, '-MapPath', $script:MapPath)
        $result.ExitCode | Should -Not -Be 0
        $result.Error | Should -Match '(?s)Uncovered.*?raw.*?input.*?line'
    }

    It 'handles heading-only, setext and unheaded guidance without heading reconstruction' {
        foreach ($text in @('### Heading only', "Setext heading`n===`nBody", 'Unheaded guidance')) {
            foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
                [IO.File]::WriteAllText((Join-Path $script:Session "$root\general.md"), $text)
            }
            $count = ($text -split "`n").Count
            $script:Map.chapters[3] = New-TestChapter 'general' 'general' 'general.md' 1 $count
            Save-TestFocusMap
            $documents = Get-TestFocusDocuments
            $output = $documents['focus-areas\focus-general.md']
            $output | Should -Match ([regex]::Escape(($text -split "`n")[0]))
            $output | Should -Match '<!-- end ss-review scope metadata -->'
            $output | Should -Not -Match "`r"
            if ($text.StartsWith('Setext')) { $output | Should -Match "Setext heading`n===" }
        }
    }

    It 'quotes embedded Markdown fences without rewriting their contents' {
        $text = "Scope example:`n``````powershell`nWrite-Output '`$u'`n``````"
        foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
            [IO.File]::WriteAllText((Join-Path $script:Session "$root\general.md"), $text)
        }
        $script:Map.chapters[3] = New-TestChapter 'general' 'general' 'general.md' 1 4
        $script:Map.chapters[3].applicability[0].declarations = @((New-TestSpan 'general.md' 1 4))
        Save-TestFocusMap
        $output = (Get-TestFocusDocuments)['focus-areas\focus-general.md']
        $output | Should -Match ([regex]::Escape(('````' + "`n$text`n" + '````')))
    }

    It 'enforces the category limit at complete assembly, not during worker categorization' {
        # Exercise the exact limit and its first rejected value from prepare-focus-areas.md.
        foreach ($count in @(128, 129)) {
            $text = (1..$count | ForEach-Object { "# Guideline $_" }) -join "`n"
            foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
                [IO.File]::WriteAllText((Join-Path $script:Session "$root\many.md"), $text)
            }
            $script:Map.categories = @(1..$count | ForEach-Object { @{ id = "category-$_"; title = "Category $_" } })
            $script:Map.chapters = @(1..$count | ForEach-Object { New-TestChapter "chapter-$_" "category-$_" 'many.md' $_ $_ })
            # Existing raw fixtures still require coverage, without increasing the category count.
            $script:Map.chapters += New-TestChapter 'scoped' 'category-1' 'scoped.md' 1 16
            $script:Map.chapters += New-TestChapter 'general' 'category-1' 'general.md' 1 3
            Save-TestFocusMap
            (Invoke-TestFocusMap).categoryCount | Should -Be $count
            if ($count -eq 128) {
                (Invoke-TestFocusMap -Complete).categoryCount | Should -Be $count
            } else {
                { Invoke-TestFocusMap -Complete } | Should -Throw '*categories*'
            }
        }
    }

    It 'assembles through the CLI, preserves outputs on invalid input, and removes obsolete categories on retry' {
        $arguments = @('-SessionStateDirectory', $script:Session, '-MapPath', $script:MapPath)
        $validated = Invoke-FocusPreparationCommand 'Test-FocusAreaMap.ps1' $arguments
        $validated.ExitCode | Should -Be 0 -Because $validated.Error
        ($validated.Output | ConvertFrom-Json).status | Should -Be 'Valid'
        $assembled = Invoke-FocusPreparationCommand 'New-FocusAreaArtifacts.ps1' $arguments
        $assembled.ExitCode | Should -Be 0 -Because $assembled.Error
        ($assembled.Output | ConvertFrom-Json).status | Should -Be 'Assembled'
        ($assembled.Output | ConvertFrom-Json).chapterCount | Should -Be 4
        $catalog = Join-Path $script:Session 'instruction-set-categories.md'
        $before = [IO.File]::ReadAllText($catalog)
        $script:Map.chapters[0].content.endLine = 99
        Save-TestFocusMap
        (Invoke-FocusPreparationCommand 'New-FocusAreaArtifacts.ps1' $arguments).ExitCode | Should -Not -Be 0
        [IO.File]::ReadAllText($catalog) | Should -BeExactly $before

        $script:Map.chapters[0].content.endLine = 8
        $script:Map.chapters[3].categoryId = 'scripts'
        $script:Map.categories = @($script:Map.categories[0])
        Save-TestFocusMap
        (Invoke-FocusPreparationCommand 'New-FocusAreaArtifacts.ps1' $arguments).ExitCode | Should -Be 0
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas\focus-general.md') | Should -BeFalse
        @(Get-ChildItem -LiteralPath $script:Session -Directory -Filter '.focus-area-assembly-*').Count | Should -Be 0
    }

    # Native focus_area_map::tests::partial_swap_failures_restore_both_previous_artifacts
    # verifies rollback after an installation failure.

    It 'rejects linked inputs and outputs without writing through them' {
        $outside = Join-Path $TestDrive 'outside'
        New-Item -ItemType Directory -Path $outside | Out-Null
        $marker = Join-Path $outside 'marker.md'
        Set-Content -LiteralPath $marker -Value 'Keep outside content.'
        # Junctions are available without elevation on Windows; use ordinary symlinks elsewhere.
        $linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $script:Session 'instruction-set-raw\linked'
        New-Item -ItemType $linkType -Path $link -Target $outside | Out-Null
        { Invoke-TestFocusMap } | Should -Throw '*must not contain links*'
        Remove-Item -LiteralPath $link
        $outputLink = Join-Path $script:Session 'focus-areas'
        New-Item -ItemType $linkType -Path $outputLink -Target $outside | Out-Null
        try {
            { Invoke-TestFocusMap -Complete } | Should -Throw '*must not contain links*'
            (Get-Content -LiteralPath $marker -Raw).Trim() | Should -BeExactly 'Keep outside content.'
        } finally {
            Remove-Item -LiteralPath $outputLink
        }
    }

    It 'rejects positional arguments and missing mandatory parameters without waiting for input' {
        (Invoke-FocusPreparationCommand 'Test-FocusAreaMap.ps1' @($script:Session, $script:MapPath)).ExitCode | Should -Not -Be 0
        (Invoke-FocusPreparationCommand 'New-FocusAreaArtifacts.ps1' @('-MapPath', $script:MapPath)).ExitCode | Should -Not -Be 0
    }

    It 'assembles worker maps through prepared append, regroup and correction commands' {
        $merged = Join-Path $script:Session 'merged-map.json'
        $arguments = @('-SessionStateDirectory', $script:Session, '-Path', $merged, '-Operation', 'Append', '-InputPath', $script:MapPath)
        foreach ($attempt in 1..2) {
            $run = Invoke-FocusPreparationCommand 'Update-FocusAreaMap.ps1' $arguments
            $run.ExitCode | Should -Be 0 -Because $run.Error
            ($run.Output | ConvertFrom-Json).chapterCount | Should -Be 4
        }
        $run = Invoke-FocusPreparationCommand 'Update-FocusAreaMap.ps1' @(
            '-SessionStateDirectory', $script:Session, '-Path', $merged, '-Operation', 'Regroup',
            '-CategoryId', 'general', '-NewCategoryId', 'scripts', '-Title', 'Script contracts'
        )
        $run.ExitCode | Should -Be 0 -Because $run.Error
        (Invoke-TestFocusMap -Path $merged -Complete).categoryCount | Should -Be 1
        $combined = Get-Content -LiteralPath $merged -Raw | ConvertFrom-Json
        $combined.categories.Count | Should -Be 1
        $combined.chapters[3].applicability[0].effectiveScope | Should -BeExactly 'No explicit restriction; interpret the guideline in context.'
        $script:Map.categories = @($script:Map.categories[0])
        $script:Map.chapters = @($script:Map.chapters[0])
        $script:Map.chapters[0].applicability[0].effectiveScope = 'Corrected scope judgment.'
        Save-TestFocusMap
        $run = Invoke-FocusPreparationCommand 'Update-FocusAreaMap.ps1' @(
            '-SessionStateDirectory', $script:Session, '-Path', $merged, '-Operation', 'Replace', '-InputPath', $script:MapPath
        )
        $run.ExitCode | Should -Be 0 -Because $run.Error
        (Invoke-TestFocusMap -Path $merged -Complete).chapterCount | Should -Be 4
        $combined = Get-Content -LiteralPath $merged -Raw | ConvertFrom-Json
        $combined.chapters.Count | Should -Be 4
        $combined.chapters[0].applicability[0].effectiveScope | Should -BeExactly 'Corrected scope judgment.'
    }

    It 'validates inherited reference chains and rejects invalidated sources without changing published artifacts' {
        foreach ($name in @('parent.md', 'child.md')) {
            [IO.File]::WriteAllText((Join-Path $script:Session "instruction-set-raw\$name"), "Guidance $name.")
        }
        Remove-Item -LiteralPath (Join-Path $script:Session 'focus-area-expansion-status.json')
        $null = Invoke-TestExpansion Initialize
        $declaration = New-TestSpan 'scoped.md' 1 4 'instruction-set-source'
        $parent = @{
            id = 'parent'; uri = 'https://example.invalid/parent'
            referrers = @(@{ parentId = $null; source = New-TestSpan 'scoped.md' 4 4 'instruction-set-source'; scope = 'Scripts'; declarations = @($declaration) })
        }
        $child = @{
            id = 'child'; uri = 'https://example.invalid/child'
            referrers = @(@{ parentId = 'parent'; source = New-TestSpan 'parent.md' 1 1; scope = 'Nested'; declarations = @() })
        }
        $null = Invoke-TestExpansion Register $parent
        $null = Invoke-TestExpansion Complete @{ referenceId = 'parent'; outputs = @('parent.md'); error = $null }
        $null = Invoke-TestExpansion Register $child
        $null = Invoke-TestExpansion Complete @{ referenceId = 'child'; outputs = @('child.md'); error = $null }
        $read = Invoke-TestExpansion Read -ReferenceId 'child'
        $read.contexts.Count | Should -Be 1
        $read.contexts[0].chain.Count | Should -Be 2
        (Invoke-TestExpansion Finalize).state | Should -Be 'Complete'

        foreach ($name in @('parent', 'child')) {
            $chapter = New-TestChapter $name 'scripts' "$name.md" 1 1
            $chapter.sources = @((New-TestSpan "$name.md" 1 1))
            $chapter.applicability[0].declarations = @($declaration)
            $chapter.applicability[0].origins = @('https://example.invalid/parent')
            if ($name -eq 'child') { $chapter.applicability[0].origins += 'https://example.invalid/child' }
            $script:Map.chapters += $chapter
        }
        Save-TestFocusMap
        $documents = Get-TestFocusDocuments
        $catalog = Join-Path $script:Session 'instruction-set-categories.md'
        $before = [IO.File]::ReadAllText($catalog)
        $script:Map.chapters[-1].applicability[0].origins = @('https://example.invalid/parent')
        Save-TestFocusMap
        { Invoke-TestFocusMap -Complete } | Should -Throw '*alternative*'
        [IO.File]::ReadAllText($catalog) | Should -BeExactly $before
        $script:Map.chapters[-1].applicability[0].origins += 'https://example.invalid/child'
        Save-TestFocusMap
        [IO.File]::WriteAllText((Join-Path $script:Session 'instruction-set-raw\parent.md'), '')
        { Invoke-TestFocusMap -Complete } | Should -Throw
        [IO.File]::ReadAllText($catalog) | Should -BeExactly $before
        [IO.File]::ReadAllText((Join-Path $script:Session 'focus-areas\focus-scripts.md')) |
            Should -BeExactly $documents['focus-areas\focus-scripts.md']
    }

    It 'produces applicability records readable and invalidated by the PowerShell review consumer' {
        foreach ($chapter in $script:Map.chapters[0..2]) {
            $chapter.applicability[0]['pathScope'] = @{ base = '.'; include = @('**/*.ps1') }
        }
        Save-TestFocusMap
        $null = Get-TestFocusDocuments
        $path = Join-Path $script:Session 'focus-areas\focus-scripts.md'
        $map = Read-ReviewApplicability -Path $path
        $map.chapters.Count | Should -Be 3
        (Test-ReviewPathScope -Scope $map.chapters[0].applicability[0].pathScope -RelativePath 'src/example.ps1') | Should -BeTrue
        (Test-ReviewPathScope -Scope $map.chapters[0].applicability[0].pathScope -RelativePath 'src/example.rs') | Should -BeFalse
        [IO.File]::AppendAllText($path, 'Modified guidance.')
        { Read-ReviewApplicability -Path $path } | Should -Throw '*Stale applicability metadata*'
    }

    It 'round trips assembled artifacts through the cache without requiring categorization intermediates' {
        $realSession = $script:Session
        $originalInstructions = $env:COPILOT_CUSTOM_INSTRUCTIONS_DIRS
        # Snapshot only the skill's built-in inputs; the fixture must not depend on user settings.
        $env:COPILOT_CUSTOM_INSTRUCTIONS_DIRS = ''
        try {
            $snapshot = Invoke-FocusPreparationCommand 'Copy-ReviewInstructionSources.ps1' @('-SessionStateDirectory', $realSession)
            $snapshot.ExitCode | Should -Be 0 -Because $snapshot.Error
            $source = $snapshot.Output | ConvertFrom-Json
            $cacheArguments = @(
                '-RecipePath', (Join-Path $script:Skill 'prepare-focus-areas.md'),
                '-CacheDirectory', (Join-Path $TestDrive 'cache'),
                '-GeneratorProfile', 'test',
                '-FreshnessDays', '7',
                '-NowUtc', '2026-10-06T12:00:00Z'
            )
            $initial = Invoke-FocusPreparationCommand 'Restore-FocusAreaCache.ps1' (@(
                '-SessionStateDirectory', $realSession, '-InputDirectory', $source.sourceDirectory,
                '-SnapshotIdentity', $source.snapshotIdentity
            ) + $cacheArguments)
            $initial.ExitCode | Should -Be 0 -Because $initial.Error
            $restoration = $initial.Output | ConvertFrom-Json
            $restoration.status | Should -Be 'Miss'
            $identity = $restoration.preparationIdentity
            $identity | Should -Not -BeNullOrEmpty
            $begin = Invoke-TestFocusNative @('begin-focus-area-generation', "--session-state=$realSession", "--preparation-identity=$identity")
            $begin.ExitCode | Should -Be 0 -Because $begin.Error
            $generation = $begin.Output | ConvertFrom-Json
            $script:Session = $generation.sessionState
            $script:MapPath = Join-Path $script:Session 'focus-area-map.json'
            $null = Invoke-TestExpansion Initialize
            $null = Invoke-TestExpansion Finalize
            $rawRoot = Join-Path $script:Session 'instruction-set-raw'
            $files = @(Get-ChildItem -LiteralPath $rawRoot -Recurse -File | Sort-Object FullName)
            $chapters = @()
            foreach ($file in $files) {
                $lines = [IO.File]::ReadAllLines($file.FullName)
                if (-not ($lines | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) { continue }
                $relative = [IO.Path]::GetRelativePath($rawRoot, $file.FullName)
                $chapters += New-TestChapter "chapter-$($chapters.Count)" 'all' $relative 1 $lines.Count
            }
            $chapters.Count | Should -BeGreaterThan 0
            $script:Map = [ordered]@{
                schemaVersion = 1
                categories = @(@{ id = 'all'; title = 'All guidance' })
                chapters = $chapters
            }
            Save-TestFocusMap
            $documents = Get-TestFocusDocuments
            $publish = Invoke-TestFocusNative @('publish-focus-area-generation', "--session-state=$realSession", "--generation-id=$($generation.generationId)")
            $publish.ExitCode | Should -Be 0 -Because $publish.Error
            ($publish.Output | ConvertFrom-Json).status | Should -Be 'Published'
            $validation = Invoke-FocusPreparationCommand 'Test-FocusAreaArtifacts.ps1' @(
                '-SessionStateDirectory', $realSession, '-PreparationIdentity', $identity
            )
            $validation.ExitCode | Should -Be 0 -Because $validation.Error
            ($validation.Output | ConvertFrom-Json).status | Should -Be 'Valid'
            $save = Invoke-FocusPreparationCommand 'Save-FocusAreaCache.ps1' (@(
                '-SessionStateDirectory', $realSession, '-InputDirectory', $source.sourceDirectory,
                '-PreparationIdentity', $identity
            ) + $cacheArguments)
            $save.ExitCode | Should -Be 0 -Because $save.Error
            ($save.Output | ConvertFrom-Json).status | Should -Be 'Stored' -Because $save.Output
            $restored = Join-Path $TestDrive 'restored'
            [void] [IO.Directory]::CreateDirectory($restored)
            $nextSnapshot = Invoke-FocusPreparationCommand 'Copy-ReviewInstructionSources.ps1' @('-SessionStateDirectory', $restored)
            $nextSnapshot.ExitCode | Should -Be 0 -Because $nextSnapshot.Error
            $nextSource = $nextSnapshot.Output | ConvertFrom-Json
            $restore = Invoke-FocusPreparationCommand 'Restore-FocusAreaCache.ps1' (@(
                '-SessionStateDirectory', $restored, '-InputDirectory', $nextSource.sourceDirectory,
                '-SnapshotIdentity', $nextSource.snapshotIdentity
            ) + $cacheArguments)
            $restore.ExitCode | Should -Be 0 -Because $restore.Error
            $hit = $restore.Output | ConvertFrom-Json
            $hit.status | Should -Be 'Hit'
            $validation = Invoke-FocusPreparationCommand 'Test-FocusAreaArtifacts.ps1' @(
                '-SessionStateDirectory', $restored, '-PreparationIdentity', $hit.preparationIdentity
            )
            $validation.ExitCode | Should -Be 0 -Because $validation.Error
            ($validation.Output | ConvertFrom-Json).status | Should -Be 'Valid'
            foreach ($path in $documents.Keys) {
                [IO.File]::ReadAllText((Join-Path $restored $path)) | Should -BeExactly $documents[$path]
            }
            Test-Path -LiteralPath (Join-Path $restored 'focus-area-map.json') | Should -BeFalse
        } finally {
            $script:Session = $realSession
            $env:COPILOT_CUSTOM_INSTRUCTIONS_DIRS = $originalInstructions
        }
    }
}
