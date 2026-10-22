```powershell
pwsh -NoProfile -NonInteractive -File "{{SKILL_ROOT}}/scripts/Publish-ReviewTrackerItems.ps1" `
  -Url "{{TRACKER_URL}}" `
  -ApprovedFindingsPath "{{SESSION_STATE}}/approved-findings.json" `
  -ContextPath "{{CONTEXT_PATH}}" `
  -DiagnosticsPath "{{SESSION_STATE}}/focus-area-cache-diagnostics.json" `
  -OutputDirectory "{{SESSION_STATE}}" `
  -WorkItemType "{{WORK_ITEM_TYPE}}" `
  -AreaPath "{{AREA_PATH}}"
```

Placeholders to replace:

| Placeholder | Description |
|-------------|-------------|
| `SKILL_ROOT` | Directory containing the active `SKILL.md`. |
| `TRACKER_URL` | Browser URL of the GitHub repository or Azure DevOps project the items are filed into. Any URL naming something inside one serves, including the pull request that was reviewed. |
| `SESSION_STATE` | Session state directory for this review. |
| `CONTEXT_PATH` | Absolute path to the latest checked capture, when reviewing a pull request. |
| `WORK_ITEM_TYPE` | Azure DevOps only, and required there. The type the work items are created as, such as `Bug` or `Task`. Omit the parameter entirely for GitHub. |
| `AREA_PATH` | Azure DevOps only, and required there. The area the work items are filed under, rooted at the project name. Omit the parameter entirely for GitHub. |
| `TENANT` | Optional Azure tenant for the destination. Use the invocation or proactor tenant when the tracker is in the reviewed organization; confirm the tenant if the user chooses a different organization. |

For Azure DevOps, append `-Tenant "{{TENANT}}"` when a tenant was supplied.

Omit `-ContextPath` only for an approved review with no pull request binding; the items are then filed without their links back to the code and the discussion.

Otherwise the artifact must carry a non-null `reviewedContextHash` and this script requires the matching checked capture before any tracker write, including on continuation. A mismatch requires freshness revalidation and rebuilding the approved set, not substituting an unchecked capture. This requirement is independent of the tracker destination. Use `query-pr-context.md` for any agent lookup of the capture's provenance, URL, or discussion; do not open the capture or replace this path with a query projection. Handle the publication status and errors as phase 9 describes.

Add these only when they apply:

| Parameter | When |
|-----------|------|
| `-ParentWorkItemId` | Azure DevOps only. Links every created work item under the named parent. |
| `-DescriptionField` | Azure DevOps only. Names the large text field the body is written to. The default is `System.Description`, and a type that also carries `Microsoft.VSTS.TCM.ReproSteps` is refused without it, because the Agile and Scrum bug forms show the repro steps and never the description. Ask the user which field their form uses. |
| `-Platform`, `-Organization`, `-Project`, `-Repository` | Names the tracker where the URL does not, or corrects what it names. `-Platform` with these and no `-Url` names a tracker outright. |

## Continuing a paused run

A run that paused at `needsAdjudication` records the rewrite awaiting a decision under `adjudication` in `tracker-publication-result.json`. Once a person has accepted it, run this same command again with the paused run's own arguments and add:

```powershell
  -AcceptStoredRewrite `
  -AcceptWorkItemId "{{ACCEPT_WORK_ITEM_ID}}" `
  -AcceptExpectedBodyHash "{{ACCEPT_EXPECTED_BODY_HASH}}" `
  -AcceptStoredBodyHash "{{ACCEPT_STORED_BODY_HASH}}"
```

| Placeholder | Description |
|-------------|-------------|
| `ACCEPT_WORK_ITEM_ID` | `adjudication.itemId` from `tracker-publication-result.json`. |
| `ACCEPT_EXPECTED_BODY_HASH` | `adjudication.expectedBodyHash`, the hash of the approved text. |
| `ACCEPT_STORED_BODY_HASH` | `adjudication.storedBodyHash`, the hash of what the tracker stored. |

This is the one time the command is run again over an existing journal. It files the findings still pending and leaves the accepted item as it stands. It refuses unless the item and both hashes name the rewrite the run paused on, and unless the approved set and the tracker settings match the paused run's, so a decision cannot be applied to a different review or a different item.
