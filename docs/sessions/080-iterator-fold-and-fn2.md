# Session 080 — `.fold(init, f)` + `Fn2` + arity-generic bound cascade

**Date:** 2026-05-24
**Outcome:** `.fold(init, f)` lands as a default-body
method on `Iterator`, alongside `.collect / .count /
.sum / .min / .max / .filter / .map`. New `Fn2<A, B,
R>` trait in the prelude; bound-propagation cascade
generalized so any-arity FnN bound works. 368 codegen
tests green (+7 from session 079).

```rune
v.iter().fold(0, |acc: i64, x: i64| acc + x)         // 10
v.iter().fold(0, add)                                 // named fn
(1..6).fold(0, |a: i64, x: i64| a + x)                // 15
v.iter().filter(|x| x > 1)
    .map(|x: i64| x * 10)
    .fold(0, |a: i64, x: i64| a + x)                  // 140
v.iter().fold(0, |a: i64, x: i64| a + x * scale)      // captures
```

## The decisive observation

Three pieces, all leverage from prior infrastructure.

### 1. Fn2 trait — new, mechanically derived

Mirrors `Fn1<A, R>`:

```rune
pub trait Fn2<A, B, R> {
    fn call(self: Self, a: A, b: B) -> R;
}
```

Capturing closures synthesize a struct + a `call(self,
a, b) -> r` method. Non-capturing closures and named
fns continue to flow as `Ty::Fn` values — at the
`f.call(acc, x)` site, mono's `resolve_method_calls`
rewrites:

- `Ty::Fn { params, ret }.call(...)` → `IndirectCall`
- `Ty::Struct(closure).call(...)` → direct `Call` to
  the synth call method

Both paths are arity-agnostic (the loop walks `args`),
so no codegen change.

### 2. Bound cascade — generalized for any arity

`user_method_sig_with_args`'s bound-propagation cascade
(session 077-078) had hardcoded `Fn1` shape:

```rust
// Old: Fn1 only.
if bound_arg_tys.len() == 2 && p.len() == 1 {
    unify_typevars(&bound_arg_tys[0], &p[0], &mut subst);
    unify_typevars(&bound_arg_tys[1], r, &mut subst);
}
```

Generalized to walk arbitrary arity:

```rust
// New: FnN for any N.
if bound_arg_tys.len() == p.len() + 1 {
    for (i, p_ty) in p.iter().enumerate() {
        unify_typevars(&bound_arg_tys[i], p_ty, &mut subst);
    }
    unify_typevars(&bound_arg_tys[p.len()], r, &mut subst);
}
```

Same generalization on the closure-struct branch
(`cp.len() == bound_arg_tys.len()` instead of `cp.len()
== 2 && bound_arg_tys.len() == 2`).

For `.fold<F: Fn2<U, Self::Item, U>, U>` the cascade
fires three positional unifications: bound[0]↔p[0],
bound[1]↔p[1], bound[2]↔r. With `p = [U, Self::Item]`
on the bound side and `[i64, i64]` on the closure side,
U gets pinned to i64 — same shape as Fn1's existing
pin, just with one more position.

### 3. `.fold` body — calls via `f.call(acc, x)`

The body must dispatch through the trait method, not
treat `f` as a direct callable:

```rune
fn fold<F: Fn2<U, Self::Item, U>, U>(self: Self, init: U, f: F) -> U {
    let mut acc: U = init;
    while true {
        match self.next() {
            Option::Some(x) => { acc = f.call(acc, x); }
            Option::None => { break; }
        }
    }
    acc
}
```

`f(acc, x)` would error: a generic `F: Fn2<...>` isn't
yet a callable value at typecheck. `f.call(acc, x)`
dispatches through the trait bound; after spec, mono
rewrites the MethodCall into either Call or
IndirectCall depending on F's concrete shape.

This matches how `Map::next` already calls
`self.f.call(x)` (session 056-061).

## "Multi-missing-generic inference"

Session 078 deferred this as a limitation. The fix
turned out to be subtle: for `.fold`, all three
generics (`Self`, `F`, `U`) pin directly via unify
because U appears in `init`'s parameter slot. No
fallback needed:

- `Self ← receiver.ty` (VecIter<i64>)
- `U ← init.ty` (i64 from literal `0`)
- `F ← f.ty` (Ty::Struct(closure_sym) or Ty::Fn)

What the session-078 doc called "multi-missing" was
really "the cascade only handles Fn1." The single-
missing fallback in mono is still there; multi-missing
remains uncalled because every Iterator default-body
method either pins all generics directly or has the
checker cascade pin them at typecheck (via
`apply_subst` to the method-call's result type).

A future `.fold_into<C: FromIter<Self::Item>>` (where C
appears only in the return type, not in any param)
would actually need mono's bound-walking fallback to
fire. Not added this session — no method demands it
yet.

## The wire-ups

```
src/checker.rs    (user_method_sig_with_args's bound-
                   propagation cascade arity-generalized
                   for both Ty::Fn and Ty::Struct
                   (closure) branches.)

src/std.rn        (Fn2<A, B, R> trait added below Fn1;
                   `.fold(init, f)` default method added
                   below `.map`. Both follow the existing
                   prelude patterns.)

tests/codegen.rs  (+7 tests: fold on Vec, fold with
                   named fn, fold with nonzero init, fold
                   on Range, fold through filter-map
                   chain, fold-multiplies, fold with
                   capturing closure.)
```

## What's tested

Codegen (+7):

- `iterator_fold_default_method` — `v.iter().fold(0,
  |acc: i64, x: i64| acc + x)` sums to 10.
- `iterator_fold_with_named_fn` — `v.iter().fold(0,
  add)` with `fn add(a: i64, b: i64) -> i64`. F binds
  to Ty::Fn, IndirectCall dispatch.
- `iterator_fold_init_nonzero` — init = 100; result
  = 106. Verifies init flows through correctly.
- `iterator_fold_on_range` — `(1..6).fold(0, |a, x|
  a + x)` = 15. RangeIter inherits .fold.
- `iterator_fold_via_filter_map_chain` — full chain
  `v.iter().filter(p).map(f).fold(0, g)` works. Three
  adapter specializations of the .fold default body
  fire (VecIter, Filter, Map).
- `iterator_fold_multiplies` — `acc * x` not `acc +
  x`. Verifies the closure body isn't hardcoded.
- `iterator_fold_with_capturing_closure` — closure
  captures `scale: i64` from outer scope; synthesizes
  Fn2-shaped closure struct. Cascade reads the call
  method's 3-arg signature and pins U from it.

## Apparent bugs that aren't / explicitly deferred

- **Unannotated closure params at method-call
  position**. `v.iter().fold(0, |acc, x| acc + x)`
  errors with "closure parameter needs a type
  annotation". The `check_method_call` site eagerly
  checks each arg with no contextual hint, so the
  closure's params get fresh inference TypeVars that
  never pin (the binop `acc + x` constraint is on
  two TypeVars rather than a TypeVar-vs-concrete). To
  fix would require hint flow at method-call sites
  (look up the method's sig before checking args,
  hint each closure arg from the corresponding param
  type). Annotated closures work; users add `|acc:
  i64, x: i64| ...`. Deferred — same wiring would
  also help `.map`, `.filter`.
- **Fn3, Fn4, ...** — no method in the prelude
  declares 3+ arg callable bounds yet. The cascade
  is arity-generic so Fn3 would work the moment we
  add the trait and a method using it.
- **Multi-missing-generic via mono fallback** — still
  deferred for the same reason as session 078: no
  shipped method demands it. A future method shape
  with N-1 generics in bounds and 1 in args would
  hit it. The cascade in the checker covers
  everything in v0.x.
- **`.fold` body's `U` type** — uses `let mut acc:
  U = init;`. Works because U is concrete at spec
  time (pinned from init's type). A future use where
  U remains TypeVar would surface "type T#NN not
  supported in codegen" — same surface as other
  unresolved-generic errors. Documented in std.rn's
  comment.

## What's next

- **Bidirectional hints at method-call sites** —
  unblocks unannotated closures in `.fold`, `.map`,
  `.filter` chains. Look up the method's sig before
  checking args; for each arg position with a
  callable-bounded TypeVar param, synthesize a
  `Ty::Fn` hint and pass it to `check_closure`.
- **Numeric trait bounds** — generalizes `.sum() /
  .min() / .max() / .fold(init, +)` beyond i64.
  `.fold` already accepts any U because U is
  monomorphic; what's missing is `.sum`'s `total +
  x` being able to use Self::Item as numeric.
- **Str-keyed HashMap iteration** — `.keys() /
  .entries()` on `HashMap<str, V>`.
- **Match-arm tuple patterns**.
- **Self-hosted bootstrap** — long-term.
