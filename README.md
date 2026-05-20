# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

**Pre-alpha — front end + JIT codegen complete.** First Rune programs run via
`rune run`. No stdlib, no AOT executables, no generics yet.

```
$ rune run examples/fib.rn
55
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
- Cranelift JIT codegen (`src/codegen.rs`): compiles to in-memory native
  code via `cranelift-jit`.
- Covers: integers (i8/i16/i32/i64 + unsigned + isize/usize), bool,
  arithmetic, comparison, bitwise, shifts, unary, short-circuit `&&`/`||`,
  `if`/`else` as both statement and expression, `else if` chains, `while`,
  `let` with mutability, assignment and compound assignment, Rune-to-Rune
  function calls (forward references, recursion, mutual recursion),
  early `return`.
- ABI: target-native (effectively `extern "C"`).
- 23 integration tests covering literal arithmetic, control flow,
  recursion (factorial, fib), short-circuit evaluation, early return.
- **Not yet:** floats (lexer + checker accept them; codegen path partially
  wired but untested), strings, arrays, `for` loops, `match`, struct/enum
  values, method/field access, `?`, `as` casts.

## Roadmap

1. AOT codegen (`cranelift-object`) + linker invocation
2. `print(i64)` host builtin + I/O
3. Float codegen tests + char/str support
4. Array codegen
5. Struct/enum field-aware codegen
6. Method calls + field type-checking
7. Generics (parametric polymorphism)
8. Self-hosted bootstrap (long-term)

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
rune tokens <file.rn>    # dump tokens
rune ast <file.rn>       # parse and dump the AST
rune check <file.rn>     # parse, resolve names, type-check
rune run <file.rn>       # JIT-compile and execute `main() -> i64`
```

## Documentation

- [LANGUAGE.md](LANGUAGE.md) — language design decisions (living document)
- [docs/sessions/](docs/sessions/) — per-session technical deep dives

## License

MIT (see [LICENSE](LICENSE)).
