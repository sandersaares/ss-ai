#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    $script:Checker = Join-Path $script:Repo 'dev\ss-review\Test-SsReviewGraphQlSchema.ps1'
    $script:State = Join-Path $script:Repo ('.graphql-check ' + [guid]::NewGuid().ToString('N'))
    [void] [IO.Directory]::CreateDirectory($script:State)
    $script:Wrapper = Join-Path $script:State 'mock-check.ps1'
    $script:Calls = Join-Path $script:State 'calls.jsonl'

    # A child PowerShell function replaces gh, so no test can reach the provider.
    $wrapper = @'
#Requires -Version 7.6
param([string] $Checker, [string] $Calls, [string] $InputPath)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
function global:gh {
    $global:LASTEXITCODE = 0
    $record = ConvertTo-Json -InputObject @($args) -Compress -Depth 100
    [IO.File]::AppendAllText($Calls, $record + [Environment]::NewLine)
    if (($args -join ' ') -match 'NoOutput') {
        $global:LASTEXITCODE = 1
    } elseif (($args -join ' ') -match 'unknownField') {
        '{"errors":[{"message":"unknownField doesn''t exist on type Query"}]}'
    } else {
        '{"data":{}}'
    }
}
if ($InputPath) {
    & $Checker -AdapterPath $InputPath
} else {
    & $Checker
}
exit $LASTEXITCODE
'@
    [IO.File]::WriteAllText($script:Wrapper, $wrapper)

    function Invoke-MockedSchemaCheck {
        param([string] $InputPath = '')
        if ([IO.File]::Exists($script:Calls)) { [IO.File]::Delete($script:Calls) }
        $previousNativePreference = $PSNativeCommandUseErrorActionPreference
        try {
            $PSNativeCommandUseErrorActionPreference = $false
            $output = & (Get-Process -Id $PID).Path -NoProfile -NonInteractive -File $script:Wrapper `
                -Checker $script:Checker -Calls $script:Calls -InputPath $InputPath 2>&1 | Out-String
            $exitCode = $LASTEXITCODE
        } finally {
            $PSNativeCommandUseErrorActionPreference = $previousNativePreference
        }
        [pscustomobject]@{ ExitCode = $exitCode; Output = $output }
    }
}

AfterAll {
    if ($script:State -and (Test-Path -LiteralPath $script:State)) {
        Remove-Item -LiteralPath $script:State -Recurse -Force
    }
}

Describe 'Offline GraphQL schema checker' {
    It 'checks every native review document with dummy required variables' {
        $result = Invoke-MockedSchemaCheck
        $result.ExitCode | Should -Be 0 -Because $result.Output
        $calls = @(Get-Content -LiteralPath $script:Calls | ForEach-Object { ,(ConvertFrom-Json -InputObject $_ -NoEnumerate) })
        $documents = @(Get-ChildItem -LiteralPath (Join-Path $script:Repo 'packages\ss-ai-review\src') -Filter '*.graphql' -File)
        $calls.Count | Should -Be $documents.Count
        foreach ($document in $documents) {
            $query = 'query=' + [IO.File]::ReadAllText($document.FullName)
            $call = @($calls | Where-Object { $_ -ccontains $query })
            $call.Count | Should -Be 1
            $call[0] | Should -Contain 'owner=x'
            $call[0] | Should -Contain 'repo=x'
            $call[0] | Should -Contain 'num=1'
        }
    }

    It 'checks explicit PowerShell adapter documents and reports schema errors' {
        $adapter = Join-Path $script:State 'adapter.ps1'
        $adapterContent = @(
            '$script:Example = @'''
            'query Example($id: ID!) { unknownField(id: $id) }'
            "'@"
        ) -join "`n"
        [IO.File]::WriteAllText($adapter, $adapterContent)
        $result = Invoke-MockedSchemaCheck -InputPath $adapter
        $result.ExitCode | Should -Be 1
        $result.Output | Should -Match 'Example'
        $result.Output | Should -Match "doesn't exist on type"
        $call = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($script:Calls)) -NoEnumerate
        $call | Should -Contain 'id=PRR_kwDOAAAAAAAAAAA'
    }

    It 'reports a failed invocation without output' {
        $document = Join-Path $script:State 'no-output.graphql'
        [IO.File]::WriteAllText($document, 'query NoOutput { viewer { login } }')
        $result = Invoke-MockedSchemaCheck -InputPath $document
        $result.ExitCode | Should -Be 1
        $result.Output | Should -Match 'No output'
    }
}
