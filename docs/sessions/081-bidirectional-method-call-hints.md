# Session 081 — Bidirectional method-call hints

**Date:** 2026-05-24
**Outcome:** Unannotated closures work at method-call
position. `v.iter().fold(0, |acc, x| acc + x)`,
`v.iter().map(|x| x * x).sum()`, and chained
`.filter(|x| ...).map(|x| ...).fold(0, |a, x| ...)` all
type-check without explicit `: i64` annotations. 372
codegen tests green (+4 from session 080).

```rune
v.iter().fold(0, |acc, x| acc + x)                  // 10
v.iter().map(|x| x * x).sum()                       // 14
v.iter().filter(|x| x > 2).count()                  // 3
v.iter()
    .filter(|x| x > 1)
    .map(|x| x * 10)
    .fold(0, |acc, x| acc + x)                       // 140
```

## The decisive observation

Session 062 wired bidirectional inference for closures in
struct-literal field positions:

```rune
let m = std::Map { iter: v.iter(), f: |x| x * mult };
// F's `Fn1<I::Item, U>` bound supplies x: i64.
```

That hint flow went through `check_struct_lit`'s
`expand_callable_typevar` — given a TypeVar field type
with a callable bound, synthesize a `Ty::Fn { params,
ret }` from the bound's arg types.

The same flow at *method-call* positions was the
missing piece — session 080's `.fold` documentation
called it out explicitly:

> `check_method_call` checks args without contextual
> hints; closure params get fresh inference TypeVars
> with no body-side concretes to pin them.

Three pieces to bridge:

### 1. `expand_callable_typevar` works for method-level generics too

The struct-side variant translates from struct-side sym
→ impl-side sym via `impl_to_struct_generic` (because
the bound info is keyed on the impl-side param). Method-
level generics have no such translation — the
`TypeVar(F_sym)` at the call site *is* the bound's keyed
sym. Fallback to using the sym directly:

```rust
let lookup_sym = self
    .res
    .impl_to_struct_generic
    .iter()
    .find(|&(_, &s)| s == *generic_sym)
    .map(|(&i, _)| i)
    .unwrap_or(*generic_sym);  // session 081: method-level
```

### 2. New `check_method_args_bidirectional` helper

For each arg position, compute the expected type under
the running substitution, build a callable hint if it's
a TypeVar with a Fn-bound, and call
`check_expr_with_hint`. Then unify the result back into
subst so later args see the latest pins.

For `.fold(init, f)`:
- arg 0 (init): expected = U. Not callable-bounded.
  Check with no hint → 0:i64. Subst pins U=i64.
- arg 1 (f): expected = F. Callable-bounded by Fn2<U,
  Self::Item, U>. expand_callable_typevar applies subst
  (U=i64, Self=VecIter<i64>) → hint = Ty::Fn { [i64,
  i64], i64 }. Closure params bind from the hint.

Returns `None` when the method isn't found via
impl_methods (builtin, dyn, etc.) — caller falls back
to the existing bottom-up check loop.

### 3. Cascade re-run inside the loop

When an arg pins a method-level generic (e.g., closure
arg pins F to a closure struct), the cascade walks F's
bound to pin further generics. The same cascade lives
in `user_method_sig_with_args`; here we run it inside
the per-arg loop so the *next* arg sees the latest
pins. Three passes max (matches the existing cascade).

## The wire-ups

```
src/checker.rs    (expand_callable_typevar's struct-side
                   lookup gains a method-level fallback;
                   new check_method_args_bidirectional
                   helper; check_method_call calls it
                   before the existing sig-resolution
                   chain.)

tests/codegen.rs  (+4 unannotated-closure tests covering
                   .fold, .map, .filter, and a full
                   .filter().map().fold() chain.)
```

## What's tested

Codegen (+4):

- `iterator_fold_unannotated_closure` — `v.iter().fold(0,
  |acc, x| acc + x)`. The closure's params get types
  from F's Fn2<U, Self::Item, U> bound; init pins U=i64
  first so the hint is fully concrete at the closure
  check.
- `iterator_map_unannotated_closure` — `v.iter().map(|x|
  x * x).sum()`. Fn1<Self::Item, U> bound supplies
  x:i64 from VecIter<i64>::Item; U remains an inference
  TypeVar that the body's binop pins.
- `iterator_filter_unannotated_closure` — `v.iter()
  .filter(|x| x > 2).count()`. P's Fn1<Self::Item,
  bool> supplies both x's type and the closure's
  expected return.
- `iterator_chain_all_unannotated` — full chain with
  three unannotated closures. The bidirectional flow
  fires three times, each closure binding its params
  from the corresponding F/P bound.

## Apparent bugs that aren't / explicitly deferred

- **Fallback path still runs for non-Ty::Struct
  receivers**. When the bidirectional helper returns
  None (builtin or dyn method), the bottom-up check
  loop runs as before. So closures at non-method-call
  positions still need annotations or surrounding
  context — `let f = |x| x + 1;` standalone still
  errors. That's session 062-style territory: the
  let-binding hint flow handles it when there's an
  explicit `let f: fn(i64) -> i64 = ...`.
- **Mixed call shapes don't compound**. `let m = std::
  Map { iter: v.iter(), f: |x| x * 2 }; m.fold(0,
  |acc, x| acc + x)` works because both sites
  independently get hints (struct-lit + method-call).
  No new infrastructure needed here.
- **`expand_callable_typevar` returns a hint for the
  arg, not a tighter constraint**. If the closure's
  body return type doesn't match the bound's R
  position (e.g., `|x: i64| true` passed where Fn1<i64,
  i64> is expected), check_assignable catches it at
  the assignment-against-param step. The hint guides
  param binding, not return validation.
- **The cascade runs three passes** (capped to bound
  the loop). All current shipped methods need at most
  two passes (one to pin from arg, one to propagate
  bound). Reserved capacity for future Fn3+ or
  chained-bound scenarios.
- **Hint synthesis ignores Self::Item when the impl
  binding isn't known yet**. For trait-method calls on
  generic receivers (`fn frob<T: Iterator>(it: T) { it
  .fold(0, |a, x| a + x) }`), Self::Item resolves to
  `Ty::Assoc(TypeVar(T), "Item")` — still opaque. The
  closure's `x` param binds to that projection; binop
  `a + x` works through session 079's Ty::Assoc-
  opaque pass-through. After mono pins T, the
  specialized body's projection resolves concretely.

## What's next

- **Numeric trait bounds** — generalizes `.sum() /
  .min() / .max() / .fold(init, +)` beyond i64.
- **Str-keyed HashMap iteration** — `.keys() /
  .entries()` on `HashMap<str, V>`.
- **Match-arm tuple patterns**.
- **Method-call-position `Into` inference** — let / fn-
  arg / struct-field hints for `.into()`.
- **Self-hosted bootstrap** — long-term.
