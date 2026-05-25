# Session 116 — Shift-amount type relaxation

**Date:** 2026-05-25
**Outcome:** Shift operators (`<<`, `>>`, `<<=`,
`>>=`) accept any integer type as the count,
regardless of the LHS's type. `(a: i32) << (n: i64)`
works without an explicit `as i32` cast. Closes the
deferral from sessions 110 / 114. 446 codegen + 223
typecheck tests green (+3 codegen, +2 typecheck from
session 115).

```rune
fn main() -> i64 {
    let a: i32 = 1i32;
    let n: i64 = 4;
    let r: i32 = a << n;   // ← no cast needed
    r as i64               // 16
}
```

## The decisive observation

The previous behavior required `lt.compatible(&rt)`
for every binop — i32 against i64 was rejected.
For shifts this is overly strict: the *result type*
comes from the LHS (the value being shifted), and
the *amount* doesn't need to match. C, Rust, Zig
all allow the amount to be any integer (Rust
requires explicit `u32` or `as`-cast for type-
inference reasons, but the underlying mental model
is "amount is just a count"). Rune adopts the
same loose semantic.

The fix is a per-op gate, both in `check_binary`
(plain `a << n`) and `check_assign_op` (`a <<= n`):

```rust
let is_shift = matches!(op, BinOp::Shl | BinOp::Shr);
let compat_ok = if is_shift {
    lt.is_integer() && rt.is_integer()
} else {
    lt.compatible(&rt)
};
if !compat_ok { error "mismatched types"; }
```

Both sides must still be *integers* — `i32 << f64`
still errors. Only the equality requirement drops.

### Codegen coercion

Cranelift's `ishl(l, r)` / `sshr(l, r)` / `ushr(l,
r)` require operands to have the same Cranelift
type. The checker now lets through `(I32, I64)`
operand pairs, so codegen needs to coerce `r` to
`l`'s width:

```rust
let l_ty = self.builder.func.dfg.value_type(l);
let r_ty = self.builder.func.dfg.value_type(r);
let r = if l_ty == r_ty {
    r
} else if l_ty.bits() < r_ty.bits() {
    self.builder.ins().ireduce(l_ty, r)     // narrow
} else {
    self.builder.ins().uextend(l_ty, r)     // widen
};
```

Shift count is taken mod bit-width at the hardware
level anyway, so truncating high bits via `ireduce`
is correct (a shift count of 35 against an i32 LHS
modulos to 3 on x86-64 — the upper bits we drop
don't matter). Widening uses `uextend` because the
checker's session 110 / 114 / 115 gates already
reject negative shift counts that const-eval.

### One unified arm at codegen

The previous `HirBinOp::Shl => ishl(l, r)` and
`HirBinOp::Shr => sshr/ushr(l, r)` arms were
collapsed into a single `HirBinOp::Shl | HirBinOp::
Shr => { ... }` block that computes the coerced `r`
once, then dispatches on the variant. Avoids
duplicating the coercion logic.

### Bool not affected

Bool isn't an integer in `is_integer()`, so `(b:
bool) << n` still errors. Same policy as session
115's bit-op compounds.

## The wire-ups

```
src/checker.rs    (Two compat-check sites get the
                   per-shift gate: finish_binary
                   (session 103) and check_binary
                   (legacy). check_assign_op gets
                   the same gate for the compound
                   forms.)

src/codegen.rs    (compile_binop_value's Shl/Shr arm
                   coerces `r` to `l`'s Cranelift
                   type via ireduce / uextend before
                   emitting the shift instruction.)

tests/codegen.rs   (+3: mixed-width shift, mixed-
                    width compound shift, narrow
                    amount widened to wider LHS)

tests/typecheck.rs  (+2: positive control accepts
                     mixed-width int, float amount
                     still rejected)
```

No lowerer / monomorphizer / runtime changes — HIR
preserves both operand HirExprs with their types,
codegen does the narrow/widen.

## What's tested

Codegen (+3 from session 115's 443):

- `shift_mixed_width_amount` — `(a: i32) << (n:
  i64)` returns the right i32 value.
- `shift_compound_mixed_width` — `(a: i32) <<= (n:
  i64)` works (same coercion path).
- `shift_narrow_amount_widens` — reverse case: `(a:
  i64) << (n: u8)` widens n to i64.

Typecheck (+2 from session 115's 221):

- `shift_amount_any_int_type_accepted` — positive
  control, mixed widths compile.
- `shift_amount_float_still_rejected` — `(a: i32)
  << (n: f64)` errors. Relaxation is integer-only.

## Apparent bugs that aren't / explicitly deferred

- **Out-of-range count via path with `as` cast.**
  `let n: i64 = 64; a <<= (n as i32)` — the cast
  produces 64 (fits i32) and session 114's shift-
  out-of-range gate fires (b >= 32 for i32). Same
  diagnostic, just reached via a different path.
- **Negative count via cast.** `let n: i32 = -1;
  a <<= n` — session 110's gate catches it via the
  const-tracked binding. Without const tracking,
  the runtime behavior is hardware-defined
  (typically truncates mod bit-width).
- **`<` vs `<<` parser ambiguity.** No change —
  Rune doesn't have `<` as a generic-args delimiter
  in expression context, so `a << n` is
  unambiguously a left-shift. (`Vec<T>` is a *type*
  expression; the parser disambiguates by context.)
- **Bool count via `as`.** `let b: bool = true;
  a <<= (b as i32)` — the `as i32` produces 0 or
  1, which is in range. Compiles. Bool *itself*
  can't be a shift count (not an integer), but a
  cast-bool is just an integer.

## What's next

- **Self-hosted bootstrap** — long-term.
