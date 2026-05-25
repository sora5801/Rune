# Session 095 — Binary-op hint flow

**Date:** 2026-05-25
**Outcome:** Binary operators with one concrete-numeric
operand and one bare numeric literal now adopt the
concrete type for the literal. `let a: i32 = 5; a + 1`
works without a suffix or cast. 422 codegen tests green
(+5 from session 094).

```rune
let a: i32 = 5;
let r = a + 1;          // r: i32 (1 adopts i32)
let s = 1 + a;          // s: i32 (1 adopts i32 via symmetric retry)
let t = a + -3;         // negative bare literal too
let f: f32 = 1.5;
let g = f * 4.0;        // g: f32 (4.0 adopts f32)
```

## The decisive observation

Session 091 wired bare-literal hint flow through
`check_expr_with_hint`. Three sites already pass hints
into it: let-binding annotations, fn-arg / method-arg
positions, and struct-lit fields. The remaining
high-friction site was binary ops — `a + 1` where `a:
i32` errored because the RHS `1` defaulted to i64
under bare bottom-up checking.

The fix has two parts, both inside `check_binary`:

### 1. RHS gets LHS as a hint (forward direction)

```rust
let lt = self.check_expr(lhs);
let rt = self.check_expr_with_hint(rhs, Some(&lt));
```

For `a + 1` with `a: i32`: lt = i32, the hint is
`Some(&Ty::Int(I32))`, `numeric_lit_hint` returns
`Some(Ty::Int(I32))`, the literal's
`expr_types[span]` gets i32. The cascade through
codegen reads i32. Same shape as session 091's
let-binding flow.

When LHS isn't concrete numeric (e.g., `str + str`),
the hint is non-numeric, `numeric_lit_hint` returns
None, and the RHS goes through normal bottom-up.
Same for TypeVar / Error LHS. Hint is always safe to
pass.

### 2. LHS retry when literal-on-LHS (symmetric direction)

`1 + a` checks LHS first (`1: i64`), then RHS with
LHS as hint (`a: i32` checks fine, hint doesn't apply
to a non-literal). The types now mismatch (i64 vs
i32), but the LHS is a bare numeric literal. Retry:
re-check LHS with RHS as hint.

```rust
let lt = if !lt.compatible(&rt)
    && lhs_is_bare_numeric_literal(lhs)
    && matches!(rt, Ty::Int(_) | Ty::Float(_))
{
    self.check_expr_with_hint(lhs, Some(&rt))
} else {
    lt
};
```

The retry overwrites the LHS's earlier `expr_types`
entry; codegen reads the corrected type. Only fires
when:
- The two types are incompatible, AND
- LHS is a bare numeric literal (or unary-neg of one
  — covers `-3 + a: i32`), AND
- RHS is a concrete numeric type

No retry for the unary-neg-on-RHS case because
session 091's intercept handles negative literals
through `check_expr_with_hint` directly.

### Suffix-bearing literals stay pinned

`a + 7i32` where `a: i32`: the suffix already pinned
`7i32`'s type at lex time; `numeric_lit_hint` returns
None when the literal carries a suffix (session
088's contract). Hint is ignored, the suffix-type is
used. Same on the symmetric side — a suffix-bearing
LHS doesn't pass the bare-literal filter and the
retry doesn't fire.

## The wire-ups

```
src/checker.rs    (check_binary's RHS check goes
                   through check_expr_with_hint with
                   LHS as hint; literal-LHS retry
                   added after the compatibility
                   check fails. New
                   lhs_is_bare_numeric_literal
                   free helper.)

tests/codegen.rs  (+5 tests: RHS literal, LHS
                   literal, negative literal, float
                   operands, suffix-overrides-hint
                   sanity.)
```

No AST / parser / lower / mono / codegen changes —
once `expr_types[lit_span]` carries the corrected
type, the lowerer's existing flow handles the rest.

## What's tested

Codegen (+5):

- `binop_hint_rhs_literal` — `let a: i32 = 5; a + 1`
  forward-direction hint.
- `binop_hint_lhs_literal` — `let a: i32 = 5; 1 + a`
  symmetric retry hint.
- `binop_hint_negative_literal` — `a + -3` covers
  the unary-neg-on-bare-literal branch.
- `binop_hint_float` — `1.5f32 * 4.0` float operands.
- `binop_hint_suffix_wins` — `a + 7i32` confirms
  suffix-bearing literals still pin their type
  (suffix-overrides-hint contract preserved).

## Apparent bugs that aren't / explicitly deferred

- **Both sides are literals** — `1 + 2` has no
  concrete numeric to hint from; both default to
  i64. Same as before this session. The
  let-binding hint (session 091) closes the gap
  when the binop's result has a typed destination.
- **Comparison ops** — `a < 5` where `a: i32`. The
  comparison's RHS is also a numeric position; this
  session's hint flow applies. The result type is
  bool regardless, but the operand types still
  need to match for the icmp to lower correctly.
  Comparison ops were already hinted via this
  session's general check_binary edit.
- **Mixed integer / float** — `let a: f32 = 1.5; a +
  3` would benefit from the int-zero-as-float rule
  (session 091) IF the literal is `0` — but `3`
  doesn't trigger the int→float coercion. Same
  shape as session 091's "non-zero int-to-float"
  limitation. Workaround: write `3.0` or
  `3.0f32`.
- **Chained mismatches** — `let a: i32 = 5; a + 1 +
  2`. The outer `(a+1) + 2`: inner is i32, outer
  RHS `2` adopts i32. Works.
  `1 + 2 + a` parses left-associatively as `(1 +
  2) + a` — the inner `1 + 2` doesn't know about
  `a` yet, so both default to i64; the outer
  binop then mismatches i64 + i32 and triggers the
  retry, but the retry only re-checks the LHS of
  the outer (which is the Binary node, not a
  literal) so it can't propagate inward.
  Workaround: parenthesize to bring the typed
  operand into the inner binop.
- **Assignment ops (`+=`, `-=`)** — `let mut a: i32
  = 5; a += 1` follows `check_assign_op`, which is
  a different path. Not yet hinted; users add
  suffixes or use `a = a + 1`.

## What's next

- **Const-eval overflow checks** — reject `100u8 +
  200u8` runtime overflow at compile time.
- **Codegen-side diagnostic polish** — friendly type
  names in codegen / aot error paths.
- **Method-receiver hint flow** — `42.method()`
  defaults 42 to i64 even when a hint is available.
- **Self-hosted bootstrap** — long-term.
