# Session 108 — Floating-point literal range checks

**Date:** 2026-05-25
**Outcome:** Float literals that don't fit their
declared type now error at typecheck. `3.4e40f32`,
`let x: f32 = 3.4e40;`, and `let x: f64 = 1e400;`
all surface with "literal `...` is out of range for
`f32`/`f64`" — previously silently rounded to
infinity. Mirrors session 092/099's integer literal
range checks. 436 codegen + 184 typecheck tests
green (+7 typecheck from session 107).

```rune
fn main() -> i64 {
    let x: f32 = 3.4e40;   // ← error: literal `3.4e40` is out of range for `f32`
    x as i64
}
```

## The decisive observation

Session 092 added `check_int_value_in_range` (range-
check int magnitude against an `IntTy`) and session 099
extended the call to hint paths. Floats lacked a
counterpart: the lexer parses `1e400` via Rust's
`parse::<f64>()` which returns `Ok(f64::INFINITY)` on
overflow rather than rejecting — so we'd get a
`Lit::Float(f64::INFINITY, ...)` in the AST and
silently emit IR with that. The fix is structural:

```rust
fn check_float_value_in_range(&mut self, v: f64, ty: FloatTy, ...) {
    let in_range = match ty {
        F32 => v.is_finite() && v.abs() <= f32::MAX as f64,
        F64 => v.is_finite(),
    };
    if !in_range { error "literal ... out of range for ..."; }
}
```

The check fires at three sites that mirror the
integer ones:

1. **Suffixed literal** (`3.4e40f32`) — in
   `check_numeric_lit_in_range`, the same place
   suffixed integer literals are checked.
2. **Hinted bare literal** (`let x: f32 = 3.4e40`) —
   in `check_expr_with_hint`'s Lit-with-hint arm,
   right after `numeric_lit_hint` pins the type.
3. **Negated hinted literal** (`let x: f32 = -3.4e40`)
   — in the parallel Unary-Neg-on-Lit arm.

### f32 cutoff

f32::MAX ≈ 3.4e38. Anything up to that (positive or
negative) round-trips losslessly through `as f32`.
Anything strictly above rounds to `f32::INFINITY` —
which is what the check catches. Subnormal-range
values (`1e-40f32` rounds to a subnormal or zero)
are *not* errors: the value is representable in f32
even if precision is lost. The trade-off is "loud
about overflow, silent about underflow" — overflow
is almost always a typo; underflow is sometimes
intentional.

### f64 cutoff

f64::MAX ≈ 1.8e308. Any literal whose source-form
parses to `f64::INFINITY` (via the lexer's
`parse::<f64>()`) fails the `is_finite()` check.
The diagnostic renders the offending value as
`inf` rather than the user's source text — we don't
have the original lexeme at this point, just the
parsed f64.

### Why `negated` is plumbed but mostly cosmetic

IEEE-754 is sign-magnitude: the sign bit is part of
the bit pattern, not a separate flag. So
`check_float_value_in_range(v, F32, negated=true,
...)` doesn't change the *check* — `v.abs()` covers
both signs uniformly. The `negated` flag only affects
the diagnostic's leading `-`. Plumbed for parity with
the int checker (which DOES use it to allow `-128i8`
even though `128i8` is out of range — magnitude vs.
signed). Floats don't have that asymmetry.

## The wire-ups

```
src/checker.rs    (+50 lines:
                    - check_float_value_in_range (new)
                    - check_numeric_lit_in_range now
                      handles Lit::Float(_, Some(ty))
                    - hint-flow Lit arm calls it for
                      Lit::Float(_, None) hinted to F32/F64
                    - Unary-Neg-on-Lit arm same)

tests/typecheck.rs  (+7 new tests: suffix overflow,
                     hinted overflow, negative hinted
                     overflow, f64-overflow-to-infinity,
                     f32-near-max accepted, f32-
                     subnormal accepted, f64-normal
                     accepted.)
```

No lexer / parser / lower / codegen / runtime
changes. Checker-only diagnostic, same shape as
session 092/099 for ints.

## What's tested

Typecheck (+7 from session 107's 177):

- `f32_literal_overflow_suffix_rejected` — `3.4e40f32`
  errors.
- `f32_literal_overflow_hinted_rejected` —
  `let x: f32 = 3.4e40;` errors via hint-flow path.
- `f32_literal_negative_overflow_rejected` —
  `let x: f32 = -3.4e40;` errors via Unary-Neg-on-Lit
  hint-flow path.
- `f64_literal_overflow_rejected` — `let x: f64 =
  1e400;` (lexer rounds to f64::INFINITY) errors.
- `f32_near_max_accepted` — `3.0e38f32` (close to but
  under f32::MAX) compiles.
- `f32_subnormal_accepted` — `1.0e-40f32` (subnormal)
  compiles. The check is for round-to-infinity, not
  round-to-zero.
- `f64_normal_accepted` — `let x: f64 = 1.0e100;`
  (well within f64) compiles.

## Apparent bugs that aren't / explicitly deferred

- **Subnormal-as-zero is silent.** `1.0e-50f32`
  rounds to f32 subnormal (or zero for tiny enough
  values) without diagnostic. The user explicitly
  wrote a tiny number; representing it as 0.0 is
  the IEEE-defined behavior. A warning would be
  reasonable but v0.x doesn't have a warning
  infrastructure — only errors.
- **NaN literal.** Rune source has no `NaN` syntax;
  the only way to get NaN is runtime arithmetic
  (`0.0 / 0.0`). The check's NaN branch is
  defensive — never expected to fire from source.
- **Const-eval through binops for floats.** Session
  106's `const_eval_int` is integer-only; there's
  no `const_eval_float`. `let big: f32 = 1e30 + 1e30;`
  doesn't catch the compound overflow (each operand
  fits f32; their sum at IEEE-754 would overflow if
  it actually executed as f32, but the const-eval
  pipeline doesn't see it). Same shape as session
  106's int extension, just for floats. Future
  session.
- **f64 fully-finite range only.** `f64::MAX_POSITIVE`
  (~1.8e308) is the only boundary; we don't reject
  any subnormal-or-zero f64 (since f64 can represent
  them exactly).
- **`as`-cast-truncation isn't a check.** `let x =
  1e300; let y = x as f32;` produces `y = f32::INFINITY`
  silently. The `as`-cast is an explicit user
  request, so we don't second-guess it at the cast
  site — overflow-via-cast falls under v0.x's
  general "user asked for truncation" policy.

## What's next

- **`as`-cast through const-tracked bindings** (still
  carried over from sessions 106/107) — add
  `Expr::Cast` arm to `const_eval_int`.
- **Shift-out-of-range diagnostic** — `x << 64` at
  typecheck.
- **Float const-eval through binops** — mirror of
  session 102/106 for floats.
- **Self-hosted bootstrap** — long-term.
