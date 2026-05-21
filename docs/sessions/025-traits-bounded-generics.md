# Session 025 — Traits + bounded generics

**Date:** 2026-05-20
**Outcome:** Traits land with static dispatch. `trait` declarations,
`impl Trait for Type`, and `<T: Bound>` bounded generics all work
end-to-end. A bounded generic function calling a trait method gets
the call resolved per-instantiation by the monomorphizer. 375 tests
green (+7 from session 024's 368).

## The headline

```rune
trait Tag {
    fn tag(self: A) -> i64;
}

struct A { v: i64 }
struct B { v: i64 }

impl Tag for A {
    fn tag(self: A) -> i64 { 1 }
}
impl Tag for B {
    fn tag(self: B) -> i64 { 2 }
}

fn id_tag<T: Tag>(x: T) -> i64 {
    x.tag()                  // resolves per specialization
}

fn main() -> i64 {
    let a = A { v: 0 };
    let b = B { v: 0 };
    id_tag(a) * 10 + id_tag(b)   // 12
}
```

`id_tag` is monomorphized twice — `id_tag$$A` and `id_tag$$B`. Each
specialization has `x.tag()` rewritten to a direct call into the
respective impl method.

## Pipeline walk

### Parser

- New `trait` keyword.
- `trait Name { fn sig; fn sig; }` — `parse_trait` reads method
  *signatures* (params + optional return type + `;`, no body).
- `impl Trait for Type { ... }` — `parse_impl` parses the first
  path, peeks for `for`, and if present treats the first path as
  the trait and parses a second for the type. `impl Type { ... }`
  (inherent) still works — `trait_path` is `None`.
- `<T: Display>` / `<T: A + B>` — `parse_optional_generic_params`
  now produces `GenericParam { name, bounds }` instead of bare
  `Ident`. Bounds are `+`-separated trait names.

AST additions:
```rust
Item::Trait(TraitDecl)
TraitDecl  { vis, name, methods: Vec<TraitMethodSig>, span }
TraitMethodSig { name, params, return_type, span }
ImplBlock.trait_path: Option<Path>
GenericParam { name: Ident, bounds: Vec<Ident> }
```

### Resolver

- `SymbolKind::Trait`.
- `declare_item` stashes a trait's method signatures into
  `Resolutions::trait_methods` (trait sym → `Vec<TraitMethodSig>`).
- `resolve_fn`: for each `<T: Bound>`, resolve `Bound` to a trait
  symbol and record `T_sym → [Bound_sym]` in `generic_bounds`.
- `declare_impl`: for a trait impl, resolve the trait path and
  verify it's actually a trait. The impl's methods register into
  the same `impl_methods` table that inherent methods use —
  `(type_sym, method_name) → method_fn_sym`. This means a method
  call on a concrete type resolves identically whether the method
  is inherent or trait-provided.

### Checker

- `check_trait_impl_conformance`: every trait method must have a
  matching impl method; arities must agree. (Full param-by-param
  type conformance with `Self` substitution is a follow-up — v0.x
  checks arity only.)
- `trait_bound_method_sig`: when a method call's receiver is a
  `Ty::TypeVar(t)` and `t` has trait bounds, search the bounds'
  declared methods for the call. Returns the method's signature
  (minus the leading `self`) so the call type-checks.
- Trait method signature types are run through `resolve_type` in
  pass 1b so `type_resolutions` has entries before any function
  body is checked.

### Monomorphizer

The key insight: a trait method call on a generic receiver can't be
resolved at lowering time (the receiver's type is still `TypeVar`).
It survives as `HirExprKind::MethodCall`. After monomorphization
substitutes `T → A`, the receiver's type becomes `Ty::Struct(A, ...)`.

`resolve_method_calls` runs on every concrete function (originals +
specializations) after the call-rewrite pass. For each `MethodCall`
whose receiver is now a concrete struct/enum:
```rust
if let Some(&fn_sym) = impl_methods.get(&(struct_sym, method)) {
    e.kind = Call { callee: fn_sym, args: [receiver, ...args] };
}
```

Builtin method calls (`str.len()`, `vec.push()`, array `.len()`)
have no `impl_methods` entry and are left as `MethodCall` for
codegen to dispatch the usual way.

`HirModule` gains `impl_methods: HashMap<(SymbolId, String),
SymbolId>`, copied from `Resolutions` by the lowerer.

### Codegen

No changes. By the time codegen runs, every trait method call is a
plain `Call`. Builtin `MethodCall`s dispatch as before.

## Why static dispatch

Every trait call is monomorphized to a direct function call. A
generic function called with N concrete types produces N
specializations, each with its trait calls resolved.

The alternative — dynamic dispatch via vtables (`dyn Trait`) —
isn't needed for the common case and adds a pointer-indirection
plus a fat-pointer representation. It's left open; the static path
is the v0.x choice and matches Rust's default (`impl Trait` /
generic bounds monomorphize; `dyn Trait` is opt-in).

## What's tested

Codegen (+3):
- `trait_impl_concrete_method_call` — trait impl on a concrete
  type; `p.mag_sq()` resolves through `impl_methods` at lowering.
- `trait_bounded_generic_static_dispatch` — `describe<T: Sized>`
  calls `x.size()`; monomorphized for Point.
- `trait_bounded_generic_two_impls` — one bounded generic, two
  implementing types, two specializations dispatching correctly.

Parser (+4):
- `parses_trait_decl`, `parses_trait_impl`,
  `parses_bounded_generic`, `parses_multi_bound_generic`.

All 368 prior tests still pass.

## File layout changes

```
src/
├── token.rs        (Trait keyword)
├── ast.rs          (Item::Trait, TraitDecl, TraitMethodSig,
│                    ImplBlock.trait_path, GenericParam)
├── parser.rs       (parse_trait; parse_impl handles `for`;
│                    parse_optional_generic_params → GenericParam
│                    with `+`-separated bounds)
├── resolver.rs     (SymbolKind::Trait; trait_methods +
│                    generic_bounds maps; declare_impl resolves
│                    trait paths; resolve_fn records bounds)
├── checker.rs      (check_trait_impl_conformance;
│                    trait_bound_method_sig; trait sig types
│                    resolved in pass 1b)
├── hir.rs          (HirModule.impl_methods)
├── lower.rs        (g.name.span for the new GenericParam shape;
│                    HirModule.impl_methods populated)
└── monomorphize.rs (resolve_method_calls rewrites MethodCall →
                     Call on concrete receivers)
tests/
├── parser.rs       (+4)
└── codegen.rs      (+3)
LANGUAGE.md         (Traits section promoted Open → Decided;
                     decision-log row)
```

## Apparent bugs that aren't

- **Trait method signatures use a concrete `self` type
  (`fn tag(self: A)`), not `Self`.** v0.x doesn't have a `Self`
  type. The trait declaration's `self` param is written with the
  expected implementing type. It's a bit unusual but keeps the
  type machinery simple — a real `Self` type is a follow-up.

- **Conformance checks arity, not types.** `check_trait_impl_
  conformance` ensures the impl has each method with the right
  parameter *count*. Full type conformance (each param type
  matches the trait's declared type, with `Self` substituted)
  needs the `Self` type first. Documented as a follow-up.

- **`impl Display for Point` requires `Point` to be a concrete
  struct.** Generic impls (`impl<T> Display for Box<T>`) aren't
  supported — the resolver's `declare_impl` rejects non-struct
  type paths. v0.x scope.

- **A bounded generic with two impls produces two specializations,
  each with a distinct mangled name.** `id_tag$$S<n>` etc. The
  trait calls inside each are independently resolved.

- **The `eat_keyword("for")` helper matches the existing `For`
  token.** `for` is already a keyword (loop syntax); reusing the
  token for `impl Trait for Type` is unambiguous because the
  parser is in item-parsing context, not expression context.

## What's next

- **`dyn Trait`** — dynamic dispatch via vtables, for collections
  of mixed types.
- **Supertraits** (`trait Ord: Eq`).
- **Associated types** and constants.
- **Generic impls** (`impl<T> Trait for Box<T>`).
- **Full conformance checking** with a real `Self` type.
- **Stdlib** — now genuinely unblocked. Traits let `Vec<T>`,
  `HashMap<K: Hash + Eq, V>`, an iterator protocol, etc. be
  expressed. Still needs a module system for `use std::*`.
