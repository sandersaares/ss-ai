Use the common rules in `workflow-data.md` and the inventory/grouping contracts
in `data-guides/file-groups.md`, included together in a grouping assignment's
`workflowData` document.

```text
ss-ai workflow set-review-input --kind "{{KIND}}" --input-path "{{INPUT_PATH}}" --path "{{OUTPUT_PATH}}" --inventory-path "{{INVENTORY_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `SKILL_ROOT` | Absolute skill directory. |
| `KIND` | `inventory` for the complete file list, or `groups` for a grouping draft. |
| `INPUT_PATH` | Absolute draft path using the `inventory` or `fileGroups` schema. |
| `OUTPUT_PATH` | Absolute `file-inventory.json` or `file-inventory-grouped.json` destination. Never an immutable work snapshot. |
| `INVENTORY_PATH` | The prepared inventory for Groups; omit this argument for Inventory. |

Use `--replace` only when the workflow explicitly regenerates preparation inputs
for freshness. Success emits `status: "Recorded"`, `path`, and `count`. Repair
invalid drafts before any dependent worker reads them; never manufacture missing
metadata or reset an active work snapshot.
