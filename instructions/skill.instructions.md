---
applyTo: "**/skills/**/*.md"
---

# Workflow chapters should be numbered

When writing a prompt/skill that executes steps in sequence, keep the chapters for each step numbered so it is clear which chapter
is a step in the workflow and which order they are to be executed in. Do not number subchapters and prefixes/appendixes. Example:

```md
# Scope
# Stage 1: configuration resolution
# Stage 2: preparation
# Stage 3: execution
## Speeding up execution
# Stage 4: validation
# References
```

# Command execution requires example and argument table

When instructing the agent to execute a command:

1. Provide an example of the command;
2. Provide a table of placeholders to expand.
3. Describe what to do with the output of the command.

Example:

```md

To validate the results, execute the following command, replacing placeholders:

> scripts/Validate-Results.ps1 -InputFile "{{IN_PATH}}" [-Modifiers "{{MODIFIERS}}"] -Verbose

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `IN_PATH` | Path to the primary input file provided by the user in the configuration phase of the skill. |
| `MODIFIERS` | The modifiers file from the preparation phase above. Drop argument if preparation phase was not executed. |

If the command reports any errors, stop the task execution and report the error to the caller.
If the command completes successfully, continue to the next step in the workflow.
```

# Diagnostics in summary

If a skill emits a summary, it should include diagnostics about the execution of the skill (key decisions made, forks taken, etc).

If the summary is posted as a GitHub comment, the diagnostics should be included in a collapsible section of the comment.
