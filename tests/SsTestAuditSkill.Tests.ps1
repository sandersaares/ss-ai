#Requires -Version 7.6

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

BeforeAll {
    $script:Skill = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-test-audit'

    # Validate submitted data with the same schema a caller consumes.
    function Test-AuditRecord {
        param([string] $Schema, [hashtable] $Record)
        $json = ConvertTo-Json -InputObject $Record -Depth 20
        Test-Json -Json $json -SchemaFile (Join-Path $script:Skill "schemas\$Schema.schema.json") -ErrorAction SilentlyContinue
    }
}

Describe 'ss-test-audit skill contract' {
    It 'ships parseable native recipes and resolves their local file references' {
        $commands = Get-Content -LiteralPath (Join-Path $script:Skill 'commands.md') -Raw
        $nativeCommands = foreach ($block in [regex]::Matches($commands, '(?ms)^```powershell\r?\n(.*?)^```')) {
            $tokens = $null
            $errors = $null
            $ast = [System.Management.Automation.Language.Parser]::ParseInput(
                $block.Groups[1].Value, [ref]$tokens, [ref]$errors
            )
            $errors | Should -BeNullOrEmpty
            foreach ($command in $ast.FindAll({
                param($node)
                $node -is [System.Management.Automation.Language.CommandAst] -and
                    $node.GetCommandName() -eq 'ss-ai'
            }, $true)) {
                $command.CommandElements[1].Value | Should -Be 'test-audit'
                $command.CommandElements[2].Value
            }
        }
        @($nativeCommands) | Should -Be @(
            'initialize', 'search', 'context', 'evidence', 'next-batch', 'record-batch', 'advance',
            'submit', 'compare', 'report'
        )
        foreach ($name in @('SKILL.md', 'commands.md')) {
            $document = Get-Content -LiteralPath (Join-Path $script:Skill $name) -Raw
            foreach ($link in [regex]::Matches($document, '\]\(([^/#:)][^#):]*)(?:#[^)]*)?\)')) {
                Test-Path -LiteralPath (Join-Path $script:Skill $link.Groups[1].Value) -PathType Leaf |
                    Should -BeTrue -Because "$name links to $($link.Groups[1].Value)"
            }
        }
        Test-Path -LiteralPath (Join-Path $script:Skill 'scripts') | Should -BeFalse
    }

    It 'validates explicit and reuse submissions without allowing incomplete reuse' {
        $record = @{
            schemaVersion = 1; batchFingerprint = 'a' * 64
            rows = @(
                @{
                    assertionId = 'A000001'; classification = 'Implicit'; evidenceStatus = 'Ambiguous'
                    actualOutcome = 'The returned value is nonempty.'; rationale = 'The method name suggests presence.'
                    citationHandles = @(); callingTestIds = @()
                }
                @{
                    assertionId = 'A000002'; reuseFrom = 'A000001'; equivalenceConfirmed = $true
                    citationHandles = @('E0123456789ab-2'); callingTestIds = @('T000002')
                }
            )
        }
        Test-AuditRecord -Schema 'compact-submission' -Record $record | Should -BeTrue
        $record.rows[1].Remove('callingTestIds')
        Test-AuditRecord -Schema 'compact-submission' -Record $record | Should -BeFalse
    }

    It 'validates canonical judgments and rejects extra caller data' {
        $row = @{
            assertionId = 'A000001'; classification = 'Documented'; evidenceStatus = 'Confirmed'
            actualOutcome = 'The method returns an empty collection.'; rationale = 'The API documents the empty case.'
            citations = @(@{
                path = 'README.md'; line = 1; endLine = 1; owner = '# API'
                sourceRole = 'markdownDocumentation'; excerpt = 'Returns an empty collection.'
            })
            callingTestIds = @('T000001')
        }
        $record = @{ schemaVersion = 1; auditId = 'audit'; snapshotSha256 = 'a' * 64; assertions = @($row) }
        Test-AuditRecord -Schema 'classification-batch' -Record $record | Should -BeTrue
        $row.unexpected = 'extra field'
        Test-AuditRecord -Schema 'classification-batch' -Record $record | Should -BeFalse
    }

    It 'validates exact revision-bound inventory corrections' {
        $record = @{
            schemaVersion = 1; repositoryRevision = 'a' * 40
            corrections = @(
                @{
                    path = 'tests/check.rs'; line = 4; endLine = 4
                    expression = 'holder.as_mut().expect("fixture still present")'
                    reason = 'The option holds callback-local test state rather than an audited API result.'
                }
            )
        }
        Test-AuditRecord -Schema 'inventory-corrections' -Record $record | Should -BeTrue
        $record.corrections[0].Remove('reason')
        Test-AuditRecord -Schema 'inventory-corrections' -Record $record | Should -BeFalse
    }

    It 'validates the completed summary and details as distinct records' {
        $identity = @{
            schemaVersion = 2; auditId = 'audit'; snapshotSha256 = 'a' * 64
            contentSha256 = 'b' * 64; indexSha256 = 'c' * 64
            toolVersion = '0.1.0'; rubricVersion = '1'
            repositoryRevision = 'unversioned'; repositoryRoot = 'repository'; scopePaths = @()
        }
        $summary = $identity + @{
            status = 'complete'; nextAction = 'complete'; finalResponseAllowed = $true
            indexedFileCount = 0; sourceLanguages = @{}; testCount = 0; assertionCount = 0
            moduleCount = 0; classificationCounts = @{}; modules = @(); diagnosticCount = 0
            noCountedAssertionCount = 0; incidentalOnlyCount = 0; unlinkedAssertionCount = 0
            fallibleValueCheckCount = 0; conditionalAssertionCount = 0
            correctionCount = 0
            consistencyDiagnosticCount = 0
        }
        $details = $identity + @{
            assertionTrace = @(@{
                assertion = @{
                    id = 'A000001'; kind = 'assertion'; language = 'rust'; path = 'src/lib.rs'
                    line = 4; endLine = 4; column = 5; expression = 'assert!(ready())'
                    fileSha256 = 'd' * 64; ownerLine = 3; ownerEndLine = 5
                    containingTestId = 'T000001'; helperName = $null; callingTestIds = @()
                    callerEvidence = @(); unresolvedCallerCandidates = @()
                }
                classification = @{
                    assertionId = 'A000001'; classification = 'Documented'
                    evidenceStatus = 'Confirmed'; actualOutcome = 'The API reports ready.'
                    rationale = 'Public documentation promises the result.'
                    citations = @(@{
                        path = 'src/lib.rs'; line = 1; endLine = 1; owner = 'pub fn ready()'
                        sourceRole = 'rustItemDoc'; excerpt = '/// Reports readiness.'
                    })
                    incidentalReason = $null; callingTestIds = @(); unlinkedReason = $null
                    unresolvedCallerReason = $null; reviewReason = $null
                }
            })
            testInventory = @(@{
                id = 'T000001'; name = 'checks_ready'; kind = 'unit'; language = 'rust'
                path = 'src/lib.rs'; line = 3; endLine = 5; repositoryRevision = 'unversioned'
                directAssertionIds = @('A000001'); sharedAssertionIds = @()
                assertionCount = 1; incidentalOnly = $false
            })
            diagnostics = @()
            corrections = @(); consistencyDiagnostics = @(@{
                priority = 'medium'; kind = 'repeatedShortRationale'
                assertionIds = @('A000001'); detail = 'The rationale repeats without a site-specific premise.'
            })
        }
        Test-AuditRecord -Schema 'summary' -Record $summary | Should -BeTrue
        Test-AuditRecord -Schema 'details' -Record $details | Should -BeTrue
        $summary.finalResponseAllowed = $false
        Test-AuditRecord -Schema 'summary' -Record $summary | Should -BeFalse
        $details.Remove('testInventory')
        Test-AuditRecord -Schema 'details' -Record $details | Should -BeFalse
        $invalidDetails = $identity + @{
            assertionTrace = @(@{}); testInventory = @(@{}); diagnostics = @()
            corrections = @(); consistencyDiagnostics = @()
        }
        Test-AuditRecord -Schema 'details' -Record $invalidDetails | Should -BeFalse
    }

    It 'validates a completed audit comparison without selecting a winner' {
        $run = @{
            auditId = 'audit'; snapshotSha256 = 'a' * 64; contentSha256 = 'b' * 64
            indexSha256 = 'c' * 64; toolVersion = '0.1.0'; rubricVersion = '1'
            repositoryRevision = 'unversioned'; testCount = 1; assertionCount = 1
        }
        $record = @{
            schemaVersion = 1; comparableContent = $true; left = $run; right = $run
            matchedAssertionCount = 1; exactLabelAgreementCount = 1
            exactLabelAgreementRate = 1; changedRationaleCount = 0
            changedEvidenceStatusCount = 0; changedReviewReasonCount = 0
            callerSetDifferenceCount = 0
            leftOnlySites = @(); rightOnlySites = @(); transitionMatrix = @()
            noCountedAssertionTests = @{ leftOnly = @(); rightOnly = @(); both = @() }
            incidentalOnlyTests = @{ leftOnly = @(); rightOnly = @(); both = @() }
        }
        Test-AuditRecord -Schema 'comparison' -Record $record | Should -BeTrue
    }
}
