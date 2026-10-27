#Requires -Version 7.6

# Reconstruct named runner arguments after Just's native argv boundary. Never evaluate
# argument text as PowerShell source; arrays use consecutive argv values.
function ConvertFrom-TestRecipeArguments {
    [CmdletBinding(PositionalBinding = $false)]
    param([string[]] $ArgumentList)

    $parameters = @{}
    $arrays = @('Path', 'FullNameFilter', 'RustTest')
    $scalars = @('Suite', 'Phase', 'PrerequisitePath', 'RustFilter')
    for ($index = 0; $index -lt $ArgumentList.Count; $index++) {
        $option = $ArgumentList[$index]
        if ($option -notmatch '^-(Path|FullNameFilter|RustTest|Suite|Phase|PrerequisitePath|RustFilter)$') {
            throw "Expected a named test runner option, got: $option"
        }
        $name = $option.Substring(1)
        $values = [Collections.Generic.List[string]]::new()
        while ($index + 1 -lt $ArgumentList.Count -and $ArgumentList[$index + 1] -notmatch '^-(Path|FullNameFilter|RustTest|Suite|Phase|PrerequisitePath|RustFilter)$') {
            $index++
            $values.Add($ArgumentList[$index])
            if ($name -in $scalars) { break }
        }
        if ($values.Count -eq 0) { throw "Missing value for $option" }
        if ($name -in $arrays) {
            # Cargo target names cannot contain commas; accept PowerShell-style target lists
            # without splitting commas that legitimately belong to paths or test filters.
            if ($name -eq 'RustTest') {
                $parameters[$name] = @($values | ForEach-Object { $_ -split ',' })
            } else {
                $parameters[$name] = $values.ToArray()
            }
        } else {
            $parameters[$name] = $values[0]
        }
    }
    $parameters
}
