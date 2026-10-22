# Focus-area preparation data

Paths in this guide are relative to the skill root. Expansion records use
`schemaVersion: 1`; categorization follows its separate schema.

## Reference expansion

`focus-area-expansion-status.json` is an `expansion` record. Each reference has a
unique `id`, canonical `uri`, nonempty `referrers`, `state`, `outputs`, and `error`.
Canonical URIs name either the final external URL or an original same-repository
source location. The agent decides equivalence across redirects or duplicate
content; once identified, register additional referrers against the same ID.

Each `referrer` names `parentId` (null for a direct instruction), a source span,
the scope introduced at this edge, and verbatim declaration spans. Spans use the
root, path, and inclusive line conventions from `focus-area-format.md`. Direct
referrers must name `instruction-set-source`; nested referrers name their
parent's frozen expanded output. Do not edit or replace reference links in those
files after registration. The manifest supplies their expanded targets.

Use `pwsh-calls/update-focus-area-expansion.md` to initialize, register, record
outcomes, query inherited contexts, and finalize. A registration draft has the
`referenceInput` shape:

```json
{
  "id": "api-guidance",
  "uri": "https://example.invalid/api",
  "referrers": [{
    "parentId": null,
    "source": { "root": "instruction-set-source", "path": "rules.md", "startLine": 4, "endLine": 4 },
    "scope": "Published APIs.",
    "declarations": []
  }]
}
```

A result draft has `referenceResult` shape:

```json
{ "referenceId": "api-guidance", "outputs": ["api-guidance\\rules.md"], "error": null }
```

Success requires existing, nonempty raw output files and a null error. Failure
requires an empty outputs array and a nonblank error. Pending references cannot
be finalized. Finalization derives `Complete` or `Degraded`; agents never assign
the aggregate state. An intentionally empty reference list finalizes as Complete.
Failed references remain in the manifest, not silently removed.

The command emits `state`, `referenceCount`, `pendingReferenceIds`,
`failedReferences` (objects with `referenceId`, `uri`, `error`), and `contexts`.
A reference query returns contexts as `{ "chain": [...] }`; each chain entry has
`referenceId`, `uri`, `source`, `scope`, and `declarations`. Each chain is a
distinct alternative; conditions within a chain combine. Parent links resolve
recursively on every read, so later referrers propagate to descendants without
rewriting their metadata. Cycles, missing parents, and competing output owners
are rejected.

The assembler and cache-save command require the finalized manifest. Storage
derives completeness from it rather than an agent-transcribed command argument.
Cache hits need only the verified rendered artifacts, not generation intermediates.

## Focus-area maps

Use `pwsh-calls/update-focus-area-map.md` to append a typed one-chapter map to a
worker map, merge worker maps, or regroup categories. It validates before every
replacement, rejects conflicting chapter IDs or category titles, and preserves
chapter scope and provenance during regrouping. The separate focus-area schema
and `focus-area-format.md` remain authoritative for map content.
