# Cargo dependency inventory

The inventory provides the dependency tidy workflow with a read-only view of
workspace manifests. It reports declarations from the supplied root manifest
and its declared members, including build and target-specific dependencies.
It does not run Cargo, access the network, or modify files.

The inventory's same-workspace classification uses explicit paths, matching
member package names, and workspace dependency inheritance. Name matching is
a tidy workflow heuristic; it does not mean Cargo resolves a registry
dependency to a local package.

Each declaration includes its original source text and line, including
comments and multiline values within its span. A dotted declaration's span
can include unrelated text between its first and last attributes. Consumers
must not assume that replacing the entire span only changes that dependency.
Absent attributes remain null, distinct from false or an empty array.
