Read `workflow-data.md` for the common rules and `data-guides/finding-plans.md`
for the selected plan contract. Initialize a plan once:

```text
ss-ai workflow plan-initialize --kind "{{KIND}}" --path "{{PLAN_PATH}}"
```

Append each schema-defined draft, including drafts returned by workers:

```text
ss-ai workflow plan-append --kind "{{KIND}}" --path "{{PLAN_PATH}}" --item-path "{{ITEM_PATH}}"
```

| Placeholder | Value |
| --- | --- |
| `KIND` | Plan kind from `data-guides/finding-plans.md`, such as `mergePlan`, `mergeCandidates`, `updates`, or `removals`. |
| `PLAN_PATH` | Absolute path to the persisted plan. |
| `ITEM_PATH` | Absolute path to one item draft following the kind's schema. |

To correct a recorded item, use the append command with
`--replace-index {{ITEM_INDEX}}`, where `ITEM_INDEX` is the item's zero-based index
in the named array. This validates the replacement and preserves all other items.

Success emits `status`, `path`, and `kind`. On error, repair the draft; do not
manually mutate the plan or overwrite an existing plan by reinitializing it.
