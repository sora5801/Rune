# Session 102 — Compound const-eval overflow

**Date:** 2026-05-25
**Outcome:** Binary-op expressions where both operands
const-eval to integer values now get their result
range-checked against the binop's result type at
compile time. `100u8 + 200u8` errors with "literal
`300` is out of range for `u8`". Closes the
compound-overflow half of pre-1.0 priority list,
complementing session 099's per-literal check. 167
typecheck tests green (+5 from session 101).

```rune
let a: u8 = 100u8 + 200u8;    // error: 300 overflows u8
let b: i8 = 100i8 * 2i8;      // error: 200 overflows i8
let c: u8 = 5u8 - 10u8;       // error: -5 doesn't fit u8
let d: u8 = 50u8 + 100u8;     // ok: 150 fits
let e: u8 = (a + b) as u8;    // not const-eval'd (operands aren't lits) — runtime
```

## The decisive observation

Session 099 added range checks for hinted literals
(`let a: u8 = 1000;` → error). The natural follow-on
is binop expressions: `100u8 + 200u8` is two valid
literals whose runtime sum overflows. Const-eval the
expression, run the same range check.

Two pieces:

### 1. `const_eval_int(&Expr) -> Option<i64>`

Recursive walker over integer expressions. Returns
`Some(v)` when the expression is pure literal
arithmetic with optional suffix, `None` otherwise.
Uses `checked_*` arithmetic to bail out on i64-level
overflow (`1_000_000_000_000 * 1_000_000_000_000`
returns None — the runtime would wrap; v0.x doesn't
attempt to detect i64 wrap).

Supports:
- Integer literals (suffixed or bare)
- Unary neg
- Binary +, -, *, /, %, &, |, ^, <<, >>

Doesn't support (returns None):
- Path expressions (variables aren't const-eval'd
  in v0.x)
- Function calls
- Cast expressions (could in principle, but `as`
  is the user's escape hatch for explicit
  truncation — keep it opaque)
- Method calls, struct lits, etc.

### 2. Check at `check_binary` after result-type determination

After the operator's result type `t` is computed (the
arithmetic / numeric arm), if `t` is `Ty::Int(_)` and
both operands const-eval to integers, run the same
arithmetic in i64 and range-check against `t`.

```rust
if let Ty::Int(result_ty) = &t {
    if matches!(op, BinOp::Add | Sub | Mul | Div | Mod
                  | BitAnd | BitOr | BitXor | Shl | Shr) {
        if let (Some(a), Some(b)) =
            (self.const_eval_int(lhs), self.const_eval_int(rhs))
        {
            let result = /* checked op a b */;
            if let Some(v) = result {
                self.check_int_value_in_range(v, *result_ty, false, span);
            }
        }
    }
}
```

Reuses session 099's `check_int_value_in_range` —
same range table for every IntTy.

### Pure-literal-arithmetic is the gate

`let a = 100u8; let b = 200u8; let c: u8 = a + b;`
doesn't trigger the check because `a` and `b` are
paths, not literals. Runtime overflow still happens
(u8 wraps to 44). This is intentional: const-eval is
about catching obviously-wrong compile-time constants,
not about general value-range tracking (which would
need abstract interpretation across let bindings).

For typical numeric workloads where literals appear
once at the binding site, the gate covers the common
case.

## The wire-ups

```
src/checker.rs    (new const_eval_int helper;
                   check_binary runs the const-eval
                   range check after type
                   determination.)

tests/typecheck.rs  (+5 tests: add overflow, mul
                     overflow, unsigned underflow,
                     in-range accepted, non-const
                     operand skipped.)
```

No AST / parser / resolver / lower / mono / codegen
changes. Pure checker enhancement.

## What's tested

Typecheck (+5):

- `const_eval_add_overflow_rejected` — `100u8 + 200u8`
  → "literal `300` is out of range for `u8`".
- `const_eval_mul_overflow_rejected` — `100i8 * 2i8`
  → "literal `200` is out of range for `i8`".
- `const_eval_unsigned_underflow_rejected` — `5u8 -
  10u8` → "literal `-5` is out of range for `u8`".
- `const_eval_in_range_accepted` — `50u8 + 100u8`
  and `10i8 * 5i8` both compile.
- `const_eval_skipped_for_non_const_operand` — `let
  a: u8 = 100u8; let b: u8 = 200u8; let c: u8 = a +
  b;` compiles (runtime overflow is u8's problem,
  not the const-eval gate's).

## Apparent bugs that aren't / explicitly deferred

- **Cross-let const-eval** — `let a: u8 = 100u8; let
  b: u8 = a + 200u8;` would need to track `a`'s
  const value across let bindings. Skipped in v0.x;
  the workaround is to put the arithmetic inline.
- **i64-level overflow during eval** — if
  `1_000_000_000_000 * 1_000_000_000_000` overflows
  i64 during the checked_mul, const_eval returns
  None and no check fires. Runtime would wrap. A
  stricter version would error at the i64 overflow
  site too, but v0.x errs toward "be silent if you
  can't be sure."
- **`as` casts** — `(300i64 as u8)` doesn't error;
  the user's intent is explicit truncation. The
  const-eval walker stops at the Cast node.
- **Division by zero** — `let a: u8 = 1u8 / 0u8;`
  the checked_div returns None, no compile-time
  error. Same shape as i64 overflow above.
  Runtime `rune_panic` would fire. Could tighten
  in a future session.
- **Float arithmetic** — const_eval_int only
  handles integers. `1.5f32 + 2.5f32` doesn't
  const-eval; no check fires. Floats have huge
  ranges so overflow is rare in practice.
- **Result type Ty::TypeVar** — if the binop's
  result type isn't pinned (rare; the binop
  type-check above already runs), the result_ty
  match doesn't fire and the check is skipped.
  Correct behavior.
- **Compound operators (`+=`)** — `check_assign_op`
  is a different path. Not yet const-eval-checked.
  Same pattern would apply if needed.

## What's next

- **Chained binop hint propagation** — `1 + 2 + a:
  i32` works without parens.
- **Floating-point Vec elements** — unblock numeric
  workloads on f64.
- **Self-hosted bootstrap** — long-term.
