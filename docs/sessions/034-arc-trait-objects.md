# Session 034 — ARC for trait objects

**Date:** 2026-05-21
**Outcome:** Trait-object boxes reclaim. Session 033 shipped `dyn
Trait` dispatch but the box, and the concrete value it wrapped,
leaked. Now the `dyn` box is an ARC type: it carries a refcount and
a drop slot, so a `dyn` local frees itself and its boxed value at
scope exit. 442 tests green (+3 from session 033's 439).

## The headline

```rune
trait Shape { fn area(self: dyn Shape) -> i64; }
struct Circle { r: i64 }
impl Shape for Circle { fn area(self: Circle) -> i64 { self.r * self.r } }

fn main() -> i64 {
    let mut total = 0;
    let mut i = 0;
    while i < 50 {
        let s: dyn Shape = Circle { r: 3 };   // boxed each iteration
        total = total + s.area();
        i = i + 1;
    }                                          // box + Circle freed here
    total
}
```

In session 033 that loop leaked 50 boxes and 50 `Circle`s. Now each
iteration's `s` is reclaimed when the body block ends — the box, and
the `Circle` inside it.

## The drop slot

Session 033's box was `[fnptr_0, .., fnptr_{N-1}, data]` — the N
method pointers, then the concrete data pointer. It was allocated
with `struct_new((N+1)*8)`, which appends an 8-byte rc slot like
every heap descriptor; that rc went unread.

ARC needs two things the old box lacked: the rc must actually be
used, and *releasing* the box must release the concrete value it
wraps. The release happens generically — at a scope exit the codegen
only knows the static type `dyn Shape`, not `Circle`. So the box
must carry, at runtime, a way to drop its own data. That is the
**drop slot**: a function pointer to the concrete struct's release.

```
[ fnptr_0, .., fnptr_{N-1}, data, drop, rc ]
   0          (N-1)*8       N*8  (N+1)*8 (N+2)*8
```

`compile_dyn_box` now allocates `struct_new((N+2)*8)` and, after
storing the data pointer, stores `func_addr` of the boxed struct's
synthesized release function (`__rune_release_struct$<sym>`) into
the drop slot. `struct_new` puts rc=1 at the end as always.

`compile_dyn_call` is untouched — data still lives at slot N and the
method pointers at `0..N`; the drop slot sits *past* data, out of the
dispatch path.

## The release function

A `dyn` box gets the same treatment as a struct, enum, or `Vec<T>`:
a per-type release function synthesized in codegen, declared in
`compile_module` pass 0 and defined in pass 3.

`__rune_release_dyn$<trait>` — keyed by trait, since the box layout
(the slot count N) is the trait's method count:

```
fn __rune_release_dyn$T(box):
    rc = box[(N+2)*8]; rc -= 1; box[(N+2)*8] = rc
    if rc > 0: return
    data = box[N*8]
    drop = box[(N+1)*8]
    call_indirect (i64)->() drop(data)   // the struct's release
    struct_dealloc(box, (N+2)*8)
```

The indirect call lands in `__rune_release_struct$Circle`, which
decrements the `Circle`'s own rc and, at zero, walks its ARC fields
and frees it. Two ARC layers, each minding its own refcount.

## Who owns the +1

A `dyn` box **owns a +1** on its boxed data — it will drop it. The
question is whether that +1 is fresh or borrowed:

- `let s: dyn Shape = Circle { r: 3 };` — the struct literal is a
  fresh `+1` producer. The box consumes that `+1`; no retain.
- `let s: dyn Shape = c;` where `c: Circle` is a local — the coerced
  expression is a borrowed `Local` read. `compile_dyn_box` retains
  the data, so both `c` and the box hold a `+1` and both releases
  net out.

This is the exact heuristic `compile_stmt`'s `let` already uses for
ARC-on-copy: retain iff the initializer is a `Local`. `is_arc_type`
returning `true` for `Ty::Dyn` then makes the rest fall out for
free — a `dyn` local is scope-tracked, retained on copy (`let b =
a`), and retained when returned.

## Pipeline

Entirely `src/codegen.rs` — `dyn` was wired through the front end in
session 033, and ARC is a codegen/runtime concern.

```
codegen.rs
├── Codegen / FnCodegen   (new field: dyn_release_funcs)
├── compile_module        (pass 0 declare, pass 3 define
│                          __rune_release_dyn$<trait>)
├── define_dyn_release    (new — rc--, call_indirect drop, dealloc)
├── is_arc_type           (Ty::Dyn => true)
├── emit_arc_call         (Ty::Dyn: retain rc++ inline,
│                          release -> dyn_release_funcs)
├── emit_release_field    (Ty::Dyn arm — for completeness)
└── compile_dyn_box       (cell (N+2)*8; store drop fnptr;
                           retain borrowed data)
```

## What's tested

Codegen (+3):

- `dyn_box_released_each_iteration` — 50 loop iterations each box a
  `dyn` and drop it; a double free in the synthesized release would
  crash.
- `dyn_box_copy_shares_refcount` — `let b = a` on a `dyn` local;
  ARC-on-copy retains the shared box, so two scope-exit releases net
  one free.
- `dyn_box_from_local_retains_data` — coercing a borrowed `Circle`
  local; the box must retain it, since both the local and the box
  release it.

A leak is invisible to a functional test; a *double free* or
use-after-free crashes the JIT'd process. Each test exercises a
distinct rc path and asserts a concrete result — the program
completing with the right answer is the proof.

## Apparent bugs that aren't

- **A `dyn` call-argument temporary still leaks.** `describe(Circle
  { r: 10 })` boxes a `dyn` for the argument; `describe`'s parameter
  is *borrowed* (callee params are never scope-tracked), so nobody
  releases the box. This is not a `dyn` bug — it is the pre-existing
  convention that *every* ARC call-argument temporary leaks. Fixing
  it is a language-wide caller-cleanup change, out of scope here.

- **The method table is still per-instance.** Each `DynBox` rebuilds
  the table with `func_addr` + stores; the drop slot is rebuilt the
  same way. A shared static vtable would be tidier — unchanged from
  session 033's reasoning.

- **No drop slot for non-struct data.** Only structs implement
  traits in Rune, so the boxed value is always a struct and the drop
  fn is always `__rune_release_struct$<sym>`. If trait impls ever
  extend to other types this slot generalizes naturally.

- **`emit_release_field` has a `Ty::Dyn` arm that nothing reaches
  yet.** A struct field or enum payload of `dyn` type would use it,
  but neither is constructible — there is no coercion at a
  struct-literal-field or variant-payload position. The arm keeps
  the release machinery uniform across all ARC types for when those
  coercion sites land.

## What's next

- **`Vec<dyn Trait>`** — coercion at method-argument positions, for
  heterogeneous trait-object collections. The element ARC already
  works (`__rune_release_vec$<dyn>` would synthesize); only the
  coercion site is missing.
- **Owned call arguments** — releasing ARC call-argument temporaries
  after the call, closing the last v0.x leak class.
- **A shared static vtable** via `write_function_addr`.
- **Supertraits, associated types, generic impls.**
