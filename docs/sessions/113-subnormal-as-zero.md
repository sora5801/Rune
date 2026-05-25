# Session 113 — Subnormal-as-zero diagnostic

**Date:** 2026-05-25
**Outcome:** Float literals whose magnitude rounds to
exactly `0.0f32` now error at typecheck with
"literal `1e-50` underflows to zero in `f32`".
`1e-50f32`, `let x: f32 = 1e-50;`, and `let x: f32
= -1e-50;` all surface diagnostics. Subnormal but
nonzero results (`1e-40f32` rounds to a positive f32
subnormal) remain accepted per session 108's policy.
436 codegen + 215 typecheck tests green (+6
typecheck from session 112).

```rune
fn main() -> i64 {
    let x: f32 = 1.0e-50f32;   // ← error: literal `1e-50` underflows
                               //   to zero in `f32`
    x as i64
}
```

## The decisive observation

Sessions 108 and 111 explicitly deferred this case
with "would need warning infrastructure (currently
checker only has errors)." The shift in this session
is policy, not architecture: we make it an *error*
on the grounds that writing `1e-50f32` literally
expresses intent that the float type can't possibly
satisfy. If the user wanted exactly zero, they'd
write `0.0f32`. If they wanted a tiny subnormal,
`1e-40f32` works (and the check explicitly allows
it). The remaining case — a nonzero source that
rounds to *exactly* zero — has no plausible reading
other than "I miscalibrated my magnitude" or "I
should be using f64."

```rust
// Inside check_float_value_in_range:
if matches!(ty, F32) && v != 0.0 && (v as f32) == 0.0 {
    error "literal `{v}` underflows to zero in `f32`";
}
```

`v as f32` performs the IEEE round-to-nearest cast
that the codegen would also perform at runtime. If
the result is exactly `0.0f32` while the source is
nonzero, we've established the underflow. The check
runs *after* the existing range check (so over-range
errors win) and gates on `v != 0.0` (so `0.0f32`
itself passes — a literal zero is intentional).

### Why f64 is exempt

The lexer's `parse::<f64>()` IS the canonical
representation — the source value either parses to
a finite f64 (representable, possibly subnormal but
nonzero) or to `f64::INFINITY` (caught by the
existing range check). There's no "f64 underflow"
between those cases: any decimal literal small
enough to round to f64-zero parses to f64-zero
directly via the lexer, with no source-level way to
distinguish.

f32, by contrast, goes through a two-step cast at
the type system: source → f64 (by the lexer) → f32
(at codegen). The intermediate f64 can be nonzero
while the final f32 is zero. That gap is what
session 113 closes.

### Subnormals still pass

`1e-40f32` lexes to f64 ≈ 1e-40, then `1e-40 as f32`
= a positive subnormal value (not zero). Subnormals
have reduced precision but they're *representable*.
The check's gate `v as f32 == 0.0` only fires when
the cast yields exactly zero — subnormals fail the
gate and pass through cleanly. Session 108's policy
holds: "loud about overflow, silent about subnormal-
precision."

### Diagnostic phrasing

"`underflows to zero in f32`" rather than
"`is out of range for f32`" — the user's value
isn't outside f32's range *per se* (it's between
zero and infinity), but it's too small to be
distinguished from zero. Explicit "underflows to
zero" tells them the exact failure mode without
suggesting they need to widen to a larger float
(they need a smaller-magnitude alternative, or
a wider type like f64).

## The wire-ups

```
src/checker.rs    (check_float_value_in_range gains
                   one new block after the existing
                   range check:
                   - Adds `return` to the range-fail
                     branch so we don't emit both
                     diagnostics for one literal.
                   - Adds the underflow check
                     gated on Ty::F32 && nonzero
                     source && zero-after-cast.)

tests/typecheck.rs  (+6 new tests: suffix underflow,
                     hinted underflow, negated
                     underflow, subnormal-still-OK,
                     literal-zero-OK, f64-exempt.)
```

No lower / codegen / runtime changes. Pure checker
diagnostic.

## What's tested

Typecheck (+6 from session 112's 209):

- `f32_literal_underflow_to_zero_rejected` —
  `1e-50f32` errors.
- `f32_literal_underflow_hinted_rejected` — `let x:
  f32 = 1e-50;` errors via the hint-flow path.
- `f32_literal_negative_underflow_rejected` — `-1e-50`
  hinted to f32 errors with the `-` rendered.
- `f32_subnormal_still_accepted` — `1e-40f32`
  (subnormal) compiles. Confirms the gate's `v as
  f32 == 0.0` is precise.
- `f32_literal_zero_accepted` — `0.0f32` literal
  zero compiles (gate's `v != 0.0` lets it through).
- `f64_underflow_not_checked` — `let x: f64 =
  1e-300;` compiles. The check is f32-specific.

## Apparent bugs that aren't / explicitly deferred

- **Runtime-computed underflow.** A multiplication
  whose result rounds to f32-zero (`1e-25f32 *
  1e-25f32` rolls to subnormal-then-zero) gets the
  IEEE result at runtime. The const-eval check
  from session 111 *would* catch this if we
  extended it — but a computed underflow is the
  natural IEEE behavior of small operands and
  often intentional. We diagnose only what the
  *user wrote literally*, where intent is clear.
- **Literal `0.0f32`.** Explicit zero passes; the
  gate is `v != 0.0`. No "did you mean a tiny
  nonzero value?" prompt.
- **f64 silent underflow.** `1e-400` lexes to
  `0.0f64` (Rust's parse_f64 rounds it to zero).
  We don't catch this case — the lexer drops the
  precision before we see it. Detection would
  require pre-parsing the source string for "is
  this nonzero magnitude?" — out of scope for
  v0.x.
- **Compound subnormal-as-zero.** `1e-20f32 *
  1e-20f32` produces 0.0f32 at runtime. Session
  111's const_eval_float would compute this, but
  the post-binop check doesn't currently run the
  underflow gate. Same intent question as runtime-
  computed; left silent.

## What's next

- **Shift compound operators (`<<=` `>>=`)** —
  parser extension. The session-110 / 112 gates
  are mechanical once the operators land.
- **Bit-op compound operators (`&= |= ^=`)** —
  parser extension. Same shape as shift compounds.
- **f64 source-string-aware underflow** — pre-parse
  the source lexeme to distinguish "user wrote
  1e-400" from "user wrote 0.0". Niche.
- **Self-hosted bootstrap** — long-term.
