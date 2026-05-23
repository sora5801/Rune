# Session 049 — Associated types

**Date:** 2026-05-21
**Outcome:** A trait can declare `type Item;` and an impl binds
`type Item = Concrete;`. `Self::Item` in trait method signatures is
abstract; in impl method signatures it resolves to the impl's bound
type. Eager resolution — no new `Ty` variant, no codegen change.
The `T::Item` projection through a type parameter is deferred. ~3
files. 490 tests green (+3 from 487).

## The gap

Traits had methods only. There was no way to give a trait a member
type each implementor specifies — the prerequisite for an iterator
protocol (`trait Iterator { type Item; fn next(...) -> Self::Item; }`)
and any abstraction that returns "whatever the implementor decides".

## The design — eager resolution

`Self` and `type` are plain identifiers in the lexer, so `Self::Item`
already parses as a two-segment `Type::Path`, and `type Item = ..;`
needs only contextual detection in the member loop. **No new
`ast::Type` variant, no new `ty::Ty` variant.**

Resolution is **eager** — by the time anything past the checker runs,
every method signature is fully concrete:

- In an **impl** method signature, `Self::Item` resolves to the
  impl's bound concrete type from a new `impl_assoc_bindings` map.
  `c.next()` on a `Counter` types as `i64` directly.
- In the **trait** declaration itself, `Self::Item` is abstract.
  It resolves to `Ty::Error`, which is compatible with everything,
  so it never propagates spurious mismatches into trait callers.

The lowerer, monomorphizer, and codegen see ordinary concrete types
throughout. None of them needed any change.

## The wire-ups

```
src/
├── ast.rs       (AssocTypeDecl, AssocTypeBinding;
│                 TraitDecl.assoc_types, ImplBlock.assoc_types)
├── parser.rs    (parse_trait / parse_impl detect a `type` member
│                 via a new `check_contextual` helper)
├── resolver.rs  (trait_assoc_types + impl_assoc_bindings maps;
│                 resolve_type short-circuits a leading `Self`
│                 segment so `Self::Item` is not reported as
│                 `Self` unresolved)
└── checker.rs   (SelfContext + Checker.current_self; resolve_type
                  for `Self::Item` consults it; current_self set
                  around register_signatures' Impl/Trait arms and
                  check_item's Impl arm; check_trait_impl_conformance
                  verifies bindings)
```

The keystone is `current_self`. With it set during pass 1b
(`register_signatures`), the stored `fn_signatures` for impl methods
already substitute `Self::Item` to the concrete type. Every later
consumer — `user_method_sig`, the lowerer, the monomorphizer — reads
those signatures as if they had been ordinary all along.

## What's tested

Codegen (+1): `assoc_type_concrete_method_call` — `trait Iterator
{ type Item; .. }`, `impl Iterator for Counter { type Item = i64; .. }`,
and `c.next()` returning `i64`.

Typecheck (+2): `impl_missing_assoc_type_rejected` (impl omits
`type Item`), `impl_unknown_assoc_type_rejected` (impl binds a name
the trait never declared).

## Apparent bugs that aren't / explicitly deferred

- **`T::Item` projection through a type parameter** (`fn sum<I:
  Iterator>(it: I) -> I::Item`) — the hard case. It needs a real
  projection `Ty` variant carried through `subst_ty`/`apply_subst`
  and resolved per-specialization in the monomorphizer. Deferring
  it is what kept this session a one-day change. It is the next
  step toward a usable iterator protocol.

- **`dyn Iterator` whose method returns `Self::Item`** types the
  call result as `Ty::Error` (the trait-side abstract resolution).
  Acceptable for v0.x and consistent with session 033's "trait
  `self` type is written but ignored" precedent.

- **Generic-impl associated bindings.** `impl<T> Iterator for Box<T>
  { type Item = T; .. }` resolves the RHS `T` to a TypeParam symbol
  scoped within `declare_impl`, which is not the same symbol the
  methods use after the parser merge — substitution would not line
  up. Concrete RHS is the supported v0.x form.

- **Associated-type defaults**, bounds on assoc types
  (`type Item: Display`), and bare `Self` as a standalone type.

## What's next

- **`T::Item` projection** — unblocks an iterator protocol.
- **Supertraits** (`trait Sub: Super`) — session 050.
