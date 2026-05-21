# Session 033 — `dyn Trait` dynamic dispatch

**Date:** 2026-05-21
**Outcome:** Trait objects. `dyn Trait` lets one function dispatch to
many concrete types at runtime, through a method table. Rune's traits
were static-dispatch (monomorphized) only; now they have a dynamic
form. 439 tests green (+6 from session 032's 433).

## The headline

```rune
trait Shape {
    fn area(self: dyn Shape) -> i64;
}
struct Circle { r: i64 }
impl Shape for Circle { fn area(self: Circle) -> i64 { self.r * self.r * 3 } }
struct Square { side: i64 }
impl Shape for Square { fn area(self: Square) -> i64 { self.side * self.side } }

fn describe(s: dyn Shape) -> i64 { s.area() }   // one function, any Shape

fn main() -> i64 {
    describe(Circle { r: 10 }) + describe(Square { side: 5 })   // 325
}
```

`describe` is compiled once and dispatches `area` to whichever
concrete type it's handed — the opposite of the monomorphized
`fn show<T: Display>` form, which compiles one copy per type.

## The representation

Rune codegen assumes every value is 8 bytes (an integer, or a pointer
to a heap descriptor). A trait object is logically a *fat* pointer —
data + method table — which is 16 bytes and doesn't fit. So a `dyn
Trait` value is **boxed**: an 8-byte pointer to a heap cell

```
[ fnptr_0, fnptr_1, ..., fnptr_{N-1}, data_ptr ]
```

— the trait's `N` method pointers in declaration order, followed by
the concrete value's pointer. The method pointers *are* the vtable;
they live inline in each box (a per-instance table) rather than in a
shared static vtable. That trades a little duplication for avoiding a
first-time use of Cranelift's `DataDescription::write_function_addr`
— `func_addr` (taking a function's address as a value) and
`call_indirect` are plain instructions and lower-risk.

## Coercion: concrete → `dyn`

A concrete struct that implements `T` becomes a `dyn T` at three
sites: `let` bindings, call arguments, and `return`. The checker's
`check_assignable` — called at each of those sites instead of a bare
`compatible` check — sees "expected `dyn T`, got `struct C`",
verifies `C` provides every method `T` declares (`struct_impls_trait`),
and records the coercion in `CheckResults::dyn_coercions`, keyed by
the expression's span.

The lowerer, in `lower_expr`, checks that map: a coerced expression
is wrapped in `HirExprKind::DynBox { value, struct_sym, trait_sym }`.

Codegen of `DynBox`: heap-allocate the cell, take each impl method's
address with `func_addr`, store them, store the data pointer.

## Dispatch: a method call on a `dyn`

`s.area()` where `s: dyn Shape` — the lowerer sees the `Ty::Dyn`
receiver and emits `HirExprKind::DynCall { receiver, trait_sym,
method, args }` instead of the usual `MethodCall`.

Codegen of `DynCall`:
- load the data pointer from the box's last slot — this is `self`;
- load the method pointer from slot `index` (the method's position
  in the trait);
- build the call signature `(self, args...) -> result` from the
  argument and result types;
- `call_indirect`.

`HirModule::trait_methods` (ordered method names per trait) gives
both the slot count `N` and a method's `index`.

## Pipeline

```
src/
├── token.rs / ast.rs / parser.rs  (`dyn` keyword, Type::Dyn)
├── ty.rs          (Ty::Dyn(trait_sym))
├── resolver.rs    (resolve_type recurses into `dyn` paths)
├── checker.rs     (resolve_type Dyn arm; check_assignable +
│                   struct_impls_trait; dyn_method_sig;
│                   CheckResults.dyn_coercions)
├── hir.rs         (DynBox, DynCall; HirModule.trait_methods)
├── lower.rs       (apply coercions; MethodCall on dyn -> DynCall)
├── monomorphize.rs(DynBox/DynCall arms in all six expr walks)
└── codegen.rs     (compile_dyn_box, compile_dyn_call;
                    func_addr + call_indirect)
```

## What's tested

Codegen (+3): `dyn_dispatch_two_impls` (one function, two concrete
types), `dyn_let_binding` (coercion at a `let`), `dyn_method_with_arg`
(a trait-object method taking an argument).

Typecheck (+3): `dyn_trait_typechecks`,
`dyn_non_implementing_struct_rejected`, `dyn_of_non_trait_rejected`.

## Apparent bugs that aren't

- **Trait objects leak.** The `dyn` box, and the concrete value it
  wraps, are never freed — `is_arc_type(Ty::Dyn)` is `false`. v0.x:
  the patterns that matter (a `dyn` call argument; a `dyn` local
  whose backing value out-lives it) never use-after-free, but they do
  leak. A proper drop needs a vtable drop slot — a follow-up.

- **The method table is per-instance, not shared.** Each `DynBox`
  rebuilds the table with `func_addr` + stores. A shared static
  vtable would be tidier; the per-instance form keeps codegen to
  plain instructions. Negligible for v0.x program sizes.

- **A trait method's `self` type is written but ignored.** A trait
  declares `fn area(self: dyn Shape)`; the `self` annotation is
  skipped by signature resolution (only the parameters *after*
  `self` matter). At the ABI level `self` is always an 8-byte
  pointer. Writing `self: dyn Shape` in the trait is the honest
  choice; the impls write their concrete `self: Circle`.

- **Coercion only at `let` / call-arg / `return`.** `Vec<dyn Shape>`
  needs a coercion at a method-argument position (`push`) — not wired
  this session. `dyn` values themselves are 8-byte pointers, so the
  *type* works in a `Vec`; only the coercion site is missing.

- **`dyn#N` in error messages.** A trait object displays as `dyn#N`
  (the trait's symbol id), like `struct#N` for structs — the
  anonymous-type rough edge, not `dyn`-specific.

## What's next

- **ARC for trait objects** — a vtable drop slot so the box and its
  data reclaim.
- **`Vec<dyn Trait>`** — coercion at method-argument positions, for
  heterogeneous collections.
- **A shared static vtable** via `write_function_addr`.
- **Supertraits, associated types, generic impls.**
