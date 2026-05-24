# Session 062 — Bottom-up closure-param inference

**Date:** 2026-05-24
**Outcome:** Unannotated closure params no longer require a
contextual hint. Two cases newly compile:

```rune
// (1) bare let with no annotation, no surrounding hint
let mult: i64 = 3;
let f = |x| x * mult;     // x: i64 inferred from binop
f(7)                       // 21

// (2) Map/Filter without the `:i64` annotation
let mapped = std::Map { iter: v.iter(), f: |x| x * mult };
//                                          ^ no `:i64`
```

The mechanism is two-pronged. Case (2) — when the closure flows
into a generic field with a callable bound (`F: Fn1<I::Item,
U>`) — synthesizes a `Ty::Fn` hint from the bound's args and
applies the current substitution. Case (1) — bare let with no
hint — mints a fresh inference TypeVar for each unannotated
param, lets the body's binops pin it via a side-channel, and
rewrites the param's type after the body check.

~3 files. 291 codegen tests green (+3 from session 061).

## The decisive observation

Two cases, two mechanisms — but both reduce to "pin an
unknown to a known thing the surrounding code already
specifies."

**Case 2 — bound-driven (Map/Filter contexts).** The pin
source is the type-system *itself*. When `check_struct_lit`'s
pass 2 substitutes the field's declared type and finds it's a
generic TypeVar (`F`), the bound `F: Fn1<I::Item, U>`
describes the call signature shape. We just read it.

`expand_callable_typevar(expected, &subst)`:
- reverse-lookup the struct-side `F` to its impl-side sym via
  `impl_to_struct_generic`;
- find a bound on that impl-side sym in `generic_bound_args`;
- translate each arg span through `translate_impl_to_struct`
  to struct-side TypeVars;
- apply the current subst so `I::Item` resolves (since `I =
  VecIter<i64>` is already pinned by the `iter` field);
- build `Ty::Fn { params: [...], ret: ... }`.

The synthesized hint replaces the bare `Ty::TypeVar(F)` going
into `check_closure`, which then binds each param from the
hint just like an explicit annotation.

**Case 1 — body-driven (bare let).** No bound, no hint. We
mint a fresh inference TypeVar via the new
`Checker::fresh_sym` (counts down from `u32::MAX` so it never
collides with the resolver's symbols), bind the param to it,
and check the body. The body's `check_binary` calls
`try_pin_infer_typevar(lt, &rt)` and the mirror call — for
each side that's an inference TypeVar, record the other
side's type as the pin. After the body check returns, the
closure walks each fresh sym, looks up its pin, and replaces
the param's `Ty::TypeVar(...)` with the pinned type (in both
`param_tys` and `local_types`). If a param has no body use
that pinned it, error.

## The wire-ups

```
src/checker.rs        (Checker gains `closure_infer_pool:
                       RefCell<HashMap<SymbolId, Option<Ty>>>`
                       and `next_fresh_sym: Cell<u32>`.
                       `fresh_sym()` mints a fresh inference
                       TypeVar sym. `try_pin_infer_typevar`
                       records a pin on a fresh sym if the
                       caller passed a concrete partner.
                       `check_binary` calls it on both
                       operands AFTER bottom-up checking, then
                       picks the concrete side as `t` so the
                       numeric / integer checks see a real
                       type. `check_closure` mints fresh syms
                       for unannotated-no-hint params; after
                       the body check, walks `fresh_syms` and
                       replaces each param's TypeVar with its
                       pin (or errors "no contextual hint and
                       no body usage to infer from" if
                       unpinned). `expand_callable_typevar`
                       and the `check_struct_lit` pass-2 hook
                       that uses it cover the bound-driven
                       case.)
```

That's it — a single file. The resolver, lowerer, and
codegen are unchanged: the inference is fully resolved inside
`check_closure` before any other compiler stage sees the
closure's signature.

## What's tested

Codegen (+3):

- `closure_capture_in_map_unannotated` — the headline.
  `Map { iter: v.iter(), f: |x| x * mult }` without `:i64`.
- `closure_capture_in_filter_unannotated` — Filter parallel
  for `|x| x > threshold`.
- `closure_in_map_unannotated_no_capture` — non-capturing
  variant of the Map case (closure value stays a `Ty::Fn`
  via session 057's anonymous-fn-item path; the bound-driven
  hint synthesis is what matters, not the closure shape).
- `closure_bare_let_unannotated` — `let f = |x| x * mult; f(7)`
  — bare let, body-driven pin.
- `closure_bare_let_unannotated_with_capture` — same, but with
  a real capture; the inference flows before the closure
  becomes a synth struct.
- `closure_bare_let_inferred_from_comparison` — `|v| v > 5`
  pins v from the literal's type, not arithmetic.

The existing closure-capture tests (`closure_capture_in_map`,
`closure_capture_in_filter`, `closure_capture_chain_*`) still
have explicit annotations; they're left as-is to keep
coverage of both paths.

## Apparent bugs that aren't / explicitly deferred

- **Inference only sees `check_binary`.** A call-arg position
  (`|x| double(x)` where the body's only constraint is the
  call's first param) doesn't trigger a pin. Same for method
  receivers and field accesses. Extending to those positions
  is mechanical — call `try_pin_infer_typevar` at each new
  site — but each addition needs its own test; deferred.
- **Multiple uses must agree.** If `|x| { x + 1; x > true }`
  appeared, the first use pins `x: i64`, the second tries to
  pin `bool` but `try_pin_infer_typevar` only writes when
  the slot is empty. The mismatch surfaces as the comparison's
  type-mismatch error (the body's later operations see x: i64
  via local_types and try to compare with true), not as a
  cleaner "ambiguous inference" diagnostic. Acceptable for
  v0.x.
- **No transitive pinning.** Two unannotated closure params
  `|x, y| x + y` can't pin each other — both sides are
  TypeVars, `try_pin_infer_typevar` bails on TypeVar-to-
  TypeVar. The diagnostic is "no body usage to infer from"
  for the first param after the body returns. Workaround:
  annotate one (`|x: i64, y| x + y`).
- **Inference TypeVars in `expr_types` may persist.** The
  fresh sym appears in `expr_types[path.span]` for the
  param's read in the body — that map isn't rewritten after
  pinning. Codegen uses `local_types` (which IS rewritten)
  for Local lookups, so this doesn't surface. A cleaner pass
  could walk `expr_types` and substitute through the pool
  before clearing it.

## Symbol-identity check

`fresh_sym` decrements from `u32::MAX`. The lowerer's
`Lowerer::fresh_sym` increments from `res.symbols.len()` —
they grow toward each other from opposite ends. A program
with `u32::MAX / 2` worth of symbols isn't representable in
practice. Within a session, the same `Checker` instance can
generate any number of fresh syms; `next_fresh_sym.wrapping_sub`
wraps gracefully, but reaching wraparound from `u32::MAX`
also isn't representable.

## What's next

- **Call-arg pin** — extend `try_pin_infer_typevar` to fire
  when a fresh-sym TypeVar reaches a fn-call arg position
  with a concrete declared param type. Same applies to method
  receivers and field-of-known-struct accesses.
- **A "callable trait" predicate** — replace the arg-count
  heuristic in `propagate_bound_inference` and
  `expand_callable_typevar` with a real check (the trait has
  a single method named `call` whose first param is `Self`).
- **HashMap, Range as RangeIter, continue, From-based `?`** —
  independent threads.
