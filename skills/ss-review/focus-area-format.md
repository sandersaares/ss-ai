# Focus-area categorization format

Categorization workers and the preparation agent exchange JSON, not Markdown.
`schemas/focus-area-map.schema.json` defines the versioned wire format.
`Test-FocusAreaMap.ps1` validates worker maps; `New-FocusAreaArtifacts.ps1`
validates the complete merged map and renders the category catalog and focus areas.
Do not write an assembly script or edit generated Markdown.

## Document

Every map has `schemaVersion: 1`, `categories`, and `chapters`. All properties
shown in the schema are required; unknown properties are rejected. Arrays remain
arrays even when empty or containing one item. Use `[]` for absent declarations,
reference origins, or uncertainty; do not omit these fields or use `null`.

Categories contain a unique `id` and a nonblank, single-line `title`. Identifiers
use lowercase letters, digits, and separating hyphens. A chapter has a unique
`id`, one existing `categoryId`, a `content` span, nonempty `sources`, and nonempty
`applicability`. Assign chapter IDs before dispatch so workers cannot rename or
silently duplicate assigned chapters. Each category must have a chapter.

Each worker writes the same document shape for its assigned chapters. The
preparation agent checks returned chapter IDs against that worker's assignment,
then combines the documents into `focus-area-map.json` using
`pwsh-calls/update-focus-area-map.md`. Workers use the same prepared Append
command for one-chapter drafts; Replace applies corrections to existing chapter
IDs without editing other records. Merge
equivalent category IDs and titles explicitly; conflicting titles require a
decision. Category regrouping changes only `categories` and chapter `categoryId`
values, not chapter content, provenance, or applicability.

## Source spans

A span has `root`, `path`, `startLine`, and `endLine`. Roots are exactly
`instruction-set-source` or `instruction-set-raw`; paths are relative to that
named root, with exact source spelling. Both path separators are accepted.
Absolute paths, traversal components, and symbolic links are not accepted.
Line numbers are one-based and inclusive. The files must remain unchanged from
categorization through assembly. A changed source requires renewed categorization
of affected spans, not a guessed line-number adjustment.

`content` must refer to `instruction-set-raw` and include the complete guideline
heading and body. Chapters' content spans must not overlap. Heading levels and
repeated heading names have no identifying role: selection uses line ranges.
Split nested sections into disjoint chapter ranges; retain the enclosing text
needed by each child in its scope declarations. A parent content range must not
include a separately assigned child's content.
`sources` names the original chapter spans in `instruction-set-source`, or
expanded raw source spans when the original is external. Direct sources retain
their original file boundary even when their raw copy is edited.

Text is read by the tools from these spans. Do not copy guideline text,
frontmatter, or enclosing declarations into JSON string fields. The renderer
preserves all characters within each selected line and uses LF line endings and
a final newline. It does not perform heading lookup or compare newline encodings.

## Applicability

Each `applicability` item describes one complete alternative context in which
the chapter applies. Within that context:

* `effectiveScope` states the combined inherited and local conditions, explicit
  exceptions, declared path base, and governing directory where relevant.
  With no explicit restriction, use exactly
  `No explicit restriction; interpret the guideline in context.`
* `declarations` contains spans for the complete original frontmatter, file-wide
  scope prose, enclosing scope, local qualifications, and related exceptions
  needed to interpret this context. Preserve enclosing or sibling exceptions
  here even when their own chapters are assigned to other categories.
* `origins` contains the reference chain's source URLs or same-repository
  reference locations, in order from referrer to target. Use an empty array for
  direct guidance. Retain every distinct referring context as its own
  applicability item; unrelated referrers are alternatives, not constraints
  to intersect.
* `uncertainties` contains objects with a nonblank `reason` and nonempty
  `evidence` spans. It is always present, including when empty. Evidence is
  copied verbatim by the renderer. Uncertainty cannot justify excluding content.

### Deterministic path scopes

Set optional `pathScope` to null for semantic, mixed path-and-role, uncertain,
unrestricted, or unsupported scopes. An absent value also requires review.
Only a complete effective scope expressible solely as path alternatives may use
`{ "base": ".", "include": ["**/*.ps1", "**/*.psm1"] }`. The base is the
repository root (`.`) or an explicit repository-relative governing directory.
Use slash-separated paths, literal segments, `*` and `?` within a segment, and
`**` as a complete segment for any directory depth. Expand simple alternatives
into separate patterns; do not use negation, braces, character classes, or
traversal segments. Non-path exceptions or cross-file applicability require null,
not an approximation based on a filename or category title.

The declaration spans must support the complete path-only meaning, with no
uncertainty. Each section and each distinct referring context retains its own
value. Preparation's scope verification checks these judgments against the
source. Matching is case-insensitive conservatively: this can retain extra
review work, but cannot exclude a possible case-sensitive match.

The assembler embeds a versioned, content-digest-bound map in each focus-area
Markdown file. Cache storage preserves it with the guidance. Review initialization
expands an exclusion only when every alternative of every section is path-only
and rejects the inventory path. A semantic or uncertain section keeps the focus
area assigned for review. Never author or edit the embedded map directly.

Declarations and uncertainty evidence may overlap other spans; they provide
context rather than another copy of the guideline to categorize.

For example, this map describes a raw chapter at lines 6-8 whose source has
frontmatter at lines 1-3 and inherited scope at line 4:

```json
{
  "schemaVersion": 1,
  "categories": [{ "id": "script-contracts", "title": "Script contracts" }],
  "chapters": [{
    "id": "named-arguments",
    "categoryId": "script-contracts",
    "content": {
      "root": "instruction-set-raw",
      "path": "external\\<root-identity>\\.github\\instructions\\powershell.instructions.md",
      "startLine": 6,
      "endLine": 8
    },
    "sources": [{
      "root": "instruction-set-source",
      "path": "external\\<root-identity>\\.github\\instructions\\powershell.instructions.md",
      "startLine": 6,
      "endLine": 8
    }],
    "applicability": [{
      "effectiveScope": "Agent-workflow scripts matching either **/*.ps1 or **/*.psm1.",
      "pathScope": null,
      "declarations": [{
        "root": "instruction-set-raw",
        "path": "external\\<root-identity>\\.github\\instructions\\powershell.instructions.md",
        "startLine": 1,
        "endLine": 4
      }],
      "origins": [],
      "uncertainties": []
    }]
  }]
}
```

The numbers in this example refer to that illustrative source layout, not fixed
positions in real instruction files.

## Validation and generated artifacts

Execute `pwsh-calls/validate-focus-area-map.md` on each worker map before accepting
it. Validation checks schema shape, identifiers, category references, safe source
paths, line bounds, and non-overlapping chapter content. It does not coerce
alternate field names or fill missing properties. Repair the producer's JSON
when validation fails.

Execute `pwsh-calls/assemble-focus-areas.md` on the merged map. Before writing
anything, the assembler additionally requires no more than 128 categories and
coverage of every nonblank raw-input line by chapter content, scope declarations,
or uncertainty evidence. Declare raw frontmatter and preambles as context, not
as standalone guidelines. This coverage check detects missing material; the
preparation agent still verifies that guidance was not misclassified as metadata.

The assembler renders `instruction-set-categories.md` and
`focus-areas/focus-<category-id>.md` in map order. Metadata is local to each
chapter, even when scopes happen to be identical. Each applicability alternative
retains its own declarations, origins, and uncertainty. Preserve separate raw
chapter contributions even when their text is identical. Deduplicate referenced
material during expansion, with every referring context retained, rather than
dropping chapter coverage during assembly.

Both outputs are replaced as a set, with rollback on installation failure.
Validation errors leave existing outputs untouched. Successful regeneration
removes obsolete generated categories. Worker maps and the merged JSON are
generation intermediates; cache hits restore the rendered, verified artifacts
and do not require these intermediates.
