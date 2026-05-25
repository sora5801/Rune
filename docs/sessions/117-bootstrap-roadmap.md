# Session 117 — Self-hosted bootstrap roadmap

**Date:** 2026-05-25
**Outcome:** Docs-only session. Maps the path from
today's Rust-hosted Rune compiler to a self-hosted
Rune that compiles itself. Identifies what's
missing, proposes a phased approach, and names the
smallest viable bootstrap (SVB).

No code changes. Tests unchanged: 446 codegen + 223
typecheck + 56 lexer + 40 lib + 21 ast + 144
parser = 930 total.

## Why self-host?

A language that can't compile itself relies forever
on its bootstrap host. Self-hosting is the proof
that the language is expressive enough to write
non-trivial systems software — the compiler is the
ultimate dogfooding target. It also liberates Rune
from Rust as a build dependency, opens the door to
compiler features that introspect Rune's own
semantics, and creates a stable reference
implementation written in the same language users
read.

Practically: the Rune compiler today is ~25k lines
of Rust + ~1k lines of C (the ARC runtime) + ~700
lines of Rune (std.rn). A self-hosted compiler
would shift the bulk to Rune itself, leaving the C
runtime and a minimal Rust-side shim (parser / codegen
entry point or a tree-walking interpreter bootstrap).

## Where we are (sessions 1–116)

Current Rune is a small statically-typed compiled
language with:

**Type system**: i8–i64 / u8–u64 / isize / usize / f32 /
f64 / bool / char / str / [T;N] / Vec\<T\> / HashMap\<K,V\>
/ Option / Result / (A,B,C) tuples / `dyn Trait` /
generic structs and enums / `fn(A) -> R` pointer types
/ closures via Fn1 / Fn2.

**Traits**: declaration, generic params with bounds,
associated types, supertraits, default-body methods,
Numeric impls for all primitives.

**Inference**: hint flow through let-bindings, fn-args,
struct-fields, binops, method-call receivers, `.into()`,
unary-neg, integer + float literal suffixes.
Bidirectional closure inference.

**Diagnostics**: integer + float literal range
checks, compound const-eval (across let bindings),
divide-by-zero, shift-out-of-range, subnormal-as-zero,
duplicate Into targets, polished type names in errors.

**Operators**: full arithmetic / comparison / logical
/ bitwise / shift, all compound assignments
(`+= -= *= /= %= <<= >>= &= |= ^=`).

**Runtime**: ARC + weak refs, str descriptors,
HashMap (open addressing, tombstones, str + i64 keys),
Vec with float elements, tuple per-shape release walks.

**Control flow**: if / else / while / for-in
(over Iterator or range or array) / match (with
tuple + or-patterns + cartesian exhaustiveness +
per-arm unreachability) / break / continue / `?`
operator with From-based err conversion.

**Two backends**: JIT (`rune run`) and AOT
(`rune build` → native .exe via cranelift-object +
external C linker).

## What's missing for self-hosted compilation

### Tier A — blocks even the parser (must-have)

1. **File I/O.** `read_file(path: str) -> Result<str,
   Err>` and `write_file(path: str, contents: str)
   -> Result<Unit, Err>`. Currently no way to read
   source code from disk; `print` is the only I/O.
2. **String manipulation primitives.**
   - `.split(sep: str) -> Vec<str>` — tokenize lines /
     paths / arguments.
   - `.starts_with(prefix: str) -> bool` /
     `.ends_with(suffix: str) -> bool`.
   - `.find(needle: str) -> Option<i64>` (byte offset).
   - `.chars() -> CharsIter` — iterate Unicode
     codepoints. Or `.byte_at(i) -> u8` for the
     simpler ASCII-only path.
   - `.to_string() -> str` for integers / floats
     (currently print_i64 lives in C; the language
     has no `format!`).
3. **Command-line args.** `std::env::args() -> Vec<str>`.
   The compiler entry point needs to read `argv`.

### Tier B — large but mechanical

4. **Module system at file granularity.** Today
   everything's in `main.rs` + `std.rn`. The
   self-hosted compiler will need to span many
   `.rn` files (lexer.rn, parser.rn, checker.rn, ...).
   Needs `mod foo;` declarations, file-to-module
   resolution, and cross-file visibility (`pub`).
5. **Format strings.** Either a `format!("{}", x)`
   macro (heavy: would need a macro system) or a
   builder-style API (`StringBuilder::new().append_int
   (x).append_str(": ").build()`). The latter is
   v0.x-shaped — no new language features needed
   beyond mutable strings.
6. **Mutable string builder.** `let mut s = String::
   new(); s.push_str("hi"); s.push_char('!');` —
   currently `str` is immutable. Either lift the
   restriction or add a distinct `String` type
   (mutable, heap-grown).
7. **Process exit codes.** `std::process::exit(code:
   i32)`. Today `fn main() -> i64` returns an exit
   code, but a long-running compiler may want to
   exit early on errors.

### Tier C — language features the compiler would lean on

8. **`Box<T>` or equivalent for recursive types.**
   The AST has self-referential nodes (`Expr` contains
   `Expr`). Today Rune uses heap allocation implicitly
   for `Struct` types (which carry pointers); a recursive
   enum like `Expr::Binary { lhs: Expr, rhs: Expr }`
   currently fails because the layout would be infinite.
   Solution: implicit boxing of recursive variants, or
   explicit `Box<T>` smart pointer.
9. **Pattern guards** (`p if cond => ...`). Useful
   for match arms that need to inspect bound values
   beyond shape.
10. **`let ... else`** for early-exit binding.
    Reduces ladder nesting in parser code.
11. **Methods that return `&str` / borrowed slices.**
    Today `str` is always owned; the substring
    operation either allocates or doesn't exist.
    For the compiler's hot path (lexer tokenizing
    a large file), repeated heap allocation of token
    text would be slow.

### Tier D — performance & ergonomics (post-MVP)

12. **Variadic generics or builtin tuple methods**
    (`zip`, `enumerate` on iterators yielding tuples).
13. **Const generics** for fixed-size arrays.
14. **Inline assembly** or platform-specific intrinsics
    (probably never; not Rune's niche).
15. **Lifetimes**. Rune is value-semantics + ARC, no
    lifetimes today. Probably stays that way through
    1.0; the cost is occasional unnecessary ARC
    increments.

### Tier X — the Cranelift bridge

The hard one. To emit native code, the self-hosted
Rune needs to *call* Cranelift. Three paths:

- **(a) FFI to Cranelift's C API.** Cranelift doesn't
  ship a stable C API; this would require maintaining
  bindings.
- **(b) Emit Rust source** that calls Cranelift —
  transpiler approach. Compiler becomes Rune → Rust
  → executable.
- **(c) Emit Cranelift IR text** (`.clif`), shell out
  to a small Rust binary that reads it and links.
  Smallest interface, slowest pipeline.
- **(d) Emit assembly or machine code directly.** The
  largest project; Rune writes its own backend.
- **(e) Tree-walking interpreter as bootstrap.** No
  codegen at all — the self-hosted compiler reads
  Rune, builds an AST, walks it directly. Slow but
  correct. Useful as Phase 1.

The intended path is **(e) → (b) → (d)**: start with
an interpreter (proves the language can express the
compiler), then move to transpilation for native
performance (lower-risk than embedding Cranelift),
then eventually a real backend.

## Phased approach

### Phase 1: capability buildout (sessions 118–~140)

Add Tier A and the easier parts of Tier B / C.
Concrete sessions (rough order):

- 118: File I/O builtins (`read_file`, `write_file`).
- 119: String methods (`split`, `starts_with`,
  `find`, `byte_at`).
- 120: `std::env::args()`.
- 121: Mutable `String` type (heap-grown,
  push_str / push_char).
- 122: Format-style methods on Numeric (`.to_str()`).
- 123: Module system at file granularity.
- 124: `Box<T>` for recursive types (or implicit
  boxing of recursive enum variants).
- 125: Pattern guards.
- 126: `let ... else`.
- 127: Process exit codes.

After Phase 1: Rune is expressive enough to be a
serious systems language. A user can write a JSON
parser, a small key-value store, a text-processing
CLI, all in pure Rune.

### Phase 2: interpreter bootstrap (sessions ~141–~170)

Write a Rune-in-Rune tree-walking interpreter.
Sub-stages:

- 141–145: Lexer in Rune. Produces `Vec<Token>` from
  a `str` input. Probably ~500 lines of Rune.
- 146–155: Parser in Rune. AST mirrors the Rust
  AST closely. ~1500 lines.
- 156–165: Resolver + type-checker in Rune. The
  meatiest phase. ~3000 lines.
- 166–170: Tree-walking evaluator. Run hello-world
  through the Rune compiler running on the Rust
  compiler. ~500 lines.

Milestone: `rune-rune fib.rn` (the Rune compiler
written in Rune, hosted by the Rust compiler) can
execute fib.rn. Self-recognition: feed it std.rn
plus the compiler's own source and watch it
typecheck itself (correctness check, not yet
self-compilation).

### Phase 3: transpiler bootstrap (sessions ~171–~200)

Replace the tree-walking evaluator with a Rune-side
HIR builder + Rust-source emitter. The Rust source
is a thin wrapper around Cranelift that imports
from the existing Rust-side codegen.rs.

Milestone: `rune-rune build fib.rn` produces an
executable fib.exe identical to what the Rust-hosted
compiler produces. Self-hosting in the build sense:
the Rust-hosted compiler is no longer used for
codegen, only for the initial bootstrap that compiled
rune-rune.

### Phase 4: full self-hosting (sessions ~201+)

Replace the Rust-side codegen.rs with a Rune-side
equivalent. Either ship our own backend (giant
project) or maintain Cranelift FFI from Rune.

Milestone: `rune-rune build rune-rune.rn` produces
a new rune-rune.exe. The Rust dependency is gone
(except for Cranelift, if path (a)).

## Smallest viable bootstrap (SVB)

What's the minimum to claim "Rune is self-hosting"?

**SVB = Phase 2 + tiny piece of Phase 3.** A Rune
program written entirely in Rune can:

1. Read a `.rn` file.
2. Lex it into tokens.
3. Parse to AST.
4. Type-check.
5. Either evaluate (interpreter) or emit Cranelift
   IR text (.clif) to a file.

Step 5's `.clif` path is the cheap escape: a 200-line
Rust shim reads the `.clif` file and shells out to
Cranelift's CLI to produce an object file, then C
linker turns it into an exe. The Rune compiler itself
emits nothing but text.

This is the smallest thing that lets us say "Rune
compiles itself" with a straight face. The Rust-side
remains as a glue layer, not a compiler.

Estimated effort: ~80 sessions of work after this
roadmap. ~6 months at the current pace.

## Risks and unknowns

- **Recursive types.** Tier C item 8 — without
  implicit boxing or `Box<T>`, no recursive AST.
  This blocks Phase 1. Might require a substantial
  redesign of how Rune handles struct layout.
- **Module system.** Rune's resolver doesn't currently
  span multiple files (std.rn is special-cased).
  Cross-file imports + visibility is non-trivial.
- **Performance.** A tree-walking interpreter that's
  100x slower than native-compiled Rune is fine for
  bootstrapping but unbearable for daily use.
  Transition to Phase 3 (transpilation) is gated on
  not being annoying to use.
- **The Cranelift interface.** Even path (b)
  transpilation requires the Rune-side compiler to
  *understand* enough of Cranelift's API to drive
  it via Rust source. Either we hand-write the
  Cranelift glue once and treat it as runtime, or
  we generate it.
- **Test infrastructure.** Self-hosting needs Rune
  to express its own test harness. Today tests are
  Rust functions calling `run_main(src)`; the
  Rune-side equivalent doesn't exist.
- **Error reporting.** Today errors carry Rust
  `Span { start, end }` indices. Source-line
  reporting works because the Rust harness has the
  source string. In a self-hosted compiler, the
  Rune side needs equivalent infrastructure.

## What's *not* on the path

- **Async / concurrency**. Rune is single-threaded
  v0.x; the compiler is single-threaded too. Not
  needed for bootstrap.
- **Macros / metaprogramming.** Nice to have, not
  needed. `format!`-style would be the only macro;
  builder API works as a substitute.
- **Reflection / runtime type info.** No need.
  Static dispatch through monomorphization is
  enough.
- **Garbage collection.** Rune's ARC is already
  the memory model. No change.
- **`unsafe` / raw pointers.** Tempting (especially
  for the Cranelift bridge) but a slippery slope.
  Defer until forced.

## What's next

- **Session 118 (next): File I/O builtins.** The
  first concrete capability step. `read_file` /
  `write_file` are the foundation; without them
  no further bootstrap work is possible.
- **Session 119: String methods.** Once a file
  contents arrives as a `str`, the lexer needs to
  walk it.
- **Session 120+**: per Phase 1 plan.
