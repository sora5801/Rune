# Session 044 — Complete the AOT runtime

**Date:** 2026-05-21
**Outcome:** Every program that JIT-compiles now also AOT-compiles.
The AOT C runtime gained `rune_weak_upgrade_or_vec` — the one
runtime function codegen could emit but the runtime never defined —
and six AOT tests now cover the heap-ARC types end to end. 479 tests
green (+6 from session 043's 473).

## Background

`rune build` links the compiled object against `RUNTIME_C`, a
hand-written C runtime embedded in `src/aot.rs`. Session 043 found
it lacked `rune_struct_new` / `rune_struct_dealloc` and added them;
the roadmap then said "complete the AOT runtime" — finish the audit.

## The audit

`declare_builtin` is the authoritative list of runtime functions
codegen can call. Comparing it against `RUNTIME_C` — and verifying
empirically by AOT-building a program for each heap type:

- **Payload enums, structs, `Vec`, `dyn` objects, generics already
  AOT-link and run.** They allocate with `rune_struct_new` /
  `rune_struct_dealloc` (added in 043) plus the long-standing
  `str` / `vec` runtime. Session 043's note that "payload enums do
  not yet AOT-link" was wrong: an enum uses `struct_new` and its
  synthesized per-enum release — never the `rune_enum_*` functions.

- **`rune_weak_upgrade_or_vec` was the one real gap.** `upgrade_or`
  lowers to a call to it, and `RUNTIME_C` never defined it — so any
  program using `upgrade_or` failed to link.

## The fix

One C function, ported faithfully from the Rust
`rune_runtime_weak_upgrade_or_vec`: return the weak target if it is
still alive (`rc > 0`), otherwise retain and return the default so
the caller owns a strong reference either way.

## Why the gap persisted

The AOT test suite exercised only integers, strings, control flow,
and (since 043) arrays — never a payload enum, a `Vec`, a struct, a
`dyn` object, `upgrade_or`, or a generic call. Nothing forced
`RUNTIME_C` to stay complete. Six AOT tests now do — payload enum,
`Vec` push/get, struct fields, `dyn` dispatch, `weak` + `upgrade_or`,
and a generic identity function — each built, linked, and run.

## What's tested

AOT (+6): `aot_payload_enum`, `aot_vec_push_get`,
`aot_struct_fields`, `aot_dyn_dispatch`, `aot_weak_upgrade_or`,
`aot_generic_identity`.

## Apparent bugs that aren't

- **`declare_builtin` still has dead `enum_*` arms.** `enum_new`,
  `enum_dealloc`, `retain_enum`, `release_enum` are declared (and
  the JIT registers `rune_enum_*`, and the Rust `rune_runtime_enum_*`
  functions and the `RuneEnum` struct exist) — but codegen emits no
  call to any of them. Enum construction uses `struct_new`; enum
  release uses the synthesized `__rune_release_enum$<sym>`. Dead,
  harmless, flagged for a separate cleanup.

- **`RUNTIME_C` is a hand transcription of the Rust runtime.** The C
  and Rust sides must be kept layout- and semantics-compatible by
  hand — exactly the drift that opened this gap. A sturdier design
  (generate the runtime, or link a static library built from the
  Rust runtime) is a future option.

## What's next

- **`dyn` coercion at struct-literal fields and enum payloads.**
- **A single-source runtime** — remove the C/Rust transcription gap.
- **Supertraits, associated types, generic impls.**
