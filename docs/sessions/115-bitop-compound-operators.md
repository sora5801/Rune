# Session 115 — Bit-op compound operators `&=` `|=` `^=`

**Date:** 2026-05-25
**Outcome:** Rune now parses `a &= mask`, `a |= flag`,
and `a ^= toggle` as bitwise compound assignments,
completing the suite of compound operators (`+= -=
*= /= %= <<= >>= &= |= ^=`). The checker rejects
bit-op compounds on non-integer LHS with a clear
diagnostic; integer LHS dispatches through the same
HirExprKind::AssignOp / compile_binop_value pipeline
as the existing operators. 443 codegen + 221
typecheck tests green (+4 codegen, +2 typecheck
from session 114).

```rune
fn main() -> i64 {
    let mut flags: u8 = 0xFFu8;
    flags &= 0xF0u8;     // mask off low nibble
    flags |= 0x0Au8;     // set bits
    flags ^= 0x33u8;     // toggle bits
    flags as i64
}
```

## The decisive observation

Three more compound assignment operators, same
infrastructure as session 114's shifts. The lexer's
challenge was disambiguating `&=` from `&&` (and
`|=` from `||`); the solution is just adding a
second `Some('=')` branch to the existing `&` / `|`
lookahead match. `^=` had no lookahead at all
previously (caret was always tokenized as Caret);
extending it to a match was the same shape one
level out.

```rust
// lexer.rs
'&' => match self.peek() {
    Some('&') => { self.bump(); AmpAmp }
    Some('=') => { self.bump(); AmpEq }   // ← new
    _ => Amp,
},
'^' => match self.peek() {
    Some('=') => { self.bump(); CaretEq } // ← new
    _ => Caret,
},
```

Parser additions are three rows in the infix table
mapping to `InfixKind::AssignOp(BinOp::BitAnd)` /
`BitOr` / `BitXor` at precedence (2,1) — same as
the other compound assigns.

### Checker: bit-op operands must be integers

The existing `check_assign_op` had a "numeric required"
gate for `Add | Sub | Mul | Div | Mod` (rejecting str
and other non-numeric LHS). Bitwise ops are stricter
— they need *integer* operands specifically. f64
isn't valid for `a &= b` even though it's numeric.
Added a parallel "integer required" gate:

```rust
let needs_integer = matches!(
    op,
    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
        | BinOp::Shl | BinOp::Shr
);
if needs_integer && !lt.is_integer() {
    error "compound assignment '...=' requires integer operands, got '{ty}'";
}
```

Mirrors `binop_result_ty`'s check for the same ops
on the regular binop path. Shifts get added to the
gate retroactively — they already errored via lt /
rt compat in the test we wrote, but the explicit
gate gives a cleaner diagnostic.

### Codegen reuse

`HirExprKind::AssignOp { op, ... }` is generic over
all BinOp variants. `compile_binop_value` already
handles `BitAnd`, `BitOr`, `BitXor` for the regular
binop path. The compound form goes through the exact
same code, so no codegen change needed.

### Bool operand

`bool` operands are allowed for non-compound bit ops
(`true & false` typechecks in the existing code).
For compound assignment we conservatively *don't*
allow bool — `let mut b: bool = true; b &= false`
would have an obscure semantic ("did the user mean
`b = b && false`? bitwise AND on booleans?"). Idiomatic
Rune is `b = b && false` for the short-circuit
semantic; if someone genuinely wants `b &= flag`
bitwise on booleans they can cast through u8. The
`needs_integer` gate excludes Bool, surfacing a
clean rejection at typecheck.

## The wire-ups

```
src/token.rs    (+3 TokenKind variants: AmpEq, PipeEq, CaretEq)

src/lexer.rs    (+1 branch each in `&`, `|`, `^` match)

src/parser.rs   (+3 InfixOp entries in the infix table)

src/checker.rs  (check_assign_op: new `needs_integer`
                 gate rejecting non-integer LHS for the
                 five bitwise compound ops, mirroring
                 binop_result_ty's check.)

tests/codegen.rs   (+4: &=, |=, ^=, all three chained)

tests/typecheck.rs  (+2: positive control on u8,
                     float LHS rejected.)
```

No HIR / lowerer / monomorphizer / runtime changes.
The generic AssignOp variant absorbs the three new
ops automatically.

## What's tested

Codegen (+4 from session 114's 439):

- `compound_bit_and_assign` — `15 &= 10 = 10`.
- `compound_bit_or_assign` — `10 |= 5 = 15`.
- `compound_bit_xor_assign` — `15 ^= 10 = 5`.
- `compound_bit_ops_chained` — three in sequence on
  i32 hex literals.

Typecheck (+2 from session 114's 219):

- `bit_ops_compound_assign_accepted` — u8 positive
  control with `&= |= ^=`.
- `bit_ops_compound_assign_on_float_rejected` —
  `a &= 1.0` for f64 LHS errors.

## Apparent bugs that aren't / explicitly deferred

- **Bool compound bitwise.** `let mut b: bool =
  true; b &= false` errors as "requires integer
  operands." Intentional — idiomatic Rune is
  `b = b && false`. The non-compound `&` allows
  bool because it has clear bitwise semantics in
  expression context; the compound form would just
  be confusing.
- **Const-eval overflow not applicable.** Bitwise
  ops can't overflow — `a & b` for any a, b in
  type T stays in T. The compound-assign const-
  eval block in check_assign_op handles div-by-
  zero and shift-out-of-range; bit ops just pass
  through.
- **Right associativity.** `a &= b |= c` parses
  as `a &= (b |= c)` — same as other compound
  assigns. `(b |= c)` evaluates b's new value
  but produces Unit (the AssignOp expr's type),
  which then can't be `&=`-assigned. Errors at
  typecheck. v0.x doesn't special-case compound
  chaining.
- **No `&&=` / `||=`.** Short-circuit compound
  assignment. Some languages have them (JavaScript
  ES2021); Rune doesn't. Niche feature; deferred
  indefinitely.

## What's next

- **Shift-amount-type relaxation** — allow `let n:
  i64 = 4; a <<= n` for `a:i32` without explicit
  cast. The hint flow only fires for bare literals
  today.
- **Self-hosted bootstrap** — long-term.
