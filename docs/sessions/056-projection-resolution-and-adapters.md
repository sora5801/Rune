# Session 056 — Projection-through-impl-generic resolution + iterator adapters

**Date:** 2026-05-23
**Outcome:** The headline session-055 deferral is closed. `Map`,
`Filter`, and `collect` are now in the prelude, the
`v.iter().map(...).filter(...) → collect(...)` pipeline works, and
`for x in v.iter() { ... }` runs directly without the
`let it = v.iter(); match it.next() { ... }` workaround. The fix
required three coordinated changes — one in the resolver, one in
the checker, one in the monomorphizer — to make the impl block's
`<T>` and the struct's `<T>` agree on a substitution path. ~7
files. 526 tests green (+7 from 519).

## The decisive observation

There were **three** bugs to close, not one. The Plan agent
identified only two; the third surfaced during implementation:

1. **`check_for` discards the iter's struct args.** The pattern
   `Ty::Struct(s, _)` ignored `_`, so the iterator's `Item` was
   looked up against `impl_assoc_bindings_ty` and returned the
   impl-block's `Ty::TypeVar(T_impl)` — typing the user's `x`
   binding as a TypeVar.

2. **The monomorphizer's `subst_ty` Assoc arm** didn't do the
   per-struct substitution that the checker side already did
   (session 055). After substituting `Ty::Assoc(I, "Item")` →
   `Ty::Assoc(Struct(s, [args]), "Item")`, the lookup returned
   the impl-block's `Ty::TypeVar(T_impl)` and that TypeVar leaked.

3. **The impl block's `<T>` and the struct's `<T>` are
   different `SymbolId`s.** The resolver interns them in separate
   scopes. So even with both fixes above, the substitution from
   `STRUCT_GENERICS[VecIter] = [T_struct]` to the binding's
   `Ty::TypeVar(T_impl)` was a no-op. The fix is to remap
   `T_impl → T_struct` at the moment the checker stores the
   binding, so the rest of the pipeline only sees the struct's
   T.

## The wire-ups

```
src/
├── hir.rs        (HirModule.struct_generics: HashMap<SymbolId,
│                  Vec<SymbolId>>)
├── lower.rs      (populate struct_generics; apply_subst_ty now
│                  resolves Ty::Assoc through impl bindings + the
│                  struct's call-site args, so the for-iterator
│                  desugar's item_ty fully resolves even when
│                  the impl declares `type Item = I::Item`)
├── resolver.rs   (intern_generic_param helper — declare_impl's
│                  two enter_scope blocks now reuse the same
│                  SymbolId per impl-generic-param via decl_to_sym
│                  keyed by g.name.span; resolve_fn already did
│                  this for the method-generics walk in session
│                  048, this generalizes it)
├── checker.rs    (register_signatures Impl arm builds an
│                  impl_T → struct_T remap from the type-path's
│                  generic args + struct_generics, then applies
│                  it to each assoc-type binding before storing;
│                  check_for substitutes through the iter's struct
│                  args so the pattern variable is typed
│                  concretely)
├── monomorphize.rs (STRUCT_GENERICS thread-local set at the top
│                  of monomorphize_module; subst_ty's Ty::Assoc
│                  arm builds a per-struct subst from the call-
│                  site args + STRUCT_GENERICS[s] and applies it
│                  to the binding before substituting outer
│                  vars; finish() iterates specialize_pending +
│                  resolve_method_calls in a loop until the
│                  instantiation cache stops growing — needed
│                  for adapter pipelines whose inner method calls
│                  surface only after the outer adapter is
│                  specialized)
└── std.rn        (Map<I, U>, Filter<I>, collect<T: Iterator> all
                   land — they were deferred in session 055)
```

The four bugs landed in the natural place each was best detected:
the resolver fix is about symbol-identity (it belongs there); the
checker remap is about pre-resolving impl bindings into a
struct-relative form (it belongs in `register_signatures`); the
monomorphizer Assoc arm + STRUCT_GENERICS plumbing is about
runtime substitution; the `check_for` substitution is about typing
pattern bindings correctly. None of these is gratuitous.

The monomorphizer's `finish()` change is the smallest visible but
the most consequential at runtime: a single
`specialize_pending; resolve_method_calls; specialize_pending`
sequence (session 048) doesn't cover adapter pipelines like
`Filter<Map<VecIter<T>, U>>` where each layer's `.next()` surfaces
only after the outer layer is specialized. Looping until the
cache stabilizes covers any depth.

## What's tested

Codegen (+5):

- `iter_for_in_v_iter` — `for x in v.iter() { ... }` direct.
- `iter_map_alone` — Map adapter alone, summed via for-in.
- `iter_filter_alone` — Filter adapter alone.
- `iter_collect_map_filter_pipeline` — the headline pipeline,
  Vec → iter → Map → Filter → collect into Vec.
- `iter_count_bounded_generic` — `count<T: Iterator>` over a
  `Map<VecIter<i64>, i64>` value.

Typecheck (+2):

- `map_wrong_fn_signature_rejected` — passing a `fn(str) -> i64`
  to Map's `f` field (declared `fn(I::Item) -> U`, resolves to
  `fn(i64) -> U`) is a real type error now that the field check
  substitutes through the inferred subst.
- `map_inferred_struct_arg_mismatch_rejected` — passing a
  `fn(bool) -> bool` where Map's `I = VecIter<i64>` infers
  `fn(i64) -> bool` is similarly rejected.

## Apparent bugs that aren't / explicitly deferred

- **`for x in StructLit { } { body }` parser ambiguity** — the
  outer `{`/`}` is both a struct-literal and a for-loop body.
  Workaround: bind the iterator to a local first (`let mapped =
  std::Map { ... }; for x in mapped { ... }`). Could be parens-
  required (Rust-style) in a future session.

- **`collect` is a free function, not a method.** Chained
  `.collect()` would need default-method trait bodies — a separate
  feature.

- **No closures.** Map/Filter callbacks must be named `fn`
  items. Sessions 057+ adds `|x| body` lambdas.

- **Map's `U` is unconstrained.** No requirement that `f`'s
  return type matches anything in particular; the user picks
  what `U` is and the typecheck does the rest. Filter forces
  `Item = I::Item`.

- **Higher-order iterators across Vec elements that aren't
  Copy.** Vec<T> for ARC-managed T works in principle but the
  adapter pipeline doesn't fully exercise the ARC interleavings
  yet — Vec<i64> is what's tested. Composes mechanically but
  not yet covered.

- **No `Range` as an Iterator-implementing struct.** The
  range-for-loop (session 020) still goes through its dedicated
  codegen path. Switching it to a `RangeIter` adapter is the
  natural follow-up.

## Symbol-identity bug check

The impl-T → struct-T remap is the heart of this session. The
risks: a future generic-impl form (e.g. `impl<T> Iterator for
Container<Wrapper<T>>`) where the type-path's generic args
aren't bare `Ty::TypeVar` — the remap collects from
`Ty::TypeVar` patterns only. State this clearly. The current
impls in std.rn (`impl<T> Iterator for VecIter<T>`, `impl<I, U>
Iterator for Map<I, U>`, `impl<I> Iterator for Filter<I>`) all
have the simple shape and work. A non-bare arg would silently
not remap.

`intern_generic_param` reuses by span. Two impls with the same
generic-param name (`impl<T> A for Foo`, `impl<T> B for Bar`)
have distinct spans because their `<T>` is at different source
positions. The cache is span-keyed, so they get distinct
SymbolIds — correct.

## What's next

- **Closures (`|x| body`) + `Fn` trait** — eliminates the
  named-fn workaround at adapter call sites.
- **`HashMap<K, V>`** — the other big collection.
- **`continue` keyword** — the last unsupported loop control-
  flow primitive.
- **`Range` as `RangeIter`** — unify the for-over-range codegen
  path with the Iterator protocol.
- **Trait default-method bodies** — would let `.collect()` be a
  chained method on Iterator.
