# Session 058 — Generic trait declarations

**Date:** 2026-05-23
**Outcome:** `trait Producer<T> { fn make(self: dyn Producer<T>) ->
T; }` parses, resolves, checks, and runs through dyn dispatch.
Generic trait declarations + `dyn Trait<args>` types + use-site
substitution of the trait's generic params at every dyn method
call. This is the load-bearing prerequisite for the closure-Fn
trait of session 057's headline; sessions 059+ can now build
`Fn1<A, R>` on this foundation. ~5 files. 538 tests green (+4
from 534).

## The decisive observation

Session 057's capturing-closure work hit a hard prerequisite:
the clean closure design requires `trait Fn1<A, R> { fn call(...)
-> R; }`, but Rune's parser didn't accept generic params on
trait declarations. Adding generic traits as a standalone
feature this session is small and self-contained — `parse_trait`
already had every piece it needed (calling
`parse_optional_generic_params` after the trait name), the
resolver already supported scope-stacking generic params via
`intern_generic_param`, and the checker's `dyn_method_sig`
already substituted `Self` at call sites. The only new work is
threading trait-generic args from the use-site `Ty::Dyn(t, args)`
into the method-signature substitution.

The `Ty::Dyn(SymbolId)` variant grew an args list:
`Ty::Dyn(SymbolId, Vec<Ty>)`. Every pattern-match across the
codebase was updated (~17 sites). The args carry the trait
instantiation through dyn-coercion and into method-sig resolution.

## The wire-ups

```
src/
├── ast.rs        (TraitDecl.generics: Vec<GenericParam>)
├── parser.rs     (parse_trait calls parse_optional_generic_params
│                  after the trait name; parse_type's Dyn arm
│                  consumes `<args>` like other type-position paths)
├── resolver.rs   (Item::Trait arm enters a scope, calls
│                  intern_generic_param for each trait generic,
│                  records the sym list in `trait_generics`;
│                  scope held over the method-signature resolution
│                  so each method's `self: dyn Trait<T>` sees
│                  `T` in scope)
├── checker.rs    (resolve_type's Type::Dyn arm reads
│                  p.generic_args and produces `Ty::Dyn(s, args)`;
│                  dyn_method_sig builds a `trait_subst` from the
│                  use-site args + trait_generics and feeds it
│                  through `apply_subst` so the method's params
│                  and return type substitute correctly;
│                  check_assignable stores trait_args in
│                  dyn_coercions)
├── lower.rs      (dyn_coercions now stores
│                  `(struct_sym, trait_sym, trait_args)`; the
│                  DynBox wrapper's resulting type is
│                  `Ty::Dyn(trait_sym, trait_args)`)
├── codegen.rs    (mangle_ty_name for `Ty::Dyn` folds the args
│                  into the mangled name; all `Ty::Dyn(_)`
│                  pattern arms became `Ty::Dyn(_, _)`)
└── monomorphize.rs (subst_ty recurses into Ty::Dyn args; the
                    is_arc_mono Ty::Dyn arm matches the new shape)
```

## What's tested

Codegen (+3):

- `generic_trait_basic` — `trait Producer<T>` with `impl
  Producer<i64> for IntBox`. Calling `d.make()` through a `dyn
  Producer<i64>` returns the expected `i64`.
- `generic_trait_two_params` — `trait Pair<A, B>` with two
  generic params. The method-sig substitution applies to both,
  one returning A and one returning B.
- `generic_trait_in_method_arg_position` — the trait's `T`
  appears in a method's *argument* type, not just return. The
  substitution covers param positions too (as it always did
  for non-generic substitutions).

Parser (+1):

- `parses_generic_trait` — `trait Producer<T> { ... }` parses
  into a TraitDecl with `generics.len() == 1`.

## Apparent bugs that aren't / explicitly deferred

- **`Ty::Dyn(SymbolId)` → `Ty::Dyn(SymbolId, Vec<Ty>)`** is a
  breaking change to the public-ish `Ty` enum. Every match site
  in the codebase was updated; the existing 534 tests stay
  green, so no behaviour regression. A future Ty-display update
  may want to show args (`dyn Iterator<Item = i64>`) but
  current display does so for the dyn case only when args are
  non-empty.
- **Generic trait + supertrait clash**. `trait Sub<T>:
  Super<T> { }` isn't tested — would need the supertrait list
  to carry trait args. Skipped for v0.x; the supertrait list is
  still `Vec<Path>` from session 054, which permits
  multi-segment paths but not generic args on the supertrait.
  This is a known gap; the next session that touches
  supertraits can lift it.
- **Generic trait + impl shape change**. `impl
  Producer<i64> for IntBox { ... }` works because the trait
  path's generic args are stored in `i.trait_path.generic_args`
  by the existing parser machinery (sessions 048/054 already
  parsed paths with generic args at impl positions). No
  resolver/checker change was needed for that side.
- **No bound on the trait's generic params**. `trait Fn1<A:
  Clone, R>` (hypothetical) — not supported. v0.x trait
  generics are unbounded.
- **No `where` clauses anywhere.** Bounds remain in the angle
  bracket syntax.
- **`dyn Trait` is still object-safe-only by structural rule.**
  Methods that return `Self::Item` still hit session 051's
  collapse diagnostic; nothing new this session adds object-
  safety machinery.

## Symbol-identity bug check

The trait's generic params are interned via
`intern_generic_param` — session 056's helper that span-keys
the SymbolId so multiple references to the same `T` (in the
trait declaration and in any `dyn Trait<T>` use site within
the trait's scope) resolve to one symbol. The
`trait_generics` map stores them in declaration order; the
checker's `dyn_method_sig` zips that with the use-site
`Ty::Dyn(t, args)` args to build the substitution. If the
trait's generic-param order ever diverged from the args order
this would silently corrupt — checked by reading the same
`trait_generics` entry on every dyn-call lookup.

## What's next

- **Closure capture + `Fn1<A, R>` trait** — the natural
  follow-through. Now that generic traits work, the prelude
  can declare `trait Fn1<A, R>` and the closure-as-struct
  design from session 057's deferred plan can land. The
  user's headline preview (`f: |x| x * mult`) becomes the
  next session's headline.
- **HashMap<K, V>** — the bigger collection.
- **Range as RangeIter struct** — polish the iterator story.
- **`continue` keyword** — the last unsupported loop primitive.
