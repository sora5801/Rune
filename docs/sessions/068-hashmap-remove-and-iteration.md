# Session 068 — HashMap remove + .keys() iteration

**Date:** 2026-05-24
**Outcome:** HashMap gains tombstone-based `remove(k)` and a
`keys()` iterator. The user can now iterate, mutate, and
re-iterate without leaking. Str-key support is deferred to
session 069.

```rune
let m: std::HashMap<i64, i64> = hashmap_new();
m.insert(1, 10); m.insert(2, 20); m.insert(3, 30);
let old: i64 = m.remove(2);          // -> 20; m.len() == 2 now
m.insert(2, 22);                      // reuses the tombstone
for k in m.keys() {
    print(k);                         // 1, 2, 3 in hash order
    print(m.get(k));                  // 10, 22, 30
}
```

~5 files. 580 tests green (+6 from session 067).

## The decisive observation

The two features share infrastructure. Tombstones (occupied
state 2) are what makes remove safe in an open-addressing
table — a removed slot would otherwise break probe chains for
keys inserted later in the chain. The iterator needs to know
about tombstones too (skip them, like empty slots). And the
per-V release walk synthesized at codegen needed to tighten
its `occupied != 0` check to `occupied == 1` to avoid
double-freeing a value the user already received from
`remove`.

Iteration without tuples is awkward — `.keys()` yields i64
keys, the user does `m.get(k)` per iteration. Two passes of
the hash table, conceptually, but the probe path is short
(75% load factor) so it's fine in practice. Cleaner than
yielding a Pair<K, V> struct that the user has to
destructure.

## The wire-ups

```
runtime.c            (occupied is tri-state: 0=empty, 1=live,
                      2=tombstone. rune_hashmap_probe walks past
                      tombstones (the live key may be further
                      down the chain). New
                      rune_hashmap_probe_for_insert tracks the
                      first tombstone passed so insert reuses
                      it. rune_hashmap_remove returns the
                      previous value (or 0) and writes occupied
                      = 2. rune_hashmap_grow drops tombstones —
                      they don't carry over to the rehashed
                      table; probe chains shrink. New
                      inspection helpers: rune_hashmap_cap,
                      rune_hashmap_is_live_at,
                      rune_hashmap_key_at. The iterator in
                      std.rn calls these directly.)

src/std.rn           (HashMapKeysIter<V> { map: HashMap<i64,
                      V>, cursor: i64 } + Iterator impl. next
                      loops cursor 0..cap, skipping non-live
                      slots, yielding the slot's key when
                      occupied. The map field's ARC keeps the
                      backing alive for the iterator's
                      lifetime.)

src/resolver.rs      (Three new PolyBuiltinFn entries for
                      hashmap_cap / hashmap_is_live_at /
                      hashmap_key_at — used by std.rn's
                      HashMapKeysIter.next body.)

src/checker.rs       (resolve_method's HashMap arm adds
                      "remove" (returns V, same shape as get).
                      builtin_vec_iter_sig is extended to also
                      handle `m.keys()` on a HashMap (returns
                      Ty::Struct(HashMapKeysIter, [V])).
                      check_poly_builtin_call handles the three
                      new inspection builtins with explicit arg
                      checks.)

src/lower.rs         (lower_poly_call dispatches the three new
                      inspection builtins to their runtime fn
                      names. New `lower_hashmap_keys` mirrors
                      lower_vec_iter — builds a
                      HashMapKeysIter struct lit from the
                      receiver. compile_method_call's HashMap
                      arm now matches "remove" alongside the
                      previous four methods.)

src/codegen.rs       (Externs + JIT symbol-binding for the
                      five new runtime fns. declare_builtin
                      signatures for hashmap_remove / cap /
                      is_live_at / key_at. The HashMap
                      method-call arm in compile_method_call
                      routes "remove" to rune_hashmap_remove
                      and treats its return value as a
                      transfer (no retain — the slot gave up
                      its +1). The per-V release-walk in
                      define_hashmap_release tightened from
                      `occupied != 0` to `occupied == 1` so
                      tombstoned slots aren't double-freed.)
```

## What's tested

Codegen (+6):

- `hashmap_remove_returns_previous_value` — round-trip:
  insert, remove, observe the returned value, contains_key=
  false, len decremented.
- `hashmap_remove_missing_key_returns_zero` — removing an
  absent key is a no-op returning 0; double-remove same.
- `hashmap_remove_then_reinsert_reuses_tombstone` — insert
  after remove restores contains_key and reuses the slot
  (probe-for-insert path).
- `hashmap_keys_iter_visits_each_live_key_once` — sum-based
  test (hash-driven order) over 5 entries, confirms all
  keys + values reached.
- `hashmap_keys_iter_skips_tombstones` — remove a middle
  key, iterate, confirm the iterator returns only live keys.
- `hashmap_keys_iter_empty_map` — empty iterator yields
  nothing.

## Apparent bugs that aren't / explicitly deferred

- **No `.values()` iterator.** `for k in m.keys() { let v =
  m.get(k); ... }` is the idiom. Adding values() would be
  trivial (mirror keys, return `V` from the slot via a new
  `hashmap_val_at`). Deferred.
- **No `.entries()` (k, v) pairs.** Rune doesn't have tuples,
  and a `Pair<K, V>` struct with destructuring sugar isn't
  there yet. The keys+get pattern works fine for now.
- **The .keys() iter holds the map by ARC**, so a `for k in
  m.keys() { m.remove(k); }` is a "modify-during-iteration"
  hazard — the user's remove writes occupied=2, the iter's
  next sees it, skips. The iterator's behavior is
  well-defined: yields any key the iterator *hasn't yet
  passed* in slot order. But inserts during iteration
  (especially those that trigger grow) would invalidate the
  iter — cap changes underneath. Documented hazard; v0.x
  expects users to collect-keys-first if they need
  iteration-during-mutation safety.
- **Tombstone load factor.** Many remove/insert cycles
  accumulate tombstones; the load factor counts only live
  entries but the probe length grows. Grow drops them, but
  grow is triggered only when (len + 1) * 4 > cap * 3 —
  pure remove never grows. Pathological remove-heavy
  workloads need explicit rebuild. Acceptable for v0.x.
- **Str-key remove and iteration** wait for session 069 —
  same tombstone shape, just a different hash + equality
  per probe step.

## What's next

- **HashMap str-keys (session 069)** — separate hash + per-
  bucket equality. Probably a tagged descriptor + branching
  in probe / hash, OR a parallel `rune_strmap_*` family.
- **Trait default-method bodies** — `.collect()` as chained.
- **`?` on Option** — currently Result-only.
- **Multi-impl `Into` disambiguation** — `impl_methods` is
  keyed by name only.
- **Self-hosted bootstrap** — long-term.
