# Session 111 — Float const-eval through binops

**Date:** 2026-05-25
**Outcome:** Float arithmetic where both operands
const-eval to f64 now gets its result range-checked
against the binop's float type. `1e30f32 * 1e30f32`,
`let a = 1e30f32; let b = a * a;`, and `1.0 / 0.0`
all surface diagnostics ("literal `inf` is out of
range for `f32`/`f64`"). Cross-let const-eval through
immutable float bindings flows in for free, mirroring
session 106 for ints. 436 codegen + 203 typecheck
tests green (+7 typecheck from session 110).

```rune
fn main() -> i64 {
    let a: f32 = 1.0e30f32;
    let b: f32 = a * a;        // ← error: literal `inf` is out of
                               //   range for `f32`
    b as i64
}
```

## The decisive observation

Session 102 / 106 built the int side: `const_eval_int`
walks literals + bindings + binops and reports
arithmetic overflow against the result type. Session
108 added the f32/f64 range check for literals. The
piece left was the float counterpart of the *binop*
const-eval — `const_eval_float` walks the same shape
as `const_eval_int`, and IEEE-754 arithmetic gives us
overflow detection "for free" via inf/NaN: a `*` that
overflows f64 rolls into `f64::INFINITY`, and
`check_float_value_in_range` already errors on non-
finite values.

```rust
fn const_eval_float(&self, e: &Expr) -> Option<f64> {
    match e {
        Expr::Lit { lit: Lit::Float(v, _), .. } => Some(*v),
        Expr::Path(p) => self.const_float_values.get(...),
        Expr::Unary { op: UnOp::Neg, expr, .. } => Some(-eval(expr)?),
        Expr::Binary { op, lhs, rhs, .. } => match op {
            Add => Some(eval(lhs)? + eval(rhs)?),
            Sub => Some(eval(lhs)? - eval(rhs)?),
            Mul => Some(eval(lhs)? * eval(rhs)?),
            Div => Some(eval(lhs)? / eval(rhs)?),
            _ => None,  // mod / bit-ops not valid on floats
        },
        _ => None,
    }
}
```

Two call sites (finish_binary and check_binary,
parallel to session 102/107/110) check the binop's
result via const_eval_float when `result_ty` is
`Ty::Float`, then run session 108's range checker.
One new field `const_float_values: HashMap<SymbolId,
f64>` on Checker, populated in `check_let` for
immutable float Ident bindings — mirroring session
106's int tracking.

### IEEE-754 = overflow detection for free

We don't need `checked_add` for floats — IEEE-754
*defines* overflow as ±infinity. `a * b` for a, b
in f64's range may return `f64::INFINITY` when the
mathematical product exceeds f64::MAX. Session 108's
`check_float_value_in_range` already rejects any
non-finite f64 (`v.is_finite() == false`), so the
infinity result trips it automatically.

Same for `0.0 / 0.0 = NaN` — IEEE-defined, but
also non-finite, so the range check catches it. And
`1.0 / 0.0 = +inf` — IEEE-defined as a valid
operation but the inf result still trips the range
check when assigned to a typed binding. Net
behavior: any operation that produces a non-finite
result errors at typecheck.

### Float div-by-zero policy

Session 107's divide-by-zero diagnostic for integers
explicitly excluded floats — IEEE-754 specifies
inf/NaN for those cases, not an error. Session 111
doesn't add a float-specific div-by-zero check.
Instead, the resulting infinity naturally fails the
range check when the binop is assigned to a typed
binding (test `float_div_by_zero_produces_inf_no_
error` covers this — the test is misnamed in the
intent but the diagnostic surfaces from the range
check, not from a div-by-zero check).

### Why mod and bit-ops fall through

Float modulo (`fmod`) isn't part of v0.x's binop
set — `%` on floats would emit a typecheck error
from `binop_result_ty`. Same for bitwise ops. So
const_eval_float only handles the four arithmetic
ops, and other patterns return None (skip the
const-eval path).

## The wire-ups

```
src/checker.rs    (+1 Checker field: const_float_values.
                   +1 const_eval_float walker.
                   +1 float compound check block in
                    finish_binary + check_binary
                    (mirrors session 102's int block).
                   +1 float tracking block in check_let
                    (mirrors session 106's int tracking).)

tests/typecheck.rs  (+7 new tests: f32 binop overflow,
                     f64 binop overflow, cross-let
                     binop overflow, in-range accepted,
                     float div-by-zero produces inf,
                     compound chain through binding,
                     negation through binding.)
```

No lower / codegen / runtime changes — checker-only
extension, IEEE-754 semantics drive the detection.

## What's tested

Typecheck (+7 from session 110's 196):

- `float_binop_overflow_f32_rejected` — `1e30f32 *
  1e30f32 = inf → error`.
- `float_binop_overflow_f64_rejected` — `1e200 *
  1e200 = inf → error`.
- `float_binop_through_let_binding_rejected` —
  cross-let const-eval: `a * a` where `a = 1e30f32`.
- `float_binop_in_range_accepted` — `1e10f32 *
  1e10f32 = 1e20f32` is finite (well within f32's
  range), compiles.
- `float_div_by_zero_produces_inf_no_error` —
  `1.0 / 0.0` rolls to +inf which fails range
  check; the diagnostic comes from session 108's
  range check, not a div-by-zero check.
- `float_compound_binop_through_chain` — `let a =
  1e308; let b = a + a;` overflows f64.
- `float_negation_through_binding` — `-a * a` where
  `a = 1e30f32` overflows (magnitude unchanged by
  negation; product still overflows).

## Apparent bugs that aren't / explicitly deferred

- **NaN as an intentional sentinel.** A user who
  wants to produce a NaN for sentinel purposes
  (e.g., uninitialized math) can't write the
  literal in source (no `NaN` syntax) — and
  arithmetic that produces NaN at compile-time
  errors. The user can still produce NaN at
  *runtime* via path-dependent operations
  (function calls). v0.x is loud about compile-
  time NaN; runtime NaN is the user's problem.
- **Subnormal results.** `1e-30f32 * 1e-30f32 =
  1e-60` rounds to f32 subnormal/zero, not an
  error. Same policy as session 108: round-to-
  infinity is loud, round-to-zero is silent. The
  user explicitly chose a small operand; the
  result is representable (as 0.0 if subnormal
  is too small).
- **Precision loss without overflow.** `1.0 +
  1e-20` in f64 stays exactly 1.0 due to
  precision; we don't diagnose. Catching that
  would require comparing exact-real arithmetic
  to f64 — out of scope.
- **Float div-by-zero shape.** Unlike integer
  div-by-zero (hardware trap, session 107
  diagnoses), float div-by-zero produces a valid
  IEEE value. We don't emit a separate
  diagnostic — the inf result naturally fails the
  range check when assigned to a typed binding.
- **`+= / -= / *= / /=` on floats.** These go
  through check_assign_op (different path) which
  doesn't currently invoke the const-eval check.
  The session-102/106/107/110/111 chain has the
  same gap for the integer side too; tightening
  is a separate session.

## What's next

- **Float compound assignment (`+=` etc.)** — wire
  the const-eval checks into check_assign_op so
  `a += 1e30; a *= 1e30;` catches the overflow
  parallel to the binop check.
- **Subnormal-as-zero warning** — needs warning
  infrastructure (currently checker only has
  errors).
- **Self-hosted bootstrap** — long-term.
