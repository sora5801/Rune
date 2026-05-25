# Session 086 — Method-call-position `.into()` inference

**Date:** 2026-05-25
**Outcome:** `.into()` now picks the right `Into<T>`
impl based on surrounding context (let-binding,
fn-arg, struct-field). Closes session 072's
last-registered-wins bug for bare `.into()` calls. 398
codegen tests green (+4 from session 085).

```rune
impl std::Into<AppErr> for IoErr { fn into(self) -> AppErr { ... } }
impl std::Into<DbErr>  for IoErr { fn into(self) -> DbErr  { ... } }

let a: AppErr = e.into();         // picks Into<AppErr>
let d: DbErr  = e.into();         // picks Into<DbErr>

fn use_db(d: DbErr) -> i64 { ... }
use_db(e.into());                  // fn-arg picks Into<DbErr>

struct Holder { err: AppErr }
Holder { err: e.into() };          // struct-field picks Into<AppErr>
```

## The decisive observation

Session 072 wired `?`-site Into disambiguation through
`try_conversions: HashMap<Span, SymbolId>` — the checker
picks the matching fn at type-check, and the lowerer
reads the table to emit a direct `Call` instead of the
default `impl_methods` lookup (which last-wins'd among
multiple Into impls).

Bare `.into()` had no such hint plumbing. The
`impl_methods[(source, "into")]` entry pointed at the
last-registered Into impl, and the user couldn't pick a
different target without restructuring.

Three places needed a hint:

1. **`let x: T = expr.into();`** — the let's
   annotation provides T. Already calls
   `check_expr_with_hint(value, declared)` (session 062).
2. **`f(expr.into())`** where `f(t: T)` — session 081's
   `check_method_args_bidirectional` already passes
   each arg's expected type via
   `check_expr_with_hint(arg, hint)`.
3. **`Struct { field: expr.into() }`** — session 062's
   struct-lit pass 2 calls
   `check_expr_with_hint(value, hint_for_closure)`.

All three converge on `check_expr_with_hint`. Add a
single intercept in that function: when the expr is an
`Expr::MethodCall` named `into` with no args AND
expected is Some, walk the receiver type's `into_impls`
list and pick the matching target.

### The intercept

```rust
if let (Expr::MethodCall { receiver, method, args, span }, Some(exp)) = (e, expected) {
    if method.name == "into" && args.is_empty() {
        if let Some(ty) = self.try_into_disambiguation(receiver, *span, exp) {
            self.expr_types.insert(*span, ty.clone());
            return ty;
        }
    }
}
```

`try_into_disambiguation` walks `res.into_impls[source_sym]`,
resolves each impl's target AST, and picks the first
match against the expected type. The chosen fn sym
goes into `into_conversions[span]` — mirror of
`try_conversions`.

### The lowerer side

`lower_expr`'s MethodCall arm intercepts before any
other dispatch:

```rust
if method.name == "into" && args.is_empty() {
    if let Some(&fn_sym) = self.check.into_conversions.get(span) {
        return HirExprKind::Call {
            callee: fn_sym,
            args: vec![receiver_hir],
        };
    }
}
```

When `into_conversions` has the span, emit a direct
`Call` to the chosen fn. Otherwise fall through to the
default `impl_methods` lookup (still fine for
single-impl cases).

### Struct-lit pass-1 deferral

Struct-lit pass 1 was checking non-closure fields with
bare `check_expr`. Extended it to also defer `.into()`
method-calls to pass 2, where the (possibly subst'd)
field type flows in as the hint:

```rust
let is_into_method = matches!(
    &init.value,
    Expr::MethodCall { method, args, .. }
        if method.name == "into" && args.is_empty()
);
if matches!(init.value, Expr::Closure { .. }) || is_into_method {
    // defer
    ...
}
```

## The wire-ups

```
src/checker.rs    (new into_conversions field on
                   CheckResults + Checker; intercept in
                   check_expr_with_hint; struct-lit
                   pass-1 defers .into() like closures.)

src/lower.rs      (MethodCall lowering reads
                   into_conversions and emits direct
                   Call to the chosen fn sym.)

tests/codegen.rs  (+4 tests: let-binding picks AppErr,
                   let-binding picks DbErr, fn-arg
                   picks DbErr, struct-field picks
                   AppErr.)
```

No resolver or monomorphize changes — into_impls
(session 072) already records all per-impl targets;
mono sees a direct Call after lowering.

## What's tested

Codegen (+4):

- `into_disambiguation_let_binding` — `let a: AppErr =
  e.into()` picks Into<AppErr> from two competing
  impls.
- `into_disambiguation_picks_other_target` — same
  structure but the let's annotation says DbErr;
  picks Into<DbErr>.
- `into_disambiguation_fn_arg` — `use_db(e.into())`
  where use_db takes DbErr; fn-arg hint picks
  Into<DbErr>.
- `into_disambiguation_struct_field` — `Holder { err:
  e.into() }` where Holder.err is AppErr; struct-field
  hint picks Into<AppErr>.

## Apparent bugs that aren't / explicitly deferred

- **Multiple impls with the same target type** — if
  `IoErr` had two `impl Into<AppErr>` blocks (one
  registered twice somehow), `try_into_disambiguation`
  picks the first match in the candidates list. The
  resolver currently doesn't reject duplicate Into
  impls with identical targets — should be a separate
  diagnostic, deferred.
- **`.into()` on a tuple / enum receiver** —
  `into_impls` is keyed by `SymbolId`, so it only
  fires for `Ty::Struct(s, _)` and `Ty::Enum(s, _)`.
  Tuple receivers (`(1, 2).into()`) would need a
  per-tuple-shape impl mechanism — same restriction
  as the rest of v0.x's "impl only on structs".
- **No-hint `.into()`** — `e.into();` as a statement
  has no surrounding type expectation. The intercept
  doesn't fire (expected is None), and the lowerer
  falls through to the last-wins behavior. Same as
  pre-086; users add a let binding to disambiguate.
- **`.into()` chained as part of a larger expression**
  — `f(e.into() + 1)` won't disambiguate because the
  `+ 1` is between the .into() and the fn-arg hint.
  The intercept needs the parent to BE the
  hint-providing context. Workaround: bind the .into()
  to a typed let first.
- **Multi-impl Into with generic targets** — `impl
  Into<Box<T>> for X` where T is a generic param.
  `compatible(resolved, expected)` would need
  TypeVar-aware unification; current implementation
  uses bare structural compatibility. v0.x's Into
  impls don't have generic targets in practice.

## What's next

- **Intrinsic Numeric impls for primitives**.
- **Cartesian-product exhaustiveness for tuple
  patterns**.
- **Numeric literal suffixes** — `10i32`, `3.14f32`.
- **Self-hosted bootstrap** — long-term.
