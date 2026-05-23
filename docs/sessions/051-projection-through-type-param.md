# Session 051 — `T::Item` projection through a type parameter

**Date:** 2026-05-22
**Outcome:** A generic function `fn f<T: Iterator>(x: T) -> T::Item`
now typechecks, monomorphizes, and runs. Each call site picks up the
impl's `type Item = …` binding for the concrete `T`. Two new `Ty`
variants (`SelfType`, `Assoc`) carry projections through the IR; the
monomorphizer resolves them at substitution time via the impl
bindings recorded by the checker. Calling such a method through
`dyn Trait` is now a precise typecheck error rather than a silent
`Ty::Error` collapse. ~7 files. 498 tests green (+4 from 494).

## The decisive observation

A function bounded `<T: Iterator>` whose return type is `T::Item`
can't be checked structurally — at the function declaration site
the projection has no concrete type yet. But it has structure: a
*base* (`T`) and a *name* (`Item`). Carry that structure through
the IR as `Ty::Assoc(Box<Ty>, String)` and the monomorphizer is the
natural place to resolve it: by the time `f` is specialized,
`T` is bound to a concrete struct, and the checker has already
recorded which `(struct, name)` pairs are bound to which types.

`Self::Item` in a trait method signature has the same shape — it's
just a projection through the special base `Ty::SelfType`. The
substitution paths fold together: when a method is pulled from the
impl side for a concrete receiver, `SelfType` is substituted to the
receiver type, then `Assoc` is resolved against the impl bindings.

## The wire-ups

```
src/
├── ty.rs           (Ty::SelfType, Ty::Assoc(Box<Ty>, String) +
│                    display arms)
├── resolver.rs     (assoc_proj_bases: Span → SymbolId; records
│                    the base type-param sym for every 2-segment
│                    path whose head is a TypeParam)
├── checker.rs      (T::Item arm in resolve_type writes Ty::Assoc;
│                    apply_subst_inner walks Assoc, resolves
│                    Assoc(Struct, name) via impl_assoc_bindings_ty;
│                    dyn_method_sig flags Assoc-through-Dyn and
│                    emits a diagnostic at the call site)
├── hir.rs / lower.rs (HirModule.impl_assoc_bindings_ty plumbed
│                    from checker to monomorphizer)
├── monomorphize.rs (IMPL_ASSOC_BINDINGS thread-local; subst_ty
│                    arms for Assoc/SelfType that consult it)
└── codegen.rs      (defensive error arm if an unresolved
                     projection survives to codegen — should
                     not happen, but the message names the bug
                     clearly instead of "unsupported type")
```

The `IMPL_ASSOC_BINDINGS` thread-local in `monomorphize.rs` is a
pragmatic choice — `subst_ty` recurses through every type
constructor and is called from ~6 sibling free functions. Threading
a bindings argument through that tree would touch ~80 call sites
for a read-only lookup. Per-thread storage is set at the top of
each `monomorphize_module` call; cargo's parallel test runners each
get their own copy.

## What's tested

Codegen (+2):

- `assoc_type_projection_through_type_param` — the headline.
  `fn bump<T: Iterator>(x: T) -> T::Item { x.next() }`. Calling
  with a `Counter` impl whose `Item = i64` returns `i64` correctly.
- `assoc_type_projection_distinct_impls` — two impls bind `Item`
  to different concrete types (`i64` and `str`). Each specialization
  of the generic picks up the right binding, so the same source
  expression `pluck(c)` types as `i64` for one receiver and `str`
  for the other. The `.len()` call on the `str` result confirms
  the substitution really happens.

Typecheck (+2):

- `assoc_type_method_rejected_through_dyn` — `(it: dyn Iterator)
  .next()` produces a precise diagnostic instead of silently
  collapsing to `Ty::Error` and dropping further messages. The
  collapse still happens in the IR (so the rest of the function
  body doesn't avalanche errors), but the call site reports the
  reason once at its origin.
- `assoc_type_projection_without_bound_rejects_method_call` — a
  `<T>` (no bound) value can't call trait methods, so `x.next()`
  reports "no method `.next` on type `T#N`" as before. The
  projection in the return type is opaque until monomorphization
  would have resolved it — at which point the missing trait
  bound is moot because the method call already failed.

## Apparent bugs that aren't / explicitly deferred

- **No diagnostic for `T::Item` with no `T: Trait` bound at the
  function declaration site.** Rune doesn't track which trait
  introduces an associated-type name. A `T::Item` projection
  binds against any impl that names `Item` on `T`'s concrete
  type, regardless of which trait it came from. The current
  diagnostic surface (the method-call error above) catches the
  common case; the missing-bound check would need name → trait
  resolution that v0.x doesn't have.

- **`dyn Trait` upcasting / flattened vtable.** Still deferred
  from session 050. With the projection diagnostic now in place,
  the user gets a clear signpost: "call it on a concrete receiver
  instead". The fix is the same work as session 050's deferred
  upcast.

- **Projections deeper than one level.** `T::Item::Inner` would
  require `Ty::Assoc` to nest (which it already does — `Assoc` is
  `Box<Ty>`) and a second resolution pass after the first
  substitution. Not exercised by any test today, no impl yet.

- **`IMPL_ASSOC_BINDINGS` is a thread-local.** Cargo's test
  parallelism is the only multi-threaded caller; each test
  `monomorphize_module` overwrites the bindings before reading
  them. No data race, but a future concurrent compilation
  scheme would need to make `MonoState` own the map.

## What's next

- **`dyn Trait` upcasting** — make supertraits available on
  trait objects, and let projection-returning methods work on
  trait objects too (likely via flattened vtable).
- **A `collections` module** — `HashMap<K, V>`, an iterator
  protocol built on top of `T::Item`. The plumbing is in place.
- **Trait-method resolution for `T::Item` projections without a
  recorded base.** Right now an unresolved projection survives
  to codegen and gets a defensive internal error; teach
  monomorphize to surface that as a "no impl of trait T for
  type S" user-facing diagnostic.
