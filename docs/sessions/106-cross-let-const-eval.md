# Session 106 — Cross-let const-eval

**Date:** 2026-05-25
**Outcome:** Const integer values flow through
immutable `let` bindings into session 102's overflow
check. `let a = 100u8; let b = 200u8; a + b` now
errors at typecheck with "literal `300` is out of
range for `u8`" — previously was a runtime wrap.
436 codegen + 171 typecheck tests green (+4 typecheck
from session 105: 5 new cross-let tests, 1 inverted
test renamed).

```rune
fn main() -> i64 {
    let a: u8 = 100u8;
    let b: u8 = 200u8;
    let c: u8 = a + b;   // ← error: literal `300` is out of range for `u8`
    c as i64
}
```

## The decisive observation

Session 102 added `const_eval_int(&Expr) -> Option<i64>`
that recursively evaluates pure-literal arithmetic.
The natural extension: when a `let` binding's init
const-evals, remember the value keyed by the binding's
symbol, so a later `Expr::Path` referencing the
binding looks up the value through that symbol.

```
let a = 100u8;        # check_let records const_values[a_sym] = 100
let b = 200u8;        # const_values[b_sym] = 200
let c = a + b;        # const_eval_int walks:
                      #   Binary(Add, Path(a), Path(b))
                      #   → const_eval_int(a) = const_values[a] = Some(100)
                      #   → const_eval_int(b) = Some(200)
                      #   → checked_add(100, 200) = Some(300)
                      # 300 > u8::MAX → error
```

One new field (`const_values: HashMap<SymbolId, i64>`
on Checker), one new arm in `const_eval_int`
(`Expr::Path` → look up in the map), and a hook in
`check_let` that records when the init const-evals.

### What's tracked vs not

Tracked:
- **Immutable** Ident-pattern let bindings
- Whose **declared/inferred type is `Ty::Int(_)`**
- Whose **init const-evals to `Some(value)`** via
  `const_eval_int` (which now sees through earlier
  bindings recursively)

Not tracked:
- `let mut a = ...` — a subsequent `a = ...`
  would invalidate the recorded value; v0.x
  doesn't track invalidations
- `let a = some_fn();` — function returns aren't
  const-evaluable
- `let a = foo.bar();` — neither are method calls
- Float bindings — `const_eval_int` is integer-only
  (floats need IEEE-754-aware folding; deferred)

The tracking is per-symbol, so shadowing (`let a = 5;
let a = 10;`) works automatically — the second `let`
mints a fresh sym; both entries coexist in the map
but only the second is reachable via path resolution.

### The signed-value bug surfaced

The negation test (`let a = 100i8; let b = -a - 100i8`)
exposed a latent issue in `check_int_value_in_range`:
it expects `(magnitude, negated_flag)` — designed for
source-level literals where the `-` parses as a
separate `Unary::Neg` wrapping a positive-magnitude
`Lit::Int`. Compound binop results, by contrast, are
already signed `i64`s.

The fix: at the finish_binary call site, translate
signed result `v` to `(magnitude, negated)` before
calling the range checker. `v < 0` → call with
`(-v, true)`; `v >= 0` → call with `(v, false)`.
i64::MIN can't be negated, so we silently skip that
edge case — it fits no signed type narrower than i64
anyway, and i64 always fits. Applied at both call
sites (finish_binary at session 103's location +
check_binary's legacy site).

## The wire-ups

```
src/checker.rs    (Checker.const_values: HashMap<SymbolId, i64>;
                   const_eval_int gets a Path arm that looks up
                   the symbol in the map; check_let records into
                   the map after binding the pattern. The compound
                   range-check site translates signed result to
                   (magnitude, negated) for check_int_value_in_range,
                   handling values that wrap negative through Sub /
                   Neg.)

tests/typecheck.rs  (-1 test renamed and inverted; +5 new tests
                     covering the closed deferral, accepted cases,
                     mutable-not-tracked, chained bindings, and
                     negation-through-binding.)
```

No lower / codegen / monomorphize changes — const_eval
is a checker-only concept that emits diagnostics; the
runtime behavior is unchanged when the diagnostic
doesn't fire.

## What's tested

Typecheck (+4 net from session 105's 167):

- `cross_let_const_eval_overflow_rejected` — the
  headline: `let a = 100u8; let b = 200u8; a + b`
  errors with `literal '300' is out of range for u8`.
  (Replaces the inverted `const_eval_skipped_for_non_
  const_operand` from session 102 which asserted the
  *absence* of this check.)
- `cross_let_const_eval_in_range_accepted` —
  complementary positive case: `50 + 100 = 150` fits
  u8.
- `cross_let_const_eval_skipped_for_mutable_binding`
  — `let mut a = 100u8` doesn't get tracked, so
  `a + 200u8` compiles. Confirms the immutability gate.
- `cross_let_const_eval_chains_through_binding` —
  `let a = 5; let b = a + 1; let c = b + 250` evaluates
  c=256 transitively, catches the u8 overflow at the
  third let.
- `cross_let_const_eval_negation_through_binding` —
  `let a = 100i8; let b = -a - 100i8` evaluates to
  b=-200, catches the i8 underflow. Surfaced the
  signed-value bug in the range check.

## Apparent bugs that aren't / explicitly deferred

- **`let mut` is intentionally untracked**. The
  alternative — track and invalidate on `=` — needs
  reassignment-flow analysis (visit every assignment,
  drop the recorded value, possibly re-record if the
  RHS const-evals). Marginal win for the complexity
  cost; v0.x doesn't carry it.
- **Const-eval doesn't escape its scope.** A const
  recorded in an inner block survives in the map
  (no scope-popping) but can't be reached from
  outside because the symbol isn't in lexical scope
  there. The map is effectively scope-correct via
  the resolver's name-resolution; we never pop
  entries.
- **Cross-function constants.** A top-level `const
  X: u8 = 100;` would need a different path (no
  Rune `const` keyword yet; only `let` inside fns).
  When const items land they'll record into the
  same map at a different call site.
- **Float const-eval.** `let pi = 3.14; let twice =
  pi + pi;` doesn't catch float-specific overflow
  (`1e300 * 1e300` → infinity silently). Float
  const-eval is its own future session.
- **`as`-cast through bindings.** `let x: i64 = 300;
  let y: u8 = x as u8;` doesn't const-eval through
  the cast — `const_eval_int` doesn't have an
  `Expr::Cast` arm. Could be added but the truncation
  semantics make "did this overflow?" ambiguous (the
  user asked for the truncation explicitly).
- **i64::MIN edge case.** A signed result of i64::MIN
  can't be magnitude-negated; we silently skip the
  range check rather than report a confusing error.
  Practical impact: zero — i64::MIN is far outside
  any narrower int's range, and the user explicitly
  exercising it would type-annotate as i64 anyway.

## What's next

- **Division-by-zero const-eval diagnostic** —
  `100 / 0` errors at typecheck.
- **Floating-point literal range checks** —
  `3.4e40f32` rounds to infinity today.
- **`as`-cast through const-tracked bindings** —
  with the magnitude/sign signed-value fix in place,
  adding a Cast arm to const_eval_int is mechanical.
- **Self-hosted bootstrap** — long-term.
