# Session 070 — HashMap insert-overwrite-releases-old-value

**Date:** 2026-05-24
**Outcome:** Closes the pre-existing V-leak from session 064.
When `m.insert(k, new_v)` overwrites an existing key, the
old value's ARC +1 is now released instead of dropped on the
floor. The runtime returns the previous slot value (0 if
fresh); codegen emits a release call when V is ARC-managed.
588 tests green (+2 from session 069).

```rune
let m: std::HashMap<i64, Vec<i64>> = hashmap_new();
for i in 0..200 {
    let v: Vec<i64> = vec_new();
    v.push(i);
    m.insert(1, v);   // pre-070: leaks each prior Vec
                       // post-070: prior Vec's +1 released
}
// Only the final Vec remains alive in the map.
```

## The decisive observation

The runtime knows when an overwrite happens (probe lands on
`occupied[i] == 1`) but doesn't know V's type to release.
The codegen-side knows V's type but didn't get a signal from
the runtime that an overwrite occurred. Bridge them with a
return value: insert returns the previous slot pointer (or 0
on fresh), and the codegen-side caller emits a per-V release
call on that pointer. The runtime's per-type release
helpers all null-check, so passing 0 in the fresh-slot case
is a safe no-op — no caller branching needed.

The user-facing `.insert` still types as Unit; the i64
return from the runtime fn is captured at the Cranelift IR
level, fed to `emit_arc_call("release", ...)`, then
discarded. Zero language-level change; just a runtime/codegen
plumbing fix.

This pattern — runtime returns the old slot value, codegen
releases — generalizes to any structure where ownership of
contents transfers (insert, swap, take_if_present). It's
also symmetric with how `.remove` already works: remove
returns the slot's value, the caller (codegen) is the new
owner with no retain. Insert-overwrite now mirrors that.

## The wire-ups

```
runtime.c       (rune_hashmap_insert's return type goes
                 void → int64_t. Returns 0 on fresh slot,
                 m->vals[i] (the old value) on overwrite.
                 No behavior change on the str-key side —
                 key ARC handling stays untouched since
                 the slot's key doesn't change on
                 overwrite.)

src/codegen.rs  (Update the extern signature: insert now
                 takes 3 args and returns i64. declare_builtin
                 "hashmap_insert" appends an I64 return.
                 compile_method_call's HashMap arm gets a
                 new branch: when m == "insert" and val_arc,
                 emit_arc_call("release", &val_ty, raw)
                 where raw is the insert call's result.
                 Returns Ok(None) regardless — insert still
                 types as Unit from the Rune side.)
```

## What's tested

Codegen (+2):

- `hashmap_insert_overwrite_releases_old_value` — tight loop
  of 200 overwrites at the same key with Vec<i64> values.
  Pre-070 each iteration's Vec leaked; post-070 the prior
  Vec's +1 is released by the next iteration's insert.
  Confirms the final slot value survives and the program
  exits cleanly.
- `hashmap_str_insert_overwrite_releases_old_str_value` —
  same shape with Str values. Str literals carry rc=-1
  (no-op release at the helper level) but the codepath
  still runs.

## Apparent bugs that aren't / explicitly deferred

- **Non-ARC V types ignore the return.** When V is i64 or
  bool, the runtime still returns the previous slot value,
  but codegen's `val_arc` check is false so no release runs.
  The return value is just discarded at the Cranelift IR
  level. Slight runtime overhead (one unused i64 from C),
  acceptable for v0.x.
- **Returning 0 for the fresh-slot case is ambiguous if V's
  legitimate values include 0.** The codegen-side only
  releases ARC types (struct/Vec/Str/HashMap/Array/dyn);
  none of those have a meaningful 0 representation — the
  release helpers all null-check. For non-ARC V (i64, bool)
  the codegen doesn't release at all, so the ambiguity is
  invisible.
- **Symmetric issue would be remove returning 0 for missing
  keys with V being a primitive whose value could be 0.**
  Already a pre-existing quirk — remove returns 0 if the
  key isn't in the map. For ARC V the caller can null-check;
  for primitive V the caller can't distinguish "not present"
  from "present with value 0." Future session: add a
  separate `contains_key` precheck or change the API to
  `Option<V>` (needs the Option enum to be constructable
  from C, which is a chunkier change).
- **The runtime's `prev = 0` initial value relies on
  uninitialized `m->vals[i]` not being read when the slot
  was empty or tombstoned**. Probe_for_insert returns the
  insertion index; for empty/tombstone slots, m->vals[i] is
  uninitialized memory. The `if (m->occupied[i] != 1)` arm
  doesn't read m->vals[i] — it sets `prev = 0` explicitly.
  ✓
- **No `.entries()` / `.values()` iterators yet.** Still
  blocked on tuples (Rune has none).
- **The runtime change is technically a binary-incompatible
  ABI break for anyone calling rune_hashmap_insert as a
  Rust foreign fn.** Internal use only (codegen) so this is
  fine, but flagged for any future C consumers.

## What's next

- **Trait default-method bodies** — `.collect()` chaining.
- **`?` on Option** — Result-only today.
- **Multi-impl `Into` disambiguation** — `impl_methods`
  keys methods by name only.
- **HashMap .values() / .entries()** — needs tuples.
- **Self-hosted bootstrap** — long-term.
