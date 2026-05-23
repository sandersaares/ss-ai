# YAML validation without Python or Node

This machine has no native Python or Node.js. The `powershell-yaml`
module (PSGallery, v0.4.12+) is the most reliable local YAML
validator:

```powershell
Install-Module powershell-yaml -Scope CurrentUser
Import-Module powershell-yaml
ConvertFrom-Yaml (Get-Content -Raw path/to/file.yaml)
```
