---
applyTo: "**/*.rs"
---

# Public API types should be unwind-safe

All public API types should implement `UnwindSafe` and `RefUnwindSafe` so they can be passed through `catch_unwind()` boundaries.
Verify this with static assertions:

```rust
static_assertions::assert_impl_all!(MyType: UnwindSafe, RefUnwindSafe);
```

If a type cannot reasonably be made unwind-safe (e.g. it wraps a lock guard and calls user code under that guard), pin
the negative contract with `assert_not_impl_any!` and a comment explaining why.

## Manual `UnwindSafe`/`RefUnwindSafe` impls

When auto-derivation fails due to interior mutability types like `UnsafeCell`, `RefCell`, or `NonNull<UnsafeCell<...>>`,
ensure that the type actually is unwind-safe and add manual `impl UnwindSafe`/`impl RefUnwindSafe` with a comment justifying
why the type's invariants prevent observing inconsistent state during unwind.
