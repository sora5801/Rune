# Session 083 — Str-keyed HashMap iteration

**Date:** 2026-05-24
**Outcome:** `.keys()` and `.entries()` work on
`HashMap<str, V>` — yielding str keys and `(str, V)`
tuples respectively. Session 069's key_kind dispatch in
the runtime was already in place; only the Rune-side
plumbing needed mirroring. 386 codegen tests green (+6
from session 082).

```rune
let m: HashMap<str, i64> = hashmap_str_new();
m.insert("ab", 10);
m.insert("cde", 20);

for k in m.keys() {                       // k: str
    total = total + k.len();
}

for kv in m.entries() {                   // kv: (str, i64)
    let (k, v) = kv;
    acc = acc + k.len() * v;
}

m.entries().count()                       // 2
```

## The decisive observation

Three small wires; everything else was already in
place.

### 1. `hashmap_key_at` is now polymorphic on K

```rust
"hashmap_key_at" => {
    match &arg_tys[0] {
        Ty::HashMap(k, _) => (**k).clone(),  // session 083
        ...
    }
}
```

The runtime function `rune_hashmap_key_at` returns the
raw 8-byte slot — `int64_t`. For i64 keys that's the
key directly; for str keys it's a pointer to a
rune_str descriptor cast to i64. Only the Rune-side
type changes; cranelift sees I64 both ways. Same
trick `hashmap_val_at` already used for V.

### 2. New iterator structs in std.rn

```rune
pub struct HashMapStrKeysIter<V> {
    map: HashMap<str, V>,
    cursor: i64,
}
pub impl<V> Iterator for HashMapStrKeysIter<V> {
    type Item = str;
    fn next(self: HashMapStrKeysIter<V>) -> Option<str> {
        // same body shape as HashMapKeysIter; `hashmap_
        // key_at` now returns `str` because the map's
        // K type drives it.
    }
}
```

Same for `HashMapStrEntriesIter<V>` (Item = `(str, V)`).
Separate structs (not a single `HashMap<K, V>`-poly
iterator) because the `map` field's type is K-typed
and Rune's generics don't yet support a "K = i64 or
str" constraint.

### 3. Builtin sig + lower dispatch on K

`builtin_vec_iter_sig` for `.keys()` / `.entries()`:

```rust
let iter_name = match k.as_ref() {
    Ty::Str => "HashMapStrKeysIter",
    _ => "HashMapKeysIter",
};
```

`lower_hashmap_keys` / `lower_hashmap_entries` match
the same dispatch — pass the receiver's K through to
the right `find_struct_sym` lookup and build the
struct literal.

The lower path previously hardcoded `HashMap<i64, V>`
as the field's type; now it passes the actual K so
the field's type matches the chosen iterator struct
(`HashMap<str, V>` for the str variants).

## The wire-ups

```
src/checker.rs    (hashmap_key_at builtin now polymorphic
                   on K; builtin_vec_iter_sig dispatches
                   .keys() / .entries() on K = Ty::Str.)

src/lower.rs      (lower_hashmap_keys + lower_hashmap_
                   entries pass K through; pick struct
                   sym from Ty::Str vs other.)

src/std.rn        (HashMapStrKeysIter<V>, HashMapStr-
                   EntriesIter<V> + their Iterator
                   impls.)

tests/codegen.rs  (+6 tests: keys iter, entries iter,
                   destructure-in-for, after-remove
                   skips tombstones, str-keys + Vec-
                   values ARC stress, .count() default
                   method on str-keyed entries.)
```

## What's tested

Codegen (+6):

- `hashmap_str_keys_iteration` — `m.keys()` yields
  str keys; sum their lengths.
- `hashmap_str_entries_iteration` — `m.entries()`
  yields `(str, i64)` tuples; combine key length +
  value.
- `hashmap_str_entries_destructure_in_for` — `for kv
  in m.entries() { let (k, v) = kv; ... }` — session
  074's tuple-destructure inside an iterator yielding
  str-keyed tuples.
- `hashmap_str_keys_after_remove_skips_tombstones` —
  insert 3, remove 1; iterator yields 2 (skipping the
  tombstone slot via is_live_at == 1, not != 0).
- `hashmap_str_keys_iter_with_vec_values` — mixed
  ARC keys + ARC vals stress test (30 iterations
  building/dropping); confirms no leak or double-free
  through the runtime's str-key release walk + the
  synth per-V (Vec) release walk.
- `hashmap_str_entries_via_count_default_method` —
  session 076's `.count()` default fires on the
  str-keyed entries iter via session 071's default-
  method inheritance.

## Apparent bugs that aren't / explicitly deferred

- **For-loop patterns still don't take tuples**. `for
  (k, v) in m.entries()` errors with "for-loop pattern
  must be an identifier or `_`". Workaround: `for kv
  in m.entries() { let (k, v) = kv; ... }` — same as
  i64-keyed entries (session 075's deferred item is
  still deferred).
- **Or-patterns inside tuple patterns rejected** —
  same as session 082; an entry destructure can't be
  `(1 | 2, x)`.
- **`hashmap_val_at` already polymorphic on V**, so
  combining ARC-managed K (str) with ARC-managed V
  (Vec, str, struct) works end-to-end. The release
  paths (per-V synth + runtime str-key release in the
  hashmap descriptor's release fn) cover the cross-
  product.
- **`HashMap<i64, V>` and `HashMap<str, V>` use the
  same runtime descriptor**. The four iterator structs
  (i64-keys / i64-entries / str-keys / str-entries)
  are pure Rune-side type-level distinctions; codegen
  emits the same loads + branches for all of them.
  Future K types (struct keys with `Hash`+`Eq`) would
  need both a runtime change (key_kind tag widens
  beyond 2 values + per-key dispatch table) and new
  iterator structs.
- **Char keys aren't a thing** — Rune's Ty::Char is
  i32-shaped, and HashMap's key array is i64. Could
  be added without runtime change if there's demand.

## What's next

- **Numeric trait bounds** — generalizes `.sum() /
  .min() / .max() / .fold(init, +)` beyond i64.
- **Method-call-position `Into` inference**.
- **Cartesian-product exhaustiveness for tuple
  patterns** (session 082's deferred item).
- **For-loop tuple patterns** — `for (k, v) in m
  .entries()`. Would need to thread tuple-destructure
  into lower_for's pattern handling; bit involved
  because for-pat binds a single sym to the iter's
  Item.
- **Self-hosted bootstrap** — long-term.
