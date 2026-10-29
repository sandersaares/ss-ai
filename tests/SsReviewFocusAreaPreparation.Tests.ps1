BeforeAll {
    $script:Skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review'
    . "$script:Skill\scripts\lib\JsonValue.ps1"
    . "$script:Skill\scripts\lib\PullRequestModel.ps1"
    . "$script:Skill\scripts\lib\FocusAreaPreparation.ps1"
    . "$script:Skill\scripts\lib\FocusAreaCache.ps1"
    $script:MoveItemCommand = Get-Command Move-Item -CommandType Cmdlet
    $script:GetChildItemCommand = Get-Command Get-ChildItem -CommandType Cmdlet
    $script:ConvertToJsonCommand = Get-Command ConvertTo-Json -CommandType Cmdlet

    # Probes actual sharing at the index and final-output boundaries without retrying or waiting.
    function Assert-TestSnapshotReaderOwnership {
        $reader = [IO.File]::Open("$($global:FocusAreaSnapshotTest.LockPath).lock", 'Open', 'Read', 'Read')
        $reader.Dispose()
        {
            $writer = [IO.File]::Open("$($global:FocusAreaSnapshotTest.LockPath).lock", 'Open', 'ReadWrite', 'None')
            $writer.Dispose()
        } | Should -Throw
    }

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
    function Invoke-FocusPreparationCommand {
        param([string] $Name, [string[]] $Arguments)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', "$script:Skill\scripts\$Name") + $Arguments) {
            $start.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            [pscustomobject]@{
                ExitCode = $process.ExitCode
                Output = $stdout.GetAwaiter().GetResult()
                Error = $stderr.GetAwaiter().GetResult()
            }
        } finally { $process.Dispose() }
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

    It 'waits for snapshot publication and holds shared ownership throughout <CommandName>' -ForEach @(
        @{ CommandName = 'Test-FocusAreaMap.ps1' }
        @{ CommandName = 'Update-FocusAreaMap.ps1' }
        @{ CommandName = 'New-FocusAreaArtifacts.ps1' }
        @{ CommandName = 'Update-FocusAreaExpansion.ps1' }
    ) {
        # Executables have their own script scopes; share fixture state across only this test's
        # mocked command boundaries and remove it when the transaction finishes.
        $global:FocusAreaSnapshotTest = @{
            LockPath = Join-Path $script:Session 'focus-area-cache-operation'
            SourceDirectory = Join-Path $script:Session 'instruction-set-source'
            SourceBackup = Join-Path $script:Session 'instruction-set-source.previous'
            GetChildItemCommand = $script:GetChildItemCommand
            ConvertToJsonCommand = $script:ConvertToJsonCommand
        }
        $global:FocusAreaSnapshotTest.Lock = Open-PullRequestFileLock -Path $global:FocusAreaSnapshotTest.LockPath
        [IO.Directory]::Move($global:FocusAreaSnapshotTest.SourceDirectory, $global:FocusAreaSnapshotTest.SourceBackup)
        # Pause publication between renames and release it only when the reader contends.
        Mock Start-Sleep {
            [IO.Directory]::Move($global:FocusAreaSnapshotTest.SourceBackup, $global:FocusAreaSnapshotTest.SourceDirectory)
            $global:FocusAreaSnapshotTest.Lock.Dispose()
        }
        Mock Get-ChildItem {
            # Another reader remains compatible, but a publisher cannot replace this generation.
            Assert-TestSnapshotReaderOwnership
            & $global:FocusAreaSnapshotTest.GetChildItemCommand -LiteralPath $LiteralPath -Recurse:$Recurse -Force:$Force
        } -ParameterFilter { $LiteralPath -eq $global:FocusAreaSnapshotTest.SourceDirectory }
        Mock ConvertTo-Json {
            Assert-TestSnapshotReaderOwnership
            & $global:FocusAreaSnapshotTest.ConvertToJsonCommand -InputObject $InputObject -Compress
        } -ParameterFilter { $Compress }
        $arguments = @{ SessionStateDirectory = $script:Session }
        switch ($CommandName) {
            'Update-FocusAreaExpansion.ps1' { $arguments.Operation = 'Read' }
            'Update-FocusAreaMap.ps1' {
                $arguments.Path = Join-Path $script:Session 'merged-map.json'
                $arguments.Operation = 'Append'
                $arguments.InputPath = $script:MapPath
            }
            default { $arguments.MapPath = $script:MapPath }
        }
        try {
            $null = & (Join-Path $script:Skill "scripts\$CommandName") @arguments | ConvertFrom-Json
            Should -Invoke Start-Sleep -Times 1 -Exactly
            Should -Invoke Get-ChildItem -Times 1 -ParameterFilter { $LiteralPath -eq $global:FocusAreaSnapshotTest.SourceDirectory }
            Should -Invoke ConvertTo-Json -Times 1 -ParameterFilter { $Compress }
            # Reader ownership must not survive the completed transaction.
            $writer = Open-PullRequestFileLock -Path $global:FocusAreaSnapshotTest.LockPath
            $writer.Dispose()
        } finally {
            if ([IO.Directory]::Exists($global:FocusAreaSnapshotTest.SourceBackup)) {
                [IO.Directory]::Move($global:FocusAreaSnapshotTest.SourceBackup, $global:FocusAreaSnapshotTest.SourceDirectory)
            }
            $global:FocusAreaSnapshotTest.Lock.Dispose()
            Remove-Variable -Name FocusAreaSnapshotTest -Scope Global
        }
    }

    It 'renders exact headings, source lines, frontmatter and independent scopes with LF endings' {
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
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
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $metadata = Format-FocusAreaMetadata -Chapter $validated.Map.chapters[0] -Files $validated.Files
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
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
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
        { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session } | Should -Throw
        Test-Path -LiteralPath (Join-Path $script:Session 'focus-areas') | Should -BeFalse
    }

    It 'rejects invalid source or assignment data: <Case>' -ForEach @(
        @{ Case = 'duplicate chapter'; Change = { $script:Map.chapters[1].id = 'named-arguments' }; Error = '*Duplicate chapter*' }
        @{ Case = 'duplicate category'; Change = { $script:Map.categories[1].id = 'scripts' }; Error = '*Duplicate category*' }
        @{ Case = 'blank title'; Change = { $script:Map.categories[0].title = ' ' }; Error = '*nonblank title*' }
        @{ Case = 'unknown category'; Change = { $script:Map.chapters[0].categoryId = 'missing' }; Error = '*unknown category*' }
        @{ Case = 'overlap'; Change = { $script:Map.chapters[1].content.startLine = 8 }; Error = '*Overlapping chapter*' }
        @{ Case = 'past EOF'; Change = { $script:Map.chapters[0].content.endLine = 99 }; Error = '*Invalid line range*' }
        @{ Case = 'reversed range'; Change = { $script:Map.chapters[0].content.endLine = 5 }; Error = '*Invalid line range*' }
        @{ Case = 'wrong root'; Change = { $script:Map.chapters[0].content.root = 'instruction-set-source' }; Error = '*must come from*' }
        @{ Case = 'traversal'; Change = { $script:Map.chapters[0].content.path = '..\scoped.md' }; Error = '*safe relative path*' }
        @{ Case = 'absolute path'; Change = { $script:Map.chapters[0].content.path = 'C:\scoped.md' }; Error = '*safe relative path*' }
        @{ Case = 'missing file'; Change = { $script:Map.chapters[0].content.path = 'missing.md' }; Error = '*not found*' }
        @{ Case = 'incorrect spelling'; Change = { $script:Map.chapters[0].content.path = 'SCOPED.md' }; Error = '*exact spelling*' }
    ) {
        & $Change
        Save-TestFocusMap
        { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session } | Should -Throw -ExpectedMessage $Error
    }

    It 'accepts partial worker maps but rejects incomplete merged coverage and empty categories' {
        $script:Map.chapters = @($script:Map.chapters[0])
        Save-TestFocusMap
        { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session } | Should -Throw '*has no chapters*'
        $script:Map.categories = @($script:Map.categories[0])
        Save-TestFocusMap
        $partial = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session
        $partial.Map.chapters.Count | Should -Be 1
        { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete } |
            Should -Throw '*Uncovered raw-input line*'
    }

    It 'handles heading-only, setext and unheaded guidance without heading reconstruction' {
        foreach ($text in @('### Heading only', "Setext heading`n===`nBody", 'Unheaded guidance')) {
            foreach ($root in @('instruction-set-source', 'instruction-set-raw')) {
                [IO.File]::WriteAllText((Join-Path $script:Session "$root\general.md"), $text)
            }
            $count = ($text -split "`n").Count
            $script:Map.chapters[3] = New-TestChapter 'general' 'general' 'general.md' 1 $count
            Save-TestFocusMap
            $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
            $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
            $output = $documents['focus-areas\focus-general.md']
            $output | Should -Match ([regex]::Escape(($text -split "`n")[0]))
            $output | Should -Match '<!-- end ss-review scope metadata -->'
            $output | Should -Not -Match "`r"
            if ($text.StartsWith('Setext')) { $output | Should -Match "Setext heading`n===" }
        }
    }

    It 'quotes embedded Markdown fences without rewriting their contents' {
        $text = "Scope example:`n``````powershell`nWrite-Output '`$u'`n``````"
        $quoted = Format-FocusAreaQuotation -Text $text
        $quoted | Should -BeExactly ('````' + "`n$text`n" + '````')
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
            (Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session).Map.categories.Count | Should -Be $count
            if ($count -eq 128) {
                (Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete).Map.categories.Count | Should -Be $count
            } else {
                { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete } | Should -Throw '*maximum is 128*'
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
        (Test-FocusAreaArtifacts -SessionStateDirectory $script:Session).status | Should -Be 'Valid'
    }

    It 'restores the prior set if installation fails after replacing one artifact' {
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
        Write-FocusAreaDocuments -Documents $documents -SessionStateDirectory $script:Session
        $oldCatalog = [IO.File]::ReadAllText((Join-Path $script:Session 'instruction-set-categories.md'))
        $documents['focus-areas\focus-scripts.md'] = '# Replacement'
        Mock Move-Item {
            if ([IO.Path]::GetFileName($LiteralPath) -ceq 'instruction-set-categories.md' -and
                [IO.Path]::GetFileName((Split-Path -Parent $LiteralPath)) -like '.focus-area-assembly-*') {
                throw 'Injected installation failure'
            }
            & $script:MoveItemCommand -LiteralPath $LiteralPath -Destination $Destination
        }
        { Write-FocusAreaDocuments -Documents $documents -SessionStateDirectory $script:Session } | Should -Throw '*Injected installation failure*'
        [IO.File]::ReadAllText((Join-Path $script:Session 'instruction-set-categories.md')) | Should -BeExactly $oldCatalog
        [IO.File]::ReadAllText((Join-Path $script:Session 'focus-areas\focus-scripts.md')) | Should -Match 'Require named arguments'
    }

    It 'rejects linked inputs and outputs without writing through them' {
        $outside = Join-Path $TestDrive 'outside'
        New-Item -ItemType Directory -Path $outside | Out-Null
        $marker = Join-Path $outside 'marker.md'
        Set-Content -LiteralPath $marker -Value 'Keep outside content.'
        # Junctions are available without elevation on Windows; use ordinary symlinks elsewhere.
        $linkType = if ($IsWindows) { 'Junction' } else { 'SymbolicLink' }
        $link = Join-Path $script:Session 'instruction-set-raw\linked'
        New-Item -ItemType $linkType -Path $link -Target $outside | Out-Null
        { Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session } | Should -Throw '*must not contain links*'
        Remove-Item -LiteralPath $link
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
        $outputLink = Join-Path $script:Session 'focus-areas'
        New-Item -ItemType $linkType -Path $outputLink -Target $outside | Out-Null
        try {
            { Write-FocusAreaDocuments -Documents $documents -SessionStateDirectory $script:Session } | Should -Throw '*must not contain links*'
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
        $combined = Read-FocusAreaMap -MapPath $merged -SessionStateDirectory $script:Session -RequireComplete
        $combined.Map.categories.Count | Should -Be 1
        $combined.Map.chapters[3].applicability[0].effectiveScope | Should -BeExactly 'No explicit restriction; interpret the guideline in context.'
        $script:Map.categories = @($script:Map.categories[0])
        $script:Map.chapters = @($script:Map.chapters[0])
        $script:Map.chapters[0].applicability[0].effectiveScope = 'Corrected scope judgment.'
        Save-TestFocusMap
        $run = Invoke-FocusPreparationCommand 'Update-FocusAreaMap.ps1' @(
            '-SessionStateDirectory', $script:Session, '-Path', $merged, '-Operation', 'Replace', '-InputPath', $script:MapPath
        )
        $run.ExitCode | Should -Be 0 -Because $run.Error
        $combined = Read-FocusAreaMap -MapPath $merged -SessionStateDirectory $script:Session -RequireComplete
        $combined.Map.chapters.Count | Should -Be 4
        $combined.Map.chapters[0].applicability[0].effectiveScope | Should -BeExactly 'Corrected scope judgment.'
    }

    It 'round trips assembled artifacts through the cache without requiring categorization intermediates' {
        $validated = Read-FocusAreaMap -MapPath $script:MapPath -SessionStateDirectory $script:Session -RequireComplete
        $documents = ConvertTo-FocusAreaDocuments -ValidatedMap $validated
        Write-FocusAreaDocuments -Documents $documents -SessionStateDirectory $script:Session
        $cacheArguments = @{
            InputDirectory = Join-Path $script:Session 'instruction-set-source'
            RecipePath = Join-Path $script:Skill 'prepare-focus-areas.md'
            CacheDirectory = Join-Path $TestDrive 'cache'
            GeneratorProfile = 'test'
            FreshnessDays = 7
            NowUtc = [DateTimeOffset]::Parse('2026-10-06T12:00:00Z')
        }
        (Save-FocusAreaCache @cacheArguments -SessionStateDirectory $script:Session -InputState Complete).status | Should -Be 'Stored'
        $restored = Join-Path $TestDrive 'restored'
        (Restore-FocusAreaCache @cacheArguments -SessionStateDirectory $restored).status | Should -Be 'Hit'
        foreach ($path in $documents.Keys) {
            [IO.File]::ReadAllText((Join-Path $restored $path)) | Should -BeExactly $documents[$path]
        }
        Test-Path -LiteralPath (Join-Path $restored 'focus-area-map.json') | Should -BeFalse
    }
}
