# Session 061 — Closures in iterator adapters

**Date:** 2026-05-24
**Outcome:** A capturing closure now fits directly into Map's `f`
field and Filter's `pred` field. The truly headline pipeline
preview compiles and runs:

```rune
let mult: i64 = 3;
let v: Vec<i64> = vec_new();
v.push(1); v.push(2); v.push(3);
let mapped = std::Map { iter: v.iter(), f: |x: i64| x * mult };
let mut total: i64 = 0;
for y in mapped { total = total + y; }   // 1*3+2*3+3*3 = 18
```

Map and Filter became generic over the callable. The trait bound
on the impl block (`F: Fn1<I::Item, U>`) drives inference: the
checker now propagates from a pinned `F` back through the bound's
generic args, pinning `U` (Map's element type) without any
field literally mentioning it. The same propagation surfaces a
diagnostic when the closure's signature doesn't match — `Map { f:
takes_str }` over `iter: VecIter<i64>` is rejected with "field
bound mismatch."

3 files. ~285 codegen tests green (+3 from session 060), 143
typecheck tests green.

## The decisive observation

The std-rn change is small:

```rune
pub struct Map<I, F, U> { iter: I, f: F }
pub impl<I: Iterator, F: Fn1<I::Item, U>, U> Iterator for Map<I, F, U> { ... }
```

What this needs from the rest of the compiler:

1. **The parser** has to read generic args on bound paths
   (`F: Fn1<I::Item, U>` is `Fn1` with `[I::Item, U]`). Previously
   `parse_path()` in bound position stopped at the `<`.
2. **Bound resolution** has to scope its args. The args reference
   *other* type params in the same generic list — sometimes a
   forward reference (`<F: Fn1<..., U>, U>`). So generic-param
   interning has to two-pass: name first, bound-args after.
3. **`generic_bound_args`** has to record each bound's resolved
   arg spans, keyed by `(param_sym, trait_sym)`.
4. **The checker** has to bridge impl-side syms (where bounds
   live, because they're declared on the impl's generics) to
   struct-side syms (where struct-lit unification keys subst).
   `impl_to_struct_generic` is the bridge — positionally aligned
   for v0.x. The bound arg types are translated through this
   map before they go into unification.
5. **`propagate_bound_inference`** does the real work: walk
   subst entries, for each `param → concrete`, find bound
   entries whose impl-side maps to that param, and unify each
   `(bound_arg, concrete_callable_part)` pair. The "concrete
   callable" is either a `Ty::Fn(P..., R)` (named fn or
   session-057 non-capturing closure) or a
   `Ty::Struct(closure_sym, [])` whose impl_methods has a
   `call` entry (session-060 capturing closure).
6. **`trait_bound_method_sig`** has to substitute the trait's
   own generic params (Fn1's `A`/`R`) with the bound's args at
   the call site. Without this, `self.f.call(x)` would set
   `e.ty` to `Fn1::R` (a TypeVar that the outer monomorphize
   subst can't reach because R is in Fn1's generic list, not
   Map's).
7. **The monomorphizer** has to rewrite `<Ty::Fn>.call(args)` to
   `IndirectCall` — when F is bound by Fn1 but the concrete
   value is a fn pointer, there's no impl_methods entry to look
   up. The IndirectCall rewrite is the "Ty::Fn satisfies Fn1"
   coercion at the call site (zero wrapper struct, zero
   allocation).

## The wire-ups

```
src/
├── std.rn               (Map<I, F, U> and Filter<I, P>; the
│                         method bodies use `self.f.call(x)` and
│                         `self.pred.call(x)` instead of
│                         `(self.f)(x)`. The Fn1 trait stays as-
│                         is — its receiver is `self: Self` and
│                         dispatch is static through the impl.)
│
├── parser.rs            (new `parse_bound_path` consumes generic
│                         args on trait-bound paths. Used by
│                         `parse_optional_generic_params` after
│                         each `+`-separated bound.)
│
├── resolver.rs          (`generic_bound_args`: per-(param, trait)
│                         spans of the trait's generic args, used
│                         by the checker. `impl_to_struct_generic`
│                         maps impl-side type-param syms to
│                         struct-side ones positionally — bridges
│                         where bounds were declared vs where
│                         subst is keyed at struct-lit time.
│                         `resolve_fn`'s generic loop now
│                         two-passes: intern all names first, then
│                         resolve bounds, so a bound's args can
│                         forward-reference a later param.)
│
├── checker.rs           (`resolve_bound_args` pre-pass walks all
│                         generic-param bound args via the AST so
│                         `type_resolutions` has entries the
│                         propagation reads later.
│                         `propagate_bound_inference` and its
│                         `_with_mismatches` sibling: do the
│                         positional unification between bound
│                         args and concrete callable shape;
│                         called from `check_struct_lit` after
│                         each unify pass. `translate_impl_to_struct`
│                         walks a Ty replacing impl-side TypeVars
│                         with their struct-side counterparts.
│                         `trait_bound_method_sig` now uses the
│                         bound's args to substitute trait
│                         generics in the method's param/ret
│                         types so the call's `e.ty` is
│                         concrete-after-substitution.)
│
└── monomorphize.rs      (`resolve_method_calls_in_expr`'s
                          MethodCall arm now rewrites
                          `<Ty::Fn>.call(args)` to IndirectCall
                          and refreshes `e.ty` from the fn's
                          actual return type — the "Ty::Fn
                          satisfies Fn1" coercion. The existing
                          struct-receiver path already handled
                          the closure-struct case via
                          impl_methods lookup; session 061 just
                          wired the Ty::Fn parallel.)
```

## What's tested

Codegen (+3):

- `closure_capture_in_map` — the headline. `f: |x| x * mult` with
  captured `mult` flows into Map's `f` field. Map's
  monomorphization is per closure-struct.
- `closure_capture_in_filter` — Filter's `pred` field accepts a
  capturing closure. The bound's bool return is concrete in the
  bound spec, so propagation just verifies (no new pins).
- `closure_capture_chain_map_filter_collect` — two captures
  across a 2-stage adapter pipeline + collect. Confirms the
  per-stage monomorphizations chain cleanly.

Existing iterator-adapter tests (`iter_map_alone`,
`iter_filter_alone`, `iter_collect_map_filter_pipeline`,
`iter_count_bounded_generic`, `closure_in_map_pipeline`,
`closure_in_filter_pipeline`, `closure_chain_map_filter_collect`)
were updated to drop the now-incorrect 2-arg `Map<I, U>`
annotation — inference picks up Map's three type args from the
struct-lit fields.

Typecheck:

- `map_wrong_fn_signature_rejected` — `f: takes_str` over
  VecIter<i64>: the bound's `I::Item = i64` clashes with the
  fn's `str` param. `propagate_bound_inference_with_mismatches`
  surfaces "bound mismatch."
- `map_inferred_struct_arg_mismatch_rejected` — `f: takes_bool`
  over VecIter<i64>: same shape, same diagnostic.

## Apparent bugs that aren't / explicitly deferred

- **The `Ty::Fn → Fn1` coercion is per call site, not per
  value.** Passing a named fn `double` to Map's `f` field works
  because the monomorphizer rewrites `self.f.call(x)` to
  `IndirectCall(self.f, [x])` once F is concretized to
  `Ty::Fn`. No wrapper struct, no allocation, no per-fn-pointer
  Fn1 impl. The cost: a Ty::Fn value in any Fn1-bounded slot
  pays the indirect-call branch even when the optimizer could
  inline it. Negligible for v0.x.
- **The 2-arg annotation `Map<VecIter<i64>, i64>` no longer
  parses through.** Map takes 3 generics now. Tests dropped the
  annotation; inference works from the struct-lit field types
  via `propagate_bound_inference`.
- **Bound propagation is positional, not name-based.** The
  callable-trait shape is detected by `arg_spans.len() ==
  c_params.len() + 1`. This works for Fn1<A, R> against Ty::Fn(P,
  R) — args=[A_pos, R_pos] match [P_pos, R_pos] positionally. A
  hypothetical `Fn2<A, B, R>` would also work. Non-callable
  traits with N+1 args would be erroneously matched, but the
  unification would silently fail at the first mismatch — no
  miscompilation, just no help. Surfacing this as an error
  requires a richer "is callable trait?" predicate; deferred.
- **`impl_to_struct_generic` is positional**, so an impl whose
  generic list reorders or omits a struct param would lose
  bound info. v0.x adapters all align with the for-type's args.
- **No FnMut, no move-capture.** Same constraints as session
  060.
- **`F::Output` as a way to drop the U generic**: tempting but
  would need closure impls to bind an associated `Output` type,
  and the prelude's Fn1 lacks one. Punted.

## Symbol-identity / forward-reference

A bound's generic args reference type params that may come
*after* the bound's owner in the generic list. `<F: Fn1<I::Item,
U>, U>` is the canonical case: F's bound mentions U, U is
declared after F. The resolver's per-fn generic loop used to be
single-pass (intern name, resolve bounds in the same iteration),
so resolving F's bound saw `U` unresolved. Session 061 splits the
loop in two: first pass interns every name into scope; second
pass resolves each bound. Same syms — interning is keyed by
span, so the second pass reuses what the first interned.

The arg spans are stored in `generic_bound_args`. When the
checker later reads them, it goes through the resolver's
`type_resolutions` to get the Ty. Those Ty's live in the impl's
TypeVar space; `translate_impl_to_struct` rewrites to struct-side
syms before unification. Forgetting that translation pins the
wrong sym (we caught this in development: subst[U_impl=54] = i64
instead of subst[U_struct=69] = i64, leaving U_struct unpinned
and TypeVar leaking to codegen as "type T#69 not supported").

## What's next

- **Bottom-up closure-param inference** — lift the
  `|x: i64|` annotation in adapter tests so unannotated
  `|x| x * mult` works inside `Map { f: ... }`. The hint flows
  through `check_struct_lit`'s pass 2, but the closure's
  parameter type isn't currently propagated from a struct-lit
  field hint when F is a generic param. Tractable follow-up.
- **Generalize the "callable trait" predicate** — replace the
  arg-count heuristic with a real check (the trait has a single
  method named `call` whose first param is `Self`). Cleans up
  the propagation's false-positive surface area.
- **Fn-pointer auto-wrap into a closure struct** for the rare
  case where a code path *needs* the value to be a concrete
  closure-struct (e.g. storing in a `dyn Fn1` box). Not yet
  needed.
- **HashMap, Range as RangeIter, continue, From-based `?`** —
  independent threads, any of which would be a fine session
  062.
