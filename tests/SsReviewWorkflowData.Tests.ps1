BeforeAll {
    $script:SchemaPath = Join-Path (Split-Path -Parent $PSScriptRoot) 'skills\ss-review\schemas\workflow-data.schema.json'
    $null = Get-Command Test-Json -Module Microsoft.PowerShell.Utility
}

Describe 'Workflow data schema asset' {
    It 'conforms to the JSON Schema draft declared by the asset' {
        # Native commands validate records with Rust validators rather than loading this schema.
        # Native producer and consumer tests exercise record validation at their boundaries.
        $text = [IO.File]::ReadAllText($script:SchemaPath)
        $schema = [System.Text.Json.Nodes.JsonNode]::Parse($text)
        $schema['$schema'].ToString() | Should -BeExactly 'https://json-schema.org/draft/2020-12/schema'
        [Json.Schema.MetaSchemas]::Draft202012.Evaluate($schema).IsValid | Should -BeTrue
    }
}
