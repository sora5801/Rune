# Session 052 — `dyn Sub` exposes supertrait methods

**Date:** 2026-05-23
**Outcome:** A value of type `dyn Sub` can call methods declared on
any of `Sub`'s transitive supertraits — `dyn Dog` (where
`Dog: Animal`) exposes both `bark` and `speak`. The dyn-box's
method-pointer table is laid out flat: the trait's own methods
first, then each supertrait's methods in BFS order, deduped
first-wins by name. ~4 files. 503 tests green (+5 from 498).

## The decisive observation

Static dispatch already walks the supertrait chain in
`trait_bound_method_sig` (session 050). The `S → dyn Sub`
coercion already enforces every-supertrait conformance via
`check_trait_impl_conformance` (impl-side). **Two pieces were
missing**: (a) method *visibility* on a `dyn` receiver — extend
`dyn_method_sig` to walk supertraits the same way — and (b)
*vtable slots* for the supertrait methods — a flat per-trait
method list. Everything else (coercion checks, release, ARC)
already works.

## Layout

For `dyn Dog` where `Dog: Animal`, the box is:

```
[bark, speak, data, drop, rc]
   0      1     2     3    4   (slot index)
```

- Dog's methods first in declaration order.
- Then each supertrait in BFS order; within each supertrait, methods in declaration order.
- Deduped first-wins by name (Rust-style; method shadowing across the chain isn't reachable today since the impl-conformance check forbids two trait methods of the same name on one struct).

The same flat list drives all three codegen sites:

- `compile_dyn_box` builds the table by iterating the flat list
  (slot `i` ← `func_addr` of `impl_methods[(struct_sym, name)]`).
- `compile_dyn_call` locates the called method's slot by
  `methods_flat.iter().position(|(_, m)| m == method)`.
- `define_dyn_release` derives the data/drop/rc offsets from
  `N_flat * 8`, `(N_flat + 1) * 8`, `(N_flat + 2) * 8`.
- `emit_arc_call`'s `Ty::Dyn` arm reads the box rc at
  `(N_flat + 2) * 8` for retain/release on the box itself.

## The wire-ups

```
src/
├── hir.rs        (HirModule.trait_methods_flat:
│                  HashMap<SymbolId, Vec<(SymbolId, String)>>)
├── lower.rs      (build trait_methods_flat via BFS over
│                  res.trait_supertraits; first-wins dedup)
├── checker.rs    (dyn_method_sig: worklist BFS through
│                  trait_supertraits, mirroring
│                  trait_bound_method_sig's pattern)
└── codegen.rs    (Codegen.trait_methods_flat; FnCodegen reads
                   it; compile_dyn_box / compile_dyn_call /
                   define_dyn_release / emit_arc_call all use
                   the flat list keyed by the call-site trait)
```

The flat list is keyed by the **call-site trait sym** — `dyn Dog`
and `dyn Animal` are distinct runtime types with distinct slot
orderings (in `dyn Dog`, `speak` is at slot 1; in `dyn Animal`,
it's at slot 0). The `HirExprKind::DynCall.trait_sym` field stays
the trait the call was made through; codegen looks up the slot
via that key, which guarantees agreement between the box layout
and the call-site offset.

## What's tested

Codegen (+3):

- `dyn_supertrait_method` — the headline. `handle(d: dyn Dog)`
  calls both `d.bark()` and `d.speak()`; result is 42.
- `dyn_supertrait_two_level_chain` — `A: B`, `B: C`. A `dyn A`
  value can call methods from all three traits. Mirrors
  `supertrait_two_level_chain` for static dispatch.
- `dyn_supertrait_box_arc_under_loop` — 100 iterations of
  `let d: dyn Dog = lab; d.bark() + d.speak()`. The flat layout
  changed the box size; a wrong rc/drop offset would corrupt
  the heap within a handful of iterations.

Typecheck (+2):

- `dyn_supertrait_missing_impl_rejected` — `impl Dog for Lab`
  without `impl Animal for Lab`. The impl-conformance check
  (session 050) catches this before any dyn coercion runs;
  diagnostic: "trait `Dog` requires supertrait `Animal` to be
  implemented for `Lab`".
- `dyn_method_not_on_chain_rejected` — `dyn Dog` calls `.purr()`.
  The supertrait BFS runs to exhaustion and returns None;
  diagnostic: "no method `.purr` on type `dyn#N`".

## Apparent bugs that aren't / explicitly deferred

- **Projection-returning methods on `dyn` stay deferred.**
  Session 051's diagnostic still fires: a method whose return
  type involves `Self::Item` cannot be called through `dyn` —
  the bindings table has no entry for the abstract dyn receiver.
  The flat-vtable approach doesn't help here; supporting it
  would need either a flattened-`Item` slot in the box (runtime
  type info) or a `dyn Sub → dyn Super` upcast.

- **No `as dyn Super` upcast syntax.** The current layout (Sub's
  methods first, then Super's) happens to allow a zero-copy
  upcast for *single-supertrait* chains by trimming the box's
  prefix — but a diamond (`B: A + C`) breaks this. Explicit
  upcasting needs box-rewriting or per-trait runtime offset
  tables; its own session.

- **Method-name shadowing across the chain.** Today impossible:
  `check_trait_impl_conformance` plus the struct's
  no-duplicate-methods rule means no struct has two trait
  methods with the same name. The flat-list dedup is
  first-wins by Sub — forward-compat for if shadowing is ever
  allowed, Sub's method wins at the call site (matching Rust).

- **`dyn Sub` and `dyn Super` are distinct types.** The same
  concrete struct, coerced separately to `dyn Sub` and
  `dyn Super`, produces two boxes of different sizes. The
  drop slot in both still calls the same per-struct release;
  ARC is correct in both directions.

- **A trait with many supertraits enlarges every box.** The box
  size is `(N_flat + 2) * 8`. For `trait Display + Debug +
  Clone + Eq + Hash`, an instance is a 7-slot box (5 methods +
  data + drop) plus rc. This is the price of a per-instance
  table; switching to a shared static vtable would amortize
  this, but is a session 033/034-scale refactor.

## Symbol-identity bug check (per session 048's lesson)

The session-048 trap was duplicate spans across declarations.
The analogous risk here: **the flat-list key matters**. Codegen
keys on the *call-site* trait sym (the type the box was
constructed at), not the *owning* trait sym (the trait that
declared the method). Confusing the two would shift offsets —
a method written into a Dog-shaped box at slot 1 (`speak`)
would be read at slot 0 (`bark`'s slot from the Animal-shaped
box layout) by codegen reading the wrong key. Verified in all
three sites: `compile_dyn_box` writes at flat-list positions
keyed by Dog; `compile_dyn_call` reads at positions keyed by
the receiver's `Ty::Dyn(Dog)`; `define_dyn_release` keys by
the trait whose release function is being synthesized.

Second risk: **BFS order must be deterministic**. The
declaration-order Vec from `trait_supertraits` and the BFS
queue both preserve order; the dedup set is keyed by
`SymbolId` (insert-once). Switching `trait_supertraits` to a
`HashSet` would silently break box-vs-call-site agreement —
flagged here, even though it's not exposed to the user today.

## What's next

- **A `collections` module + iterator protocol** — built on top
  of `T::Item` (session 051) and now the full trait machinery.
  `Iterator { type Item; fn next(...) -> Option<Self::Item>; }`
  becomes implementable; `for x in iter` could desugar to it.
- **`From`-based error conversion for `?`** — small,
  self-contained, ergonomic win.
- **`dyn Sub → dyn Super` explicit upcast syntax** — completes
  the dyn story for cases where you want to pass a `dyn Sub`
  to an API that takes `dyn Super`.
