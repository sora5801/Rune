# Session 043 — Heap-allocated arrays

**Date:** 2026-05-21
**Outcome:** Arrays are heap blocks, not stack slots. An array can
now escape the frame that built it — returned by value, or carried
in a struct that escapes — without dangling. Arrays join the
heap-ARC family (structs / Vec / enums / dyn boxes): a refcounted
block with a synthesized release function. 473 tests green (+3 from
session 042's 470).

## The problem

`compile_array` allocated a `create_sized_stack_slot`; an array
value was a pointer *into the current frame*. So

```rune
fn make() -> [i64; 3] { [1, 2, 3] }   // returns a dangling pointer
struct Box { xs: [Vec<i64>; 2] }      // xs dangles once Box escapes
```

were unsound. Sessions 041 and 042 documented this each time; the
fix was always "heap-allocate arrays".

## The change

`compile_array` now allocates with `rune_struct_new(field_size)` —
the same heap allocator structs, payload enums, and `dyn` boxes use
— a block of `N` element slots followed by a trailing rc. An array
is now a heap pointer, safe to return and to store.

Arrays join the heap-ARC family. The shape — a refcounted heap block
plus a synthesized release function, declared in Pass 0 and defined
in Pass 3 — already served structs (`__rune_release_struct$`),
payload enums, Vecs, and `dyn` boxes. Arrays are the fifth:
`__rune_release_array$<ty>`, one per distinct array type, the set
gathered by `collect_array_tys` in the monomorphizer.

- `is_arc_type(Ty::Array)` is now unconditionally true — every heap
  array is a refcounted block, even `[i64; 3]`. (It used to follow
  the element type.)
- `emit_arc_call` retain bumps the block's rc inline, like a struct;
  release dispatches to the synthesized function — decrement, and at
  zero walk the ARC elements then `struct_dealloc`.
- Array copy (`let b = a`) retains the *block* — in session 041,
  with stack arrays, a copy retained each element instead.
- `array_field_size` rounds the element area up to 8 bytes so the
  trailing rc word stays aligned even for narrow element types.

## Two AOT gaps this exposed

Both pre-existed; making every array ARC-managed surfaced them.

1. **The AOT test harness never monomorphized.** `tests/aot.rs`
   went `lower_module` → `build_object`, skipping
   `monomorphize_module` — so `array_tys` (and `vec_arc_elem_tys`)
   were empty. It had worked only because no AOT test used generics
   or an ARC-managed `Vec`, and arrays weren't ARC. Now every array
   needs a release function, so the harness must monomorphize, the
   way `main.rs` already does.

2. **The AOT C runtime lacked `rune_struct_new` /
   `rune_struct_dealloc`.** Heap structs, payload enums, and `dyn`
   boxes had simply never been exercised in AOT. Heap arrays need
   those two allocators, so they are added to `RUNTIME_C`.

## What's tested

Codegen (+3): `array_returned_by_value` — an array returned from a
function and indexed in the caller; `struct_with_array_escapes` — a
struct with an array field returned by value, 200×;
`heap_array_of_vecs_returned` — an array of `Vec`s returned, its
elements reclaimed when the array's rc hits zero.

The existing AOT array tests (`aot_array_sum`, `print_in_loop`,
`aot_concat_in_loop`, `array_index_*`) now exercise heap arrays
end-to-end through the linker.

## Apparent bugs that aren't

- **The AOT runtime is still incomplete.** It has the `str` / `Vec`
  / `weak` functions and now `rune_struct_new` /
  `rune_struct_dealloc`, but not the enum ARC functions
  (`rune_enum_new`, `rune_retain_enum`, …). Payload enums therefore
  still won't AOT-link — a pre-existing gap, untested in AOT, left
  for the session that adds AOT enum coverage. Heap arrays needed
  only the two struct allocators.

- **Empty arrays remain unsupported** — `compile_array` still
  rejects a zero-element literal.

## What's next

- **Complete the AOT runtime** — the enum ARC functions, so payload
  enums and heap structs link ahead-of-time.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
