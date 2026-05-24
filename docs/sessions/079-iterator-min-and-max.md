# Session 079 — Iterator `.min()` and `.max()`

**Date:** 2026-05-24
**Outcome:** Two more default-body methods land on
`Iterator` — `.min()` and `.max()`. i64-only (Self::Item =
i64); empty-iterator surface is `Option<i64>::None`,
non-empty surface is `Option::Some(best)`. Every Iterator
impl inherits both. 361 codegen tests green (+7 from
session 078).

```rune
v.iter().min()                              // Option<i64>
v.iter().max()                              // Option<i64>
(5..9).min()                                // Some(5)
v.iter().filter(|x| x > 1)
        .map(|x: i64| x * 10)
        .max()                              // chained — three
                                            // adapter specs of
                                            // the same default
                                            // body fire
```

## The decisive observation

Pure leverage of session 071's default-body machinery and
session 076's `.sum()` precedent — but a checker hole
surfaced on the very first test.

The min/max bodies look exactly like `.sum()`:

```rune
fn min(self: Self) -> Option<i64> {
    let mut best: Option<i64> = Option::None;
    while true {
        match self.next() {
            Option::Some(x) => {
                match best {
                    Option::Some(b) => {
                        if x < b { best = Option::Some(x); }
                    }
                    Option::None => { best = Option::Some(x); }
                }
            }
            Option::None => { break; }
        }
    }
    best
}
// .max is identical but `x > b`
```

`x` has type `Self::Item` — a `Ty::Assoc(TypeVar(self),
"Item")` at type-check time. Mono pins it to the concrete
impl's Item at spec time (Vec→i64, Range→i64, Map<…, _,
i64>→i64). But the comparison `x < b` is type-checked
*before* mono runs, so the checker sees the unresolved
projection.

`check_binary`'s `BinOp::Lt | Gt | Le | Ge` arm previously
rejected non-numeric / non-char operands — surfaced as
"operator `<` requires ordered operands, got `T#50::Item`"
on every single codegen test (354 → 0) because the prelude
gets type-checked alongside the user program.

## The fix

Mirror how `compatible()` already treats `Ty::Assoc` /
`Ty::TypeVar` — accept opaquely at typecheck, let mono /
codegen sort it out.

```rust
BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
    // Session 079: Ty::Assoc / Ty::TypeVar are opaque
    // at typecheck (a `T::Item` won't resolve until
    // mono pins the impl's binding); accept them as
    // ordered-or-fail-later. Same shape `compatible`
    // already uses for type-equality.
    let opaque = matches!(t, Ty::Assoc(_, _) | Ty::TypeVar(_));
    if !opaque && !t.is_numeric() && !matches!(t, Ty::Char) {
        self.error(span, format!(
            "operator `{}` requires ordered operands, got `{}`",
            binop_symbol(op), t.display()
        ));
        return Ty::Error;
    }
    Ty::Bool
}
```

Why this is safe: after spec, the body's `Self::Item`
resolves to a concrete type. If the impl's Item isn't
i64-compatible, the spec'd body has `Lt`/`Gt` over a
concrete non-numeric type — same arm fires the error, just
with a concrete type name in the diagnostic. If the impl's
Item *is* i64, the icmp emits normally.

So the "ordered-or-fail-later" deferral is honest — type
errors that would have fired at typecheck still fire at
spec-time, just one phase later.

## The wire-ups

```
src/checker.rs    (check_binary's Lt/Gt/Le/Ge arm: opaque
                   pass-through for Ty::Assoc and
                   Ty::TypeVar)

src/std.rn        (.min() and .max() added to Iterator's
                   default-method block. Both return
                   Option<i64>; both use nested match for
                   the Option<i64> threading on `best`.)

tests/codegen.rs  (+7 tests: min/max default on Vec, min
                   on Range, max on Range, max through
                   chain, min via Map adapter, min on
                   empty returns None.)
```

Three impls (VecIter, RangeIter, Map, Filter) gain the
.min and .max default-body fills via session 071's
declare_impl arm — no manual per-impl wiring.

## What's tested

Codegen (+7):

- `iterator_min_default_method` — `v.iter().min()` on a
  4-element Vec returns Some(10) (the actual minimum).
- `iterator_max_default_method` — same shape, max = 40.
- `iterator_min_on_empty_returns_none` — exercises the
  `Option::None` sentinel path from the initial best.
- `iterator_min_on_range` — `(5..9).min()` returns
  Some(5). Tests RangeIter's inheritance.
- `iterator_max_on_range` — `(5..9).max()` returns
  Some(8). 5..9 is half-open, max is 8 not 9.
- `iterator_max_through_filter_and_map_chain` — full
  chain `v.iter().filter(p).map(f).max()` — three
  adapter specializations of the default body fire
  (VecIter, Filter, Map).
- `iterator_min_via_map_adapter` — `Map { iter, f }.min()`
  via struct-lit construction (mirror of the closure
  cases from session 076).

## Apparent bugs that aren't / explicitly deferred

- **Numeric trait bounds.** Only i64 today. Generalizing
  would need a `Numeric` trait (Add/Sub/PartialOrd-style),
  intrinsic impls for i8/i16/i32/i64/u*/f32/f64, and the
  `.sum`/`.min`/`.max` bodies bounded on `Self::Item:
  Numeric`. The default-body machinery already handles
  bounds on method-level generics (session 077) so the
  trait-bound side wouldn't be the blocker — the intrinsic
  primitive impls would. Deferred to a focused session.
- **`.min_by(cmp)` and `.max_by(cmp)`** — the closure-
  arg form would inherit session 077-078's bound-
  propagation cascade. Mechanical extension; would land
  alongside the numeric generalization since the
  unbounded form needs the comparator.
- **Negative-zero / NaN semantics for floats** — moot
  for i64. When `.min`/`.max` extends to f64, NaN comparisons
  need a policy (Rust's `partial_cmp` returns Option;
  `.min` could either skip NaNs or propagate them). Defer
  with the numeric trait work.
- **First-element-wins vs last-element-wins on equal
  values** — `.min()` returns the first occurrence (the
  `<` comparison rejects equal elements from overwriting
  best). Consistent with Rust's `min` (which uses `<=`
  to do last-wins) — but the choice is intentional here,
  so users can rely on stable first-occurrence semantics.

## What's next

- **Numeric trait + intrinsic primitive impls** — unlocks
  `.sum() / .min() / .max()` over arbitrary numeric
  Self::Item. Touches checker (numeric bound resolution),
  std.rn (Numeric trait + impls), monomorphize (numeric
  bound substitution).
- **`.fold(init, f)`** — still waits on multi-missing-
  generic inference (session 078 deferred).
- **Str-keyed HashMap iteration** — `.keys()` /
  `.entries()` on `HashMap<str, V>`.
- **Match-arm tuple patterns** — `match pair { (1, x) =>
  ..., _ => ... }`.
- **Self-hosted bootstrap** — long-term.
