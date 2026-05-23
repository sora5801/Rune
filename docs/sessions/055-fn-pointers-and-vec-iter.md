# Session 055 — Function-pointer values + `Vec::iter()`

**Date:** 2026-05-23
**Outcome:** Named functions are now first-class values: a `fn` item
can be assigned to a binding, stored in a struct field, and called
through that binding or field. `Vec<T>` gains a builtin `iter()`
method that returns a prelude `VecIter<T>` struct implementing the
session-053 `Iterator` trait. The map/filter/collect adapters were
explicitly scoped out — they exposed a deeper projection-through-
impl-generic substitution gap that needs its own session. ~8 files.
519 tests green (+4 from 515).

## The decisive observation

`HirExprKind::Fn(_)` codegen had erroured with "first-class function
values are not supported" since session 027 (generics). Lifting that
restriction is the load-bearing prerequisite — once a named `fn`
becomes a value, every downstream feature falls out: a struct field
typed `fn(T) -> U` can hold one, a parameter typed `fn(T) -> U` can
receive one, an indirect call through a Local of `Ty::Fn` is just
`call_indirect`. The dyn machinery from session 033 was already
emitting `call_indirect` against a loaded fn pointer; this session
generalizes that pattern to user-callable fn values.

## The wire-ups

```
src/
├── ast.rs        (Type::Fn { params: Vec<Type>, ret: Option<Box<Type>>, span })
├── parser.rs     (parse_type detects leading `fn`; `fn(T1, T2) -> R`
│                  or `fn(T1, T2)` for unit-returning)
├── checker.rs    (resolve_type builds Ty::Fn { params, ret };
│                  builtin_vec_iter_sig adds `vec.iter()`;
│                  find_struct_sym helper; Ty::compatible loosens
│                  for Ty::Assoc and recurses through Ty::Fn;
│                  unify_typevars walks Ty::Fn + Ty::Vec;
│                  apply_subst_inner_with takes &Resolutions and
│                  substitutes the impl's own generic params using
│                  the call-site struct args; check_struct_lit goes
│                  two-pass so a field of type fn(I::Item)->U sees
│                  the inferred subst from a sibling iter: I field;
│                  check_place_root_mutable already allows
│                  param-field mutation from session 053)
├── hir.rs        (HirExprKind::IndirectCall { callee, args })
├── lower.rs      (ast::Expr::Call where callee isn't a path-symbol
│                  lowers to IndirectCall; vec.iter() intercept
│                  emits a VecIter StructLit; subst_struct_typevars
│                  helper substitutes the impl's generic param
│                  using the call-site struct args when building
│                  the for-iterator desugar's item_ty)
├── codegen.rs    (cranelift_type: Ty::Fn => I64; HirExprKind::Fn(sym)
│                  emits func_addr; new IndirectCall arm builds a
│                  sig from the callee's Ty::Fn and call_indirects)
├── monomorphize.rs (subst_expr_kind / walk_tys_expr /
│                  walk_expr_collect_syms / collect_requests /
│                  rewrite_calls / resolve_method_calls all walk
│                  IndirectCall; unify recurses through Ty::Fn)
└── std.rn        (pub struct VecIter<T> { vec: Vec<T>, idx: i64 };
                   pub impl<T> Iterator for VecIter<T>)
```

The `vec.iter()` method is recognized by the checker through a new
`builtin_vec_iter_sig` arm (alongside `vec_get` / `vec_push` /
`vec_len`) returning `Ty::Struct(VecIterSym, [elem])`. The lowerer
intercepts the resulting `MethodCall` on `Ty::Vec(_)` with method
`"iter"` and rewrites it into a `HirExprKind::StructLit` with
`vec = receiver, idx = 0` — so the rest of the pipeline sees an
ordinary struct construction and the monomorphizer + codegen need
no special-case for iterators on Vec.

## What's tested

Codegen (+4):

- `fn_pointer_basic` — a named `fn` stored in a struct field of
  `fn(i64) -> i64` type, called via `(b.f)(21)`. End-to-end check
  that the new plumbing works.
- `fn_pointer_through_local` — `let f: fn(i64) -> i64 = add_one;
  f(41)`. Verifies the IndirectCall path for a Local of fn-pointer
  type.
- `vec_iter_via_next` — `v.iter()` returns a `std::VecIter<i64>`;
  calling `.next()` directly returns `Option<i64>` and advances
  the underlying index.
- `vec_iter_exhausts_returns_none` — after the last element,
  `next` returns `None`. Confirms index bookkeeping + the
  `< len` guard.

## Apparent bugs that aren't / explicitly deferred

- **No `map` / `filter` / `collect` adapters yet.** Writing
  `Map<I, U> { iter: I, f: fn(I::Item) -> U }` as a prelude struct
  exposed a projection-resolution gap: when monomorphization
  specializes `Map<VecIter<i64>, i64>::next`, `I::Item` needs to
  become `i64` (via `VecIter<i64>`'s impl binding plus the impl's
  own `T_VecIter = i64` substitution). The two-layer substitution
  (struct-args → impl-T, then T → concrete) works inside the
  checker via `apply_subst_inner_with`'s new `&Resolutions`
  argument but doesn't yet plumb through the monomorphizer's
  `subst_ty` thread-local. The adapter API ships in a follow-up
  session that closes this loop.

- **No `for x in v.iter() { ... }` loop yet.** The same
  projection-resolution gap blocks the for-loop desugar's
  `MethodCall.next() → Option<Self::Item>` path when the iter
  type itself carries a generic arg. The minimal `let it =
  v.iter(); match it.next() { ... }` shape works (the manual
  match version of session 053's test pattern), but the implicit
  desugar version still leaks a TypeVar to codegen.

- **No closures.** Callbacks must be named `fn` items today.
  Closures (`|x| body`) need an environment-capture lowering and
  a `Fn` trait — a session of their own.

- **The `Ty::compatible` widening for `Ty::Assoc`.** A
  projection like `T::Item` is now treated as compatible with
  any type at typecheck time, on the assumption that
  monomorphization will resolve it to a concrete type and a
  real mismatch surfaces then. Reasonable for the iterator
  pipeline but technically a check that's been loosened — a
  future session may want to tighten it once projection
  resolution is fully wired through.

- **`check_struct_lit` two-pass.** The previous one-pass code
  checked each field's value against its declared type
  immediately. With fields whose declared type *references*
  another field's type (`Map`'s `f: fn(I::Item) -> U` after
  `iter: I`), the one-pass check would fail because `I` wasn't
  yet bound. Pass 1 now infers the substitution from all fields,
  pass 2 substitutes and check_assignables. Net new code but
  small.

## Symbol-identity bug check (per session 048's lesson)

- **`find_struct_sym` walks by name.** Same heuristic as
  `find_iterator_sym` / `find_option_sym`. A user-defined
  `VecIter` struct would shadow the prelude's only if it were
  parsed first — the prelude always is.
- **Synthetic SymbolIds in `vec.iter()` desugar.** The lowerer's
  `lower_vec_iter` builds a `StructLit` using existing
  `VecIterSym` (looked up by name) and references `v` (the
  receiver) as a child expression — no fresh syms invented
  here, so no collision risk.
- **`apply_subst_inner_with`'s `res` parameter.** The
  `Checker::apply_subst` method now passes `Some(self.res)`;
  callers that constructed a one-off `apply_subst_inner` (none
  outside checker.rs) would silently miss the struct-arg
  substitution and get the v0.x partial-resolution behavior.
  Verified no such callers exist.

## What's next

- **Projection-through-impl-generic resolution in the
  monomorphizer.** The checker's `apply_subst_inner_with` now
  walks both layers; the monomorphizer's thread-local
  IMPL_ASSOC_BINDINGS substitution should mirror that. Closes
  the gap that blocked `Map` / `Filter` this session.
- **Map / Filter / collect** — re-enabled once the projection
  story is solid.
- **Closures + `Fn` trait** — lambdas eliminate the named-fn
  workaround at adapter call sites.
- **`HashMap<K, V>`** — the other major collection.
- **`continue` keyword** — small, mirror of `break` over the
  loop_exit_stack pattern.
