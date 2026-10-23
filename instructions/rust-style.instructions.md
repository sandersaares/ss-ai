---
applyTo: "**/*.rs"
---

# Cloning into closures

When cloning a variable to move it into a closure, create a separate scope for the
clone and shadow the variable instead of polluting the parent scope with renamed variables.

This applies to all closure types: async blocks, thread spawns, and regular closures.

Bad:

```rust
let handle = mutex.clone();
spawn(async move { handle.something(); });
```

Good:

```rust
spawn({
    let mutex = mutex.clone();
    async move {
        mutex.something();
    }
});
```

The same pattern applies to thread spawning:

```rust
thread::spawn({
    let set = Arc::clone(&set);
    move || {
        set.do_work();
    }
});
```

# Variable shadowing for type conversions

When converting a variable to a different form (e.g. removing a `Pin` wrapper,
dereferencing a pointer to a reference, or re-pinning a reference), prefer
shadowing the original variable name instead of inventing a new abbreviated name.
This keeps the code readable and avoids cryptic short names like `aw`, `aw_ref`,
or `raw`.

Good:

```rust
let awaiter = unsafe { awaiter.get_unchecked_mut() };
let awaiter = ptr::from_mut(awaiter);
let awaiter = unsafe { &*awaiter };
```

Bad:

```rust
let aw = unsafe { awaiter.get_unchecked_mut() };
let raw = ptr::from_mut(aw);
let aw_ref = unsafe { &*raw };
```

Only use a different name when both forms are needed simultaneously in the same
scope.

# Code files do not have chapters

Do not use section separator comments (e.g. `// --- Section ---` or `// ======= Title =======`)
to create visual "chapters" in code. Code organization should be clear from naming and structure.

# Type names

Do not hardcode type names in string literals (e.g. in `impl fmt::Debug`).
Instead use `type_name::<Self>()` or similar to refer to type names.

# Import the types we need and do not reference items via absolute paths

Types, functions and other items we reference in our code should be imported via `use` statements.

Unless there is a specific need to disambiguate between similarly named types,
do not use absolute paths for referencing items.

This is good:

```rust
use std::time::Instant;

fn foo(i: Instant) {}
```

This is bad:

```rust
fn foo(i: std::time::Instant) { }
```

Referencing types in `std` via absolute paths for no reason is especially sinful.

It is fine to rely on the Rust prelude — no need to explicitly import types from the Rust prelude.
We always use the latest Rust version, concerns about what is in the prelude are not relevant.

`use` statements go at the top of the file or module, not inside functions.

Do not `use super::` except in unit tests. Instead, use the full path through `use crate::`.

# Import items by their public path

If an item is available via a private (e.g. `pub(crate)`) path and a public path,
use the public path.

Wrong:

```rust
use crate::session::AllocationTrackingSession;
```

Correct:

```rust
use crate::AllocationTrackingSession;
```

# Use the type system to express invariants and state

A type should reflect the state it is in. For example, if an app is loading "entities", then validating them, and then working on
the validated entities, the pre-validation and post-validation entities should be different types. This may take the form of parsing
some `RawEntity` from a file, feeding it into an `EntitySetBuilder` which performs the validation, and getting back an `EntitySet`
collection whose `Entity` members are guaranteed valid.

# Prefer `NonZero<T>` for non-zero integers

If a parameter or field is a non-zero integer, prefer using `NonZero<usize>` and similar, instead of manually checking for zero values.