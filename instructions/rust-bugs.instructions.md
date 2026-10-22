---
applyTo: "**/*.rs"
---

# `!Sync` markers and the `PhantomData<Cell<()>>` pattern

When a `!Sync` marker is needed, use `PhantomData<Cell<()>>`. The `Cell<()>` type
is natively `Send + !Sync`, so no `unsafe impl Send` is required. Since `Cell`
wraps `UnsafeCell` which is `!RefUnwindSafe`, a manual `impl RefUnwindSafe` is
needed on the containing type. This is sound only if all other fields are also
ref-unwind-safe — verify this before adding the impl. Do not use
`PhantomData<*mut ()>` with `unsafe impl Send` — while this achieves the same
`!Sync` effect, it triggers a rustc bug where async generator auto-trait
inference cannot prove `Send` when the type parameter is a trait object
(`dyn Trait`).

More broadly, never put a `'static` bound on an `unsafe impl Send` or
`unsafe impl Sync`. If the struct definition already requires `T: 'static`, the
bound is redundant on the impl and repeating it triggers the same rustc bug
(rust-lang/rust#110338). The higher-ranked region solver fails to prove `'static`
when the type parameter is a trait object. Since the struct bound already enforces
`'static` at construction time, omitting it from the impl is safe.
