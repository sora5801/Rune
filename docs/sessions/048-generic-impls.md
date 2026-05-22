# Session 048 — Generic `impl` blocks

**Date:** 2026-05-21
**Outcome:** `impl<T> Foo<T> { .. }` — methods on a generic struct,
generic over the type's own parameters, in both inherent and trait
form. The design folds the impl's `<T>` into each method's own
generic list, so a generic-impl method is just a generic function
and the existing monomorphizer specializes it. 487 tests green (+4
from 483).

## The gap

Generic structs (`struct Box<T>`) and generic functions (`fn
id<T>`) both existed, but `ast::ImplBlock` had no generic-parameter
list — so a generic struct was a type you could hold but never give
behavior. `impl<T> Box<T> { fn get(self: Box<T>) -> T { self.val } }`
did not parse.

## The design — fold impl generics into method generics

An `impl<T>` block's `<T>` is a type parameter of *every* method.
So the parser **prepends the impl's `<T>` into each method's
`FnDecl.generics`**. After that step a generic-impl method simply
*is* a generic function: the resolver scopes its `T`, the checker
types it with `Ty::TypeVar`, and the monomorphizer specializes it
per call site by unifying the receiver's struct arguments — `b.get()`
on `Box<i64>` infers `T = i64`. Most of the pipeline needed no new
code.

## What did need wiring

1. **Parser / AST.** `ImplBlock` gains `generics`; `parse_impl`
   parses `impl<T>` and uses `parse_type` for the type-path
   (`parse_path` stopped at the `<` of `Foo<T>`).

2. **Resolver.** `declare_impl` resolves the type-path `Foo<T>`, and
   `resolve_path` recurses into generic args — so it now scopes the
   impl's `<T>` first. And `resolve_fn` *reuses* a generic
   parameter's symbol, keyed by source span, when one already
   exists: the parser-merged copies of the impl's `<T>` all share
   one span, and every method must resolve it to a single
   `SymbolId` (see below).

3. **Checker.** `user_method_sig` now unifies the declared `self`
   type against the concrete receiver and substitutes through the
   rest of the signature — so `b.get()` on `Box<i64>` types as
   `i64`, not `T`.

4. **Monomorphizer.** The collect → drain → rewrite cycle became
   `specialize_pending`, and it now runs *again* after
   `resolve_method_calls`: a method call rewritten into a `Call` on
   a generic method — the trait-bound path, `x.tag()` inside `fn
   apply<U: Tagged>(x: U)` — needs a second specialization pass.
   Idempotent via the dedup cache.

## The symbol-identity subtlety

The parser merges by *cloning* `GenericParam`s, so every method's
copy of the impl's `<T>` carries the same source span. `lower_fn`
resolves a generic parameter to its `SymbolId` by span. If each
method's `resolve_fn` interned a fresh symbol, the last method to
resolve would win the span, and earlier methods' `HirFn.generics`
would name the wrong type variable. `resolve_fn`'s reuse-by-span
makes the first method intern the impl's `T` and every later method
of the same impl reuse that one symbol.

## What's tested

Codegen (+3): `generic_impl_inherent_method` (one method, two
instantiations — `Box<i64>` and `Box<bool>`);
`generic_impl_multiple_methods` (several methods sharing the impl's
`T`, plus a method with an extra non-generic parameter);
`generic_impl_trait_bound` (a trait `impl<T>` called directly and
through a `<U: Tagged>` function — the second-pass path).

Typecheck (+1): `generic_impl_typechecks`.

## Apparent bugs that aren't

- **An impl type parameter must be inferable from a value
  argument.** A method like `fn make() -> Foo<T>` with `T` in no
  parameter cannot be specialized — the monomorphizer infers type
  args only from arguments. A `self: Foo<T>` method always carries
  `T`. Pre-existing limitation of the free-function monomorphizer.

- **`apply_subst` does not recurse into `Vec`.** A method parameter
  typed `Vec<T>` would not have `T` substituted — a pre-existing
  gap in the checker's substitution helper, not specific to generic
  impls.

## What's next

- **Associated types** — `trait Iterator { type Item; }`.
- **Supertraits** — `trait Sub: Super`.
