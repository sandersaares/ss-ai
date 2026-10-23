# Working with Azure DevOps via Azure CLI

Applies to: agent workflows and scripts used by them.

When creating a pull request via the `az` command (Azure CLI), use the `@file` syntax to provide a multi-line description from a Markdown file. DO NOT try to give multiline input to `az devops` - it does not work.

Warning: Azure CLI seems to discard UTF-8 characters and downsample content to ANSI. In some situations, this may result in loss of non-ASCII characters in descriptions and other text fields. You may need to use raw REST APIs when working with non-ASCII content.

# Azure DevOps bug reporting

Applies to: agent workflows.

When asked to report bugs in Azure DevOps (ADO), provide the content (title, description, attachments etc) and structural categorization (area path)
but, unless explicitly instructed to do so, do not assign the created work item to anyone or otherwise change its metadata from default values provided by the work item template.

# Azure DevOps does not support pull request shortlinking

Applies to: agent workflows and scripts used by them.

Azure DevOps does NOT support `#123` style shortlinking of pull requests, only work items. Do NOT use shortlink syntax for referencing pull requests - always reference pull requests by URL.