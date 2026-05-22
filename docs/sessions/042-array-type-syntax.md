# Session 042 — Array type syntax `[T; N]`

**Date:** 2026-05-21
**Outcome:** `[T; N]` is now a writable type. Array type
annotations, array-typed function parameters, and array-typed struct
fields and enum payloads all parse and check. Because session 041
already made arrays full ARC citizens, this is a front-end change —
no new codegen. ~50 lines. 470 tests green (+5 from session 041's
465).

## The gap

Arrays existed as values — the literal `[a, b, c]`, indexing,
`.len()` — but the *type* could not be written. `parse_type` had no
case for `[`, so

```rune
let nums: [i64; 3] = [10, 20, 30];   // parse error
fn sum3(a: [i64; 3]) -> i64 { .. }   // parse error
struct Grid { cells: [Vec<i64>; 2] } // parse error
```

were all unsayable. An array type only ever arose from *inferring*
an array literal. Session 041 ran into this directly.

## Why it is front-end only

Session 041 made arrays full ARC citizens — `is_arc_type(Ty::Array)`
follows the element type, and `emit_arc_call` / `emit_release_field`
have `Array` arms that walk the element slots. And an array's
runtime representation is an 8-byte pointer — exactly the width of a
struct field slot or an enum payload slot. So once the type can be
*named*, a struct field or enum payload of array type needs no new
codegen: it stores the array pointer like any other 8-byte field.

## Pipeline

```
src/
├── ast.rs       (Type::Array { elem, len, span })
├── parser.rs    (parse_type: `[` Type `;` IntLiteral `]`)
├── resolver.rs  (resolve_type recurses into the element)
└── checker.rs   (resolve_type → Ty::Array, recorded in
                  type_resolutions)
```

## The one ARC wire-up

A struct's array field must be walked when the struct is released.
The `struct_arc_fields` filter (`lower.rs`) — which selected `Vec` /
`Str` / ARC-struct fields — now also selects an array field whose
element type is ARC, through a small recursive `field_ty_is_arc`
helper. `define_struct_release` then passes the field to
`emit_release_field`, whose `Array` arm (session 041) walks it.

Enum array payloads needed nothing: `define_enum_release` already
selects payloads by `is_arc_type`, true for an array of ARC
elements since session 041.

## What's tested

Typecheck (+2): `array_type_annotation_checks` (a `let` and a
parameter), `struct_and_enum_array_fields_check`.

Codegen (+3): `array_let_annotation_and_param`;
`struct_array_field_arc` — a `[Vec<i64>; 2]` struct field, 200
iterations, the release walks and reclaims each element;
`enum_array_payload_arc` — a `[Vec<i64>; 2]` enum payload, same.

## Apparent bugs that aren't

- **A struct or enum holding an array dangles if it escapes its
  frame.** An array is a stack slot; a struct/enum field of array
  type stores a *pointer* to that slot. Within the defining function
  frame everything is sound — construction, access, and ARC release
  all work (the tests loop 200×). But returning such a struct by
  value, or storing it somewhere longer-lived, leaves the array
  pointer dangling. This is the same root limitation bare arrays
  already have (`fn f() -> [T; N]` returns a dangling pointer). The
  fix is heap-allocated arrays — a deliberate future change.

- **`Vec<[T; N]>` is still rejected.** `vec_element_supported`
  excludes arrays — unchanged.

- **Only the array *type* `[T; N]` is new.** Repeat-form array
  literals (`[expr; N]`) are not added; an array literal is still
  the comma form `[a, b, c]`.

- **The length is a non-negative integer literal.** No
  const-expression array lengths.

## What's next

- **Heap-allocated arrays** — so an array can escape its defining
  frame soundly, making array-typed struct fields fully usable.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
