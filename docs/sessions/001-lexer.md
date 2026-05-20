# Session 001 — Lexer

**Date:** 2026-05-19
**Outcome:** Hand-rolled lexer for Rune, 21 integration tests green, repo
bootstrapped at <https://github.com/sora5801/Rune>.

## Goal

Get from nothing to a working tokenizer that recognizes everything the planned
Rust/Swift-like syntax will need at the parser stage. No parser, no type
checker, no codegen yet.

## Token model

```rust
struct Token { kind: TokenKind, span: Span }
struct Span  { start: usize, end: usize }  // half-open byte range
```

Payloads live on the variants for literals and identifiers
(`Int(i64)`, `Float(f64)`, `Str(String)`, `Char(char)`, `Ident(String)`).
Keywords, operators, and delimiters are payload-free variants.

This is the textbook "Crafting Interpreters" shape — easy to consume in a
parser, allocates `String`s for identifiers but those are short and rare. If
identifier allocation shows up in a profile later we'll switch to interned
symbols (a `SymbolTable` mapping `&str → SymbolId`, with `Ident(SymbolId)`).

## Span representation

Byte offsets, not `(line, column)`. Lines and columns are derived on demand by
the diagnostic layer — we'll build a `LineIndex` over the source once and
binary-search into it at error-print time.

Trade-off:
- **Pro:** constant-size span (16 bytes), no need to track lines while lexing,
  trivial to use as a `source[span.start..span.end]` index.
- **Con:** turning a byte offset into a human-readable position costs O(log N)
  in the line count at print time. Negligible.

`Token` derives `Clone, PartialEq` but is not `Copy` because of the `String`
payload variants. A future optimization is the "thin token" pattern: the
lexer's rich token (with values) becomes a stream of `(kind_tag, span)` pairs
the parser walks, with literal values fetched lazily from a side table. Not
worth doing yet.

## Error recovery

The lexer accumulates errors into a `Vec<LexError>` and keeps going. The
alternative — fail on first error — is hostile to editors and to anyone
debugging multiple issues at once.

Cost: malformed literals still produce a token (with a sentinel zero value),
so the parser will see a syntactically-OK token where the lexer recorded an
error. The parser must check `errors.is_empty()` before trusting downstream
analysis.

## UTF-8 handling

Source is `&str`, iterated via `Chars` (yields Unicode scalar values). Byte
position is tracked manually with `len_utf8()` on each consumed char.

Identifiers and keywords are **ASCII-only** on purpose: `is_ascii_alphabetic`,
`is_ascii_alphanumeric`. Allowing arbitrary Unicode identifiers (XID_Start /
XID_Continue, as Rust does) requires the `unicode-ident` crate or an
equivalent table. Not worth a dependency yet, and we can add it without
breaking anything when the time comes.

String and char literal **contents** accept arbitrary UTF-8.

## Number literals

Decimal, hex (`0x`), binary (`0b`), octal (`0o`) integers; floats with
optional fractional part and optional exponent (`e`/`E` ± digits). Underscores
are accepted as digit separators and stripped before `i64::from_str_radix` or
`f64::parse`.

### The `1..10` ambiguity

When the lexer is sitting after the `1` and sees a `.`, is this:
- `Int(1)`, `DotDot`, `Int(10)` (range), or
- `Float(1.)` followed by `Int(10)`?

The fix is two-character lookahead: commit to "this `.` is a fractional point"
only if the next char is also an ASCII digit. So:

- `1.5` — `.` followed by `5` → fractional → `Float(1.5)`.
- `1..10` — `.` followed by `.` → range → `Int(1) DotDot Int(10)`.
- `42.method()` — `.` followed by `m` → not fractional → `Int(42) Dot ...`.

See `Lexer::number` in [`src/lexer.rs`](../../src/lexer.rs).

### What numbers don't do yet

- No numeric type suffixes (`42i64`, `3.14f32`). The lexer returns the value
  as `i64` or `f64`; type assignment is the type checker's job.
- No hex floats (`0x1.fp10`).
- No `NaN`/`inf` literals (those will be `f64::NAN`-style constants in stdlib).

## Comments

- Line: `// ...` to end of line.
- Block: `/* ... */`, **nested** Rust-style — a `/*` inside a block comment
  increments a depth counter, `*/` decrements. This means commenting out code
  containing block comments works naturally.
- Unterminated block comments produce an error pointing at the opening `/*`.

## What the lexer deliberately does **not** do

These are deferrals, not bugs. Anything not on this list that's missing *is* a
bug.

| Feature | Status | Notes |
| --- | --- | --- |
| Unicode identifiers | Deferred | ASCII only; add via `unicode-ident` later |
| Raw strings (`r"..."`) | Deferred | Single-line only for now |
| Triple-quoted strings | Deferred | TBD whether to add at all |
| Numeric suffixes | Deferred | `42i64`, `3.14f32` later |
| Hex floats | Deferred | `0x1.fp10` later |
| Unicode escapes | Deferred | Only `\n \t \r \\ \' \" \0` for now |
| String interpolation | Open | TBD whether Rune wants this |
| Shebang line | Open | Decide once we have something runnable |

## File layout established

```
Rune/
├── Cargo.toml
├── Cargo.lock
├── LICENSE          # MIT
├── README.md
├── LANGUAGE.md      # design doc
├── src/
│   ├── lib.rs       # re-exports
│   ├── main.rs      # CLI: `rune tokens <file>`
│   ├── token.rs     # Token, TokenKind, Span, keyword table
│   └── lexer.rs     # Lexer impl, LexError
├── tests/
│   └── lexer.rs     # 21 integration tests
├── examples/
│   └── hello.rn     # Rune source for poking the lexer
└── docs/
    └── sessions/
        └── 001-lexer.md   # this file
```

Convention: every coding session ends with `docs/sessions/NNN-topic.md`,
zero-padded so they sort lexically.

## Next session

**Parser.** Hand-rolled recursive descent for items (`fn`, `struct`, `enum`)
and statements; Pratt parser for expressions with operator precedence.
Concrete goal: parse `examples/hello.rn` into an AST and dump it via
`rune ast <file>`.

Design decisions in [LANGUAGE.md](../../LANGUAGE.md) that influence the AST
shape: mutability model (Decided), error handling (Tentative — affects `?`
operator), memory model (Open — affects lifetime annotations).
