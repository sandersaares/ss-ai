# Auto-approval rules

A finding is obvious when it matches any of these categories:

* It recommends aligning documentation and code, in either direction, where a mismatch was detected. Applies to user-facing API documentation as well as internal documentation like design documents and implementation guides.
* It recommends that conflicting, misleading, or ambiguous documentation be clarified or otherwise improved.
* It recommends an action that improves maintainability by adding clarity, simplifying logic, or removing something unnecessary.
* It recommends a clear performance win that does not require any design tradeoffs to be made.
* It recommends adding missing documentation/justification to code or configuration.
* It recommends adjusting benchmark/test coverage or structure to better align with desired practices.
* It recommends handling additional error cases that do not require logic changes beyond simple detection (e.g. no need for adding new recovery paths or refactoring existing code to enable such detection).
* It recommends strengthening the data model to make greater use of the type system (e.g. more narrowly bounded types like `NonZero<u32>`, replacing stringly typed fields with concrete types like `String` -> `EmailAddress`, reflecting validity/state via transforming data into different types like `RawCustomer` before validation to `Customer` after validation, and similar).
* It recommends improving correctness of parsing/processing some schematized/well-defined format/protocol/data-structure.
* It recommends addressing a broken guarantee in a comment or error message (e.g. "this never happens because X" but it turns out X is not always true).
* It recommends fixing an order of operations where the existing order has clearly identifiable defects.
* It recommends addressing a possible panic/exception that may be raised and cause unexpected logic flow.

Every other finding is important. When in doubt, treat a finding as important and leave it for the user's audit.

An obvious finding is auto-approved without presenting it. Record the category it matched as the reason. An auto-approved finding has the same outcome as a user-approved one: it is carried into the approved set and published. Do not ask the user about it or append anything to `audit-feedback.json` for it.
