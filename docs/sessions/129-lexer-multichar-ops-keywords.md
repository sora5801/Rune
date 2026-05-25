# Session 129 — Lexer: multi-char operators + keyword recognition

**Date:** 2026-05-25
**Outcome:** Extends session 128's bootstrap lexer
with two-char operators (`==`, `!=`, `<=`, `>=`,
`&&`, `||`, `::`, `->`, `=>`) and 22 keyword
tokens (`fn`, `let`, `if`, `else`, `while`, `for`,
`return`, `match`, `struct`, `enum`, `trait`,
`impl`, `pub`, `mod`, `use`, `mut`, `as`, `in`,
`true`, `false`, `break`, `continue`). The lexer
now tokenizes a realistic Rune fragment like `pub
fn double(x: i64) -> i64 { x * 2 }` into 16
tokens correctly. 510 codegen + 47 AOT + 223
typecheck tests green (+7 codegen from session
128).

```rune
// examples/bootstrap/lexer.rn
pub fn tokenize("pub fn double(x: i64) -> i64 { x * 2 }")
// → [Pub, Fn, Ident("double"), LParen, Ident("x"),
//    Colon, Ident("i64"), RParen, Arrow, Ident("i64"),
//    LBrace, Ident("x"), Star, Int(2), RBrace, Eof]
```

## The decisive observation

Two extensions to session 128's lexer:

1. **One-byte lookahead via `peek_byte(src, j, n)`**
   — returns `src.byte_at(j)` if `j < n`, else `0`
   (a sentinel that doesn't match any real
   continuation byte). For each lead byte that can
   start a two-char token (`=`, `!`, `<`, `>`, `&`,
   `|`, `:`, `-`), check the next byte and emit
   the two-char token if it matches, else fall
   through to the one-char token. Same pattern as
   the Rust-side lexer uses for `<<` / `<<=` etc.

2. **`keyword_of(name: str) -> Token`** — a helper
   that ladders through `if name == "fn"` etc. and
   returns the keyword's Token, falling through to
   `Token::Ident(name)` for non-keywords. The
   tokenize loop's identifier branch calls this
   after gathering the alpha-then-alnum run.

```rune
pub fn keyword_of(name: str) -> Token {
    if name == "fn"     { return Token::Fn; }
    if name == "let"    { return Token::Let; }
    if name == "if"     { return Token::If; }
    // ... 19 more arms ...
    Token::Ident(name)   // non-keyword fallback
}
```

This is the simplest possible recognition function
— 22 sequential `==` checks. A real production
lexer would use a perfect hash or a trie for O(1)
lookup, but for a 22-keyword set the ladder is
fine. Each `==` is two memcmp-style byte
comparisons after a length filter, so the worst
case is ~44 ops per identifier — negligible
compared to the lex overhead per token.

### Why explicit `if return` instead of `match`

The natural shape would be:

```rune
match name {
    "fn" => Token::Fn,
    "let" => Token::Let,
    ...
    _ => Token::Ident(name),
}
```

v0.x's match doesn't yet support `str` literal
patterns (literal patterns are int / bool / char
only). The `if name == "..." { return ... }`
ladder is the working equivalent. Future session
could lift the restriction (str-literal patterns
would need the codegen to emit a `rune_str_eq` call
in the pattern-check). For now, the ladder works.

### Composition stays clean

The session-128 lexer composed bytes / runs /
single-char ops cleanly. The two-char extension
adds *one* new helper (`peek_byte`) and *one* new
fan-out per lead byte. The structure stays linear:
the main `tokenize` while-loop reads one or two
bytes per iteration, pushes one Token, advances `i`
by 1 or 2. No backtracking, no stateful machine.

### What's still missing

Tracking against session 117's roadmap for the
lexer:

- ✅ Whitespace skipping (session 128)
- ✅ Identifiers (session 128)
- ✅ Integer literals (session 128)
- ✅ Single-char operators (session 128)
- ✅ Multi-char operators (this session)
- ✅ Keyword recognition (this session)
- ⏳ String / char literals (session 130)
- ⏳ Float literals (session 131)
- ⏳ Numeric suffixes (session 131)
- ⏳ Comments (session 132)
- ⏳ Source spans (session 132)
- ⏳ Error recovery + diagnostics (later)

The bootstrap lexer is ~50% complete by surface
area. The "headline" Rune features (let / fn / if
/ match) are all tokenizable. What remains is
literals, comments, and spans.

## The wire-ups

```
examples/bootstrap/lexer.rn  (~150 LOC after session 129's
                              additions, up from ~100 in
                              session 128. New: Token variants
                              for multi-char ops + keywords;
                              peek_byte helper; keyword_of
                              function; 8 new lookahead branches
                              in tokenize)

tests/codegen.rs   (BOOTSTRAP_LEXER_RN const updated to mirror
                    the new lexer; +7 multi-file tests covering
                    keyword recognition, keyword-vs-ident
                    disambiguation, ==, -> and =>, && and ||,
                    one-char fallback when lookahead fails,
                    realistic function-signature lex)
```

No Rust-side compiler changes.

## What's tested

Codegen (+7 from session 128's 503):

- `rune_lexer_recognizes_keywords` — `"fn let if
  else"` first token is `Token::Fn`, not Ident.
- `rune_lexer_keyword_vs_ident_disambiguation` —
  `"fn function"` lexes as Fn + Ident("function"),
  not Fn + something-else.
- `rune_lexer_two_char_eq_eq` — `"=="` is one
  EqEq token.
- `rune_lexer_two_char_arrows` — `"-> =>"` is
  Arrow + FatArrow + Eof.
- `rune_lexer_two_char_logical_ops` — `"&& ||"`
  is AmpAmp + PipePipe + Eof.
- `rune_lexer_single_char_op_after_lookahead_fails`
  — `"=x"` is Eq + Ident + Eof (the `=` doesn't
  consume the `x`).
- `rune_lexer_realistic_function_signature` —
  `"pub fn double(x: i64) -> i64 { x * 2 }"`
  produces exactly 16 tokens.

## Apparent bugs that aren't / explicitly deferred

- **Match on str literals.** Would let
  `keyword_of` use a single match instead of 22
  if-returns. v0.x match patterns are int / bool
  / char / variant only. Extending the
  exhaustiveness checker + pattern lowerer to
  handle str-literal patterns is a future
  session.
- **Three-char operators (`..=`, `..`)** not yet
  handled. The bootstrap parser will need `..` for
  ranges. Mechanical extension: after seeing `.`,
  peek twice. Session 130 or 131 will add it.
- **No keyword conflict with raw identifiers.**
  Rust supports `r#fn` to use `fn` as an
  identifier name. Rune doesn't have raw idents;
  reserved words are always reserved. Fine for
  v0.x.
- **Whitespace inside multi-char operators is
  not allowed.** `=  =` lexes as Eq + Eq, not
  EqEq. Matches every other language. Same
  behavior as the Rust-side lexer.
- **`peek_byte` returning 0 as out-of-bounds
  sentinel** could theoretically collide with a
  real `\0` byte in source. Rune source is UTF-8
  text that conventionally doesn't contain `\0`,
  but a binary file passed to read_file could
  surface this. Not a bootstrap concern.

## What's next

- **Session 130: String + char literals.** Quote
  handling, escape sequences (`\n`, `\t`, `\\`,
  `\"`, `\'`). Token::Str(content) + Token::Char(c).
- **Session 131: Float literals + numeric suffixes.**
  Dot detection in digit runs; exponent (`1e10`);
  suffix scanning (`42i32`, `3.14f64`).
- **Session 132: Comments + source spans.** `//`
  line comments + `/* */` block comments; per-
  token `Span { start, end }` for the parser to
  report errors against.
- **Session 133+: Begin the parser.**
