# Session 109 — `as`-cast through const-tracked bindings

**Date:** 2026-05-25
**Outcome:** Const-tracked values (session 106) now
flow through `as`-casts. `let a: i64 = 300; let b: u8
= a as u8; let c: u8 = b + 250u8;` catches the c-
overflow even though the value reaches `c` via an
explicit width-narrowing cast. The cast itself emits
no diagnostic — the user wrote `as` for truncation
deliberately — but the truncated value enters the
tracking map so downstream arithmetic sees through it.
436 codegen + 190 typecheck tests green (+6 typecheck
from session 108).

```rune
fn main() -> i64 {
    let a: i64 = 300;
    let b: u8 = a as u8;        //   no error here:
                                //   user asked for truncation
    let c: u8 = b + 250u8;      // ← error: literal `294`
                                //   is out of range for `u8`
    c as i64
}
```

## The decisive observation

Session 106's `const_eval_int` already walked
`Expr::Path` (looking up the recorded value) and
`Expr::Binary` (folding checked arithmetic). The
missing arm was `Expr::Cast` — and the cost is one
match-arm plus a `cast_int_value(v, IntTy)` helper
that does what Rust's `as` does at runtime.

```rust
Expr::Cast { expr, span, .. } => {
    let v = self.const_eval_int(expr)?;
    match self.expr_types.get(span)? {
        Ty::Int(it) => Some(cast_int_value(v, *it)),
        _ => None,
    }
}
```

The cast's *result type* is already cached in
`expr_types[span]` by `check_cast` (which ran during
the normal typecheck pass that precedes any
const_eval_int call). We don't re-resolve the
syntactic `Type`.

### How `cast_int_value` matches runtime

The Rust `as` operator on integers truncates to
the target's bit width then sign- or zero-extends
back. `cast_int_value` mirrors that exactly:

```rust
match to {
    I8  => v as i8  as i64,   // truncate + sign-extend
    I16 => v as i16 as i64,
    I32 => v as i32 as i64,
    I64 | ISize => v,         // no truncation
    U8  => (v as u8)  as i64, // truncate + zero-extend
    U16 => (v as u16) as i64,
    U32 => (v as u32) as i64,
    U64 | USize => (v as u64) as i64,  // bit-pattern preserved
}
```

`300 as u8` = `44`; `-1 as u8` = `255`; `-1 as i8`
= `-1`; `i8::MIN as i64` = `-128`; `u64::MAX as i64`
= `-1` (same bit pattern, different interpretation).
The storage convention matches `check_int_value_in_
range`'s expected encoding so downstream range
checks compare against the right bounds.

### Why no diagnostic at the cast site

`as` is the user's explicit "I want truncation"
signal. Emitting an error at `300 as u8` would
defeat the purpose of having the cast operator —
the user could always have written `(300 & 0xff)
as u8` or some such, but `as u8` IS the idiomatic
way to say "give me the low 8 bits." So we silently
track the *result* of the cast for downstream
benefit without complaining about the cast itself.

The downstream benefit is concrete: subsequent
arithmetic on the cast result still gets the
session-102 overflow check. The user can `as`-cast
to lose precision, but `(a as u8) + 250u8` will
still warn if the sum doesn't fit u8.

### U64 / USize bit-pattern caveat

For u64 and usize targets, the cast result is
stored as a bit-pattern-preserved i64. Values
larger than `i64::MAX` (e.g. `-1 as u64` = `u64::
MAX`) are stored as the negative i64 with the same
bit pattern. Downstream operations that const-eval
treat this as negative (since `const_eval_int`
returns i64) — fine for arithmetic that preserves
bit patterns (add/sub/xor), questionable for
unsigned-range checks against `>= 0`. v0.x ships
the simpler bit-pattern-preserving approach because:

1. Source-level idioms rarely produce out-of-i64
   u64 values; the cast `-1 as u64` is the main
   way.
2. Compound binops involving them still produce
   correct bit patterns (which is what the runtime
   does too).
3. The alternative — gating tracking when the
   value can't be represented as positive i64 —
   would silently drop tracking for u64 chains
   that *would* have caught real bugs (`let x: u64
   = 5; let y: u64 = x as u64 * 4_000_000_000u64;`
   — needs tracking even though intermediate steps
   exceed i64::MAX).

## The wire-ups

```
src/checker.rs    (+1 match-arm in const_eval_int
                    for Expr::Cast.
                   +1 free helper `cast_int_value`
                    next to vec_element_supported.)

tests/typecheck.rs  (+6 new tests covering:
                     - cast-then-overflow-detected
                     - cast-itself-no-diagnostic
                     - signed-to-unsigned bit pattern
                     - signed-to-signed sign preservation
                     - widening (no truncation)
                     - chained casts through 3+ types)
```

No lower / codegen / runtime changes — checker-only
extension to a checker-only walker.

## What's tested

Typecheck (+6 from session 108's 184):

- `as_cast_propagates_const_value_overflow` — the
  headline: `300 as u8` records 44; `+ 250u8`
  overflows.
- `as_cast_truncates_no_diagnostic_at_cast_site` —
  the cast itself doesn't error.
- `as_cast_signed_to_unsigned_preserves_bit_pattern`
  — `-1 as u8` records 255; `+ 1u8` overflows.
- `as_cast_signed_to_signed_preserves_sign` — `-100
  as i8` records -100; `- 50i8` underflows i8.
- `as_cast_widens_without_loss` — `i8(100) as i64`
  preserves 100; downstream `as u8 + 200u8`
  overflows.
- `as_cast_chain_through_bindings` — `i64 → i32 →
  u8 + 250u8` chain catches the overflow at the
  final binop.

## Apparent bugs that aren't / explicitly deferred

- **Cast at the cast site is silent.** Deliberate
  policy — `as` is the user's "truncate me" signal.
  A future *warning* infrastructure could surface
  "this cast loses information" but v0.x only has
  errors.
- **u64 / usize values > i64::MAX stored as
  negative i64.** Bit-pattern preserved through
  arithmetic; range-checks against `v >= 0` would
  spuriously reject. Edge case, not exercised by
  common source idioms.
- **Float casts return None.** `let x: f64 = 3.14;
  let y: i64 = x as i64;` doesn't const-eval —
  `const_eval_int` is int-only. Float const-eval
  remains its own future session.
- **`as`-cast in const-eval'd binops.** `let x =
  300 / (4i8 as i64);` would need both operands
  to const-eval through casts; the new arm
  handles it (the cast inside the binop folds
  before the outer Div check fires).
- **No cast result-range check at the cast site.**
  Already deferred from session 106; the cast
  always succeeds at the cast site, errors only
  surface at *subsequent* arithmetic.

## What's next

- **Shift-out-of-range diagnostic** — `x << 64`
  at typecheck. Pattern: same as session 107's
  div-by-zero, just `op = Shl / Shr` and `b >=
  bit_width` instead of `b == 0`.
- **Float const-eval through binops** — parallel
  to session 102/106 for floats; checked f64
  arithmetic, then session 108's range check on
  the result.
- **Self-hosted bootstrap** — long-term.
