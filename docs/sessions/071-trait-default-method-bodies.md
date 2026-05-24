# Session 071 — Trait default-method bodies

**Date:** 2026-05-24
**Outcome:** Traits can declare method bodies. Impls that don't
override inherit the default; monomorphization specializes per
Self at each call site. The headline: `.collect()` as a chained
method on any Iterator. 590 tests green (+2 from session 070).

```rune
pub trait Iterator {
    type Item;
    fn next(self: dyn Iterator) -> Option<Self::Item>;
    fn collect(self: Self) -> Vec<Self::Item> {
        let v: Vec<Self::Item> = vec_new();
        while true {
            match self.next() {
                Option::Some(x) => { v.push(x); }
                Option::None => { break; }
            }
        }
        v
    }
}

// All five Iterator impls (VecIter, RangeIter, Map, Filter,
// HashMapKeysIter) inherit collect without a single line in the
// impl block:
let r: Vec<i64> = v.iter().collect();
let q: Vec<i64> = std::Map { iter: v.iter(), f: |x| x * mult }.collect();
```

## The decisive observation

A default body is a generic free function in disguise. Its `Self`
is a fresh type-param sym, bounded by the surrounding trait. The
body's `self.next()` resolves via `trait_bound_method_sig`
(session 051's machinery) — because Self is bound by the trait,
its trait methods are reachable. Each impl that omits the
method gets `impl_methods[(impl_struct, method)] = default_fn_sym`;
method dispatch reads `impl_methods` uniformly, so the call site
looks identical to a normal method. The monomorphizer specializes
per Self at each call site; the body's `Self::Item` projection
resolves once Self is bound to a concrete struct with a known
`type Item = ...`.

Five small pieces, no new IR or new lookup path:

1. AST/parser accepts an optional body block.
2. Resolver mints a synth fn sym + Self type-param sym (bounded
   by the trait) when it encounters a default body, then resolves
   the body's identifiers like a regular fn body.
3. Checker stashes a fn signature for each default + type-checks
   the body. Self::Item resolves to `Ty::Assoc(TypeVar(self_sym),
   "Item")` (substitutable) instead of `Ty::Assoc(SelfType, ...)`
   (opaque) via a new `current_self_param` slot.
4. Lowerer emits one `HirFn` per default — its generics list is
   `[Self, ...trait_generics]`.
5. Each impl that doesn't override the method gets
   `impl_methods[(impl_struct, method)] = default_fn_sym` at
   declare_impl time. Conformance check skips missing methods
   that have a default.

The monomorphizer needed zero changes — `resolve_method_calls`'s
existing `impl_methods` lookup and `specialize_pending`'s generic-fn
specialization handled everything, because the default fn really
*is* a generic free function from the IR's perspective.

## What's tested

Codegen (+2):

- `trait_default_method_collect_chained` — `v.iter().collect()`
  returns a Vec of the same contents.
- `trait_default_method_collect_through_map` —
  `Map { iter: v.iter(), f: |x| x * mult }.collect()` works
  through the closure-bound Map, exercising default-method
  dispatch on a non-trivial Iterator impl. Confirms the
  monomorphizer specializes the default per Self (here `Map<...>`)
  and the body's `self.next()` routes to `Map::next`.

## Apparent bugs that aren't / explicitly deferred

- **Default bodies use `self: Self`, not `self: dyn Trait`.** The
  default body operates on the concrete impl type via static
  dispatch. Calling a default on a `dyn` receiver would need a
  different path (synthesize per-vtable thunks). Not blocked, just
  not implemented.
- **`Self::Item` resolves via the `current_self_param` slot only
  in default-body context.** Trait-method-signature contexts still
  produce `Ty::Assoc(SelfType, "Item")`, which `trait_bound_method_sig`
  rewrites at impl-call sites. The two paths don't collide because
  `current_self_param` is only set inside check_item's Trait arm
  for body-bearing methods.
- **The `impl_methods` fill happens in declare_impl, which runs
  after declare_items.** If a trait is declared AFTER an impl
  block in source order, the fill misses it (trait_defaults not yet
  populated when the impl is processed). In practice impls come
  after the trait they implement; the std prelude follows this
  convention. A two-pass fill could fix it but isn't needed for
  v0.x.
- **A default's `self.next()` call resolves via
  `trait_bound_method_sig`.** The trait_bound machinery walks
  `generic_bounds[self_sym]` looking for traits whose methods
  match — for the synth self_sym, that's the surrounding trait.
  So a default body can only call methods declared in its own
  trait or supertraits. Calling impl-specific methods of Self
  wouldn't typecheck.
- **The standalone `collect` free function (session 056) still
  exists** for callers who want to write `collect(iter)` instead
  of `iter.collect()`. Both compile and produce equivalent code.

## What's next

- **`?` on Option** — Result-only today.
- **Multi-impl `Into` disambiguation** — `impl_methods` keys
  methods by name only.
- **HashMap .values() / .entries()** — needs tuples.
- **More default-body trait methods** — `.map(f)`, `.filter(p)`,
  `.fold(...)`, `.count()`, `.sum()` would all flow through the
  same machinery.
- **Self-hosted bootstrap** — long-term.
