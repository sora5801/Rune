# Session 002 — Parser

**Date:** 2026-05-19
**Outcome:** AST + recursive-descent / Pratt parser. 37 new integration tests
(58 total). `rune ast <file>` works on `examples/hello.rn`.

## Goal

Take a stream of tokens to a typed AST that's usable as input for later
phases. Concrete bar: parse `examples/hello.rn` end-to-end with zero errors.

## AST shape

Single-file at [`src/ast.rs`](../../src/ast.rs). All variants derive
`Debug, Clone, PartialEq`.

- `Module { items, span }` — top-level container.
- `Item ::= Fn | Struct | Enum | Const`. Visibility is a flat `Pub`/`Private`
  enum, not a more elaborate path-based visibility (yet).
- `Stmt ::= Let | Expr(Expr, has_semi: bool) | Item`. The `has_semi` boolean
  distinguishes a normal statement-terminated expression from a block's
  trailing value expression.
- `Expr` is one big enum with ~20 variants covering literals, paths,
  unary/binary, assignment, call/method-call/field/index, try (`?`),
  cast (`as`), array literals, blocks, `if`/`while`/`for`/`match`, and
  `return`/`break`/`continue`.
- `Type` has only `Path` for now. Function, tuple, reference, and array
  types are deferred.
- `Pattern` is intentionally narrow: wildcard, identifier (with `mut`),
  literal. No destructuring yet.

Every node carries a `Span`. `Expr::span()` is a fan-out method to recover
it polymorphically without matching by hand at every call site.

### Why a flat `Expr` enum instead of separate node types?

A flat enum is the standard "Crafting Interpreters" shape, ergonomic for
pattern matching, and explicit about indirection (`Box<Expr>` on every
recursive position). The enum itself stays ~80 bytes since the boxes don't
inline child sizes.

If profiling later flags allocation pressure we can move to interned
`NodeId`s + side tables (rustc-style). Not worth the bookkeeping cost yet.

## Parsing strategy

Recursive descent for items, statements, types, patterns. **Pratt (precedence
climbing)** for expressions.

### Precedence table

Implemented in `infix_binding_power` as `(left_bp, right_bp)` pairs. Left-
associative: `lbp < rbp`. Right-associative: `lbp > rbp`.

| Level | Operators | Assoc |
| ---: | --- | --- |
| 1 (lowest) | `=`, `+=`, `-=`, `*=`, `/=`, `%=` | right |
| 2 | `\|\|` | left |
| 3 | `&&` | left |
| 4 | `==`, `!=`, `<`, `>`, `<=`, `>=` | left |
| 5 | `\|` | left |
| 6 | `^` | left |
| 7 | `&` | left |
| 8 | `<<`, `>>` | left |
| 9 | `+`, `-` | left |
| 10 | `*`, `/`, `%` | left |
| 11 | `as` | left |
| 12 (highest infix) | postfix `(...)`, `[...]`, `.x`, `.x(...)`, `?` | n/a |
| prefix | unary `-`, `!`, `~` | right |

Comparison is currently left-associative. Rust treats it as non-associative
(`a < b < c` is a syntax error). Rune may want to follow — TBD. The
parser-precedence test [`comparison_below_arithmetic`](../../tests/parser.rs)
demonstrates the current behavior, not a commitment.

### The "no block expr in condition" hack

`if`, `while`, `for`, and `match` all parse a condition / scrutinee expression
followed by a `{ ... }` body. A naïve parser would consume the body as a
block expression in the condition position:

```
if cond { body }
   ^^^^ ^^^^^^
   cond body, or block-expr `{ body }` as part of cond?
```

The parser tracks a `no_block_expr: bool` flag. Before parsing a condition
we set it to `true`; inside `parse_primary` the `LBrace` arm is gated on
`!no_block_expr`. The flag pushes/pops correctly across nested subexpression
contexts — parenthesized exprs, calls, indices, and array literals all reset
it because they're their own scoped expression contexts.

Once struct literals (`Foo { x: 1 }`) land, this generalizes to
`no_struct_lit` — the same hack Rust uses (see `r#Restrictions` in rustc's
parser).

### Error recovery

Every `parse_*` returns `Result<T, ParseError>`. At item-level,
`parse_module` records the error and calls `synchronize_item` which advances
to the next item-starting keyword (`fn`, `struct`, `enum`, `const`, `pub`).

Mid-statement recovery is **not yet implemented**. A malformed `let` poisons
its enclosing function. The test
[`recovers_at_next_item`](../../tests/parser.rs) confirms recovery works at
the function boundary; nothing finer.

Improvement candidate for a later session: recovery at statement boundaries
(skip to next `;`) so an editor can show multiple errors per function.

## CLI

New subcommand: `rune ast <file>` reads the file, lexes, parses, and dumps
the module with `{:#?}`. The output is verbose Debug, not pretty syntax —
useful for poking at parser output, awkward for humans. A real source-style
pretty-printer is a future cleanup.

## File layout added

```
src/
├── ast.rs     (new, ~200 lines)
├── parser.rs  (new, ~550 lines)
├── lib.rs     (updated — re-exports parser + ast)
└── main.rs    (updated — `ast` subcommand)
tests/
└── parser.rs  (new, 37 tests)
```

## What the parser deliberately does **not** do

| Feature | Status | Notes |
| --- | --- | --- |
| Generics | Deferred | `<T>` clashes with `<` comparison; needs context-sensitive disambiguation |
| Traits / `impl` blocks | Deferred | After generics |
| Closures | Deferred | `\|x\| x + 1` |
| Range expressions | Deferred | `0..n`, `0..=n` — lexer emits the tokens, parser ignores |
| Struct literals | Deferred | `Foo { x: 1, y: 2 }` — needs the `no_struct_lit` flag in condition position |
| Destructuring patterns | Deferred | `let (a, b) = ...`, `let Foo { x } = ...` |
| Function / tuple / reference / array types | Deferred | `Type` only handles paths today |
| `use`, `mod` | Deferred | Multi-file is a later concern |
| Macro syntax | Open | TBD whether Rune wants macros |

## Apparent bugs that aren't

- **`_` is "an identifier" in expressions and "wildcard" in patterns.** The
  lexer doesn't have a separate token; the parser disambiguates by context.
  Intentional.
- **`a < b < c` parses** as `(a < b) < c` rather than being a syntax error.
  Comparison-left-assoc is a placeholder; the type checker can flag it
  (`bool < c` is a type error). May upgrade to non-associative parsing later.
- **`return` with no value** is allowed inside a function returning a
  non-`()` type. The parser doesn't check return-type compatibility — the
  type checker will.

## Next session

**Resolver + minimal type checker.** Walk the AST, build symbol tables,
check that names resolve and that types line up. Start monomorphic — `i64`,
`bool`, `str`, user-named structs and enums. No generics, no inference
beyond `let x = rhs` taking `rhs`'s type.

Decisions to pin down before that lands:
- Default numeric type for unannotated literals. Currently the AST stores
  the parsed `i64` regardless of context; the type checker will need an
  inference policy (default to `i32`? to `i64`?).
- Mutability checking — `let x = 0; x = 1;` should be rejected. Today the
  parser accepts it; the checker must reject it.
- Path resolution scope — function-local first, then module-level, then
  built-ins. Simple lexical scoping; no imports yet.
