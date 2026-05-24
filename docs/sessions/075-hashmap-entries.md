# Session 075 — HashMap `.entries()`

**Date:** 2026-05-24
**Outcome:** `m.entries()` yields `(key, value)` tuples for
every live slot. Combines session 074's tuple machinery with
session 068's HashMapKeysIter shape. 603 tests green (+3
from session 074).

```rune
let m: std::HashMap<i64, i64> = hashmap_new();
m.insert(5, 100);
m.insert(7, 200);

let mut total: i64 = 0;
for kv in m.entries() {
    total = total + kv.0 * kv.1;
}                                  // 5*100 + 7*200 = 1900

// Or with destructuring (session 074):
for kv in m.entries() {
    let (k, v) = kv;
    total = total + k * v;
}
```

## The decisive observation

Every piece of infrastructure was already in place. The
runtime needed a single new helper — `rune_hashmap_val_at`
mirroring `rune_hashmap_key_at` — and std.rn got a
`HashMapEntriesIter<V>` struct identical to
`HashMapKeysIter<V>` except its `Iterator::Item` is `(i64,
V)` instead of `i64`. The lowerer's `lower_hashmap_entries`
is a copy-paste-renamed of `lower_hashmap_keys` building the
same `{ map, cursor }` struct literal.

The only real wrinkle was a latent bug surfaced by the test:
`apply_subst_inner_with` in the checker had a catch-all `_
=> ty.clone()` that silently dropped substitutions for
`Ty::Tuple`, `Ty::HashMap`, and `Ty::Dyn`. The tuple variant
hadn't been added when session 073 introduced tuples; with
no entries iter exercising it, the gap was invisible until
now. Fixed by adding explicit element-wise arms for all
three.

## The wire-ups

```
runtime.c            (rune_hashmap_val_at — companion to
                      _key_at, returns vals[i] at slot i.
                      Caller is responsible for ARC retain
                      on consumption, same as
                      rune_hashmap_get.)

src/codegen.rs       (extern + JIT symbol-binding for
                      rune_hashmap_val_at. declare_builtin
                      "hashmap_val_at" signature.)

src/resolver.rs      (intern hashmap_val_at as PolyBuiltinFn.)

src/checker.rs       (check_poly_builtin_call hashmap_val_at
                      arm returns the map's V element type.
                      builtin_vec_iter_sig adds an "entries"
                      arm returning Ty::Struct(EntriesIter
                      sym, [V]).
                      **Plus**: apply_subst_inner_with gains
                      Ty::Tuple / Ty::HashMap / Ty::Dyn arms
                      — previously the catch-all dropped
                      these, which was harmless until a
                      generic tuple needed to flow through
                      Iterator::Item substitution.)

src/lower.rs         (lower_poly_call dispatches
                      hashmap_val_at to the runtime name.
                      lower_hashmap_entries mirrors
                      lower_hashmap_keys — builds a struct
                      lit for HashMapEntriesIter. Method-
                      call lowering intercepts `.entries()`
                      on a HashMap receiver.)

src/std.rn           (HashMapEntriesIter<V> struct +
                      Iterator impl yielding `(i64, V)`
                      tuples. The body reuses the same
                      cursor + cap pattern as HashMapKeysIter:
                      skip non-live slots, return
                      `Option::Some((k, v))` when live.)
```

## What's tested

Codegen (+3):

- `hashmap_entries_iter_yields_pairs` — three inserts;
  for-loop sums keys and values separately via `kv.0` /
  `kv.1`. Confirms the tuple is constructed correctly and
  TupleIndex on the for-pat binding works.
- `hashmap_entries_destructure_in_for` — inside the loop,
  `let (k, v) = kv` destructures the pair. Combines session
  074's destructuring with session 075's iter.
- `hashmap_entries_with_str_values_doesnt_leak` — str-
  valued map (the values are str pointers). Tight loop of
  50 iterations; tuple per-shape release + str-rc=-1
  literals all clean up correctly. Total = 50 × (2+3) = 250.

## Apparent bugs that aren't / explicitly deferred

- **String-keyed entries aren't covered yet.** Session 069's
  str-keyed HashMaps work for insert/get/contains_key/remove
  but the entries iter (and the keys iter from session 068)
  pin K=i64. Adding str-key variants is mechanical: a
  parallel `HashMapStrEntriesIter<V>` plus a runtime helper
  that returns the slot's key as a `rune_str*` cast to
  int64. The current entries iter errors at type-check on
  `HashMap<str, _>` because builtin_vec_iter_sig matches
  HashMap<_, V> but the impl only handles `HashMap<i64, V>`.
- **The apply_subst_inner_with catch-all fix is a quiet
  correctness improvement.** Tuple and HashMap and Dyn
  generic-param substitution all worked correctly in the
  monomorphizer's subst_ty; this was the *checker*'s
  apply_subst leaving them as TypeVars during
  trait-bound-method dispatch and Self::Item projection.
  Pre-075, code that depended on this path would have hit
  the catch-all and silently kept TypeVars; in practice
  no shipped feature exercised it until entries-iter
  combined Self::Item, Ty::Tuple, and impl_assoc_bindings
  in one path.
- **No `.values()` iterator** — same shape but yields V
  alone, useful when you don't care about keys. One more
  copy-paste struct in std.rn.

## What's next

- **HashMap str-key iter** — keys, entries on str-keyed maps.
- **`.values()` iterator** — V-only iteration.
- **Match-arm tuple patterns** — `match opt { Some((k, v))
  => ... }`.
- **More default-body trait methods** — `.map(f)`,
  `.filter(p)`, `.fold(...)`, `.count()`, `.sum()`.
- **Self-hosted bootstrap** — long-term.
