# Session 103 — Chained binop hint propagation

**Date:** 2026-05-25
**Outcome:** Binary-op chains with a typed destination
now propagate the destination type inward to every
operand. `let r: i32 = 1 + 2 + a;` works without
parens. Closes the deferred limitation from session
095. 428 codegen + 167 typecheck tests green (+4
codegen from session 102).

```rune
let a: i32 = 5;
let r1: i32 = 1 + 2 + a;       // works
let r2: i32 = a + 1 + 2;       // works
let r3: i32 = 1 + 2 + 3 + 4;   // works (all-literal chain)
let r4: i32 = 1 * 2 + 3 - a;   // mixed ops
```

## The decisive observation

Session 095 added bidirectional binop hint flow at
ONE level: a binop with a typed operand hints the
other operand. But chains parse left-associatively:
`1 + 2 + a` is `(1 + 2) + a`. The outer binop checks
its LHS first, which is the inner binop. The inner
binop has no hint at that point — both `1` and `2`
default to i64. Then the outer binop sees lhs=i64
and rhs=i32, mismatch, error.

The fix flips the direction: when an outer typed
context (a let-binding, fn-arg, struct-field with
expected numeric type) wraps the chain, propagate
that expected type INWARD to every binop in the
chain. Each binop's operands get the same hint;
literals adopt it; sub-binops recurse via the same
intercept; paths and other typed-already exprs are
unaffected.

### Where to intercept

`check_expr_with_hint` is the universal hint entry
point — sessions 091/094/095/096/099 all added match-
arms there. Session 103 adds one more: when `expected
is Some(Ty::Int(_) | Ty::Float(_))` and `e is
Expr::Binary`, pre-hint BOTH sides with the expected
type, then run a shared "finish binop" path.

```rust
if let (Expr::Binary { op, lhs, rhs, span },
        Some(exp @ (Ty::Int(_) | Ty::Float(_)))) = (e, expected) {
    let lt = self.check_expr_with_hint(lhs, Some(exp));
    let rt = self.check_expr_with_hint(rhs, Some(exp));
    return self.finish_binary(*op, lhs, rhs, lt, rt, *span);
}
```

The recursive `check_expr_with_hint` on the inner
binop fires this same intercept, hinting THAT binop's
operands too. Chains propagate naturally; the
recursion terminates at literals / paths / casts
where the hint either pins (literals) or is silently
ignored (already-typed exprs).

### `finish_binary` refactor

The existing `check_binary` did three things:
1. Check both operands (with hint flow from session
   095)
2. Validate operand compatibility
3. Apply the op-specific result-type rule (numeric
   for +/-/*/% , bool for comparison, etc.)

To share (2) and (3) with the new hint path, I
extracted them into two helpers:

- `finish_binary(op, lhs, rhs, lt, rt, span) -> Ty`
  — given pre-checked operand types, validate
  compatibility, run const-eval overflow (session
  102), dispatch to `binop_result_ty`.
- `binop_result_ty(op, t, span) -> Ty` — the
  op-specific rules: + concatenates Str / requires
  numeric / etc. Pure function of (op, unified type).

`check_binary` still owns operand-checking-with-
session-095's-hint-flow (LHS first, RHS with LHS as
hint, optional LHS retry if literal-on-LHS). After
both sides are checked, it calls `finish_binary` with
their types. Session 103's intercept also calls
`finish_binary` after pre-hinting both sides with the
outer expected type.

### Why this doesn't double-up

A binop with a numeric expected goes through session
103's intercept and runs `finish_binary` directly —
`check_binary` doesn't run. A binop without an
expected goes through session 095's path. The two
paths converge on `finish_binary` for the validation
and result-type computation.

The const-eval overflow check (session 102) now
lives in `finish_binary`, fired from both paths.

## The wire-ups

```
src/checker.rs    (check_expr_with_hint gains a
                   Binary-with-numeric-hint arm;
                   new finish_binary and
                   binop_result_ty helpers extracted
                   from check_binary; check_binary
                   itself trimmed to call these.)

tests/codegen.rs  (+4 tests: literal+literal+var
                   chain, var+literal+literal chain,
                   all-literal chain, mixed-op
                   chain.)
```

No AST / parser / resolver / lower / mono / codegen
changes — pure checker refactor + hint-flow
extension.

## What's tested

Codegen (+4):

- `binop_hint_chain_literal_then_var` — `1 + 2 + a:
  i32` (the canonical case).
- `binop_hint_chain_var_then_literals` — `a + 1 + 2`
  (symmetric).
- `binop_hint_chain_all_literals` — `1 + 2 + 3 + 4`
  with no typed-var anchor; outer let's i32 still
  propagates.
- `binop_hint_chain_mixed_ops` — `1 * 2 + 3 - a`
  with mul + add + sub. Result: 1.

## Apparent bugs that aren't / explicitly deferred

- **Mismatched-op-types in a chain** — if the outer
  binop expects f32 but the chain has integer
  literals via a path, propagation produces an
  error. Same shape as the single-level hint flow;
  no regression.
- **Comparison ops at the top** — `let b: bool =
  1 + a < 5;` — the outer expected is bool, so the
  intercept doesn't fire (bool isn't Int/Float).
  The inner `1 + a < 5` falls through to
  `check_expr` which calls `check_binary`, which
  uses session 095's per-level hint flow. Works
  for the common case (`a + 1` adopts a's type)
  but not for chains-inside-comparison.
- **Assignment-op chains** — `a += b + 1` where
  `b: i32`. The compound-assign path
  (`check_assign_op`) doesn't yet route through
  the hint flow. Future session.
- **Float chains** — `1.0 + 2.0 + f: f32` works
  identically; the intercept fires on Ty::Float
  too.
- **The session 095 LHS-literal retry** still
  lives in `check_binary` and only fires when
  there's no outer hint. With an outer hint,
  session 103's intercept takes precedence and
  the retry isn't needed.
- **Bool / str chains** — `+` for str concat and
  `&&`/`||` for bool both work; the intercept
  only fires for Int/Float hints, so non-numeric
  chains fall through to `check_binary` /
  `binop_result_ty` unchanged.

## What's next

- **Floating-point Vec elements** — unblock
  numeric workloads on f64.
- **Cross-let const-eval** — propagate const
  values through let bindings.
- **Division-by-zero const-eval diagnostic**.
- **Self-hosted bootstrap** — long-term.
