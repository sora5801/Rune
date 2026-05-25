# Session 087 — Intrinsic impls for primitive types

**Date:** 2026-05-25
**Outcome:** `impl Numeric for i64 { ... }` works.
Lifts the "impl only on structs" resolver restriction;
primitives are now first-class participants in
trait-bound generic code, closing session 084's
deferred half. 401 codegen tests green (+3 from
session 086).

```rune
impl std::Numeric for i64 {
    fn add(self: i64, other: i64) -> i64 { self + other }
    fn lt(self: i64, other: i64) -> bool { self < other }
}

fn smaller<T: std::Numeric>(a: T, b: T) -> T {
    if a.lt(b) { a } else { b }
}

fn main() -> i64 { smaller(50, 30) }   // → 30
```

## The decisive observation

The pieces existed; the v0.x resolver just rejected
non-struct impl targets.

### 1. Primitives are already `BuiltinType` syms

The resolver init interns `i8`, `i16`, ..., `i64`,
`f32`, `f64`, `bool`, `char`, `str` as
`SymbolKind::BuiltinType(Ty)` entries. The
`resolve_path("i64")` call in `declare_impl` finds
that sym fine — the only problem was the immediately-
following `matches!(.., SymbolKind::Struct)` guard,
which rejected anything not interned as a Struct.

Lift the guard to also accept
`BuiltinType(Ty::Int(_) | Ty::Float(_) | Ty::Bool |
Ty::Char | Ty::Str)`. With that single relaxation, the
rest of `declare_impl` runs — registering the impl's
methods in `impl_methods[(anchor_sym, name)]` exactly
like a struct impl would.

### 2. Receiver dispatch via primitive anchor lookup

`user_method_sig_with_args` and
`check_method_args_bidirectional` (both in checker)
plus the lowerer's MethodCall arm all gated on
`Ty::Struct(s, _)` for impl_methods lookup. For
primitive receivers we need to map back from the
primitive `Ty` to its anchor sym.

Added `Resolutions::primitive_anchor(ty: &Ty) ->
Option<SymbolId>` that walks `symbols` looking for a
`BuiltinType(ty)` match. Cheap — ~15 primitives, linear
scan. Both checker callers and the lowerer dispatch
through this helper.

### 3. Monomorphize plumbing

Mono's `resolve_method_calls` is what fires after a
generic `<T: Numeric>` spec pins T to a concrete
receiver — including primitives. It needs the same
anchor lookup but doesn't have access to the
`Resolutions`. Solution: a new `HirModule
::primitive_anchors: HashMap<Ty, SymbolId>` pre-built
at lower time (one linear scan over symbols) and
threaded through `resolve_method_calls` /
`resolve_method_calls_in_expr` as an extra parameter.

The extra plumbing was the bulk of the diff —
`resolve_method_calls_in_expr` recurses into every
HIR variant, so every call site (~25 internal call
sites) gains the new arg.

## The wire-ups

```
src/resolver.rs       (declare_impl accepts BuiltinType
                       primitive targets;
                       Resolutions::primitive_anchor
                       helper.)

src/hir.rs            (HirModule::primitive_anchors
                       field.)

src/lower.rs          (populates primitive_anchors at
                       module-construction time;
                       MethodCall arm uses anchor
                       lookup for primitive receivers.)

src/checker.rs        (user_method_sig_with_args and
                       check_method_args_bidirectional
                       use the anchor lookup for
                       primitive receivers.)

src/monomorphize.rs   (resolve_method_calls /
                       resolve_method_calls_in_expr
                       take primitive_anchors as an
                       extra param and route primitive
                       receivers through the same
                       impl_methods lookup.)

tests/codegen.rs      (+3 tests: i64 impl through a
                       generic `<T: Numeric>` fn,
                       i64 impl's .add called through
                       the bound, direct .lt call on
                       a primitive receiver.)
```

## What's tested

Codegen (+3):

- `numeric_impl_on_i64_primitive` — `impl Numeric for
  i64` body uses native `+` / `<`; `smaller<T:
  Numeric>(a, b)` dispatches `.lt(b)` through the
  bound after mono specializes T → i64.
- `numeric_impl_on_i64_combined` — same shape,
  exercises `.add` through the bound. `sum_two(7,
  8)` = 15.
- `numeric_primitive_method_direct_call` — `5.lt(7)`
  called outside any generic context. Resolver +
  checker + lowerer all dispatch through the i64
  anchor sym uniformly.

## Apparent bugs that aren't / explicitly deferred

- **`.into()` on a primitive receiver still rejected**
  via the resolver's "impl only on structs" path —
  wait, no, that's lifted now. Actually `impl
  Into<X> for i64` works structurally. Untested. The
  session 086 disambiguation should fire fine via
  the same anchor lookup. Future test.
- **Generic primitive impls** — `impl<T> SomeTrait
  for i64 { ... }` with T appearing in method
  signatures. Should work but untested; impl
  generics scope inside declare_impl orthogonally.
- **Float and bool / char / str impls** — accepted
  by the resolver in this session but not tested.
  Same dispatch path, no special handling needed.
- **Trait conformance check on primitives** — when a
  user writes `impl SomeTrait for i64` but omits a
  required method, the conformance check in the
  checker walks `impls_for` and validates against
  the trait's signatures. Currently `impls_for` is
  keyed by the impl's anchor sym; the check should
  fire the same way for primitives. Untested
  exhaustively.
- **`f32` / `f64` arithmetic in impl bodies** — works
  because `+` / `<` on float operands emit `fadd` /
  `fcmp` in codegen. The Numeric body using
  `self + other` and `self < other` doesn't care
  whether Self is integer or float.
- **`.sum` / `.fold(init, +)` over `<T: Numeric>`** —
  still requires a "default value" mechanism that
  v0.x traits can't express (no const fns). The
  Numeric trait shape here doesn't include a `.zero`
  method; adding one wouldn't help `.sum`'s body
  call it without an instance to call it on. Future
  work needs trait const fns or static-method-on-
  type syntax.

## What's next

- **Numeric literal suffixes** — `10i32`, `3.14f32`,
  `42u64`. Together with this session's primitive
  impls, would unlock natural typed numeric workloads.
- **Cartesian-product exhaustiveness for tuple
  patterns** — session 082's deferred item.
- **Same-target Into duplicate detection** — session
  086's deferred item.
- **Self-hosted bootstrap** — long-term.
