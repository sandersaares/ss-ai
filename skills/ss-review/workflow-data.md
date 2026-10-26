# Machine-readable workflow data

Agents supply judgments and draft content, not serialization or state-management
code. Use the prepared commands to assemble persisted records. Do not patch JSON
documents with replacement strings, write temporary helper scripts, or invent
alternative property names.

For command failures, apply [error recovery](error-recovery.md): correct
orchestration mistakes and retry safe transient failures without weakening
validation or human decision gates.

`schemas/workflow-data.schema.json` defines named input and record schemas.
Commands validate drafts before writing and consumers validate stored records.
Use the exact fields, empty values, and versions defined by the assigned data
guide and schema. Unsupported versions require explicit new preparation or
reassessment, not silent migration.

Plain-text feedback, reference documents, overview prose, and personal compaction
notes remain text. Mechanical state does not live in those notes.
