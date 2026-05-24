# Session 077 — Method-level generics in trait methods

**Date:** 2026-05-24
**Outcome:** Trait method declarations can carry their own
generic params (with bounds). `.filter(p)` lands as the first
default-body method using this, with the closure / named-fn
predicate driven by a method-level `P: Fn1<Self::Item, bool>`.
612 tests green (+2 from session 076).

```rune
fn gt2(x: i64) -> bool { x > 2 }

// Named fn:
v.iter().filter(gt2).count()                  // 3

// Closure:
v.iter().filter(|x| x > 2).count()            // 3
```

## The decisive observation

Two pieces of plumbing made this work, plus one informative
limitation:

1. **Parser + AST + resolver**: trait methods now accept
   `<F: Fn1<X, Y>, U>` between the name and the param list.
   The resolver scopes these method-level generics on top of
   Self + the trait's own generics, mirroring `resolve_fn`'s
   pattern. Each gets its bounds resolved into
   `generic_bounds` (and `generic_bound_args`, session 061).
2. **Method-call type inference**: `user_method_sig` now also
   unifies the method's param types against the call-site arg
   types — so a closure or fn-pointer passed as `p` pins the
   method's `P` generic, which propagates into the return
   type (`Filter<Self, P>`). Pre-077 only Self was unified;
   method-level generics stayed as TypeVars and leaked
   through the chain.
3. **U-only-in-bound generics don't propagate** without
   explicit bound-args walking. `.map(f)` would declare
   `<F: Fn1<Self::Item, U>, U>` — F gets pinned by the arg
   but U sits in the bound, not the params. I added a
   bound-propagation hook in `user_method_sig_with_args`
   (walks each pinned method-generic, reads its Fn1 bound's
   args via `generic_bound_args`, unifies positionally with
   the concrete F's call signature). The hook fires for
   simple cases but the cascade doesn't reach all the way
   to `.map(...).sum()`-shaped chains yet. v0.x users
   continue to construct `std::Map { ... }` via struct lit;
   `.map(f)` as a method is deferred.

## The wire-ups

```
src/ast.rs        (TraitMethodSig.generics: Vec<GenericParam>.)

src/parser.rs     (parse_trait reads optional generic params
                   between the method name and `(`.)

src/resolver.rs   (Trait pass-2 body resolution interns each
                   method-level generic via
                   `intern_generic_param`, resolves bounds and
                   their args into `generic_bounds` /
                   `generic_bound_args`. Same pattern
                   resolve_fn uses for fn-level generics.)

src/lower.rs      (lower_trait_default appends method-level
                   generic syms to the synth fn's `generics`
                   list AFTER Self + trait generics — order
                   matters for call-site type-arg inference.)

src/checker.rs    (user_method_sig_with_args replaces
                   user_method_sig; takes the call-site
                   arg_tys, unifies them against the method's
                   params after the Self-unify, propagates
                   from Fn1 bounds when a method-level
                   generic gets pinned to Ty::Fn or
                   Ty::Struct(closure_sym).)
```

## What's tested

Codegen (+2):

- `iterator_filter_as_method_with_named_fn` —
  `v.iter().filter(gt2).count()` = 3 (`gt2` is a named
  `fn(i64) -> bool`).
- `iterator_filter_as_method_with_closure` —
  `v.iter().filter(|x| x > 2).count()` = 3 (closure
  predicate; method-level P inferred + closure-param
  inference via session 062 kicks in because the closure's
  expected type carries an `Fn1<i64, bool>` bound).

## Apparent bugs that aren't / explicitly deferred

- **`.map(f)` deferred.** The closure / fn-pointer's
  return type U sits only in F's `Fn1<Self::Item, U>` bound,
  not in the param list. The bound-propagation hook reads
  F's signature and tries to pin U, but the cascade through
  the rest of the inference doesn't always fire — chains
  like `.map(f).sum()` leave U as TypeVar at codegen. Users
  keep using `std::Map { iter, f }` struct lits.
- **`.fold(init, f)` similarly stalled** — both the init
  type and the closure's accumulator type need to be pinned
  through bound propagation that's not yet robust.
- **Closure-param inference for method-level F bounds**
  works on the simple `.filter(|x| x > 2)` case but isn't
  systematically tested across all the inference paths.
  Session 062's logic walks `generic_bound_args` for struct
  fields; the same machinery sees method-level generics
  through the new resolver wiring.
- **The "call to undeclared function" failure for `.map`**
  was traced to a TypeVar U leaking through to a Map<I, F,
  TypeVar(U)> at the call site, then to count's spec, then
  to filter_next's spec where self.pred has type TypeVar.
  resolve_method_calls in mono can't dispatch a `.call` on
  a TypeVar receiver, codegen emits a stub call to an
  unmaterialized fn sym → undeclared.
- **`user_method_sig` (the old single-arg version) was
  reabsorbed** into `user_method_sig_with_args` with an
  empty arg list for callers that don't have args at hand.
  Today only check_method_call calls it; future callers
  would need to provide arg_tys for accurate inference.

## What's next

- **Bound propagation cascade** — make the U-only-in-bound
  case land. `.map(f)` then unblocks; same shape for
  `.fold`.
- **Numeric trait bounds** — generalize `.sum()` /
  `.min()` / `.max()` beyond i64.
- **Str-keyed HashMap iteration**.
- **Match-arm tuple patterns**.
- **Self-hosted bootstrap** — long-term.
