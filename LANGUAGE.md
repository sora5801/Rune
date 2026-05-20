# Rune — Language Design

Living document. Each section is tagged with a **status**:

- **Decided** — pinned down; code may depend on it.
- **Tentative** — leaning a direction, not yet committed; reversible.
- **Open** — actively undecided; options listed.

Last updated: 2026-05-19.

---

## Mission

Rune is a small, statically-typed, compiled, general-purpose programming
language. It leans toward systems use cases (predictable performance, no
mandatory GC, FFI to C) but isn't strictly an OS-level language. Native code
via Cranelift; compiler written in Rust.

**Non-goals:**
- Replacing Rust, Zig, or any existing production language.
- Maximum performance — optimization is deferred indefinitely.
- Backwards compatibility before a 1.0 milestone.

## Syntax

**Status: Decided.** Rust/Swift-flavored.

- `fn name(arg: Type) -> Ret { ... }` for functions.
- `let x: T = expr;` for immutable bindings; `let mut x: T = expr;` for mutable.
- Type annotations after `:`.
- Expression-oriented blocks (`{ ... }` evaluates to its last expression when
  unterminated by `;`).
- Curly-brace bodies; semicolon-terminated statements.
- `match` for pattern matching (`=>` for arms).
- `::` for paths, `->` for return type, `..` and `..=` for ranges.

## Numeric types

**Status: Tentative.** Rust-like sized scalars.

| Family | Types |
| --- | --- |
| Signed integers | `i8`, `i16`, `i32`, `i64` |
| Unsigned integers | `u8`, `u16`, `u32`, `u64` |
| Pointer-sized | `isize`, `usize` |
| Floating point | `f32`, `f64` |

- Default integer type for unannotated literals: `i32`.
- Default float type: `f64`.
- Numeric literal suffixes (e.g. `42i64`, `3.14f32`) will be added in the lexer
  once the type checker needs them.

**Why sized:** maps cleanly to Cranelift's primitive IR types. Reasonable for
a systems-leaning language; trivial to interop with C.

**Alternative rejected:** one unsized `Int` (`i64`) and one `Float` (`f64`).
Cleaner for a scripting language; awkward for bitfields, FFI, and predictable
memory layout.

## Mutability

**Status: Tentative.** Immutable bindings by default; opt-in mutability with
`let mut`.

- `let x = 5;` — immutable binding.
- `let mut x = 5; x += 1;` — mutable.
- Function parameters are immutable in their declaration; rebinding inside a
  function body uses ordinary `let`/`let mut`.

**Why:** matches Rust/Swift. Encourages explicit mutation. Aligns with the
parser's existing keyword set.

## Memory model

**Status: Open.** The most consequential remaining decision; shapes the type
system and lifetime story.

| Option | Story | Complexity | Note |
| --- | --- | --- | --- |
| Manual (C-like) | `alloc`/`free`, raw pointers | Low | Maximum freedom, easy to get wrong |
| Arena-only | One bump arena per region/frame | Low | Simple, fast, leaks until arena dies |
| ARC (Swift) | Implicit reference counting + weak refs | Medium | Per-op cost; cycles need user discipline |
| Borrow checker | Compile-time ownership | High | Defining feature if pursued; months of work |
| Tracing GC | Stop-the-world or concurrent collector | High | Real runtime; harder to bootstrap |

**Tentative recommendation:** start arena-only for the bootstrap. Every
function-local allocation goes into an arena freed when the function returns;
globals get their own arena. Arena lifetimes map directly to Cranelift's
stack-frame model and we sidestep designing an ownership system before we have
runnable programs.

Once code can actually run, decide whether to graduate to ARC, ownership, or
stay arena-based. This is reversible — no decision needed before codegen lands.

## Error handling

**Status: Tentative.** Result + `?`, like Rust.

```rune
enum Result<T, E> { Ok(T), Err(E) }

fn parse(s: str) -> Result<i32, ParseError> { ... }

let n = parse(input)?;
```

- No exceptions.
- No panics-as-control-flow.
- A `panic` exists for unrecoverable bugs only (out-of-bounds, etc.).

## Strings

**Status: Tentative.** UTF-8 throughout, two types.

- `str` — owned, heap-allocated, growable (rough equivalent of Rust's `String`).
- `&str` — borrowed slice into existing storage.

Indexing is **byte-indexed**. Slicing on a non-char-boundary is a runtime
panic (or a checked error — TBD). UTF-8-only avoids the UTF-16/wchar
complication.

**Open:** raw string literals (`r"..."`), triple-quoted multi-line strings,
string interpolation (`"\(expr)"` or `f"{expr}"`).

## Type system

**Status: Open.** Pinned roughly to:

- Static, nominal types.
- Type inference for local bindings; explicit return types on functions.
- User-defined `struct` and `enum` (tagged unions).
- Generics (parametric polymorphism) — yes, but not in the first iteration.
- Traits / protocols — desirable; deferred until after generics.

**Open questions:**
- Trait objects vs monomorphization. (Likely monomorphization first.)
- Higher-kinded types. (Almost certainly no.)
- Effect tracking / IO purity. (No, initially.)

## Modules and visibility

**Status: Open.** Probable shape:

- One file = one module.
- `mod name;` to declare a submodule.
- `use path::Item;` to bring into scope.
- `pub` for public visibility; private by default.

Defer concrete decisions until the parser is more than a toy.

## Compilation model

**Status: Tentative.**

- Whole-program compilation initially; no incremental builds.
- Single output: a native executable.
- No separate `.o` linker invocations from user code.
- FFI to C via `extern "C"` once we have functions working.

## Concurrency

**Status: Open.** Deliberately deferred. Threads / async are a post-v0 concern.

## Self-host

**Status: Aspirational.** Long-term goal: Rune compiles itself, written in
Rune. Far off; shouldn't influence near-term decisions.

---

## Decision log

| Date | Section | Change |
| --- | --- | --- |
| 2026-05-19 | Initial draft | Syntax decided; numerics, mutability, error handling, strings, compilation model tentative; memory model, type system, modules, concurrency open |
| 2026-05-19 | Parser implemented | Syntactic decisions pinned via implementation: Pratt precedence table, postfix `?` and `as`, `match` arm shape (`pat => expr,`), `else if` chains, expression-oriented blocks with optional trailing expression. Comparison operators currently left-associative (Rust treats them as non-associative — open). |
