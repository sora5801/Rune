# Session 114 — Shift compound operators `<<=` `>>=`

**Date:** 2026-05-25
**Outcome:** Rune now parses `a <<= 4` and `a >>= 2`
as shift-left / shift-right compound assignments,
mirroring `+= -= *= /= %=`. Session 112's compound-
assign const-eval gates fire for free, so `a <<= 64`
(LHS i64) and `a <<= n` (where `n` const-tracks to a
negative or over-range amount) error at typecheck.
439 codegen + 219 typecheck tests green (+3 codegen,
+4 typecheck from session 113).

```rune
fn main() -> i64 {
    let mut a: i32 = 1i32;
    a <<= 4;        // 16
    a >>= 1;        // 8
    let mut b: i32 = 1i32;
    b <<= 32;       // ← error: left shift amount `32` is out of
                    //   range for `i32` (must be 0..32)
    a as i64
}
```

## The decisive observation

Two lexer characters, two parser entries, one shared
const-eval gate. The lexer already disambiguates
`<` / `<=` / `<<` via lookahead at `peek()`; extending
to `<<=` is the same pattern one level deeper —
after seeing `<<`, peek again for `=`. The parser's
infix table already maps PlusEq / MinusEq / etc. to
`InfixKind::AssignOp(BinOp::Add)` / `Sub` / ... at
precedence (2,1); ShlEq / ShrEq slot in identically
with `BinOp::Shl` / `Shr`.

```rust
// lexer.rs
'<' => match self.peek() {
    Some('<') => {
        self.bump();
        if self.peek() == Some('=') { self.bump(); ShlEq } else { Shl }
    }
    ...
}

// parser.rs
TokenKind::ShlEq => InfixOp { kind: InfixKind::AssignOp(BinOp::Shl), bp: (2, 1) },
TokenKind::ShrEq => InfixOp { kind: InfixKind::AssignOp(BinOp::Shr), bp: (2, 1) },
```

The HIR / lowerer / codegen path needed no changes —
`HirExprKind::AssignOp { op, ... }` is generic over
the BinOp variant, and `compile_binop_value` already
handles `Shl` / `Shr` (the same paths used for the
regular `<<` / `>>` operators).

### Hint flow for the RHS

Session 112's check_assign_op did `rt = check_expr(rhs)`
— bottom-up, no hint. That works for `+=` between
matching-typed operands but breaks for shifts where
the user idiomatically writes `a <<= 4` with `a:i32`
and `4` as a bare i64-defaulting integer literal. The
existing compatibility check `lt.compatible(&rt)`
then errors "mismatched operand types: `i32` vs
`i64`."

Fix: use `check_expr_with_hint(rhs, Some(&lt))` to
hint the RHS from LHS. Mirrors session 095's
bidirectional binop flow. Now `a <<= 4` with `a:i32`
adopts i32 for the `4`, and the shift-amount check
runs against `IntTy::I32`'s bit width (32).

### Shift-amount diagnostic

The check in check_assign_op parallels session 110's
finish_binary check exactly — same gate (`b < 0 ||
b >= bits`), same diagnostic phrasing ("`left shift
amount \`32\` is out of range for \`i32\` (must be
0..32)`"). Cross-let const-eval flows in: `let n =
-1i32; a <<= n` catches the negative shift through
the binding tracker from session 106.

### Operator precedence

`<<=` and `>>=` land at precedence (2,1) —
right-associative, same as the other compound
assigns. Standard semantics: `a <<= b + 1` parses as
`a <<= (b + 1)`. Test coverage confirms chain like
`x <<= 5; x >>= 1;` works as separate statements.

## The wire-ups

```
src/token.rs    (+2 TokenKind variants: ShlEq, ShrEq)

src/lexer.rs    (+2 lookahead branches after `<<` and
                 `>>` for the `=` suffix)

src/parser.rs   (+2 InfixOp entries in the infix table)

src/checker.rs  (check_assign_op: rhs goes through
                 check_expr_with_hint(Some(&lt)) for the
                 type-hint flow; new shift-out-of-range
                 gate mirrors session 110's finish_binary
                 gate)

tests/codegen.rs  (+3: <<=, >>=, chained shifts)

tests/typecheck.rs (+4: out-of-range positive amount,
                    out-of-range >>=, negative amount
                    via tracked binding, positive control)
```

No HIR / lowerer / monomorphizer / runtime changes —
the existing HirExprKind::AssignOp generic handling
absorbs the new variants automatically.

## What's tested

Codegen (+3 from session 113's 436):

- `compound_shift_left_assign` — `let mut x = 1;
  x <<= 4;` → 16.
- `compound_shift_right_assign` — `let mut x = 16;
  x >>= 2;` → 4.
- `compound_shift_chain` — multiple shifts in
  sequence on i32 with cast.

Typecheck (+4 from session 113's 215):

- `shl_eq_out_of_range_rejected` — `a <<= 32` for
  i32 errors.
- `shr_eq_out_of_range_rejected` — `a >>= 64` for
  i64 errors.
- `shl_eq_negative_amount_rejected` — `let n = -1;
  a <<= n;` catches the tracked-negative amount.
- `shl_eq_in_range_accepted` — `a <<= 4; a >>= 1;`
  positive control.

## Apparent bugs that aren't / explicitly deferred

- **Bit-op compounds (`&= |= ^=`)**. Not in this
  session's scope — the lexer doesn't tokenize
  them yet (`&=` would conflict with `&&` lookahead;
  `|=` with `||`; `^=` is fresh). Mechanical
  extension once the lookahead pattern is in place.
- **Mismatched-width shift amount via path**. `let
  n: i64 = 4; a <<= n` (a:i32) errors at the
  compatibility check ("mismatched operand types")
  because `n` is i64 and a is i32. The hint flow
  only fires for *bare literals* (`numeric_lit_
  hint`'s suffix-skip gate); typed paths get
  bottom-up checking. Idiomatic Rune: write `a
  <<= n as i32` or use a same-typed shift count.
  Future sessions could relax shift-amount typing
  to "any integer," but that's a semantic shift
  with cross-cutting implications.
- **Runtime-trap shifts.** Without a const-tracked
  amount, `a <<= n` where `n` happens to be out
  of range at runtime gets the IEEE-undefined
  result Cranelift emits (`ushr`/`sshr` may saturate
  or wrap, hardware-dependent). v0.x only catches
  the const-eval'able case.

## What's next

- **Bit-op compound operators (`&= |= ^=`)** —
  lexer + parser extension. The session-112 gate
  shape extends to bitwise too (no overflow,
  but a stylistic completeness item).
- **Self-hosted bootstrap** — long-term.
