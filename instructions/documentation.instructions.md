# Design document

Applies to: agent planning output, manifest/project files of library or app projects.

Every nontrivial library or app should have a `docs/design.md` that describes the user-visible behavior and the design tenets that it
embodies. If a project is internally split into multiple layers (e.g. `foo` and `foo_impl` and `foo_macros`), only the top layer is
required to have this documentation, although if the internal layers are nontrivial, it may still be beneficial to document their
design decisions.

The design document should focus on what is the achieved behavior and what are the design tenets. It should NOT include
implementation details or make vague statements without being specific about the user-visible impact of those statements.

The specific topics to cover depend on the problem domain. There is no standard set.

When working on a new library or app, create this documentation before coding, covering the entire expected functionality scope.

When working on changes, update existing design documents to keep design and implementation in sync.

When working on changes to libraries or apps that lack a design document, reverse-engineer the design and create a
corresponding `docs/design.md` to capture it. You may constrain this reverse-engineered design to only the affected parts
of the library or app if it is a large codebase.

In a complex library or app, the design document may be further subdivided into individual documents (e.g. a high-level `design.md`
that links to area-specific design documents like `cli.md` and `rest-api.md`).

This chapter does not apply to small in-repo helper libraries/apps that are never published.

# Implementation guide

Applies to: agent planning output, manifest/project files of library or app projects.

Every nontrivial library or app should have a `docs/implementation.md` that describes how the documented design is to be implemented.
This contains all the high-level thinking about the implementation, covering areas that are NOT user-visible (which go in `design.md`
instead). If a project is internally split into multiple layers, only the top layer is required to have this documentation,
although if the internal layers are nontrivial, it may still be beneficial to document their design decisions.

It is critical that the implementation guide explain the WHY behind implementation choices, not merely re-state the code in prose.

The specific topics to cover depend on the problem domain. There is no standard set.

Treat the implementation guide in an equivalent manner to the design document - the primary difference is that it focuses
on the internal implementation details rather than user-visible behavior. Keep it in sync with any changes.

This chapter does not apply to small in-repo helper libraries/apps that are never published.

# Documentation principles

Applies to: documentation files separate from code such as `docs/*.md`, readme files or book chapters.

Document the target state achieved when a described design is implemented. Do not document the "status" of a document - assume everything is done.

# 0.x always means unstable API

Applies to: documentation files separate from code such as `docs/*.md`, readme files or book chapters.
Applies to: code files containing public API definitions, internal developer documentation and any standalone public API documentation files.

Version 0.x APIs are understood to be unstable - there is no need to document API stability guarantees for such APIs,
as everything is contractually unstable.

# Documents read from high level to low level

Applies to: agent planning output, documentation files separate from code such as `docs/*.md`, readme files or book chapters.

The first chapters of documents describe goals and high-level concepts, introducing and defining terms relevant to the document.
Later chapters can expand on details within the context of the high-level concepts already introduced. Do not introduce significant
new concepts in chapters talking about fine details - that suggests some high-level chapter is missing that should have introduced
those concepts.

In document collections (e.g. books) this applies recursively - the start of a book should consist of high-level documents, then guides
on specific topics, then later more advanced use cases. Within each document in the book, content should flow similarly from high-level to details.

# Precise is not the same as useful

Applies to: all documentation in any shape or form.

Prefer generalizations and weaker claims instead of documenting every edge case. "X sometimes does Y" is much preferred over "X does Y except when Q, or P, or S".

# General statements are not expected to be comprehensive

Applies to: agent planning output, all documentation in any shape or form.

Generalizations that are later contradicted by specific cases are perfectly fine and expected. Do not pollute general statements with
caveats and a listing of special cases. Instead, document the generalization and then document the special cases separately when they
are relevant.

For example, merely the presence of some fallback path when an API is not usable does not need to be documented in the
general statement about the design being implemented via that API, even if this leads to a degraded experience.

# Terms are defined before usage

Applies to: agent planning output, code files containing public API definitions, internal developer documentation and any standalone public API documentation files.

Before using a specialized term, define it. A reader going through a document linearly should see a specialized term introduced and/or
defined before encountering it in prose.

In non-document scenarios (e.g. API documentation), it is still important to define key terms but the definitions should typically be
located on a suitably high-level page (e.g. a package/module overview), which we can assume a reader reads before individual API entries.

# Prefer active voice in documentation

Applies to: agent planning output, code files containing public API definitions, internal developer documentation and any standalone public API documentation files.

Passive voice creates ambiguity. Prefer active voice to clearly indicate the actor performing an action.
For example, instead of writing "X is called," write "The owner calls X."

# Use established terminology

Applies to: agent planning output, code files containing public API definitions, internal developer documentation and any standalone public API documentation files.

Avoid inventing new terminology if you can use established terminology.

If you do have to invent new terms, you must:

1. Clearly define the new term, referencing it (or at least its page/document) whenever it is used elsewhere.
2. Prefer compound terms that modify established terminology over introducing entirely new jargon (e.g. "a request that has not yet been cleaned up" over "a retained request").