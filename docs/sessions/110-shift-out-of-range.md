# Session 110 — Shift-out-of-range diagnostic

**Date:** 2026-05-25
**Outcome:** `x << b` and `x >> b` with `b` const-
evaluable error at typecheck when `b < 0` or `b >=
bit_width(x's type)`. `1i32 << 32`, `200u8 >> 8`,
and `1 << amt` (where `amt = 100`) all surface
diagnostics. Closes the integer-misuse loop started
by session 102's overflow check and session 107's
divide-by-zero. 436 codegen + 196 typecheck tests
green (+6 typecheck from session 109).

```rune
fn main() -> i64 {
    let x: i32 = 1i32 << 32;   // ← error: left shift amount `32`
                               //   is out of range for `i32`
                               //   (must be 0..32)
    x as i64
}
```

## The decisive observation

Session 107's divide-by-zero diagnostic was the
shape this needed: one new check in the const-eval
block, fires when the rhs const-evals out of range,
specific diagnostic text. The shift case needs an
extra detail — the *bit width* depends on the LHS
type (i.e., the binop's result type), so the bound
is `0..bit_width(result_ty)` not a constant `0..64`.

```rust
if matches!(op, BinOp::Shl | BinOp::Shr) {
    if let Some(b) = self.const_eval_int(rhs) {
        let bits = int_bit_width(*result_ty);
        if b < 0 || b >= bits as i64 {
            self.error(span,
                format!("{} shift amount `{}` is out of range for `{}` \
                         (must be 0..{})",
                        direction, b, result_ty.name(), bits));
        }
    }
}
```

One new `int_bit_width` free helper (8/16/32/64 by
`IntTy`). Two call sites (finish_binary and
check_binary, parallel to session 107). Cross-let
const-eval (session 106) flows in for free —
`let amt = 100; x << amt` looks up `amt`'s recorded
value and catches it.

### Surfaced a latent issue in the const-eval block

Adding a positive control (`let a: i64 = 1 << 63;
let b: i32 = 1i32 << 31;`) caught a bug that pre-
dated this session: `1i32 << 31` in the const-eval
block computes `1.checked_shl(31)` in i64 = 2147483648.
Then `check_int_value_in_range(2147483648, I32, ...)`
sees `v > i32::MAX = 2147483647` and errors —
falsely, because `1i32 << 31` is exactly `i32::MIN`
in i32 form, a legitimate bit-shift result.

The fix is to skip the post-eval range check for
shifts. Their result is an intentional bit pattern
that may exceed the type's positive range while
still fitting the type. The new session-110
out-of-range check above is the *only* shift
diagnostic; arithmetic overflow doesn't apply.

```rust
if let Some(v) = result {
    let skip_range = matches!(op, BinOp::Shl | BinOp::Shr);
    if !skip_range {
        // existing range check
    }
}
```

Bit-and/or/xor are *not* skipped — those produce
results that naturally fit the type when operands
fit (e.g., `100u8 | 200u8 = 236` fits u8), so the
range check catches any genuine misuse.

### Why bit width is per-type, not always 64

The runtime semantics of `x << b` follow Cranelift's
`ishl` instruction, which is parameterized by the
LHS type's bit width. `1i8 << 8` is UB (8 >= 8);
`1i64 << 64` is UB (64 >= 64). The diagnostic's
upper bound reads from `int_bit_width(result_ty)`
so it adapts: i8 → 8, i32 → 32, i64 → 64, etc.

## The wire-ups

```
src/checker.rs    (+1 shift-range check block in
                    finish_binary + check_binary
                    (mirrors session 107's two-
                    site pattern).
                   +1 free helper `int_bit_width`.
                   Range check skipped for shifts in
                    the existing eval block —
                    surfaced as a fix for the
                    `1i32 << 31` positive control.)

tests/typecheck.rs  (+6 new tests: at-bit-width,
                     above-bit-width left and right,
                     negative amount, through-binding,
                     and positive control covering
                     i64/i32/u8 shifts.)
```

No lower / codegen / runtime changes. Checker-only
diagnostic.

## What's tested

Typecheck (+6 from session 109's 190):

- `shift_left_at_bit_width_rejected` — `1 << 64`
  errors (i64 bit width is 64).
- `shift_left_above_bit_width_rejected` — `1i32
  << 32` errors with bit width 32.
- `shift_right_above_bit_width_rejected` — `200u8
  >> 8` errors with "right shift amount" naming.
- `shift_negative_amount_rejected` — `let n: i64
  = -1; 1 << n` catches the negative amount via
  cross-let const-eval.
- `shift_through_const_tracked_binding_rejected` —
  `let amt: i64 = 100; 1 << amt` errors at the
  shift site even though the amount is named.
- `shift_inside_bit_width_accepted` — positive
  control: `1 << 63`, `1i32 << 31`, `1u8 << 7` all
  compile cleanly.

## Apparent bugs that aren't / explicitly deferred

- **Runtime shift amount.** When the rhs isn't
  const-evaluable (a function call, a method
  receiver), the runtime semantics still apply —
  Cranelift's ishl/ushr on x86-64 mask the shift
  amount to the low bits of the operand width
  (Intel's `shl` masks rhs to 5 or 6 bits
  depending on operand size). The diagnostic
  catches only what we can know at compile time.
- **`u32` and `u64` shifts on isize/usize**. We
  use the abstract bit width — 64 for isize/usize
  on every Rune target today. A future 32-bit
  target would need to update `int_bit_width`.
- **`1u8 << 7u32`.** The shift amount type is
  always treated as "some integer" by the checker;
  we don't enforce a specific type for the rhs.
  The const-eval check operates on the numeric
  value regardless of operand type.
- **Arithmetic overflow combined with shifts.** A
  chain `(1u8 << 7) + 200u8` evaluates the inner
  shift to 128 (in i64 form), then the outer Add
  to 328 → overflow detected. Skipping the range
  check for the inner shift was necessary to make
  this case work without false positives.

## What's next

- **Float const-eval through binops** — parallel
  to session 102/106 for floats. Checked f64
  arithmetic + session 108's range check on the
  result.
- **Subnormal-as-zero diagnostic** — `1e-50f32`
  silently rounds to subnormal/zero. Would need
  warning infrastructure (currently checker only
  has errors).
- **Self-hosted bootstrap** — long-term.
