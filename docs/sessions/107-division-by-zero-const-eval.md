# Session 107 — Division-by-zero const-eval

**Date:** 2026-05-25
**Outcome:** `100 / 0` and `5 % 0` error at typecheck
with "division by zero" / "remainder by zero". Cross-
let const-eval (session 106) flows in: `let z = 0;
let x = 100 / z;` also catches the divisor. 436
codegen + 177 typecheck tests green (+6 typecheck
from session 106).

```rune
fn main() -> i64 {
    let z: i64 = 0;
    let x: i64 = 100 / z;   // ← error: division by zero
    x
}
```

## The decisive observation

Session 102's const-eval block already used
`a.checked_div(b)` and `a.checked_rem(b)`, both of
which return `None` on divisor=0. But `None` is also
what overflow returns (`i64::MIN.checked_div(-1)`), so
the existing code silently swallowed both cases without
distinguishing. The fix is to const-eval the divisor
*separately* and emit a specific diagnostic when it's
exactly zero — before (and independent of) the
overflow check.

```rust
if matches!(op, BinOp::Div | BinOp::Mod) {
    if self.const_eval_int(rhs) == Some(0) {
        self.error(span, format!("{} by zero",
            if op == BinOp::Mod { "remainder" } else { "division" }));
    }
}
```

Three lines, two call sites (finish_binary and
check_binary — both still carry the const-eval block).
Session 106's `const_eval_int` already walks through
let bindings, so divisor zeros that arrive via a
named binding are caught the same way as bare
literals.

### Why "remainder by zero" not "division by zero"

The `%` operator's reduction is *remainder*, not
*division* — and the IEEE / language standards
distinguish: `100 / 0` is division by zero, `100 % 0`
is remainder by zero. The diagnostic mirrors the
operator's actual semantics. Tiny detail, but the
right precision.

### Surfaced two tests that relied on runtime trap

`logical_and_short_circuits` and `logical_or_short_
circuits` (codegen) used `10 / 0` to demonstrate
short-circuit evaluation — the design intent was
"this would trap at runtime if evaluated; short-circuit
prevents it." With session 107, the const-eval catches
the `/ 0` at typecheck, so the test compiles fail
before reaching runtime.

Fix: replace `10 / 0` with `10 / z` where `z` is a
mutable binding (`let mut z: i64 = 0; z = 0;`). Session
106's const-tracking only fires for immutable bindings,
so the divisor isn't recognized as const-zero. The
runtime trap (now never actually exercised because of
the short-circuit) is preserved; the test's intent —
"the rhs is never executed" — survives.

## The wire-ups

```
src/checker.rs    (Two divide-by-zero blocks added: one in
                   finish_binary (session 103's location),
                   one in check_binary (session 102's legacy
                   site). Each fires when `op` is Div / Mod
                   and the divisor const-evals to 0, before
                   the existing overflow check.)

tests/codegen.rs  (Two existing short-circuit tests updated
                   to use a mutable-binding divisor so they
                   stay runtime-only.)

tests/typecheck.rs (+6 new tests: bare `/ 0`, bare `% 0`,
                    div-through-binding, div-through-
                    compound-const, mutable-divisor-skipped,
                    and the positive-control divide-by-non-
                    zero.)
```

No lower / codegen / runtime changes — diagnostic
only. Const-eval stays a checker-only concept; the
runtime semantics of integer division on a non-const
zero (a trap on signed division, wrap on unsigned —
actually, runtime trap on x86-64 for any int / 0
because Cranelift lowers to `sdiv`/`udiv` which fault)
are unchanged.

## What's tested

Typecheck (+6 from session 106's 171):

- `div_by_zero_literal_rejected` — `100 / 0` errors.
- `mod_by_zero_literal_rejected` — `100 % 0` errors
  with "remainder by zero".
- `div_by_zero_through_let_binding_rejected` —
  `let z = 0; let x = 100 / z;` errors. Cross-let
  const-eval from session 106 flows in.
- `div_by_zero_through_compound_const_rejected` —
  `42 / (5 - 5)` — `5 - 5` const-evals to 0, then
  the outer divisor check fires.
- `div_by_zero_skipped_for_mutable_divisor` —
  `let mut z = 0; let x = 100 / z;` compiles
  (session 106's mut-not-tracked gate is intact).
- `div_by_nonzero_accepted` — `let z = 4; 100 / z`
  compiles cleanly.

## Apparent bugs that aren't / explicitly deferred

- **`let mut` divisor**. Intentional: session 106's
  tracking gate excludes mutables. If a user
  *really* wrote `let mut z = 0;` and meant `z=0`
  forever, that's their mistake — but the more
  common shape (`let mut z = 0; ...; z = compute();`)
  is correct to allow at compile-time. Runtime trap
  on signed `/ 0` is the safety net.
- **Float divide-by-zero.** IEEE-754 specifies
  `1.0 / 0.0 = infinity`, `0.0 / 0.0 = NaN`. These
  aren't errors — they're valid float values. No
  diagnostic; runtime gets the right IEEE result.
- **Runtime-only div by zero.** A divisor that comes
  from a function call or a method receiver still
  traps at runtime. Hardware faults on signed
  `sdiv` with zero divisor (an `idiv` exception on
  x86-64); Cranelift propagates this. v0.x doesn't
  intercept — the user gets the SIGFPE / illegal-
  instruction signal.
- **`as`-cast through tracked binding**. Still
  deferred from session 106. `let z: i64 = 100;
  let x: i64 = 50 / (z as u8 - 100u8);` — the cast
  isn't visible to `const_eval_int`, so the inner
  `(z as u8 - 100u8) = 0` doesn't get caught. Future
  session would add an Expr::Cast arm.
- **Shift-by-too-large**. `100i32 << 64` lowers to
  Cranelift's `ishl` which is undefined for shifts
  >= bit-width; const_eval_int already returns None
  (via the `if b < 0 || b >= 64` gate on Shl/Shr).
  Should probably also be a diagnostic, but it's
  not divide-by-zero — separate concern.

## What's next

- **Floating-point literal range checks** —
  `3.4e40f32` rounds silently to f32::INFINITY today.
- **`as`-cast through const-tracked bindings** —
  add Expr::Cast arm to const_eval_int.
- **Shift-out-of-range diagnostic** — same pattern,
  different operator. `x << 64` errors at typecheck
  if the shift amount const-evals to ≥ bit-width.
- **Self-hosted bootstrap** — long-term.
