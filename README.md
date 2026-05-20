# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

**Pre-alpha — lexer + parser.** No name resolution, type checker, or codegen yet.

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

### Type checker — not started
### Code generation — not started

## Roadmap

1. Resolver / name resolution
2. Type checker
3. HIR + lowering
4. Cranelift codegen (hello-world first)
5. Minimal stdlib (print, arithmetic, basic collections)
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
```

## Documentation

- [LANGUAGE.md](LANGUAGE.md) — language design decisions (living document)
- [docs/sessions/](docs/sessions/) — per-session technical deep dives

## License

MIT (see [LICENSE](LICENSE)).
