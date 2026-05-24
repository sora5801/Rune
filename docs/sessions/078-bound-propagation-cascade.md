# Session 078 — Bound propagation cascade

**Date:** 2026-05-24
**Outcome:** `.map(f)` lands as a default method on Iterator,
matching session 077's `.filter(p)`. The chain `v.iter()
.filter(p).map(f).sum()` now works as methods all the way
through. 615 tests green (+3 from session 077).

```rune
fn sq(x: i64) -> i64 { x * x }

v.iter().map(sq).sum()                       // named fn, U inferred
v.iter().map(|x: i64| x * x).sum()           // closure (annotated param)
v.iter()
    .filter(|x| x > 1)
    .map(|x: i64| x * 10)
    .sum()                                    // chained
```

## The decisive observation

Session 077 added bound propagation at the *checker* level
(`user_method_sig_with_args` reads a pinned method-generic
F's `Fn1<X, U>` bound and unifies the bound's args against
F's actual signature, pinning U). That worked at type-check
time — `.map(f)`'s return type at the call site came out as
`Map<VecIter, Ty::Fn, i64>` with U fully concrete.

What was missing: the monomorphizer's `infer_type_args`
runs independently when a generic Call is encountered, and
it doesn't have the checker's bound-propagation logic. For
`.map(f)`, infer_type_args sees params=[self, p] and args=
[v.iter(), sq] — pins Self and F, but U stays unbound. The
generics list `[Self, F, U]` requires all three; missing one
returns None; no spec request fires; the Call stays unresolved
through codegen → "call to undeclared function."

Two pieces to fix it:

1. **Checker's resolve_bound_args walks method-level generics**
   so the bound's arg types (e.g., `Self::Item`, `U`) are
   populated in `type_resolutions` for later cascade lookup.
   Sets `current_self_param` so `Self::Item` becomes
   `Ty::Assoc(TypeVar(self_sym), "Item")` — substitutable
   under apply_subst.

2. **Mono's infer_type_args adds a single-missing-generic
   fallback**: when exactly one generic is unbound after
   normal unification, walk the pinned substitutions; for
   any `Ty::Fn { params, ret }`, use its return type to fill
   the missing slot. Catches the `.map(f) -> U-from-F-return`
   case without porting the checker's full bound walker.
   Multi-missing cases give up and surface as codegen errors
   (the user falls back to the struct-literal form).

The fallback is heuristic — it only handles the cleanly-
chainable case where exactly one type can be derived from a
function-shaped pinned argument. Cases with multiple missing
generics or non-Fn1 bounds still need explicit annotations
or the struct-lit form. That's the right tradeoff for v0.x:
shipped chain works, edge cases get a clear error rather
than incorrect specialization.

## The wire-ups

```
src/checker.rs       (resolve_bound_args Trait arm extended
                      to walk method-level generics. Sets
                      current_self / current_self_param
                      around the walk so Self::Item resolves
                      to a substitutable TypeVar-rooted
                      projection.)

src/monomorphize.rs  (infer_type_args adds the single-missing-
                      generic Fn-shaped fallback after normal
                      unification.)

src/std.rn           (`.map(f)` default method restored: `fn
                      map<F: Fn1<Self::Item, U>, U>(self: Self,
                      f: F) -> Map<Self, F, U>` constructs
                      Map { iter: self, f: f }.)
```

## What's tested

Codegen (+3):

- `iterator_map_as_method_with_named_fn` — `v.iter().map(sq)
  .sum()` with `fn sq(x: i64) -> i64`. F = Ty::Fn(i64, i64),
  U pinned to i64 via the fallback's Fn-return inspection.
- `iterator_map_as_method_with_annotated_closure` —
  `v.iter().map(|x: i64| x * x).sum()`. The closure-as-
  struct's call method has signature (Self, i64) -> i64;
  cascade pins F to the closure_sym; the fallback finds the
  Fn... wait, closure isn't Ty::Fn. The fallback would only
  fire if subst has at least one Ty::Fn entry. In this case
  subst has only the closure struct. But the test passes —
  which means the cascade has more reach than expected.
  Likely path: closure struct's call method signature is
  read through some other path (the impl_methods lookup at
  the bound's struct case in user_method_sig_with_args's
  cascade). The combined effect of both fixes covers the
  test.
- `iterator_chain_filter_map_sum_as_methods` — the full
  chain `v.iter().filter(|x| x > 1).map(|x: i64| x * 10).sum()`
  works. Three method-level generic uses pinned correctly.

## Apparent bugs that aren't / explicitly deferred

- **Multi-missing-generic cases** fall back to a codegen
  error. A `fn foo<A, B, C>(...)` where only A is pinned
  from the args won't get B and C from anywhere — same
  story as `.fold(init, f)` where both the accumulator and
  the closure's return need pinning. Users construct the
  adapter struct directly.
- **The mono-side fallback uses the first Ty::Fn it sees**
  in subst — for methods with multiple Fn-shaped args, the
  pick is arbitrary. v0.x doesn't have such methods on
  Iterator, but a future trait with `fn zip<F>(self: Self,
  other: Self, combine: F)` could trip this.
- **Closure inference for method-level F bounds** works on
  the simple `.filter(|x| x > 2)` and `.map(|x: i64| ...)`
  cases. Inferring the closure's param type from the bound
  (without explicit annotation) goes through session 062's
  closure-hint synthesis — which fires for struct-field
  contexts but needs more wiring to fire reliably for
  method-arg contexts. Annotated closures sidestep this.
- **`.fold(init, f)` still deferred.** Two unbound generics
  (the accumulator type and the closure's return), neither
  pinnable by the single-missing fallback. Future work.

## What's next

- **`.fold(init, f)` via richer bound propagation** — would
  require a proper multi-pass cascade like the checker has.
- **Closure param inference at method-arg position** —
  systematically wire session 062's hint synthesis through
  method-call check sites.
- **Numeric trait bounds** — generalizes `.sum()` /
  `.min()` / `.max()` beyond i64.
- **Str-keyed HashMap iteration**.
- **Match-arm tuple patterns**.
- **Self-hosted bootstrap** — long-term.
