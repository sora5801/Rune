# Session 032 — The `?` operator

**Date:** 2026-05-21
**Outcome:** `expr?` works — the error-propagation operator for
`Result`. The parser already accepted postfix `?`; this session
type-checks it and desugars it to a `match`. 433 tests green (+6 from
session 031's 427).

## The headline

```rune
fn chain(ok: bool) -> std::Result<i64, i64> {
    let v = parse(ok)?;          // Ok -> unwrap to v; Err -> return early
    std::Result::Ok(v + 1)
}
```

`parse(ok)?` evaluates `parse(ok)`; if it's `Ok(x)` the operator
yields `x`, if it's `Err(e)` the enclosing function immediately
`return`s `Err(e)`.

## What `?` desugars to

```rune
expr?
```
becomes
```rune
match expr {
    Result::Ok(v)  => v,
    Result::Err(e) => return Result::Err(e),
}
```

So `?` is pure sugar over constructs Rune already had — `match`,
payload-variant binding, `return`, enum construction. The resolver,
monomorphizer, and codegen needed no `?`-specific code.

## Checker

`check_try` (the `Expr::Try` arm):

- The operand must be a **`Result`-shaped enum** — `Ty::Enum(s, [T,
  E])` with two type args and an enum carrying `Ok` and `Err`
  variants. (Identifying it structurally rather than pinning
  `std::Result` means a user's own `Ok`/`Err` enum works too.)
- `expr?` has type `T` (the `Ok` payload).
- The **enclosing function must return a `Result`** with the same
  enum `s` and an error type matching `E` — otherwise the `return
  Err(e)` the desugar emits wouldn't type-check. The checker already
  tracks `current_return`, so this is a direct comparison.

Misuse gets a dedicated message: `the ?` operator requires a
`Result``, `... can only be used in a function returning a `Result``,
or `?` propagates an error of type `X`, but ... `Y``.

## Lowerer

`lower_try` builds the `HirExprKind::Match` directly. It reads the
operand's type (`Ty::Enum(rsym, [ok_ty, err_ty])`), looks up the
`Ok`/`Err` discriminants off the enum (no assumption about
declaration order), and assembles two arms:

- `Ok(v) => v` — an `EnumPayload` pattern binding `v`, body `Local(v)`.
- `Err(e) => return Err(e)` — body is a `Return` of an
  `EnumPayloadCtor` re-wrapping `e`.

The synthetic arm nodes get their `Ty`s assigned **directly** — they
have no source span, so they can't go through the span-keyed
`expr_types` table the way real expressions do. Building HIR directly
sidesteps any span-collision problem.

### Fresh binding symbols

The desugared `v` and `e` bindings need `SymbolId`s. The `Lowerer`
gained a `Cell<u32>` counter (`next_sym`) initialized past every
resolver symbol; `fresh_sym()` hands out collision-free ids. Codegen
treats locals purely through its per-function `var_map`, so a
lowerer-minted symbol works like any other.

## Two supporting fixes

`?` shook out two latent bugs:

1. **The monomorphizer's symbol scan was incomplete.**
   `walk_expr_collect_syms` only descended into a handful of expr
   kinds — not `Match`, `Return`, `Binary`, `If`, ... — so it computed
   too low a max symbol, and the monomorphizer's fresh specialization
   syms could collide. (Latent before `?`; the synthetic `?` binding
   syms made it matter.) Rewrote it to walk every expr kind and every
   match-arm pattern binding.

2. **`compile_match` rejected a diverging arm.** The `Err` arm body is
   a `return`. `Return` codegen emits the return, then switches into a
   fresh empty block (so any dead code after a `return` still has a
   block) — which reads as *not filled*. `compile_match` then expected
   the arm to yield a value and errored "match arm produced no value".
   Fix: an arm whose body type is `Ty::Never` is diverging — terminate
   that trailing unreachable block with a trap and skip the merge
   jump. This also fixes a plain `match x { 0 => return 5, _ => 10 }`.

## Pipeline

```
src/
├── checker.rs      (check_try — type-check expr?)
├── lower.rs        (lower_try — desugar to a match; Cell next_sym)
├── monomorphize.rs (walk_expr_collect_syms — now exhaustive)
└── codegen.rs      (compile_match — handle a diverging arm)
```

`ast.rs`, `parser.rs`, and `resolver.rs` were already done — `?` has
parsed since the parser sessions.

## What's tested

Codegen (+2):
- `try_operator_ok_and_err` — `?` unwraps an `Ok`, and propagates an
  `Err` by returning early.
- `try_chains_multiple` — three `?` in one function (each desugar
  allocates its own fresh symbols).

Typecheck (+4): `try_typechecks_ok`, `try_on_non_result_errors`,
`try_in_non_result_fn_errors`, `try_error_type_mismatch`.

## Apparent bugs that aren't

- **`?` requires exact error-type match.** `expr?` where `expr`'s
  error type differs from the function's is an error — there's no
  `From`-style error conversion (Rust's `?` coerces via `From`). v0.x
  keeps it exact; conversion needs a `From` trait first.

- **`if cond { Ok(x) } else { Err(y) }` doesn't type-check.** The
  checker's branch `unify` is equality-based, and `Result<i64, ?>`
  (from `Ok`) doesn't equal `Result<?, i64>` (from `Err`). Write the
  function with an early `return` for one branch instead. This is a
  pre-existing checker limitation, unrelated to `?` — but it shapes
  how `Result`-returning functions are written today.

- **`?` is sugar — errors point at the desugared shape in places.**
  The checker gives `?`-specific messages, but a downstream issue in
  the lowered `match` would be phrased as a match error. Acceptable
  for v0.x.

## What's next

- **`From`-based error conversion** for `?`, so an inner error can be
  widened to the function's error type.
- **Branch-type unification** for `Result` so `if { Ok } else { Err }`
  works without an early `return`.
- `dyn Trait` dynamic dispatch — the larger feature deferred from this
  session's pairing.
