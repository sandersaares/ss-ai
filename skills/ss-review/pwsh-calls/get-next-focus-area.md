```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}\scripts\Get-NextReviewItem.ps1" `
  -QueuePath "{{FOCUS_AREA_QUEUE}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `FOCUS_AREA_QUEUE` | Absolute `focusAreaQueuePath` returned by work initialization. |

Call only when a worker slot is available. The JSON result has `exhausted`, `item`, and `remainingCount`. When `exhausted` is false, `item` is the claimed manifest entry; immediately dispatch that focus area. Even when `remainingCount` is zero, the returned item still requires processing. Only `exhausted: true` with `item: null` means no more focus areas remain to dispatch.

The command exclusively locks and atomically updates the queue. On an error, do not infer exhaustion or dispatch an item. Report the error; lock contention can be retried after the competing command returns. An interrupted or uncertain claim requires manifest reconciliation, not a blind second dequeue.
