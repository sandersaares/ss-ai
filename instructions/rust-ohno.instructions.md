---
applyTo: "**/*.rs"
---

# Use `ohno::error` for error handling in Rust libraries

Every unique error that a library may encounter should be emitted as a new struct annotated with `#[ohno::error]`, for example:

```rust
#[ohno::error] pub(crate) struct CannotOpenConfigFile;
#[ohno::error] pub(crate) struct InvalidJson { line: u64 };
#[ohno::error] pub(crate) struct ListTooLong;
```

Such "leaf" errors are only visible within the same crate. Their primary function is to allow unit tests to assert in a strongly
typed way that a specific error was encountered, with specific field values.

When returning errors from public API surface of a library, an aggregate error type should be used.
This is a struct marked `#[ohno::error]` that can be converted from the leaf errors, for example:

```rust
#[ohno::error]
#[from(CannotOpenConfigFile)]
#[from(InvalidJson)]
#[from(ListTooLong)]
#[from(std::io::Error)]
pub struct FrobnicatorError;
```

There should be one aggregate error type used for each logical scope where the same family of leaf errors may be encountered.
This boundary is often either a module or a crate, although no strict rule exists - judge by behavior, not by structure.

If and only if there exists a real use case for callers to inspect the kind of the aggregate error, it may expose
`fn is_foo(&self) -> bool` methods for this purpose. This categorization does not need to be an exact match to the
leaf errors (and perhaps should not be, considering it is versioned public API surface). This implies manual
`impl From<LeafError> for AggregateError` implementations, which assign the correct kind to the aggregate error.

The use cases for distinguishing each kind must be documented as justification for this design decisions. The default
state is to offer no kind inspection API surface on aggregate error types.

# Use `ohno::AppError` for error handling in Rust apps

Application code often benefits from a simple catch-all error type, without error categorization by multiple leaf types.
Use the `ohno::AppError` error for this purpose, as it provides many built-in conveniences for application-level errors.
Refer to type API documentation for details and examples.