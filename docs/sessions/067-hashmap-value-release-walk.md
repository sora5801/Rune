# Session 067 — HashMap value-release walk

**Date:** 2026-05-24
**Outcome:** Closes the V-leak from session 064. A
`HashMap<i64, Vec<i64>>` (or any other ARC-managed V type) now
walks its occupied slots on release, dropping each value
before freeing the descriptor. Per-V release functions are
synthesized at codegen time — same shape as Vec's per-elem
release. 574 tests green (+2 from session 066).

```rune
fn build() -> i64 {
    let m: std::HashMap<i64, Vec<i64>> = hashmap_new();
    let v: Vec<i64> = vec_new();
    v.push(10); v.push(20);
    m.insert(1, v);
    m.get(1).get(0)
}
// At m's scope exit: synth __rune_release_hashmap$Vec_i64
// walks occupied[i], releases vals[i] (a Vec<i64>), then
// frees the descriptor. No leak.
```

## The decisive observation

Codegen had already solved this exact problem for `Vec<T>`
with ARC elements: collect distinct element types during
monomorphization, declare a per-elem release function in
Pass 0, define it in Pass 3, and route through it from
`emit_release_field` / `emit_arc_call` when releasing a Vec
value. HashMap V is structurally identical — distinct V types
get a synthesized walker keyed on `Ty`, the walker loops
`i in 0..cap` checking `occupied[i] != 0`, releases `vals[i]`
through `emit_release_field` (which recurses for nested
ARC types), then hands off to the runtime's `release_hashmap`
for the standard rc-- + arrays-free + weak-release.

The runtime's `struct rune_hashmap` layout in offsets: keys@0,
vals@8, occupied@16, len@24, cap@32, rc@40, weak_count@48 —
all i64 except `occupied` which points at a parallel
`int8_t[cap]` array.

## The wire-ups

```
src/hir.rs            (HirModule gains hashmap_arc_val_tys: Vec<Ty>
                       parallel to vec_arc_elem_tys. Lower
                       initializes empty.)

src/monomorphize.rs   (New scan_ty_for_hashmap_vals + walks every
                       fn signature/body/struct-field/enum-payload
                       to populate hashmap_arc_val_tys after
                       monomorphization. is_arc_mono adds Ty::HashMap.
                       scan_ty_for_vec_elems and scan_ty_for_arrays
                       both recurse into HashMap's V so a
                       Vec<HashMap<i64, Vec<S>>> picks up the inner
                       Vec<S> the same way.)

src/codegen.rs        (Codegen<M> gains hashmap_release_funcs:
                       HashMap<Ty, FuncId>. Pass 0 declares
                       __rune_release_hashmap$<V> per distinct V;
                       Pass 3 defines via the new
                       define_hashmap_release. FnCodegen gains a
                       reference to the map. emit_release_field and
                       emit_arc_call's release path both check
                       Ty::HashMap with an ARC V and route through
                       the synth fn before the runtime helper. The
                       synth body: null-guard, rc==1 fast path,
                       loop i in 0..cap, if occupied[i] != 0
                       release vals[i] via emit_release_field (so
                       nested ARC types recurse), then call
                       release_hashmap to finish.)
```

## What's tested

Codegen (+2):

- `hashmap_value_is_vec_releases_at_scope_exit` — builds a
  `HashMap<i64, Vec<i64>>` 100 times in a tight loop. If the
  inner Vecs leaked, RSS would balloon under a tracker; here
  we just pin "constructs + releases without crashing" and
  the cumulative value (210 per iter × 100 = 21000) is right.
- `hashmap_value_is_str_releases_via_walk` — same shape with
  Str values. String literals carry rc=-1 so the per-slot
  release is a no-op at the helper level, but the
  release-walk codepath still runs through them.

## Apparent bugs that aren't / explicitly deferred

- **Same i64::MAX-iteration-cost concern as Vec.** If a
  HashMap has cap = 2^k for k > 30, the release walk runs
  cap iterations. In practice cap stays in the thousands for
  realistic workloads; the per-iteration cost is one load,
  one compare-with-zero, and a release call (skipped for
  empty slots).
- **Per-V release is keyed on `Ty`**, like Vec's per-elem.
  Two structurally-identical `Ty::Struct(s, [])`s collapse
  to the same fn — correct, but if Rune ever gets phantom
  generics it'd need richer keying.
- **No remove() to test against.** When session 067+ adds
  remove, the release walk will need to consult tombstones
  (occupied == 2 in a future extension). Today's "occupied
  != 0" check skips empty slots only; tombstones would
  silently get released, which is wrong. Document for the
  remove-session author.
- **The Variable::new(0) hack.** The synthesized release
  function reuses the same Variable index across the loop
  (mirrors Vec's). Cranelift's SSA-construction handles the
  loop-back phi — battle-tested by the array/Vec release
  paths. Worth re-checking if Cranelift's API ever changes.

## What's next

- **HashMap str-keys** — separate hash + per-bucket equality.
- **HashMap remove + iteration** — tombstones + HashMapIter.
- **`?` on Option** — currently Result-only.
- **Trait default-method bodies** — `.collect()` as chained.
- **Self-hosted bootstrap** — long-term.
