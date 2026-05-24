# Session 076 — Iterator `.count()` and `.sum()`

**Date:** 2026-05-24
**Outcome:** Two more default-body methods on Iterator —
`.count()` returns the number of elements, `.sum()`
accumulates i64 elements. Every Iterator impl inherits both
through session 071's machinery; specialization happens
per Self at each call site. 610 tests green (+7 from
session 075).

```rune
v.iter().count()                  // 5
(0..7).count()                    // 7
v.iter().sum()                    // 60 (for [10, 20, 30])
(1..6).sum()                      // 15
std::Map { iter: v.iter(),
           f: |x| x * x }.sum()   // 30 (for [1,2,3,4])
```

## The decisive observation

Pure leverage of session 071. The Iterator trait gains two
new bodies; nothing else changes. Each impl block continues
to declare only its `type Item` and its `next` method, but
the per-impl default-method fill (resolver's `declare_impl`
arm from session 071) now copies three default fns —
`collect`, `count`, `sum` — into each impl's `impl_methods`
table. Monomorphization specializes per Self at each call
site; `self.next()` inside the body resolves via the
specialized impl's next method.

`.count()` is shape-monomorphic (works for any Self::Item).
`.sum()` requires `Self::Item = i64` because the body's
`total + x` is i64 arithmetic — Self::Item is substituted at
spec time, and if it isn't i64 the resulting body fails to
typecheck (caller-side error, not a trait bound). v0.x has
no `Numeric` bound to express the constraint formally; once
added it would generalize sum to `Self::Item: Add<Output =
Self::Item>` like Rust's.

## The wire-ups

```
src/std.rn        (Iterator trait gains two default bodies:
                   `count(self: Self) -> i64` and
                   `sum(self: Self) -> i64`. Both use the
                   while-true + match self.next() pattern
                   that collect already established. Both
                   declare `let mut` locals — the first
                   default-body methods to do so.)
```

That's it. Session 071's resolver + lower + checker work
covers everything else — the default fns get HirFn entries
per session 071's `lower_trait_default`, and the impl_methods
fill loops over `trait_defaults` so every Iterator impl
picks them up.

## What's tested

Codegen (+7):

- `iterator_count_default_method` — `v.iter().count()` = 5.
- `iterator_count_on_range` — `(0..7).count()` = 7 (RangeIter
  inherits via session 063's RangeIter Iterator impl).
- `iterator_count_on_filter_adapter` — Filter inherits; the
  count walks only elements that pass the predicate.
- `iterator_count_through_filter_and_map` — chained adapters
  + .count() at the end. Confirms specialization across the
  Iterator chain.
- `iterator_sum_default_method` — `v.iter().sum()` for
  `[10, 20, 30]` = 60.
- `iterator_sum_on_range` — `(1..6).sum()` = 15.
- `iterator_sum_on_map_adapter_with_closure` — Map of `x *
  x` over `[1,2,3,4]` summed = 30. Exercises Map's inherited
  sum (since Map's Item = U after closure transformation).

## Apparent bugs that aren't / explicitly deferred

- **`.sum()` is i64-only.** Body uses `total + x` with
  `total: i64`. For non-i64 Self::Item the specialized body
  fails at spec time (the `total + x` typecheck operates on
  the substituted Self::Item). Future generalization needs a
  numeric trait bound or impl-block specialization per
  element type.
- **`.filter(p)` and `.map(f)` aren't methods yet.** Both
  would need method-level generic parameters (`fn
  map<F, U>(...)`) which v0.x's trait method declarations
  don't parse. Users still construct adapters as struct
  literals (`std::Filter { ... }`, `std::Map { ... }`).
  Lifting this needs work in the parser for method generics
  + the resolver to scope them.
- **`.fold(init, f)` similarly needs method generics** for
  the accumulator type and the closure type.
- **No `.min()` / `.max()` either**, for the same reason as
  sum — they'd require ordering bounds. `.min()` could be
  hardcoded for i64 if useful.
- **`.collect()` from session 071 still requires the result
  type to be `Vec<Self::Item>`.** A user-specified collect
  target type (e.g. `.collect::<HashSet<i64>>()`) would need
  turbofish syntax.

## What's next

- **Method-level generics in trait methods** — unblocks
  `.map`, `.filter`, `.fold` as default methods.
- **Numeric trait bounds** — generalizes `.sum()` /
  `.min()` / `.max()`.
- **Str-keyed HashMap iteration** — keys/entries on
  `HashMap<str, V>`.
- **Match-arm tuple patterns**.
- **Self-hosted bootstrap** — long-term.
