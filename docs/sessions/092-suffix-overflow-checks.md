# Session 092 — Suffix overflow checks

**Date:** 2026-05-25
**Outcome:** Suffixed numeric literals that don't fit
their declared type are rejected at type-check.
`1000u8`, `200i8`, `-129i8`, `-5u8` all error with a
clear diagnostic. The negated-range case (`-128i8`
fits i8 even though `128i8` doesn't) is handled
correctly. Closes session 088's deferred half. 417
codegen + 152 typecheck tests green (+6 typecheck,
codegen unchanged).

```rune
let x: u8 = 1000u8;     // error: literal `1000u8` is out of range for `u8`
let x: i8 = 200i8;      // error: literal `200i8` is out of range for `i8`
let x: i8 = -129i8;     // error: literal `-129i8` is out of range for `i8`
let x: u8 = -5u8;       // error: literal `-5u8` is out of range for `u8`

let x: i8 = -128i8;     // valid — i8 range is -128..=127
let x: u8 = 255u8;      // valid — u8 max
```

## The decisive observation

The lexer parses digit magnitudes into `i64`; sign
comes from a separate AST node (`Unary::Neg` wrapping
a `Lit::Int`). So the overflow check has two cases
that the same logic shouldn't conflate:

- **Bare suffix literal** (`1000u8`) — check positive
  bound: `v <= ty.max_positive()`.
- **Negated suffix literal** (`-128i8`) — check the
  magnitude against the type's negative range. Signed
  types accept one more magnitude on the negative
  side (`i8: -128..=127`, so `v <= 128` when
  negated). Unsigned types reject any negation
  regardless of magnitude.

The check fires at two points in `check_expr_inner`:

1. **`Expr::Lit { lit, span }`** — call
   `check_numeric_lit_in_range(lit, span, false)`.
   Bare positive case.
2. **`Expr::Unary { op: Neg, expr: Lit, .. }`** —
   intercepted in `check_unary` before recursing into
   the inner expr; call
   `check_numeric_lit_in_range(lit, lit_span,
   true)`. Returns the lit_type directly (skipping
   the outer numeric guard) so the positive-bound
   check at (1) doesn't fire and reject `-128i8`
   spuriously.

The bare case fires at every Lit, but only does work
when a suffix is present (the function early-returns
for `Lit::Int(_, None)` etc.).

### Why intercept in `check_unary`, not `check_expr_with_hint`

Session 091's hint flow (`let a: i8 = -128;`) already
threads a hint through `check_expr_with_hint` for
unhinted bare numeric literals. The suffix check
operates on the *source-level suffix*, which lives in
the AST regardless of any contextual hint. It needs
to fire even when no hint exists — `-128i8;` as a
top-level expression should still be checked. Putting
it in `check_unary` gets the right coverage.

### Boundary value choice

The signed types use `v <= 2^(N-1)` for the negated
case rather than `v < 2^(N-1) + 1` because `2^(N-1)`
fits in `i64` for N up to 63. For `i64::MIN` (the
literal magnitude `9223372036854775808`), the lexer
already rejects it as out-of-range for an i64
literal, so the case `negated: i64 + v == 2^63`
never reaches the check.

For unsigned `u64`, the magnitude can be up to
`i64::MAX = 2^63 - 1` from the lexer; values from
`2^63` to `2^64 - 1` would need lexer support for
larger magnitudes (future). For now, `let x: u64 =
18446744073709551615u64;` would over-flow the lexer's
i64 parse and reject there instead of here. Not
ideal but consistent.

## The wire-ups

```
src/checker.rs    (check_numeric_lit_in_range helper;
                   call from check_expr_inner's Lit arm
                   and from check_unary's Neg branch
                   when wrapping a Lit. Inner Lit's
                   span gets typed in expr_types so
                   codegen reads the right
                   cranelift_type.)

tests/typecheck.rs  (+6 tests: u8 overflow, i8
                     positive overflow, i8 min
                     accepted, i8 one-past-min
                     rejected, unsigned negation
                     rejected, in-range boundary
                     literals accepted.)
```

No AST / parser / lower / mono / codegen changes —
overflow check is a checker diagnostic.

## What's tested

Typecheck (+6):

- `suffix_overflow_u8_rejected` — `1000u8`.
- `suffix_overflow_i8_positive_rejected` — `200i8`.
- `suffix_overflow_negative_signed_min_accepted` —
  `-128i8` is valid (boundary).
- `suffix_overflow_negative_one_past_min_rejected` —
  `-129i8`.
- `suffix_overflow_negative_unsigned_rejected` —
  `-5u8` (any negation on unsigned).
- `suffix_overflow_in_range_accepted` — `255u8`
  and `127i8` boundary values.

## Apparent bugs that aren't / explicitly deferred

- **u64 literals from `2^63` to `2^64 - 1`** still
  can't be expressed — the lexer parses magnitudes
  as `i64`, so the upper half of u64's range
  isn't reachable as a literal. Lifting that needs
  parsing into i128 / explicit string-based
  arithmetic. v0.x rarely needs the upper half;
  programs that do can construct via runtime
  arithmetic.
- **Hex / binary / octal suffix overflow** — the
  radix-prefixed path (`0xff_u8`, `0b1010_u8`) goes
  through the same Lit constructor and gets checked.
  `0xff_u8` (= 255) is valid; `0x1ff_u8` (= 511)
  would be rejected.
- **Float overflow** — `1e500f32` parses as `f64
  ::INFINITY` in the lexer's `f64::parse`. The
  suffix would then claim f32, but no overflow check
  fires for floats here. Float precision is
  inherently lossy and 1e500 is a "valid f32
  approximation" by IEEE-754 rules (infinity). Not
  worth a separate diagnostic.
- **Integer-to-float zero coercion** (session 091)
  still works — `let pi: f64 = 0;` uses the hint
  path which precedes the suffix check (the literal
  has no suffix in that case).
- **Range patterns with suffixed bounds** — `0u8..
  =255u8` works; bounds past the type's range would
  be rejected by this check at lit time, before the
  range pattern's bound-equality test. No new
  pattern-level work needed.
- **Compound expressions** — `let x: u8 = 100u8 +
  200u8;` doesn't error here (both operands are in
  range individually; the runtime sum overflows but
  that's not a suffix issue). Compound-expression
  overflow checking is a separate session
  (compile-time const evaluation).

## What's next

- **Binary-op hint flow** — `a: i32; a + 1` lets the
  `1` adopt i32 from the LHS.
- **Per-arm unreachability in tuple matches** —
  session 089's deferred item.
- **Polished diagnostics pass** — many error
  messages still reference internal sym indices
  (`struct#83`) instead of friendly names.
- **Self-hosted bootstrap** — long-term.
