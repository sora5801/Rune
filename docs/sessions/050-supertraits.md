# Session 050 — Supertraits

**Date:** 2026-05-21
**Outcome:** `trait Sub: Super { .. }`. A trait declares one or more
supertraits; implementing the subtrait requires implementing each
ancestor; a value bounded by the subtrait can call the supertrait's
methods. Static dispatch only — `dyn Sub` exposing `Super`'s
methods is deferred. The whole feature is checker + resolver work;
nothing past the checker changed. ~4 files. 494 tests green (+4
from 490).

## The decisive observation

For a bounded generic `fn f<T: Sub>(x: T)`, every method call on
`x` becomes a direct `Call` after monomorphization, looked up in
`impl_methods[(concrete_struct, method_name)]`. The trait identity
is gone before codegen runs. So **the entire static-dispatch slice
is checker + resolver work** — zero changes to `lower.rs`,
`monomorphize.rs`, `codegen.rs`. Supertraits become a checker rule
about *which methods are visible* and *which impls are required*.

`dyn Sub` is asymmetric. Its method table is laid out per-trait in
sessions 033/034's box format. Exposing `Super`'s methods on a `dyn
Sub` would need a flattened vtable or a `dyn Sub → dyn Super`
upcast — both significant box-layout work. **Deferred**: a
`dyn Sub` continues to expose only `Sub`'s own methods.

## The wire-ups

```
src/
├── ast.rs       (TraitDecl.supertraits: Vec<Ident>)
├── parser.rs    (parse_trait reads an optional `: A + B`)
├── resolver.rs  (trait_supertraits + impls_for maps;
│                 validate_supertrait_cycles)
└── checker.rs   (check_trait_impl_conformance walks the
                  supertrait closure and requires impls_for;
                  trait_bound_method_sig walks supertraits
                  transitively for method lookup)
```

- `trait_supertraits` is populated in pass 2 (so every trait symbol
  exists), by resolving each `Ident` to a trait `SymbolId`.
- `impls_for: HashMap<SymbolId, HashSet<SymbolId>>` is strict: it
  records only what the user wrote as `impl Trait for Type`.
  Inherent methods that happen to share a trait method's name do
  not satisfy a supertrait requirement — diagnostics name the
  missing `impl` explicitly.
- Cycles in the supertrait graph (`trait A: B`, `trait B: A`) are
  surfaced as a clear `supertrait cycle through `…`` error. The
  worklist walks in the checker also carry a `visited` set, so
  even an undiagnosed cycle never hangs the compiler.

## What's tested

Codegen (+2):

- `supertrait_method_via_bound` — `trait Dog: Animal`; `<T: Dog>`
  calls both `Dog`'s `bark` and the supertrait's `speak`. The
  method lookup walks the supertrait chain.
- `supertrait_two_level_chain` — `A: B`, `B: C`; `<T: A>` calls all
  three. Verifies transitive method lookup and conformance.

Typecheck (+2):

- `impl_missing_supertrait_rejected` — `impl Dog for Lab` without
  `impl Animal for Lab` → "trait `Dog` requires supertrait `Animal`
  to be implemented for `Lab`".
- `unresolved_supertrait_rejected` — `trait Dog: Unknown` →
  "unresolved trait `Unknown`".

## Apparent bugs that aren't / explicitly deferred

- **`dyn Sub` does not expose `Super`'s methods.** Calling a
  supertrait method on a `dyn Sub` is a type error today. The fix
  needs either a flattened vtable (revisiting the session-033 box
  layout) or an explicit `dyn Sub → dyn Super` upcast — its own
  session.

- **Cross-module supertraits.** `TraitDecl.supertraits: Vec<Ident>`
  mirrors `GenericParam::bounds` and accepts single-segment names
  only. A supertrait in another module must be brought into scope
  with `use`. Both forms can be lifted to `Vec<Path>` later as a
  single change.

- **Supertrait method whose signature mentions `Self::Item`.** In
  a trait declaration, `Self::Item` resolves to `Ty::Error`
  (session 049) — compatible with everything, so a `<T: Sub>` call
  returning `Self::Item` types as `Ty::Error` and propagates
  without spurious diagnostics. The `T::Item` projection through
  a type parameter is its own future session.

- **Method-name collision between sub and supertrait.** Rune
  already rejects two impls of the same method name on a struct.
  So `impl Sub for S` and `impl Super for S` cannot both declare
  the same method name. Consistent with existing behavior; no
  shadowing in v0.x.

## What's next

- **`T::Item` projection through a type parameter** — completes the
  associated-types story and unblocks an iterator protocol.
- **`dyn Sub` upcasting / flattened vtable** — makes supertraits
  available on trait objects.
- **A `collections` module** — `HashMap<K, V>`, iterator protocol
  on top of the now-rich trait system.
