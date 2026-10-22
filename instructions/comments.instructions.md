# Comments must justify decisions

Applies to: all files containing code, declarative structures (e.g. YAML) or configuration data.

If code/declaration/configuration reflects a taken design/implementation decision, leave a comment with:

1. a reference to the design document or implementation guide, if the decision is documented there; or
2. an inline justification for the decision in sufficient depth that the reader can reproduce the decision independently.

For example:

```rust
let Some(customer) = document.customer() else {
    // Unknown/unregistered customers get standard pricing.
    // Ref: docs/pricing.md, "Fallback pricing".
    return standard_pricing();
}
```

Design decisions can be as simple as an if-statement. If not obvious why an if-statement exists (e.g. when some condition causes
expected work to be skipped), leave a comment to justify the reasoning behind it and reference the relevant design document or
implementation guide (where applicable).

# Comments must justify literal values

Applies to: all files containing code, declarative structures (e.g. YAML) or configuration data.

If code/declaration/configuration contains a literal value that reflects a design/implementation decision, leave a comment with:

1. a reference to the design document or implementation guide, if the decision is documented there; or
2. an inline justification for the literal value in sufficient depth that the reader can reproduce the decision independently.

For example:

```rust
/// If an order is bigger than this, it must be split into multiple orders.
/// Ref: docs/limits.md; docs/orders.md "Splitting orders".
const MAX_ORDER_SIZE: u64 = 100;

/// File extension for emitted order book files.
/// This is chosen to match the value used by the .NET verison of the app (see ../../dotnet/src/OrderBook.rs).
const FILE_EXTENSION: &str = ".obk";
```

Obvious things like `len` having a default value of `0` do not require justification, of course. Even the fact that a value is arbitrary
and has no special meaning may be worth commenting (e.g. to clarify whether a test is testing an edge case or an "ordinary" value).

Fixed syntax and established external identifiers are not design choices and need no rationale unless another value is plausibly viable
or evidence demonstrates a synchronization or drift risk (e.g. the value must match something else somewhere else).

A nearby higher-level comment may justify a class of values when it states one reproducible selection rule that applies to all of them. Do not require repetitive per-value comments or infer an additional synchronization rule that the content does not establish.

It is fine if the justification is arbitrary. For example, "we do not know what value to use so we picked 100 because it felt good" is a perfectly fine justification if that reflects reality - what matters for maintainability is that the justification is documented. The value judgements that lead to the justification may be worth reviewing in their own light but are not a maintainability concern.

Values like "0", "1" and "1024" are magic values that have no inherent meaning in most situations. The benchmark is: would another maintainer plausibly decide to use a different value? If yes, the choice must be justified.

Counterexample: `len() == 0` does not require justification because it is an obvious "is empty" check - another maintainer would not use a different value like `len() == 1` for an "is empty" check.
Note: an "is empty" check itself (not the literal `0`) may still require justification for other reasons (e.g. why are we checking for emptyness - would another maintainer find it obvious that we need to even do such a check?).

# Comments must document the role of types

Applies to: all files containing code.

For each nontrivial type/item/function, an item-level comment block is required. This applies both to public and private items.
This comment block needs to explain (briefly but clearly) the role of the type in context of the overall design and implementation
logic of the library or app (putting it in context of `docs/design.md` and `docs/implementation.md`, specifically, if such files exist).

Do not make the readers reverse engineer code to understand the purpose of a type.

Focus on justifying the role ("why does this exist") rather than describing what it does (which is often obvious from reading the code).

For example:

```rust
/// Builds an order book.
///
/// Implements the order book assembly process (design.md, "Order book assembly";
/// implementation.md, "Order book" & "Builder pattern"). At the end of this process,
/// after calling `build()`, the caller will have a fully constructed order book that
/// conforms to all requirements specified in the design and implementation documentation.
///
/// Automatically splits orders that exceed the maximum allowed size (`MAX_ORDER_SIZE`).
struct OrderBookBuilder {

}
```

# Comments on values must not duplicate the value

Applies to: all files containing code, declarative structures (e.g. YAML) or configuration data.

Do not list the value (in any form, even recomputed or derived) in a comment on that value.
Doing that would just create an opportunity for defects if the two ever diverge.

Bad examples:

```rust
/// 100 because docs/limits.md said so.
const MAX_ORDER_SIZE: u64 = 100;

/// 3600 seconds to match .NET implementation.
const TIMEOUT: Duration = Duration::from_hours(1);
```

Good examples:

```rust
/// Because docs/limits.md said so.
const MAX_ORDER_SIZE: u64 = 100;

/// Value chosen to match .NET implementation.
const TIMEOUT: Duration = Duration::from_hours(1);
``` 