# Session 112 — Compound assignment const-eval

**Date:** 2026-05-25
**Outcome:** `a /= 0` and `a %= 0` (and the
cross-let `let z = 0; a /= z;`) now error at
typecheck with "division by zero" / "remainder by
zero", matching session 107's binop behavior. The
divide-by-zero check from session 107 was the only
check that doesn't depend on the LHS value — and
the LHS in `a += b` is necessarily mutable, so we
don't track it. 436 codegen + 209 typecheck tests
green (+6 typecheck from session 111).

```rune
fn main() -> i64 {
    let mut a: i64 = 100;
    a /= 0;              // ← error: division by zero
    a
}
```

## The decisive observation

Compound assignment `a op= b` is semantically `a = a
op b`, but the AST keeps it as a single
`Expr::AssignOp { op, lhs, rhs }` node that flows
through `check_assign_op` — *not* `finish_binary`.
So the sessions-102-through-111 chain that wired
diagnostics into the binop path never reached the
compound assignment form.

The LHS of a compound assignment is necessarily
mutable (so session 106's tracking doesn't record
it), and the binop conceptually "uses" the LHS's
value (so we can't const-eval the result without
tracking). What survives the constraints:

1. **Divide / remainder by zero** — divisor const-
   evaluates independent of LHS. Same shape as
   session 107.
2. **Shift-out-of-range** — if Rune had `<<=` /
   `>>=`, same shape as session 110. The parser
   doesn't currently produce them.
3. **RHS-only overflow** — if the RHS itself is a
   compound expression that overflows on its own,
   session 102's check at the *inner* binop fires.
   No new logic needed.

That last point is the most interesting: `a += 100u8
+ 200u8` doesn't need any check inside
`check_assign_op` because the parser parses the
RHS as a binary expression, the checker recurses
into the binary expression via `check_expr(rhs)`,
and session 102's overflow check fires at the
*inner* `100u8 + 200u8` site. The compound-assign
just delegates to existing infrastructure.

### What's NOT checked

Float div-by-zero: parallel to session 111's
binop policy — IEEE-754 specifies inf/NaN, not an
error. The result lands in the mutable `a` so
there's no downstream range check either; the
runtime gets the IEEE result.

Arithmetic overflow on `a += b`: we don't know
`a`'s value, so we can't say if `a + b` overflows.
The session-102 chain catches it only when both
operands are const-evaluable, which `a` (mutable)
never is.

`a <<= 64`: not a real case — parser has no `<<=`
operator in v0.x. The check is defensively
*absent* from check_assign_op (would be unreachable
code). If shift compounds land in the parser
later, copy the gate from finish_binary verbatim.

## The wire-ups

```
src/checker.rs    (check_assign_op gains one new
                   block for `Ty::Int(_)` LHS:
                   if op is Div or Mod and the rhs
                   const-evals to 0, emit "division
                   by zero" / "remainder by zero".
                   Mirrors session 107's gate at
                   finish_binary, just one less
                   const_eval call (we don't need
                   `a`'s value, only the divisor's).)

tests/typecheck.rs  (+6 new tests: div-by-zero,
                     mod-by-zero, div-through-binding,
                     rhs-overflow-via-inner-binop,
                     in-range positive control,
                     float-div-by-zero no diagnostic.)
```

No lower / codegen / runtime changes. Checker-only
extension to an existing path.

## What's tested

Typecheck (+6 from session 111's 203):

- `compound_assign_div_by_zero_rejected` — `a /= 0`
  errors.
- `compound_assign_mod_by_zero_rejected` — `a %= 0`
  with "remainder by zero".
- `compound_assign_div_by_zero_through_binding_
  rejected` — `let z = 0; a /= z` catches via
  cross-let.
- `compound_assign_rhs_overflow_caught_via_inner_
  binop` — `a += (100u8 + 200u8)` errors at the
  inner binop, no special compound-assign logic
  needed.
- `compound_assign_in_range_accepted` — all the
  legitimate compound ops compile (`+= -= *= /= %=`
  with valid operands).
- `compound_assign_float_div_by_zero_no_check` —
  `let mut a: f64 = 1.0; a /= 0.0;` compiles
  (IEEE float div-by-zero produces inf, not an
  error; matches session 111's binop policy).

## Apparent bugs that aren't / explicitly deferred

- **`a += b` overflow tracking.** The LHS is
  mutable, so session 106's const tracking skips
  it. An arithmetic overflow that depends on
  `a`'s value can't be caught at typecheck.
  Runtime wraps (the integer semantics) or, for
  floats, rolls into IEEE-754 inf/NaN.
- **Shift compounds.** `<<=` and `>>=` aren't in
  the parser today. The shift-out-of-range check
  is intentionally *not* present in check_assign_op
  because it would be unreachable. Adding the
  operators is a separate parser change.
- **Bit-op compounds.** Same — `&= |= ^=` aren't
  in the parser.
- **Float div-by-zero in compound.** Deliberately
  silent: IEEE-754 specifies inf/NaN as valid
  results. The mutable LHS absorbs the inf
  without a range check fire.

## What's next

- **Subnormal-as-zero warning** — `1e-50f32`
  silently rounds; would need warning
  infrastructure (currently checker only has
  errors).
- **Shift compound operators (`<<=` `>>=`)** —
  parser extension; the check_assign_op gate
  is mechanical to add once the operators exist.
- **Self-hosted bootstrap** — long-term.
