# Public API documentation

Applies to: code files containing public API definitions and any standalone public API documentation files.

API documentation exists for documenting **contractual promises** of the API. Do not document implementation details and design
choices that are not part of the actual API contract demanded by the requirements. If unsure, assume you are not dealing with
API contract promises. In an interactive session, you may ask the user for clarification about requirements.

# Requirements are contract, consequences are not

Applies to: code files containing public API definitions and any standalone public API documentation files.

When documenting requirements in API documentation, state the requirements without describing the consequences of violating the
requirements. The requirements are part of the API contract, the consequences are implementation details and should not be documented.

# Design documents are development tools, not public API

Applies to: code files containing public API definitions and any standalone public API documentation files.

Do NOT reference design documents or implementation guides such as any `design.md` or `implementation.md` documents in public API
documentation. Design documents are only intended for internal use by maintainers, not for users of a library or app.

If there is information in the design or implementation documents that is relevant to users of the library or app, extract this information
and present it to readers in a way that integrates into the existing public documentation (e.g. part of API documentation or separate book).