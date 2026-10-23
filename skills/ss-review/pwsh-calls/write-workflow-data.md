Read `workflow-data.md` for the common rules and `data-guides/finding-plans.md`
for the selected plan contract. Initialize a plan once:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\New-WorkflowData.ps1" -Kind "{{KIND}}" -Path "{{PLAN_PATH}}"
```

Append each schema-defined draft, including drafts returned by workers:

```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Add-WorkflowDataItem.ps1" -Kind "{{KIND}}" -Path "{{PLAN_PATH}}" -ItemPath "{{ITEM_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute path to the skill. |
| `KIND` | Plan kind from `data-guides/finding-plans.md`, such as `mergePlan`, `mergeCandidates`, `updates`, or `removals`. |
| `PLAN_PATH` | Absolute path to the persisted plan. |
| `ITEM_PATH` | Absolute path to one item draft following the kind's schema. |

To correct a recorded item, use the append command with
`-ReplaceIndex {{ITEM_INDEX}}`, where `ITEM_INDEX` is the item's zero-based index
in the named array. This validates the replacement and preserves all other items.

Success emits `status`, `path`, and `kind`. On error, repair the draft; do not
manually mutate the plan or overwrite an existing plan by reinitializing it.
