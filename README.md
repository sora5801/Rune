# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

**Pre-alpha — front end (lexer, parser, resolver, type checker) complete.**
No code generation yet.

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
- Error recovery (lexer accumulates errors instead of failing)
- 21 integration tests

### Parser — done

- Items: `fn`, `struct`, `enum`, `const` (with optional `pub`)
- Statements: `let` (with `mut`, type annotation, initializer),
  expression statements, items inside blocks
- Expressions:
  - All literals, paths, parenthesized
  - Unary `-`, `!`, `~`
  - Full binary operator precedence via Pratt — arithmetic, comparison,
    logical, bitwise, shifts
  - Assignment `=` and compound assignment `+= -= *= /= %=` (right-associative)
  - Postfix: function call, method call, field access, indexing, `?` (try),
    `as` (cast)
  - Block expressions with optional trailing value expression
  - `if` / `else if` / `else` chains
  - `while`, `for ... in ...`, `match` with arms and guards
  - `return`, `break`, `continue`
  - Array literals
- Patterns: wildcard `_`, identifier (with `mut`), literal
- Types: paths only (`i64`, `std::io::Result`)
- Error recovery synchronizes at item-starting keywords
- 37 integration tests

### Resolver — done

- Two-pass: declare top-level items, then resolve bodies. Order-independent
  forward references between items work.
- Built-in type names (`bool`, `char`, `str`, `i8`–`i64`, `u8`–`u64`,
  `isize`/`usize`, `f32`/`f64`) pre-populated as symbols.
- Lexical scoping with shadowing allowed inside the same scope.
- Resolves identifier paths in expression and type position.
- Records `path → symbol` and `declaration → symbol` mappings for the
  type checker to consume.

### Type checker — done

- Primitive types: `bool`, `char`, `i8`–`i64`, `u8`–`u64`, `isize`,
  `usize`, `f32`, `f64`, `str`, `()`.
- Inferred array types from literals (`[1, 2, 3]` → `[i64; 3]`).
- Unannotated `int` literals default to **`i64`**, `float` literals to
  **`f64`**.
- `let` checks initializer type against annotation; infers type when no
  annotation; rejects bindings with neither type nor init.
- **Mutability is strictly enforced**: assignment to an immutable binding,
  parameter, or const is a type error.
- Binary operators check operand types: arithmetic and bitwise require
  matching numeric / integer operands; comparison returns `bool`; logical
  `&&`/`||` require `bool`; comparison on `<`/`>`/`<=`/`>=` requires
  ordered (numeric or `char`).
- Unary: `-` numeric, `!` bool, `~` integer.
- `if`/`else` branches must unify; `if` without `else` must yield `()`.
- `while` condition must be `bool`. `for x in arr` binds element type.
- Function calls check arity and argument types against the declared
  signature; returns the declared return type.
- `as` casts allowed between numeric / `bool` / `char` / integer pairs.
- Cascading errors are suppressed via a sentinel `Error` type.
- 40 integration tests.

### Code generation — not started

## Roadmap

1. HIR + lowering
2. Cranelift codegen (hello-world first)
3. Minimal stdlib (print, arithmetic, basic collections)
4. Method calls + struct field type-checking
5. Generics (parametric polymorphism)
6. Self-hosted bootstrap (long-term)

## Planned syntax

Rust/Swift-flavored. Expression-oriented, statically typed with inference,
immutable by default.

```rune
fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() {
    let answer = fib(10);
}
```

## Build

```
cargo build
cargo test
```

## CLI

```
rune tokens <file.rn>    # dump tokens from a source file
rune ast <file.rn>       # parse and dump the AST
rune check <file.rn>     # parse, resolve names, type-check
```

## Documentation

- [LANGUAGE.md](LANGUAGE.md) — language design decisions (living document)
- [docs/sessions/](docs/sessions/) — per-session technical deep dives

## License

MIT (see [LICENSE](LICENSE)).
