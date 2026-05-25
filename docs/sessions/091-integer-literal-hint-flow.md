# Session 091 — Integer literal hint flow

**Date:** 2026-05-25
**Outcome:** Bare numeric literals adopt the
surrounding context's expected type when one is
provided — `let a: i32 = 10;`, `f(10)` with `f(x:
i32)`, `Struct { n: 10 }` with `n: i32`. No `as i32`
cast or `10i32` suffix needed. Closes the polish-pass
deferred from session 088. 417 codegen tests green
(+6 from session 090).

```rune
let a: i32 = 10;                       // i32, not i64
let f: f32 = 3.14;                     // f32, not f64
let n: i32 = -10;                      // negative literal too

fn use_i32(x: i32) { ... }
use_i32(42);                            // arg picks i32

struct Holder { n: i32 }
Holder { n: 42 };                      // field picks i32

let zero: f64 = 0;                     // integer-shaped zero
                                        //   accepted as float
```

## The decisive observation

A literal's type is committed in exactly one place
(`lit_type`); session 088 made that respect a suffix
when present. The missing piece for session 091 is
that bare literals (no suffix) should also be able to
inherit a contextual hint instead of locking in the
i64/f64 default.

Three plumbing sites already converge on
`check_expr_with_hint`:

- **let-binding annotations** (session 062 wired it).
- **method-call args** (session 081 wired it).
- **struct-lit fields, pass 2** (session 062).

So extending `check_expr_with_hint` itself with a
literal-aware branch propagates to all three contexts
automatically. Two more sites needed targeted patches:

- **`check_call` args** were checked with bare
  `check_expr`. Change to `check_expr_with_hint` with
  the callee's param type when callee is a known
  `Ty::Fn`.
- **`check_struct_lit` pass 1** was checking
  non-closure / non-`.into()` field values with bare
  `check_expr`. Pass the declared field type as a
  hint — `numeric_lit_hint` returns None for
  TypeVar hints, so the generic-field case still
  falls through to the bottom-up i64 default that
  pins generic inference.

### The intercept

```rust
if let (Expr::Lit { lit, span }, Some(exp)) = (e, expected) {
    if let Some(ty) = self.numeric_lit_hint(lit, exp) {
        self.expr_types.insert(*span, ty.clone());
        return ty;
    }
}

fn numeric_lit_hint(&self, lit: &Lit, expected: &Ty) -> Option<Ty> {
    match (lit, expected) {
        // Suffix wins.
        (Lit::Int(_, Some(_)), _) | (Lit::Float(_, Some(_)), _) => None,
        (Lit::Int(_, None), Ty::Int(ty)) => Some(Ty::Int(*ty)),
        // Integer `0` accepted as float (idiomatic zero).
        (Lit::Int(0, None), Ty::Float(ty)) => Some(Ty::Float(*ty)),
        (Lit::Float(_, None), Ty::Float(ty)) => Some(Ty::Float(*ty)),
        _ => None,
    }
}
```

### Unary `-N`

Negative literals (`-10`) are `Expr::Unary { op: Neg,
expr: Lit(10) }` at the AST level, not a single Lit
node. The intercept gets a parallel arm: if the
unary expr is a literal, run the same hint logic on
the inner Lit. Both the outer unary's span AND the
inner Lit's span are inserted into `expr_types` so
codegen reads the right `cranelift_type` for both
nodes (a missing inner-span entry caused the literal
to default-back to i64 in the lowerer; the Cranelift
backend then panicked on a "declared type i32, got
value i64" mismatch).

### Suffix-wins is preserved

`numeric_lit_hint` returns `None` whenever the
literal already has an explicit suffix. So `let x:
i32 = 10i64;` correctly errors (suffix-i64 vs
annotation-i32) instead of silently picking the hint
over the source-level suffix. Session 088's "suffix
wins" contract holds.

### Integer zero as float

The `(Lit::Int(0, None), Ty::Float(ty))` arm
accommodates `let pi: f64 = 0;` and `Default { val:
0.0 }` — both write `0` for the additive identity,
even when the surrounding type is a float. Only the
literal value 0 gets this coercion; `let pi: f64 =
3;` still errors (the user almost certainly meant
3.0, and silent truncation/promotion would mask
typos).

## The wire-ups

```
src/checker.rs    (check_expr_with_hint gains the
                   Lit and Unary-Neg-on-Lit branches;
                   numeric_lit_hint helper; check_call
                   args use the hint; struct-lit
                   pass 1 hints with the declared
                   field type.)

tests/codegen.rs  (+6 tests: let-binding hint,
                   fn-arg hint, struct-field hint,
                   float hint, negative-literal hint,
                   suffix-overrides-hint sanity.)
```

No AST / parser / lower / mono / codegen changes —
once `expr_types[lit_span]` carries the right typed
Ty, the lowerer's existing `HirLit::Int(v, int_ty)`
path emits the correct codegen.

## What's tested

Codegen (+6):

- `integer_literal_hint_let_binding` — `let a: i32 =
  10;` arithmetic.
- `integer_literal_hint_fn_arg` — `add_i32(5, 7)`
  with fn-arg-typed i32 params.
- `integer_literal_hint_struct_field` — `Holder { n:
  42 }` with `n: i32`.
- `integer_literal_hint_float` — `let pi: f32 =
  3.14;` float hint.
- `integer_literal_hint_negative` — `let a: i32 =
  -10;` covers the unary-Neg branch.
- `integer_literal_suffix_overrides_hint` — `let a:
  i64 = 10i64;` sanity: suffix-bearing literals
  bypass the hint path.

## Apparent bugs that aren't / explicitly deferred

- **Binary-op hint flow** — `let a: i32 = 5; a + 1`
  still errors: the `1` is bare-hint-less in the
  binop, defaults to i64, mismatches i32. To handle
  this generally would need binop-side type
  propagation (when one operand has a concrete
  numeric type, hint the other). Future polish.
- **Match-arm bodies** — `match x { y => 1 }` where
  the match has expected return type T doesn't yet
  hint the arm bodies. The match's return type is
  computed bottom-up from arms; adding hint flow
  there would require threading the outer expected
  through.
- **Non-zero integer-to-float coercion** still
  errors — `let pi: f64 = 3;` (without `.0`) does
  not auto-coerce. Intentional: silent
  int→float promotion would hide typos. Users
  write `3.0` or `3 as f64`.
- **i64 hint when the literal is already i64** — a
  no-op; same shape goes through the bottom-up
  default. The intercept fires regardless, sets
  expr_types again with the same Ty, returns. Cheap
  and correct.
- **Generic-struct-field literal** —
  `MyVec<T> { contents: [10] }` with T-typed
  contents: the hint is `Ty::TypeVar(T)`,
  `numeric_lit_hint` returns None, the literal
  defaults to i64, and unify pins T=i64 (same as
  pre-091 behavior). Preserves session 056's generic
  inference.
- **Range bounds** — `let r = 0..10;` — both bounds
  are literals, both defaulted to i64. No hint flow
  in range parsing yet; range bounds for i32 etc.
  still need suffixes.

## What's next

- **Binary-op hint flow** — `a: i32; a + 1` lets the
  `1` adopt i32 from the LHS.
- **Suffix overflow checks** — reject `1000u8`.
- **Per-arm unreachability in tuple matches** —
  session 089's deferred item.
- **Self-hosted bootstrap** — long-term.
