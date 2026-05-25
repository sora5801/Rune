# Session 099 — Hinted-literal overflow checks

**Date:** 2026-05-25
**Outcome:** Bare numeric literals hinted by a let-
binding (or fn-arg, struct-field, binop) annotation
now get range-checked against the hint's type.
`let a: u8 = 1000;` and `let a: i8 = -200;` both
error at compile time. Closes the half of session 092
that suffix-only checks couldn't reach. 162 typecheck
tests green (+4 from session 098).

```rune
let a: u8 = 1000;     // error: literal `1000` out of range for `u8`
let b: i8 = -200;     // error: literal `-200` out of range for `i8`
let c: u8 = 200;      // ok
let d: i8 = -100;     // ok
let e: u8 = 1000u8;   // already-rejected (session 092)
```

## The decisive observation

Session 092 added suffix-bearing overflow checks
(`1000u8` → error). But hinted literals — bare `1000`
adopted into u8 via session 091's hint flow — never
ran that check. The fix is mechanical: factor the
range-check out of session 092's
`check_numeric_lit_in_range(lit, ...)` (which gates
on the suffix presence) into a lower-level
`check_int_value_in_range(v, ty, ...)` that takes
the raw `(value, IntTy)` pair regardless of source.

```rust
fn check_int_value_in_range(
    &mut self,
    v: i64,
    ty: IntTy,
    negated: bool,
    span: Span,
) {
    // unchanged bound logic from session 092 — sign / unsigned
    // tables for the 10 IntTy variants
}

// Session 092's helper now delegates:
fn check_numeric_lit_in_range(&mut self, lit, span, negated) {
    let (v, ty) = match lit {
        Lit::Int(v, Some(ty)) => (*v, *ty),
        _ => return,
    };
    self.check_int_value_in_range(v, ty, negated, span);
}
```

### Hint-path call sites

The two hint sites in `check_expr_with_hint` now call
`check_int_value_in_range` before stamping the type:

```rust
// `let a: u8 = 1000;`
if let (Expr::Lit { lit, span }, Some(exp)) = (e, expected) {
    if let Some(ty) = self.numeric_lit_hint(lit, exp) {
        if let (Lit::Int(v, None), Ty::Int(it)) = (lit, &ty) {
            self.check_int_value_in_range(*v, *it, false, *span);
        }
        self.expr_types.insert(*span, ty.clone());
        return ty;
    }
}

// `let a: i8 = -200;`
if let (Expr::Unary { op: Neg, expr, span }, Some(exp)) = (e, expected) {
    if let Expr::Lit { lit, span: lit_span } = expr.as_ref() {
        if let Some(ty) = self.numeric_lit_hint(lit, exp) {
            if let (Lit::Int(v, None), Ty::Int(it)) = (lit, &ty) {
                self.check_int_value_in_range(*v, *it, true, *lit_span);
            }
            // ...
        }
    }
}
```

The negated-range check on hinted literals mirrors
session 092's negated check on suffixed literals
(I8: `v <= 128` for `-128 == i8::MIN`, etc.).

### What this isn't

This session adds **literal** overflow checks. It does
**not** add full const-eval for compound expressions:
`let a: u8 = 100 + 200;` (where the sum overflows u8)
still compiles — both `100` and `200` individually fit
u8, but the binop's runtime result is 300 mod 256 = 44.

The pre-1.0 audit's "const-eval overflow checks"
roadmap entry envisioned both kinds. Compound const-
eval is a larger feature: it'd need a recursive
`const_eval_int(e) -> Option<i64>` walker over
arithmetic / unary expressions, with range checks at
the result's typed-context (let / fn-arg / etc.). The
walker is mostly mechanical, but designing the
"when to fire" policy (at every binop? at typed
contexts only? what about overflow at the literal
level inside a wrapping_add() pattern?) is its own
session.

This session is the small, complete piece: every
literal you write — typed by suffix or by hint — gets
range-checked.

## The wire-ups

```
src/checker.rs    (extract check_int_value_in_range
                   from session 092's
                   check_numeric_lit_in_range; call
                   it from the two hint sites in
                   check_expr_with_hint for both
                   positive and negated cases.)

tests/typecheck.rs  (+4 tests: u8 over-range hinted,
                     i8 negated over-range hinted,
                     in-range sanity, suffixed
                     over-range still works.)
```

No AST / parser / resolver / lower / mono / codegen
changes — purely a checker enhancement.

## What's tested

Typecheck (+4):

- `hinted_literal_overflow_u8_rejected` — `let a: u8
  = 1000` errors with "literal `1000` is out of
  range for `u8`".
- `hinted_literal_overflow_i8_negated_rejected` —
  `let a: i8 = -200` errors with "literal `-200`
  is out of range for `i8`".
- `hinted_literal_in_range_accepted` — sanity: in-
  range u8 / i8 / i32 all compile.
- `suffix_literal_overflow_still_rejected` —
  confirms session 092's suffix-side check still
  fires after the refactor.

## Apparent bugs that aren't / explicitly deferred

- **Compound const-eval overflow** — `100u8 + 200u8`
  still compiles and emits a truncated runtime
  value. Needs a recursive const_eval walker; a
  focused future session. Suggested entry point:
  `let a: u8 = 100u8 + 200u8;` where the let's
  annotation gives the result's expected type.
- **Float range checks** — `let a: f32 = 1e500;`
  technically overflows f32, but f64 doesn't either,
  and the lexer parses to f64 which then converts to
  f32 with Inf at codegen. Pragmatic: leave alone.
- **Hex / binary suffix range** — `0xff_u8`
  (255u8) is the max u8 value; `0x100_u8` (256u8)
  overflows. Session 088's `int_with_radix` calls
  the same suffix path so the check fires for radix
  literals too — confirmed via the existing
  suffix-overflow test.
- **i64::MIN edge case** — `let a: i64 = -9223372036854775808;`
  is the minimum i64 value. The lexer parses
  `9223372036854775808` as i64 (which fails: it's
  one past i64::MAX), so the user can't write this
  literal without a workaround. Same gap exists in
  Rust and is a known papercut.

## What's next

- **Compound const-eval overflow** — `100u8 + 200u8`
  errors at compile.
- **Chained binop hint propagation** — `1 + 2 + a:
  i32` works without parens.
- **Floating-point Vec elements** — unblock numeric
  workloads on f64.
- **Self-hosted bootstrap** — long-term.
