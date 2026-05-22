# Session 046 — Dead enum-runtime cleanup

**Date:** 2026-05-21
**Outcome:** The last unreachable `enum_*` runtime remnants are gone —
four `declare_builtin` match arms and two `arc_helper_name` arms in
`src/codegen.rs`. Pure dead-code removal: 479 tests still green, no
behaviour change.

## Background

Session 044 flagged that `declare_builtin` carried `enum_new` /
`enum_dealloc` / `retain_enum` / `release_enum` arms that codegen
never emits a call to, and deferred a "separate cleanup". Session 045
(the single-source-runtime refactor) then deleted the entire Rust
runtime from `codegen.rs` — including the `rune_runtime_enum_*`
functions and the `RuneEnum` struct — and dropped the `new_jit`
symbol registrations with it. But a refactor that *moves* the runtime
is not a dead-code pass: session 045 left the dead arms in place and
re-flagged them under "apparent bugs that aren't". This session
removes what is left.

## Why the code was dead

Payload enums were introduced in session 020 with a dedicated heap
layout — a 24-byte `RuneEnum { tag, payload, rc }` descriptor — and
runtime helpers `rune_enum_new`, `rune_enum_dealloc`,
`rune_retain_enum`, `rune_release_enum`. The same session then
unified enum allocation onto the struct allocator (`rune_struct_new`)
and moved release to a codegen-synthesized per-enum function,
`__rune_release_enum$<sym>`. The helpers were "retained as a
fallback" that nothing fell back to. A payload enum today:

- **constructs** via `rune_struct_new`,
- **retains** by an inline rc bump in `emit_arc_call`,
- **releases** through the synthesized `__rune_release_enum$<sym>`.

No path touches `enum_new`, `enum_dealloc`, `retain_enum`, or
`release_enum`.

Since session 045 the `declare_builtin` arms were not merely dead but
mildly hazardous: each builds an `unsafe extern "C"` import
declaration for a symbol — `rune_enum_new` and friends — that
`runtime.c` does not define. Harmless while unreachable (the arm is
never selected, so the import is never declared), but a link failure
waiting for anyone who wired up a call to them by mistake.

## What was removed

All in `src/codegen.rs`:

1. The four `declare_builtin` arms `enum_new`, `enum_dealloc`,
   `retain_enum`, `release_enum`.
2. The two `arc_helper_name` arms `("retain", Ty::Enum(_, _))` and
   `("release", Ty::Enum(_, _))`.

A stale comment in `define_enum_release` ("`enum_dealloc` is now
redundant") lost its dangling reference too.

The `rune_runtime_enum_*` functions, the `RuneEnum` struct, and the
`new_jit` symbol registrations were already gone — session 045
removed them with the rest of the Rust runtime.

## Verifying the `arc_helper_name` arms were unreachable

`arc_helper_name` is the generic fallback both ARC dispatchers end
with, so removing a *live* arm would turn a real call into a
`CodegenError`. It has exactly two callers:

- **`emit_release_field`** — its `Ty::Enum` arm *always* returns
  early: via the synthesized `__rune_release_enum$<sym>` for a
  payload enum, or `Ok(())` for a tag-only enum (a bare i64, no heap
  descriptor to release). It never falls through to the trailing
  `arc_helper_name("release", ty)`.

- **`emit_arc_call`** — its `Ty::Enum` arm is guarded by
  `enum_has_payload.contains(sym)` and handles both `retain` and
  `release` for a payload enum, returning. A *tag-only* enum would
  fall through — but `emit_arc_call` only ever receives a `Ty::Enum`
  through an `is_arc_type` guard, which for an enum *is*
  `enum_has_payload.contains(sym)`; its one unguarded caller,
  `compile_dyn_box`, coerces a struct (the `DynBox` HIR node carries
  a `struct_sym`). A tag-only enum never reaches `emit_arc_call` at
  all.

So no `Ty::Enum` of either kind reaches `arc_helper_name`. With the
arms gone, a hypothetical `Ty::Enum` hits the `_ => Err(...)`
fallback — the honest result for a type the function is never asked
about.

## What's tested

Nothing new — this is a removal. The existing 479 tests (40 AOT, 241
codegen, 21 lexer, 54 parser, 123 typecheck) all still pass, which is
the assertion that matters: enum construction, retain, release,
`match` destructure, and AOT linking are all exercised and unchanged.

`README.md` needed no edit — it describes enums functionally (the
runtime-internal helper names never appeared there) and the
per-suite test counts are unchanged.

## What's next

Unchanged from session 045's list:

- `dyn` coercion at struct-literal fields and enum payloads.
- Supertraits, associated types, generic impls.
