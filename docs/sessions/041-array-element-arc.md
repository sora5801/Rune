# Session 041 — Array element ARC

**Date:** 2026-05-21
**Outcome:** Arrays of ARC-managed elements no longer leak. An array
local releases each element when it leaves scope, `compile_array`
takes ownership of a borrowed element, and a copied array retains.
The last structural reclaim hole the ARC sessions left open. ~30
lines. 465 tests green (+3 from session 040's 462).

## The hole

An array is a **stack slot** — `compile_array` calls
`create_sized_stack_slot` and stores the elements inline; an array
*value* is the slot's address. No descriptor, no refcount. So
`is_arc_type` returned `false` for `Ty::Array`, and an array slipped
past every piece of ARC:

```rune
let arr = [make_vec(), make_vec(), make_vec()];
// three fresh Vecs, rc 1 each — never released
```

Sessions 036–040 reclaimed every *other* temporary; arrays were
called out each time as the one structural gap.

## The keystone

```rust
Ty::Array(elem, _) => is_arc_type(elem, ..),
```

An array is ARC-managed exactly when its elements are. This one line
folds arrays into *everything*: an array local becomes a
scope-tracked `arc_local`; an array call argument is reclaimed by
owned call arguments (036); an array `match` scrutinee, method
receiver, and discarded statement value are all cleaned up — with no
further code, because that machinery is all keyed on `is_arc_type`.

## Walking the slots

An array has no refcount to bump. So `emit_arc_call` and
`emit_release_field` gain an `Array` arm: retaining or releasing an
array **walks its `N` element slots** and applies the action to
each. `N` is static, so the walk is unrolled; the recursion handles
nesting (`[[Vec; 2]; 3]`).

```rust
if let Ty::Array(elem, n) = ty {
    for i in 0..*n {
        let ev = load(elem_cty, value, i * esize);
        self.emit_arc_call(action, elem, ev)?;   // recurse
    }
}
```

## Ownership at construction and copy

`compile_array` now retains a borrowed-`Local` element — the array
slot becomes a second owner — while a fresh producer transfers its
`+1` straight in. The array analog of struct-field initialization
and `Vec::push`.

A copy, `let b = a`, duplicates the array *pointer*: `a` and `b`
alias the same slot. ARC-on-copy retains (the `Array` arm walks and
retains every element); both bindings release at scope exit, so the
counts balance.

## Composition

Arrays now ride the whole 036–040 pipeline through `is_arc_type`:

- `f([make_vec()])` — the array argument temporary is released after
  the call by owned call arguments.
- `arr[0].len()` — the index read retains (037), the receiver-temp
  release drops it (038).
- `[v, v]` — `v` borrowed into two slots, retained twice; the
  array's release returns both, the binding releases the last.

## What's tested

Codegen (+3):

- `array_elements_released` — `[make(1), make(2), make(3)]` 200×;
  every `Vec` reclaimed at scope exit.
- `array_of_borrowed_local` — `[shared, shared]`, one `Local` in two
  slots; the construction retains balance the scope-exit releases.
- `array_copy_retains` — `let b = a` aliases the slot; the copy
  retains so each binding's release is balanced.

## Apparent bugs that aren't

- **Struct and enum fields of array type.** A struct's array field
  is not walked on release — the `struct_arc_fields` filter
  (`lower.rs`) lists `Vec` / `Str` / nested struct, not `Array` — so
  `struct S { a: [Vec; 2] }` still leaks `a`. The `emit_release_field`
  `Array` arm *is* in place, so an enum array payload (which
  `define_enum_release` selects via `is_arc_type`) releases
  correctly; the struct-field filter is the remaining gap.

- **Arrays returned by value dangle.** `fn f() -> [Vec; 2] { [..] }`
  returns the address of `f`'s stack frame — broken before this
  session, unchanged by it.

- **Array types are inference-only.** `[T; N]` is not parseable as a
  type annotation; an array type only ever arises from inferring an
  array literal. Orthogonal to ARC.

## What's next

- **Array-typed struct fields and enum payloads** — extend the
  `struct_arc_fields` walk to `Ty::Array`.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
