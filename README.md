# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

**Pre-alpha — full pipeline lex → parse → check → codegen, both JIT and AOT.**
Arrays + for loops, host `print(i64)` builtin, and `--release` AOT mode all
work. No heap allocator yet, no generics.

```
$ rune build examples/hello_world.rn --release && ./hello_world.exe
rune: linked with clang -> hello_world.exe
Hello, world!
$ rune run examples/greet.rn
Hello, world!
Hello, Rune!
Hello, Cranelift!
greeted (count, total bytes):
3
42
$ rune build examples/primes.rn --release && ./primes.exe ; echo $?
rune: linked with clang -> primes.exe
2 3 5 7 11 13 17 19
77
```

## Implementation state

### Lexer — done

- Keywords: `let`, `mut`, `fn`, `return`, `if`, `else`, `while`, `for`, `in`,
  `break`, `continue`, `true`, `false`, `struct`, `enum`, `match`, `pub`,
  `const`, `as`
- Identifiers (ASCII)
- Integer literals: decimal, hex (`0x`), binary (`0b`), octal (`0o`); `_`
  digit separators
- Float literals: fractional part with optional `e`/`E` exponent
- String literals with `\n \t \r \\ \' \" \0` escapes
- Char literals with the same escape set
- All single- and multi-char operators: `+ - * / %`, `== != < > <= >=`,
  `&& || !`, `& | ^ ~ << >>`, `-> => :: .. ..=`, `+= -= *= /= %=`, `? .`
- Delimiters: `( ) { } [ ]`, `, ; :`
- Line comments and **nested** block comments
- UTF-8 source input
- Byte-offset spans on every token
- Error recovery
- 21 integration tests

### Parser — done

- Items: `fn`, `struct`, `enum`, `const` (with optional `pub`)
- Statements: `let`, expression statements, items inside blocks
- Expressions: literals, paths, parenthesized, unary, all binary operators
  with Pratt precedence, assignment and compound assignment, postfix
  (call/method/field/index/`?`/`as`), block expressions, `if`/`else if`/`else`,
  `while`, `for ... in ...`, `match` with arms and guards, `return`,
  `break`, `continue`, array literals
- Patterns: wildcard, identifier (with `mut`), literal
- Types: paths only
- Error recovery at item-starting keywords
- 37 integration tests

### Resolver — done

- Two-pass: declare top-level items, resolve bodies. Forward references
  between items work.
- Built-in type names pre-populated (`bool`, `char`, `str`, `i8`–`i64`,
  `u8`–`u64`, `isize`/`usize`, `f32`/`f64`).
- Lexical scoping with same-scope shadowing allowed.

### Type checker — done

- Primitives + inferred array types.
- Unannotated integer literals default to `i64`; floats to `f64`.
- `let` checks annotation vs initializer; mutability is strictly enforced.
- Arithmetic / comparison / logical / bitwise / unary checked.
- `if`/`else` branches unify; `while`/`if`-without-else require unit body.
- Function calls check arity and argument types.
- `as` casts allowed between numeric / bool / char / integer pairs.
- 40 integration tests.

### HIR + Cranelift codegen — done

- AST-shaped HIR (`src/hir.rs`) with `Ty` on every node; paths resolved to
  `SymbolId`. Unsupported variants funneled into `Unsupported(msg)`.
- Lowering pass at `src/lower.rs`.
- Cranelift codegen (`src/codegen.rs`) generic over `Module` —
  parameterized backend used by both JIT and AOT paths.
- Covers:
  - Integers (i8/i16/i32/i64 + unsigned + isize/usize), floats (f32/f64),
    bool — arithmetic, comparison, bitwise, shifts, unary.
  - Short-circuit `&&`/`||`, `if`/`else` (expression form, `else if`
    chains), `while`, `let` with mutability, assignment and compound
    assignment.
  - Rune-to-Rune function calls (forward references, recursion, mutual
    recursion), early `return`.
  - **Array literals** stack-allocated via Cranelift `StackSlot`,
    **indexing** via address arithmetic + `load`, **`for x in arr`**
    desugared to a counter-based while loop.
  - **String literals** as a 16-byte (ptr, len) descriptor — bytes in
    the object's data section, descriptor on the function's stack.
    **`==`/`!=`** for strings via runtime `rune_str_eq`.
    **`+` / `+=`** (concatenation) via runtime `rune_str_concat` that
    mallocs a fresh descriptor + buffer; result is process-lifetime
    heap (no free yet).
  - Host builtins: **polymorphic `print(x)`** dispatches on argument
    type to `print_i64` (for any int) or `print_str` (for `str`).
    Explicit-typed variants `print_i64` and `print_str` remain callable
    directly. All three are registered with `JITBuilder::symbol` for
    JIT and defined in the embedded C runtime for AOT.
  - **Method calls** dispatch on `(receiver_ty, method_name)`. First
    three methods: `str.len()` and `str.is_empty()` (inline `load` +
    optional `icmp`), `arr.len()` (static constant from the array's
    type). The mechanism extends to future methods.
  - **String indexing** (`s[i]`) reads one byte and zero-extends to
    `i64`. **String slicing** (`s[a..b]`, `s[a..=b]`) heap-allocates a
    fresh substring (clamps out-of-range bounds, never panics).
  - **Range iteration**: `for i in 0..n { }` and `for i in 1..=n { }`
    work via a counter-based loop.
  - **String predicates**: `s.starts_with(p)`, `s.ends_with(p)`,
    `s.contains(p)` via runtime calls.
  - **Structs** with field access: `struct Point { x: i64, y: i64 }`,
    constructed via `Point { x: 1, y: 2 }`, accessed via `p.x`.
    8-byte-per-field padding (v0.x simplification).
  - **`impl` blocks** for inherent methods on structs:
    `impl Point { fn magnitude_sq(self: Point) -> i64 { ... } }`.
    Methods dispatched at lowering time; `self` becomes the first
    argument of a regular Cranelift function with a mangled name.
  - **`Vec`** as a concrete heap-allocated growable list of i64:
    `vec_new()`, `.push(x)`, `.get(i)`, `.len()`. Element type
    generalizes to `Vec<T>` once generics arrive.
- ABI: target-native (effectively `extern "C"`).
- 33 JIT tests + 14 AOT tests.

### AOT executables — done

- `rune build <file> [--release] [-o out]` produces a native executable
  via `cranelift-object` + an external C-style linker driver.
- `src/aot.rs`: `build_object` renames Rune's `main` to `__rune_main`,
  emits a synthesized `int main(void)` that calls it and truncates the
  i64 return to the i32 exit code. `link` writes a small `RUNTIME_C`
  string to a `.rt.c` file and passes it to the linker driver alongside
  the `.o` — drivers compile and link in one shot.
- Linker discovery: `clang` → `gcc` → `cc`; `$RUNE_LINKER` overrides.
- `--release` sets Cranelift's opt level to `speed`; default is `none`
  for fast iteration.
- Output: `<input-stem>.exe` on Windows, `<input-stem>` elsewhere.
  `-o <path>` overrides.

**Not yet codegen'd:** strings, struct/enum values, method/field access,
`?`, `as` casts, `match`, returning/passing arrays across function
boundaries. All emit `Unsupported(msg)` at lowering with a clear error
if reached.

## Roadmap

1. Field assignment (`p.x = 5`)
2. Generics so `Vec<T>` (and `print<T>`) become first-class
3. Enum variant construction + `match` codegen
4. Bounds checks on array indexing (panics or Result wrapping)
5. Reclamation for heap allocations (ARC, arenas, or GC)
6. Traits / interfaces
7. Self-hosted bootstrap (long-term)

## Planned syntax

Rust/Swift-flavored. Expression-oriented, statically typed with inference,
immutable by default.

```rune
fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() -> i64 {
    fib(10)
}
```

## Build

```
cargo build
cargo test
```

## CLI

```
rune tokens <file.rn>                       # dump tokens
rune ast <file.rn>                          # parse and dump the AST
rune check <file.rn>                        # parse, resolve names, type-check
rune run <file.rn>                          # JIT-compile and execute `main() -> i64`
rune build <file.rn> [-o out] [--release]   # AOT-compile to a native executable
```

`rune build` requires a C-style linker on PATH. The discovery order is
`clang` → `gcc` → `cc`. Override with `RUNE_LINKER=<name>`. `--release`
maps to Cranelift's `OptLevel::Speed`.

## Documentation

- [LANGUAGE.md](LANGUAGE.md) — language design decisions (living document)
- [docs/sessions/](docs/sessions/) — per-session technical deep dives

## License

MIT (see [LICENSE](LICENSE)).
