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

**Status: Decided.** Rust-like sized scalars.

| Family | Types |
| --- | --- |
| Signed integers | `i8`, `i16`, `i32`, `i64` |
| Unsigned integers | `u8`, `u16`, `u32`, `u64` |
| Pointer-sized | `isize`, `usize` |
| Floating point | `f32`, `f64` |

- **Default integer type for unannotated literals: `i64`** (pinned 2026-05-19).
- **Default float type: `f64`** (pinned 2026-05-19).
- Numeric literal suffixes (e.g. `42i64`, `3.14f32`) will be added in the lexer
  once the type checker needs them.

**Why `i64` over `i32`:** Rune targets 64-bit native via Cranelift; word-sized
default keeps array indexing and `usize` interop ergonomic. Swift made the
same choice with `Int`. Rust's `i32` default is historical (32-bit-friendly,
pre-2015 era) and forces noisy casts for any sized container.

**Why sized:** maps cleanly to Cranelift's primitive IR types. Reasonable for
a systems-leaning language; trivial to interop with C.

**Alternative rejected:** one unsized `Int` (`i64`) and one `Float` (`f64`).
Cleaner for a scripting language; awkward for bitfields, FFI, and predictable
memory layout.

## Mutability

**Status: Decided.** Immutable bindings by default; opt-in mutability with
`let mut`. Enforcement is **strict** — assignment to an immutable binding is
a type error, not a warning.

- `let x = 5;` — immutable binding. `x = 6;` is rejected.
- `let mut x = 5; x += 1;` — mutable. Compound assignment also requires `mut`.
- Function parameters are immutable in their declaration. To mutate a
  parameter's value inside the function body, rebind with `let mut`.

**Why:** matches Rust/Swift. Encourages explicit mutation. The `mut` keyword
isn't decorative — the type checker rejects writes to immutable bindings.

## Name resolution

**Status: Decided.** Lexical scoping with shadowing allowed.

- Innermost scope first, then enclosing scopes, then the module, then
  built-ins (primitive type names).
- **Shadowing within the same scope is allowed.** `let x = 1; let x = "hi";`
  rebinds `x` to a new binding (possibly with a different type). Each
  `let` creates a fresh symbol; references between the two declarations
  resolve to whichever was most recently declared.
- Forward references between top-level items work (two-pass resolution:
  collect declarations first, then resolve bodies).

**Why allow shadowing:** retains Rust's idiomatic stepwise refinement
(`let x = parse(); let x = x?;`). Banning it would also break the natural
`let mut x = ...; let x = x;` pattern for "freeze this after building it."

## Memory model

**Status: Tentative.** Stack-frame arena for now; heap and ownership deferred.

Concrete state as of 2026-05-19:

- **Arrays are stack-allocated** at the point of literal. A `let xs = [1, 2, 3];`
  allocates a Cranelift `StackSlot` sized `3 * sizeof(i64) = 24 bytes` and the
  binding holds the slot's address. Lifetime is the function frame.
- **Arrays cannot escape a function** — no array returns, no array params
  yet. The codegen errors on either.
- **No heap allocator wired up.** No `Box`, no `Vec`, no `String`.
- **No bounds checks on indexing.** `arr[i]` trusts `i`. Adding checks is
  cheap; deferred until errors-as-values are designed.

| Option | Story | Complexity | Note |
| --- | --- | --- | --- |
| Manual (C-like) | `alloc`/`free`, raw pointers | Low | Maximum freedom, easy to get wrong |
| Arena-only | One bump arena per region/frame | Low | Simple, fast, leaks until arena dies — **current direction** |
| ARC (Swift) | Implicit reference counting + weak refs | Medium | Per-op cost; cycles need user discipline |
| Borrow checker | Compile-time ownership | High | Defining feature if pursued; months of work |
| Tracing GC | Stop-the-world or concurrent collector | High | Real runtime; harder to bootstrap |

**Tentative recommendation:** continue arena-only. Lift "stack-frame arena"
to "explicit named arenas" once we want arrays escaping their function. Heap
+ ownership is a v2 conversation.

Once code can actually run end-to-end with stdlib, decide whether to graduate
to ARC, ownership, or stay arena-based. Reversible.

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

**Status: Tentative.** UTF-8 throughout, one type: `str`.

Implementation as of 2026-05-19:

- `str` is a **fat pointer** — a 16-byte (`ptr`, `len`) descriptor.
- The descriptor lives on the **function's stack frame** (current memory
  model is stack-frame arena; same as arrays).
- String **literals** have their bytes embedded in the object's data
  section via `cranelift_module::declare_data`. The descriptor's `ptr`
  is a relocation to that static data; `len` is a constant.
- Strings are **immutable** in this iteration. No concat, no methods,
  no indexing-into-str (slicing UTF-8 needs char-boundary care).
- **Equality** (`==`/`!=`) is implemented — codegen routes through a
  runtime `rune_str_eq` that does length compare + memcmp.
- **`print_str(s: str) -> ()`** builtin prints the bytes followed by a
  newline. (Future: unify with `print(i64)` once we have overloading
  or generics.)

Empty strings (`""`) compile to a descriptor with `ptr = null` and
`len = 0`. The runtime checks `len == 0` before dereferencing, so
the null is safe.

**Not yet:**

- Owned/borrowed split (`String` vs `&str`). May never split if a single
  immutable type is good enough.
- Concatenation. Would require heap allocation — the next memory-model
  conversation.
- `.len()`, `.bytes()`, indexing, slicing. Methods aren't codegen'd at
  all yet.
- Raw string literals (`r"..."`), triple-quoted multi-line strings,
  interpolation (`"\(expr)"` or `f"{expr}"`).
- Returning strings from functions. Stack-allocated descriptor means
  it can't escape its frame.

Indexing semantics, once added, will be **byte-indexed**. Slicing on a
non-char-boundary is a runtime panic (or a checked error — TBD).

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

**Status: Decided.**

- **Backend:** Cranelift. Two output modes:
  - `cranelift-jit` (in-memory codegen) for `rune run` — fastest feedback.
  - `cranelift-object` + external C linker driver for `rune build`,
    producing a native executable. `--release` flips Cranelift's opt
    level from `none` to `speed`.
- **ABI:** the target's default native calling convention — SystemV
  on Linux, WindowsFastcall on Windows, AAPCS on ARM. Effectively
  `extern "C"`. Trivial C interop later; a Rune-specific CC isn't
  planned for v0.x.
- **Entry point:** `fn main() -> i64`. The host calls main and prints
  the returned i64. `fn main() -> ()` will become valid once a
  `print` builtin lands.
- **Pipeline:** AST → HIR → Cranelift IR → machine code. The HIR is
  AST-shaped with types attached (not MIR-style basic blocks); features
  we can't codegen yet (method calls, field access, arrays, match,
  cast, try) are funneled into an `Unsupported` variant.
- **Optimization:** Cranelift's `opt_level = "none"` for fast compile.
  Switch to `speed` once correctness is settled.
- Whole-module codegen, no incremental compilation.
- No user-side FFI yet — will land alongside an `extern "C"` keyword
  on function items.

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
| 2026-05-19 | Resolver + type checker | Three previously-tentative decisions pinned: default integer type → `i64` (was `i32` placeholder); mutability enforcement → strict (immutable bindings can't be reassigned); name resolution → lexical scoping with shadowing allowed. Bottom-up monomorphic type checker landed. |
| 2026-05-19 | HIR + Cranelift codegen | Compilation model promoted to Decided. Cranelift JIT backend, target-native ABI (`extern "C"`), `fn main() -> i64` as entry. AST-shaped HIR with `Ty` on every node (over MIR/CFG). First runnable Rune: `rune run examples/fib.rn` prints `55`. |
| 2026-05-19 | AOT executables | `rune build <file>` produces a native `.exe` via `cranelift-object` + external C linker driver. Default linker discovery: `clang` → `gcc` → `cc`, overridable via `$RUNE_LINKER`. Rune's `main` is renamed internally to `__rune_main`; a synthesized `int main(void)` calls it and truncates the i64 return to a 32-bit OS exit code. `rune build examples/fib.rn && ./fib.exe; echo $?` → `55`. |
| 2026-05-19 | print + floats + --release + arrays/for | First host builtin: `print(i64)`, callable from both JIT (registered via `JITBuilder::symbol`) and AOT (embedded `RUNTIME_C` compiled inline by the linker driver). Float codegen tests landed (paths existed, now exercised). `rune build --release` maps to Cranelift `OptLevel::Speed`. Array literals stack-allocated via `StackSlot`; indexing via `iadd + load`; `for x in arr` desugars to a counter-based while loop. Memory model promoted Open → Tentative: stack-frame arena. |
| 2026-05-19 | Strings | `str` as a 16-byte (ptr, len) descriptor; descriptor on the function's stack frame, bytes in the object's data section via `cranelift_module::declare_data`. `print_str(s: str)` builtin and `==`/`!=` for strings (runtime `rune_str_eq`: length compare + memcmp). Immutable; no concat, no methods, no slicing. Empty strings use `ptr = null + len = 0`; the runtime checks `len == 0` before dereferencing. `examples/hello_world.rn` prints "Hello, world!". |
