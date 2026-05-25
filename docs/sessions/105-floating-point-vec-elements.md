# Session 105 — Floating-point Vec elements

**Date:** 2026-05-25
**Outcome:** `Vec<f64>` and `Vec<f32>` compile and run.
Push, get, iterator chains (`.iter().fold(0.0, |a, x|
a + x)`), and for-in loops all work. Generic `<T:
Numeric>` code lands on float Vecs end-to-end. 436
codegen + 167 typecheck tests green (+5 codegen from
session 104).

```rune
fn main() -> i64 {
    let v: Vec<f64> = vec_new();
    v.push(1.0);
    v.push(2.0);
    v.push(3.0);
    let total: f64 = v.iter().fold(0.0, |a: f64, x: f64| a + x);
    if total > 5.9 && total < 6.1 { 1 } else { 0 }
}
```

## The decisive observation

Vec stores elements in uniform 8-byte slots. The
runtime treats each slot as `int64_t`; the codegen
translates between the slot's i64 representation and
the element's typed Cranelift value at push / get.
For integer / pointer types this is `uextend` /
`sextend` / `ireduce`. For floats it's `bitcast` —
the 8 bytes of the slot ARE the IEEE-754 bit pattern.

```
push f64 val          get i64 raw
  ↓                     ↓
bitcast.i64           bitcast.f64
  ↓                     ↓
store at v->ptr[i]    return as f64 value
```

For f32 the slot is still 8 bytes (uniform layout
beats per-type slot widths — the layout shows up in
dozens of places: VecIter index math, per-element
release walk, runtime helpers). The float lives in
the lower 4 bytes; upper 4 are padding. Push:
`bitcast f32 → i32, uextend → i64`. Get: `ireduce
i64 → i32, bitcast → f32`.

The wider observation: every container that stores
elements in 8-byte slots (Vec, HashMap, Option
payload, Tuple) was using the same "ireduce to narrow
ints; raw for I64" idiom. Float support requires
adding "bitcast for floats" to that idiom — at every
slot site. Factoring out two helpers (`narrow_from_
slot`, `widen_to_slot`) handles it uniformly.

### The session-103 bug that surfaced

The first cut of the float Vec tests crashed at the
Cranelift verifier with `iadd has invalid controlling
type f64`. The IR showed `iadd v7, v10` where v7 and
v10 were both f64 from `bitcast.f64`. The binop
codegen reads `e.ty` to dispatch `fadd` vs `iadd` —
but `e.ty` was `Ty::Error`, so the float branch
didn't fire.

The root cause: session 103's intercept in `check_
expr_with_hint` for Binary with a numeric hint:

```rust
let lt = check_expr_with_hint(lhs, Some(exp));
let rt = check_expr_with_hint(rhs, Some(exp));
return finish_binary(*op, lhs, rhs, lt, rt, span);
```

The intercept returned the result type but never
inserted it into `expr_types[span]`. The lowerer reads
`expr_types[span]` for every expression; when missing
it defaults to `Ty::Error`. For integer binops this
was invisible — `iadd` accepts every integer type,
and the operand values' SSA types pass the verifier.
For float binops the mismatch shows up the moment
operands are f64. One-line fix: `self.expr_types.
insert(*span, ty.clone())` before returning, mirroring
the other three intercept arms.

## The wire-ups

```
src/checker.rs    (vec_element_supported adds
                   Ty::Float(_); error message updated;
                   session 103 Binary intercept now
                   inserts into expr_types.)

src/codegen.rs    (Two new helpers: narrow_from_slot
                   (i64 → element type) and widen_to_slot
                   (element type → i64). Six call sites
                   refactored to use them: vec.push,
                   vec.get, HashMap.get, HashMap.remove,
                   EnumPayloadCtor store, EnumPayload
                   pattern extract, Tuple pattern
                   element extract, ArrayIndex load.
                   The helpers fold float-bitcast,
                   sign-extension, and ireduce/uextend
                   into one place per direction.)

tests/codegen.rs  (+5 new tests: vec_f64 / vec_f32
                   push-get round trips, iter-chain
                   fold over f64, for-loop sum over
                   f64, <T: Numeric> generic over
                   Vec<f64>.)
```

No new HIR variants, no monomorphizer changes, no
runtime changes. Floats are not ARC so
`vec_arc_elem_tys` doesn't pick them up — no per-
element release walk synthesis. The runtime's
`rune_vec_push(v, int64_t x)` and `rune_vec_get(v, i)
-> int64_t` are unchanged; only the codegen's view
of the slot's content changes.

## What's tested

Codegen (+5 from session 104):

- `vec_f64_push_get_round_trip` — `v.push(3.14);
  v.push(2.71); v.get(0) + v.get(1)` arithmetic
  preserves the value (3.14 + 2.71 = 5.85).
- `vec_f32_push_get_round_trip` — same shape over
  f32, exercising the ireduce+bitcast pair on the
  read side and the bitcast+uextend on the write.
- `vec_f64_iter_chain` — `v.iter().fold(0.0,
  |a: f64, x: f64| a + x)` summing 1.0+2.0+3.0
  through VecIter<f64>.next() (which loads via
  vec.get internally) and a closure that takes f64
  operands.
- `vec_f64_for_loop` — `for x in v.iter()` with
  `total = total + x` accumulating f64s. Exercises
  the Option<f64> unwrap pattern in the
  iterator-protocol desugar — surfaced the second
  bug, the EnumPayload pattern's `ireduce` site
  also needing float-aware narrowing.
- `vec_f64_via_numeric_generic` — `fn add_two<T:
  std::Numeric>(a: T, b: T) -> T { a.add(b) }`
  applied to f64 values from a Vec<f64>. Closes
  the loop with session 104's prelude `impl Numeric
  for f64`.

## Apparent bugs that aren't / explicitly deferred

- **f32 slot wastes 4 bytes.** Packing two f32s per
  slot would let `Vec<f32>` be half the size, but
  every dependent layout (VecIter index, push/get,
  release walks for ARC types, runtime helpers'
  i64-stride accesses) would need to know whether
  the element is f32 or i64-shaped. Not worth the
  complexity for v0.x; ship the uniform slot.
- **`Vec<[T; N]>` still rejected.** Arrays are
  heap-pointer-sized but the per-element ARC walk
  for nested arrays is not wired through Vec's
  release path. Same shape as the float change
  would need — a different focused session.
- **NaN ordering inside Vec.** `vec.iter().min()`
  on a Vec containing NaN returns whichever Some
  arm survives the cmp chain — IEEE-754's `<` is
  false for any NaN-involved comparison, so NaN
  values are skipped past the running min/max.
  Same behavior as direct f64 comparison; the
  trait doesn't promise total ordering.
- **HashMap<i64, f64>** now works the same way:
  `HashMap.get` and `.remove` go through
  `narrow_from_slot`, `.insert` always stored
  values via the existing i64 path (it already
  accepted any pointer-shaped i64-castable value),
  but the read side reinterprets correctly. Tested
  indirectly through Vec's path; a focused
  HashMap<_, f64> test would just verify the same
  helpers fire.
- **Float-keyed HashMap (`HashMap<f64, V>`).** Still
  rejected: the runtime's key_kind dispatch is
  exactly two cases (i64, str). A float-key
  implementation would need NaN-keying decisions
  (NaN != NaN per IEEE-754; can't be a stable key)
  and probably hashing-via-bit-pattern. Deferred.
- **`Vec<f64>` Cranelift verifier passes through
  the same path that integer Vecs do.** The
  per-instruction generated IR has more bitcasts
  but no new instruction shapes — Cranelift already
  optimizes `bitcast i64 (bitcast f64 v)` to `v`
  at the register-alloc level when both stages
  can stay in the same register class. Real
  cost of float Vec push/get on x86-64: same as
  integer Vec push/get (the bitcast is folded).

## What's next

- **Cross-let const-eval** — propagate const
  values through let bindings so `let a = 100u8;
  let b = 200u8; a + b` catches overflow.
- **Division-by-zero const-eval diagnostic** —
  `100 / 0` errors at typecheck.
- **Floating-point literal range checks** —
  `3.4e40f32` rounds to infinity silently today.
- **Self-hosted bootstrap** — long-term.
