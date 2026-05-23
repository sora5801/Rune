# Session 053 — Iterator protocol + `for x in iter`

**Date:** 2026-05-23
**Outcome:** A user struct that implements the prelude's
`std::Iterator` trait can be the right-hand side of `for x in iter`.
The desugar runs `while true { match iter.next() { Some(x) => ...,
None => break } }`, picking up the impl's `type Item` binding for
the loop variable. The pattern composes with the bounded-generic
form: `fn count<T: Iterator>(it: T)` works for any concrete
implementor. `break` is wired through codegen as a real
control-flow construct. The session also lifts two latent checker
restrictions (field-assign through a param; `if`/`else` unification
across enums with mismatched type-arg lists) — both were blockers
for the iterator pattern but neither was strictly iterator-specific.
~7 files. 510 tests green (+7 from 503).

## The decisive observation

The hard work was already done. Session 049 supports
`type Item = ...` bindings; session 051 resolves `T::Item` through
substitution; session 050's supertrait BFS gives bounded-generic
method lookup transitively. The only missing pieces are (a) wiring
`HirExprKind::Break` through codegen (it had been parser-level
since the beginning but stubbed to `Unsupported` in lower) and (b)
teaching `lower_for` to dispatch on "is this iter's type a struct
or type-param that implements Iterator?" and emit the desugar.
Everything else flows through existing machinery.

## The wire-ups

```
src/
├── hir.rs        (HirExprKind::Break — no payload, Ty::Never)
├── lower.rs      (ast::Expr::Break -> HirExprKind::Break; new
│                  lower_for_iterator builds the while-match desugar;
│                  find_iterator_sym / find_option_sym walk the
│                  prelude's symbol table once per for-loop)
├── codegen.rs    (FnCodegen.loop_exit_stack: Vec<(Block, snapshot)>;
│                  compile_while/for/for_range push their exit block;
│                  HirExprKind::Break releases ARC-locals back to
│                  the snapshot and jumps to the top entry's exit)
├── checker.rs    (check_for: Ty::Struct(s, _) if impls Iterator -> item
│                  from impl_assoc_bindings_ty; Ty::TypeVar(t) with
│                  bound that closes over Iterator -> Ty::Assoc;
│                  check_place_root_mutable lifts the Param restriction)
├── ty.rs         (Ty::unify pick-concrete-args helper for
│                  Enum/Struct/Vec — `Some(v)` and `None` now unify
│                  to the same enum sym with the concrete args from
│                  whichever side has them)
└── std.rn        (pub trait Iterator { type Item;
                       fn next(self: dyn Iterator) -> Option<Self::Item>; })
```

`Break` releases ARC locals by snapshot: each loop-entry pushes
`(exit_block, arc_locals.len())` onto a stack; `Break` releases
everything past the snapshot before jumping. The synthesized `__it`
local from the iterator desugar lives across iterations — it's
pushed before the loop starts and released only when the loop
exits (either via the natural `None` arm or via a user `break`).

The `lower_for` dispatch is three-way:
1. `iter: Ty::Array(elem, n)` → existing `HirExprKind::For`
   (counted loop, no method dispatch).
2. `iter: Ty::Struct(s, _)` where `s` implements `Iterator` → new
   while-match desugar. Item type is `impl_assoc_bindings_ty[(s, "Item")]`
   from the checker's pass 1.
3. `iter: Ty::TypeVar(t)` where `t`'s bound closure contains
   `Iterator` → same desugar; item type is
   `Ty::Assoc(TypeVar(t), "Item")` which monomorphization resolves
   per call site.

## Two checker fixes the iterator pattern revealed

**`check_place_root_mutable` lifted the Param restriction.** Rune
structs are heap-allocated descriptor pointers; assigning to
`param.field` mutates the heap location both caller and callee
share. There was never a stack-aliasing hazard — the check was
defensive but overly so. Without lifting it, an iterator's `next`
couldn't advance `self.n`. The fix is one match arm; the old
test `field_assignment_on_param_is_error` becomes
`field_assignment_on_param_allowed`.

**`Ty::unify` got a pick-concrete-args rule for generic types.**
The placeholder `[]` args list that variant-construction sites
emit (`None: Ty::Enum(option, [])`) now unifies with a concrete
use-site list (`Some(v): Ty::Enum(option, [i64])`), picking the
non-empty side. Mirrors the existing `Ty::compatible` rule —
both should treat generics by sym, args from whichever side has
them. Without this, `if c { Some(v) } else { None }` failed to
typecheck as `Option<i64>` (the canonical Iterator `next` body
shape).

## What's tested

Codegen (+5):

- `iter_counter_for_in` — the headline. A `Counter { n, limit }`
  impl walks `n` from 1 to 5; the body sums to 15.
- `iter_break_from_loop_body` — `break` inside the for body exits
  the loop, releasing `__it`. Counter walks to 7 then breaks; sum
  is 21.
- `iter_bounded_generic` — `fn count<T: Iter>(it: T) -> i64`
  consumes any iterator. Specialized for Counter, returns 7 for a
  Counter of limit 7. (Uses `use std::Iterator as Iter;` because
  generic bounds parse as single Idents today, not paths.)
- `iter_early_return_from_for_body` — `return` inside the body
  releases `__it` via `release_all_arc_locals`. Counter walks to
  first n > 41 and returns it.
- `iter_nested_for_array_inside_iterator` — outer for-in over a
  Counter, inner for-in over an array `[x, x*2, x*3]`. Tests that
  the dispatch in `lower_for` is per-call-site, not per-function.

Typecheck (+2):

- `for_in_non_iterator_struct_rejected` — `for x in bag` where
  Bag has no Iterator impl → "does not implement `std::Iterator`".
- `iterator_impl_missing_next_rejected` — `impl Iterator for
  Counter` without `fn next` → existing conformance check fires.

Plus the regressed test `field_assignment_on_param_is_error` is
flipped to `field_assignment_on_param_allowed`.

## Apparent bugs that aren't / explicitly deferred

- **Generic bounds accept only single-Idents.** A
  `<T: std::Iterator>` parse-errors today. Workaround: `use
  std::Iterator as Iter; fn count<T: Iter>(...)`. Session 050
  flagged this as a known limitation (the supertrait list has the
  same shape and same limitation). Lifting to `Vec<Path>` is a
  small follow-up session.

- **No `Vec::iter()`, `slice::iter()`, range iter as struct.** The
  user writes their own iterator struct. The compiler's `Vec` and
  `Array` for-in paths still go through the existing
  counted-loop codegen — they're not built on top of `Iterator`.
  This will change when collections become user-written.

- **No iterator adapters (`map`, `filter`, `collect`, etc.).**
  These are surface-level — once user types want them, they'll
  be plain trait methods. Out of scope this session.

- **`continue` still stubs to `Unsupported`.** Only `break` is
  wired this session. The same loop_exit_stack pattern would work
  for continue (jump to header instead of exit, plus release ARC
  locals back to the snapshot) but the iterator desugar doesn't
  need it.

- **`loop { }` keyword not added.** Users write `while true { ...
  break; }`. The desugar itself uses this idiom internally.

- **Iterator-trait method dispatch is static, not dynamic.**
  `(it: dyn Iterator).next()` still hits session 051's collapse
  diagnostic — the method returns `Option<Self::Item>` and `Self`
  can't be projected through `dyn`. Correct behavior; not a
  regression. The for-in desugar works through static dispatch
  (concrete struct or bounded type-param) only.

- **The synthesized `__it` and `__x` SymbolIds are fresh from
  the lowerer's `Cell<u32>` counter** (introduced session 031
  for `?`'s desugar). They're not added to `Resolutions::symbols`
  — codegen looks them up only via `var_map`, which doesn't need
  the symbol table. If anything ever wants to *resolve* the
  synthetic syms back to source, that lookup must be moved.

## Symbol-identity bug check (per session 048's lesson)

Three risks, all handled:

- **`find_iterator_sym` / `find_option_sym` walk symbols by name.**
  A user defining their own `trait Iterator` or `enum Option` in
  some module would produce a second symbol with the same bare
  name. The prelude is parsed first, so the prelude's symbol is
  interned first — the walk finds it. A user-defined `Iterator`
  with their own struct impl would NOT match `impls_for[s]
  .contains(prelude_iterator_sym)`, so the for-in dispatch
  correctly skips it.
- **Synthetic `__it` and `__x` SymbolIds.** Fresh from
  `Lowerer.next_sym`, which was initialized past
  `res.symbols.len()`. No span collision with any user-written
  binding because they have no associated span at all.
- **Cross-test cache state.** The Iterator/Option sym lookups
  happen per call site in the checker/lowerer. Both walk fresh
  each test run (a new `Resolutions::symbols` per compilation),
  so no stale cache.

## What's next

- **Generic bounds (and trait supertraits) accept paths.** A
  one-session fix that lifts the `use std::Iterator as Iter;`
  workaround.
- **`Vec::iter() -> SomeIter` and friends.** Make the existing
  collections iterable through the new protocol; eventually
  rewrite the for-over-Vec/Array codegen path on top of it.
- **Iterator adapters** — `map`, `filter`, `collect`. Trait
  methods that return new iterators; nothing the language
  needs to add, just stdlib.
- **`HashMap<K, V>`** — the other half of a usable collections
  module.
