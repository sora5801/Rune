# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

Very early. Currently just the lexer.

## Roadmap

- [x] Lexer
- [ ] Parser / AST
- [ ] Type checker
- [ ] HIR / lowering
- [ ] Cranelift codegen
- [ ] Minimal standard library
- [ ] Self-hosted bootstrap (eventually)

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
```

## License

MIT (see [LICENSE](LICENSE)).
