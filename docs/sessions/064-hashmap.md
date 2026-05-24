# Session 064 — HashMap&lt;K, V&gt;

**Date:** 2026-05-24
**Outcome:** Runtime-backed open-addressing HashMap lands in the
prelude. Keys are i64; values are any 8-byte-fitting type
(integers, bool/char, str, Vec, struct, enum, dyn). Operations:
`hashmap_new()`, `m.insert(k, v)`, `m.get(k)`, `m.contains_key(k)`,
`m.len()`. ARC-managed.

```rune
let m: std::HashMap<i64, str> = hashmap_new();
m.insert(1, "one");
m.insert(2, "two");
m.get(2).len()                  // 3
```

~5 files. 306 codegen tests green (+6 from session 063).

## The decisive observation

HashMap mirrors Vec end-to-end: a heap-allocated descriptor
managed by C runtime helpers, a new `Ty::HashMap(K, V)` variant
parallel to `Ty::Vec(T)`, a `BuiltinType` marker in the
resolver, method calls intercepted in codegen's `compile_method_call`,
and ARC wiring through `is_arc_type` + `arc_helper_name`. Adding
a new builtin parametric type is now a 5-file pattern.

The runtime uses Murmur3-style multiplicative-mix for i64 hashing,
linear probing for collision resolution, and a 75% load-factor
growth threshold (cap doubles when `(len + 1) * 4 > cap * 3`). No
deletion in v0.x (no tombstones). Initial cap is 8.

## The wire-ups

```
runtime.c            (struct rune_hashmap { keys, vals, occupied,
                      len, cap, rc, weak_count } + new/insert/
                      get/contains_key/len/retain/release/
                      weak_release. rune_hashmap_hash_i64 is the
                      finalizer-style mix; rune_hashmap_probe is
                      the linear-probe lookup;
                      rune_hashmap_grow is the rehash-on-double.)

src/ty.rs            (Ty::HashMap(Box<Ty>, Box<Ty>) added.
                      Display, compatible, unify, mangle all
                      handle it parallel to Ty::Vec.)

src/resolver.rs      (Intern `HashMap` and `std::HashMap` as
                      BuiltinType(Ty::HashMap(Ty::Error, Ty::Error))
                      sentinels — the checker's path-resolution
                      rebuilds the type from the path's generic
                      args. `hashmap_new` and `std::hashmap_new`
                      are PolyBuiltinFn("hashmap_new"); the
                      checker fills in the K=i64 and a fresh V
                      TypeVar to be pinned by the surrounding
                      annotation.)

src/checker.rs       (resolve_type for Path → builds
                      Ty::HashMap(K, V) when sym_kind is
                      BuiltinType(Ty::HashMap(_, _)). Enforces
                      `K = i64` and `V is hashmap_value_supported`
                      (wider than vec_element_supported: also
                      allows Str). check_poly_builtin_call's
                      "hashmap_new" arm returns Ty::HashMap with
                      a fresh inference TypeVar for V.
                      resolve_method matches Ty::HashMap for the
                      four method names with their signatures.)

src/lower.rs         (lower_poly_call dispatches "hashmap_new"
                      to a BuiltinCall("hashmap_new", []). Method
                      calls on Ty::HashMap stay as HirExprKind::
                      MethodCall — codegen intercepts the kind
                      in compile_method_call.)

src/codegen.rs       (cranelift_type for Ty::HashMap → I64.
                      mangle_ty_name → "HM_<k>_<v>". is_arc_type
                      → true. arc_helper_name → retain_hashmap /
                      release_hashmap. declare_builtin handles
                      the seven new runtime fns (hashmap_new/
                      insert/get/contains_key/len + retain/
                      release). compile_method_call has a new
                      arm for Ty::HashMap that mirrors the Vec
                      arm: insert retains ARC values on borrowed
                      args, get retains ARC values on the
                      returned slot. The JIT entrypoint binds the
                      seven C-runtime symbols.)
```

## What's tested

Codegen (+6):

- `hashmap_basic_insert_get` — insert three k→v, get them back.
- `hashmap_overwrite_returns_latest` — repeated insert on the
  same key replaces the value; len stays 1.
- `hashmap_contains_and_missing_returns_zero` — contains_key
  for present/missing keys, get's "missing returns 0" behavior.
- `hashmap_grows_past_initial_cap` — insert 30 entries forces
  multiple grow + rehash cycles; all reads still resolve.
- `hashmap_value_is_str` — str values exercise the ARC-on-insert
  path. The string-literal sentinel rc=-1 makes them safe even
  though v0.x's map release doesn't walk values.
- `hashmap_count_distinct_via_insert` — distinct-counting via
  `insert(x, 1)` over an array.

## Apparent bugs that aren't / explicitly deferred

- **Keys are i64-only.** Str keys (the next most useful) need a
  separate hash function and a per-bucket equality check that
  compares string contents rather than i64. Adding involves a
  type-discrimination layer in the runtime, OR a separate
  `rune_strmap_*` family. Punted.
- **get on a missing key returns 0**, matching `Vec.get`'s
  out-of-range behavior. The user can't distinguish `m.get(k)
  == 0` from "key exists with value 0". The fix is to return
  `Option<V>` instead, which needs a way to construct enum
  values from C — the runtime would need to know the
  Option<V> layout (descriptor pointer + tag + payload) for
  each V. Workaround today: call `m.contains_key(k)` first.
- **No deletion** (`remove`). Tombstones require a third
  occupied-state value and complicate the probe loop. v0.x
  programs that need delete can rebuild the map without the
  key — wasteful but correct.
- **No iteration.** A `HashMapIter` struct + `Iterator` impl
  parallel to `VecIter` would land naturally; deferred.
- **Values are not walked on release.** When a HashMap goes out
  of scope, the descriptor's keys/vals/occupied arrays are
  freed but the per-slot ARC values aren't. If V is Vec or
  Str, dropping the map LEAKS them — the test cases above use
  i64 values or string LITERALS (rc=-1 sentinel) to avoid
  noticing. Fixing requires a value-type tag on each map
  instance + a runtime walk through the slots calling the
  right release helper.
- **No HashMap-as-iter-source.** `for (k, v) in m { ... }`
  would need an iterator + tuple destructuring; both deferred.

## What's next

- **From-based `?` error conversion** — let the `?` operator
  call `From::from(err)` when the surrounding fn's error type
  differs from the inner result's.
- **HashMap value-release walk** — close the leak above by
  recording the V type per map instance.
- **HashMap remove + iteration** — tombstones + a HashMapIter
  struct.
- **Open-ended ranges** (`..n`, `n..`).
