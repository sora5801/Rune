# Session 069 — HashMap str-keys

**Date:** 2026-05-24
**Outcome:** `HashMap<str, V>` works alongside `HashMap<i64, V>`.
Two distinct `rune_str` descriptors with the same content
hash to the same slot and compare equal (memcmp, not pointer
identity). The runtime owns the str key ARC for str-keyed
maps; the codegen path keeps owning value ARC. 586 tests
green (+6 from session 068).

```rune
let m: std::HashMap<str, i64> = hashmap_str_new();
m.insert("one", 1);
m.insert("two", 2);
let a: str = "hel" + "lo";
let b: str = "he" + "llo";
m.insert(a, 42);
m.get(b)                        // 42 — content equality wins
m.remove("one");                // 1; tombstone; m.len() == 2
for k in m.keys() {             // session 068 — still works
    print(k);
}
```

## The decisive observation

The runtime descriptor gets a single new field — `key_kind`
(0 = i64, 1 = str). Hash and equality branch on it. That's
the whole structural change; everything else
(probe/insert/remove/grow/iterator) routes through
`hash_key` and `keys_equal` helpers that read `key_kind`.
One descriptor type, one set of public functions, one tag
byte. No parallel `rune_strmap_*` family, no per-call
function pointers, no per-K codegen synthesis.

The asymmetry with values: the runtime knows there are only
two possible key types (i64 and str), so it handles key ARC
directly — retain on fresh insert for str, release on
remove or final drop. Values can be any type, so the codegen
synthesizes per-V release walks (session 067). Different
mechanisms because different cardinalities of "kinds the
compiler must dispatch on."

The `hashmap_str_new()` constructor is a separate
PolyBuiltinFn from `hashmap_new()`. The user types the
distinction at call site — there's no inference of "you
annotated the type as `HashMap<str, _>` so I'll route to
the str variant," because the polybuiltin call typechecks
*before* the let annotation flows back. Cleaner this way:
two names, two return types, no implicit routing.

## The wire-ups

```
runtime.c            (struct rune_hashmap gains key_kind:
                      int64_t. rune_hashmap_str_new sets
                      key_kind=1; rune_hashmap_new sets 0.
                      New rune_hashmap_hash_str does FNV-1a
                      over the str bytes. Static helpers
                      rune_hashmap_hash_key + rune_hashmap_keys_equal
                      branch on key_kind. probe /
                      probe_for_insert / grow all use them.
                      Insert retains the str key on fresh
                      slot. Remove releases the slot's str
                      key when tombstoning. release_hashmap
                      walks live slots releasing each str
                      key before freeing arrays — runs after
                      the synth per-V release walk's
                      release_hashmap call.)

src/resolver.rs      (Two new PolyBuiltinFn entries —
                      hashmap_str_new and std::hashmap_str_new
                      — parallel to hashmap_new.)

src/checker.rs       (K=str now joins K=i64 in the allowed
                      key types for Ty::HashMap. check_poly_
                      builtin_call adds "hashmap_str_new"
                      returning Ty::HashMap(Str, TypeVar) —
                      same fresh-V-inference pattern as
                      hashmap_new but with str instead of i64.
                      Resolve_method's HashMap arm is
                      unchanged: insert/get/contains_key/
                      remove already pull K from the
                      receiver's HashMap(K, V) — for str-keyed
                      maps that K is Ty::Str, so method args
                      typecheck as str.)

src/lower.rs         (lower_poly_call dispatches
                      "hashmap_str_new" to "hashmap_str_new"
                      runtime name — no arg-type
                      discrimination needed since both
                      take no args; the call site's name
                      tells the lowerer which constructor
                      to invoke.)

src/codegen.rs       (Extern + JIT symbol-binding for
                      rune_hashmap_str_new. declare_builtin
                      "hashmap_str_new" signature.
                      compile_method_call's HashMap arm
                      already handled K as part of the
                      receiver's Ty — the only thing that
                      changes for str keys is that the
                      I64 slot now holds a rune_str pointer
                      instead of an actual integer, and the
                      runtime branches internally on the
                      descriptor's key_kind. No codegen-side
                      branch needed.)
```

## What's tested

Codegen (+6):

- `hashmap_str_keys_insert_get` — round-trip with three
  string literals.
- `hashmap_str_keys_content_equality_not_pointer` — `"hel"
  + "lo"` and `"he" + "llo"` are distinct heap descriptors
  but content-equal; `get` on the second finds the slot
  stored under the first. Confirms memcmp-based equality.
- `hashmap_str_keys_missing_returns_zero` — contains_key
  for present/missing keys, get's missing → 0.
- `hashmap_str_keys_remove_then_reinsert` — remove returns
  the value, reinsert restores it; len is right at the end.
- `hashmap_str_keys_grow_past_initial_cap` — 12 distinct
  keys force grow + rehash; reads still resolve through
  the rehashed table.
- `hashmap_str_keys_release_with_vec_values` — combine str
  keys with Vec values; both sides get ARC-walked at the
  map's scope exit. Tight loop sanity-checks no leak / no
  double-free.

## Apparent bugs that aren't / explicitly deferred

- **No int-keyed map can be mixed with str-keyed map at
  runtime.** The two constructors return distinct types
  (`HashMap<i64, V>` vs `HashMap<str, V>`) — the type
  checker enforces homogeneity. There's no v0.x route to a
  `dyn HashMap` or similar.
- **`hashmap_new` and `hashmap_str_new` are separate names.**
  Could go either way — one alternative would be to infer
  K from the surrounding annotation. The current design is
  explicit which is easier to read and avoids subtle
  surprises when the inference fails.
- **The string-key ARC is owned by the runtime.** This
  asymmetry with value ARC (owned by codegen-synth release
  walks) reflects the type-knowledge gap: the runtime
  knows there are exactly two key types, but doesn't know V.
  When session 070+ wants a third key type (e.g. struct
  keys with `Hash` + `Eq` impls), this will need redoing —
  probably toward function-pointer hash/eq stored on the
  descriptor.
- **Overwriting an existing key's value doesn't release
  the old value.** Pre-existing leak from session 064, not
  specific to str keys. Inserting `m.insert(k, new_v)`
  when `k` is already live drops the old value's ARC on the
  floor. Documented for the future-session fix.
- **No `.values()` or `.entries()` iterator** — session
  068 already deferred these (Rune has no tuples).
- **FNV-1a hash quality.** Standard choice; good
  distribution for short ASCII strings. Adversarial inputs
  could trigger collisions but the hashmap is in-process,
  not exposed to network input, so DoS isn't a concern.

## What's next

- **Trait default-method bodies** — `.collect()` chaining.
- **`?` on Option** — Result-only today.
- **Multi-impl `Into` disambiguation** — `impl_methods`
  keys methods by name only.
- **HashMap overwrite-releases-old-value** — close the
  pre-existing leak.
- **HashMap .values() / .entries()** — needs tuples.
- **Self-hosted bootstrap** — long-term.
