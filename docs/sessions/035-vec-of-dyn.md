# Session 035 — `Vec<dyn Trait>`

**Date:** 2026-05-21
**Outcome:** Heterogeneous trait-object collections. A
`Vec<dyn Shape>` holds boxed `Circle`s and `Square`s side by side;
`push` coerces a concrete struct to a trait object, `get` hands one
back, and releasing the Vec reclaims every box and the struct it
wraps. Three small changes — sessions 033 (`dyn`) and 034 (`dyn`
ARC) had already built everything else. 447 tests green (+5 from
session 034's 442).

## The headline

```rune
fn main() -> i64 {
    let mut shapes: Vec<dyn Shape> = vec_new();
    shapes.push(Circle { r: 10 });    // coerces Circle -> dyn Shape
    shapes.push(Square { side: 5 });  // coerces Square -> dyn Shape
    let mut total = 0;
    let mut i = 0;
    while i < shapes.len() {
        let s: dyn Shape = shapes.get(i);
        total = total + s.area();      // dispatches per element
        i = i + 1;
    }
    total                              // 125
}
```

## The one new thing: coercion at a method argument

Session 033 wired `dyn` coercion at three sites — `let`, call
arguments, `return` — through the checker's `check_assignable`,
which both *accepts* a `struct C → dyn T` coercion and *records* it
in `CheckResults::dyn_coercions` (keyed by span). Everywhere else,
type checking used the bare `Ty::compatible`, which knows nothing of
coercion.

`v.push(Circle { r: 10 })` checks the argument against `push`'s
parameter type — and `push` on a `Vec<dyn Shape>` has parameter type
`dyn Shape`. That check lived in `check_method_call` and used
`compatible`, so a concrete struct argument was rejected.

The fix is one substitution: `check_method_call` now checks each
argument with `check_assignable` instead of `compatible`.
`check_assignable` is a strict superset — it tries `compatible`
first — so no existing method call changes behaviour; the only new
acceptance is `struct → dyn`. Every method argument is now a
coercion site, `push` being the one that matters here.

## Why the lowerer and codegen needed nothing

`lower_expr` applies `dyn_coercions` **by span, for every
expression** — it checks the map at its entry and wraps a hit in
`HirExprKind::DynBox`. So once the checker records a coercion at the
`push` argument's span, the lowerer boxes it with no `push`-specific
code.

Codegen's `compile_method_call` Vec arm was already generic over the
element type: it reads `cranelift_type(elem)`, `is_arc_type(elem)`,
and `elem_size(elem)` — all of which gained `Ty::Dyn` arms in
sessions 033/034. A `dyn` element is an 8-byte pointer, so it drops
straight into the Vec's 8-byte slot. `push` of a fresh `DynBox`
transfers the box's `+1` into the slot; `push` of a `dyn` *local*
retains (a borrowed element gets a second owner); `get` retains the
box it returns. Nothing new.

## ARC for the elements

Releasing a `Vec<dyn Shape>` has to release each boxed trait object.
The per-element release function `__rune_release_vec$<elem>` is
synthesized for every ARC-managed Vec element type — collected,
after monomorphization, by `scan_ty_for_vec_elems`, which gates on
`is_arc_mono`. That predicate did not list `Ty::Dyn`; adding it is
the third and last change.

With it, `Vec<dyn Shape>` records `dyn Shape` as an ARC element type
and codegen synthesizes `__rune_release_vec$dyn<N>`. Its body walks
the live elements calling `emit_release_field(Ty::Dyn, ..)` — which
dispatches to `__rune_release_dyn$Shape` (session 034). Reclaim is
three layers deep:

```
release Vec<dyn Shape>
  -> __rune_release_vec$dynShape   walk elements
       -> __rune_release_dyn$Shape   per box: rc--, at 0 drop slot
            -> __rune_release_struct$Circle   the concrete struct
```

## The whole change

```
checker.rs
  vec_element_supported   + Ty::Dyn  (Vec<dyn T> is a valid type)
  check_method_call       compatible -> check_assignable
                          (coercion at method-argument positions)
monomorphize.rs
  is_arc_mono             + Ty::Dyn  (synthesize __rune_release_vec$dyn)
```

Three edits. No new functions, no parser/resolver/HIR/lowerer
change.

## What's tested

Codegen (+3):

- `vec_of_dyn_dispatch` — two concrete types in one `Vec<dyn Shape>`,
  dispatched in a loop (125).
- `vec_of_dyn_reclaimed` — 200 iterations each build, fill, and drop
  a `Vec<dyn Shape>`; a double free in the three-layer release would
  crash.
- `vec_of_dyn_push_existing_dyn` — `push` of a `dyn` local (a
  borrowed element) retains, so the box has two owners and both
  releases net out.

Typecheck (+2):

- `vec_of_dyn_typechecks` — `Vec<dyn Shape>` is a valid type and a
  conforming struct coerces at `push`.
- `vec_of_dyn_rejects_non_impl` — pushing a non-implementing struct
  is rejected (no coercion available).

## Apparent bugs that aren't

- **A `get(i)` result used inline still leaks.** `shapes.get(i)`
  retains the box it returns; bound to a `let` that box is
  scope-tracked, but `shapes.get(i).area()` discards the retained
  temporary unreleased. This is the call-argument / temporary leak
  class — the same one tracked for owned call arguments, not
  specific to `Vec<dyn>`.

- **Coercion still misses struct-literal fields and enum payloads.**
  `Holder { s: Circle { .. } }` where the field `s: dyn Shape`, or
  `Some(Circle { .. })` at `Option<dyn Shape>`, do not coerce —
  those positions don't run `check_assignable`. `let` / call-arg /
  `return` / method-arg do.

- **`vec_new()` still yields a `Vec<i64>` placeholder.** The
  annotation on `let mut shapes: Vec<dyn Shape>` refines it;
  `Ty::compatible` treats any Vec as compatible with any Vec, and
  codegen drives off the binding's declared type. Same as every
  other `Vec<T>` since session 028.

## What's next

- **Owned call arguments** — release ARC argument temporaries after
  the call, closing the last v0.x leak class (session 036).
- **Coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
