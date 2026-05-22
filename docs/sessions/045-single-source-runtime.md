# Session 045 — A single-source runtime

**Date:** 2026-05-21
**Outcome:** The Rune runtime is now one file — `runtime.c`. It had
been duplicated: ~25 Rust `rune_runtime_*` functions in `codegen.rs`
for the JIT, and a hand-transcribed C copy in `aot.rs` for AOT. The
two drifted (sessions 043 and 044 each fixed a gap that drift
caused). `build.rs` now compiles `runtime.c` into the `rune` binary
for the JIT, and the AOT path compiles the same file. ~540 lines of
duplicated Rust deleted. 479 tests green — a pure refactor.

## The duplication

The runtime — string / Vec allocators, the `struct_new` heap block,
ARC retain/release, the panic handlers — existed twice:

- **JIT**: Rust `extern "C" fn rune_runtime_*` in `codegen.rs`;
  `new_jit` registered each as a host symbol.
- **AOT**: `RUNTIME_C`, a C string in `aot.rs`, compiled and linked
  when building a target executable.

The two had to be kept layout- and semantics-identical *by hand* —
`struct rune_str`, `struct rune_vec`, every refcount operation.
Session 043 found `rune_struct_new` missing from the C side;
session 044 found `rune_weak_upgrade_or_vec` missing. Drift, twice.

## The unification

One `runtime.c`, the single source of truth.

- **JIT**: `build.rs` uses the `cc` crate to compile `runtime.c`
  and link it into the `rune` binary. `new_jit` registers each
  `rune_*` symbol's address; the JIT-compiled program calls them.
- **AOT**: `aot.rs` `include_str!`s `runtime.c` and compiles it
  when linking a target executable — the same mechanism as before,
  now sourced from the file instead of an inline string.
- **`codegen.rs`** drops the Rust runtime entirely — every
  `rune_runtime_*` function and the `RuneStr` / `RuneVec` /
  `RuneEnum` structs — keeping only an `unsafe extern "C"` block
  declaring the symbols so the JIT can take their addresses.

## Why C is the single source

AOT links through a C toolchain (`clang` / `gcc` / `cc`), so it
needs the runtime as a C-linkable artifact — that side cannot
change. Making the runtime a `.c` file unifies on it: the file is
trivially `include_str!`'d (a compile-time path) and is compiled by
both `build.rs` and the AOT linker. A Rust static library would
have needed extra machinery to deliver the `.a` to the AOT linker.

## The one new requirement

Building `rune` from source now needs a C compiler — `build.rs`
invokes one through `cc`, which finds the system toolchain. The JIT
itself still needs no C compiler at *runtime*: the runtime is baked
into the binary. AOT still needs a C toolchain at link time, as it
always has.

## Verification

A pure refactor — no new tests. The existing 479 verify it: all 241
JIT tests pass with the JIT now calling the C runtime, and all 40
AOT tests (session 044's heap-type coverage included) pass
compiling the same `runtime.c`. The drift class is closed — there
is exactly one runtime.

## Apparent bugs that aren't

- **`declare_builtin` still has dead `enum_*` arms.** The Rust
  `rune_runtime_enum_*` functions are gone (deleted with the
  runtime) and `new_jit` no longer registers `rune_enum_*` —
  `runtime.c` never had them. The four `declare_builtin` match arms
  remain, still unreachable; a cosmetic remnant, flagged for the
  separate dead-code cleanup.

## What's next

- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
