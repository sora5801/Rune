# Session 128 — First lexer module in Rune (Phase 2 begins)

**Date:** 2026-05-25
**Outcome:** The first real Rune-in-Rune module:
`examples/bootstrap/lexer.rn` tokenizes a meaningful
subset of Rune source — identifiers, integer
literals, single-char operators, parens / braces /
semi / comma — into a `Vec<Token>`. ~100 LOC of
Rune. Runs end-to-end via `cargo run -- run
examples/bootstrap/main.rn`. Phase 2 of the
self-hosted bootstrap (session 117) starts here.
503 codegen + 47 AOT + 223 typecheck tests green
(+7 codegen from session 127).

```rune
// examples/bootstrap/main.rn
mod lexer;

fn main() -> i64 {
    let src: str = "let x = 1 + 2;";
    let toks: Vec<lexer::Token> = lexer::tokenize(src);
    toks.len()    // 8: let, x, =, 1, +, 2, ;, Eof
}
```

```
$ cargo run -- run examples/bootstrap/main.rn
8
```

## The decisive observation

Phase 1 (sessions 118–127) added file I/O, string
methods + Vec<str>, command-line args, mutable
String, integer formatting, modules, recursive types,
pattern guards, let-else. Sessions 124-127 found
that most "blockers" were already in place. The
language is now sufficient to express a lexer in
itself.

This session ships that lexer. It's not yet the
*full* Rune lexer — that one is ~600 LOC of Rust at
`src/lexer.rs` and handles every token kind, every
literal form, comments, error recovery, source
spans. The Rune-side lexer covers maybe 30% of that
surface, but it's a real working tokenizer that:

- Walks source byte-by-byte via `byte_at`
- Skips whitespace runs
- Lexes integer literals via digit-run detection +
  `i64::from_str`
- Lexes identifiers via alpha-then-alnum runs (with
  ASCII underscore support)
- Dispatches single-char operators / punctuation via
  match on byte values
- Produces a fresh-+1 `Vec<Token>` with a trailing
  Eof sentinel
- Emits `Token::Error(byte)` for unrecognized bytes
  rather than panicking

The structure is straightforward: one `tokenize`
function with three inner cases (digits, idents,
single-chars) plus whitespace skip, separated by
`continue` in a `while`-loop driven by an `i: i64`
position.

### Why this matters

Phase 2's plan (per session 117):

> Phase 2: interpreter bootstrap (sessions ~141–
> ~170). Write a Rune-in-Rune tree-walking
> interpreter. Sub-stages:
> - 141–145: Lexer in Rune. ~500 lines of Rune.
> - 146–155: Parser in Rune.
> - 156–165: Resolver + type-checker in Rune.
> - 166–170: Tree-walking evaluator.

Session 128 starts ahead of the schedule (the
roadmap predicted lexer work starting around
session 141 because Phase 1 was projected to take
~20 sessions but only took 10 — sessions 124-127
all turned into "already done" verifications). The
lexer is ~100 LOC, exercising:

- `pub enum Token { ... }` declaration
- `pub fn tokenize(...)`
- Recursive helper fns: `is_digit`, `is_alpha`,
  `is_alnum`, `is_whitespace`
- `while` loop with `continue`
- `match` on `u8` byte values
- Cross-method calls (tokenize calls helpers;
  helpers call helpers)
- str slicing `src[i..j]`
- `Vec<Token>::push` with mixed-payload variants
- `i64::from_str` (session 123)
- `str.byte_at(i)` (session 119)
- `str.len()`

The fact that all of these features compose
cleanly is the bootstrap's central feasibility
question. Answer: yes, they do.

### Test design

The codegen tests use the multi-file shape
(`run_main_files` with `mod lexer;` + inline lexer
source). 7 tests covering:

- Token count for a typical assignment
- Empty-source-yields-Eof-only
- Whitespace runs don't affect token count
- Int token's payload survives the lex
- Ident token's lexeme survives the lex
- Each single-char operator produces one token
- Unknown byte produces Token::Error

Each test exercises the lexer end-to-end and
asserts a property of the result. The tests are
self-contained — no Rust-side mocking, no shared
fixtures, just multi-file Rune programs.

### What's NOT in this lexer

Honest scope-setting:

- **Multi-char operators**: no `==`, `!=`, `<=`,
  `>=`, `&&`, `||`, `::`, `->`, `=>`, `..`, `..=`.
  Adding them needs lookahead — peek at `byte_at(i+1)`
  to decide. Mechanical extension.
- **String / char literals**: no `"..."`, no `'a'`.
  Need quote-handling + escape sequences.
- **Float literals**: `3.14`, `1e10` — needs dot /
  exponent detection.
- **Numeric suffixes**: `42i32`, `3.14f64` — needs
  post-digit suffix scanning.
- **Comments**: `//` and `/* */` — needs comment-
  loop handling.
- **Keyword recognition**: every alpha-run becomes
  `Token::Ident(name)`. A real lexer would compare
  against `"fn"`, `"let"`, `"if"`, etc. and produce
  keyword tokens. Easy to add via a helper that
  takes a str and returns Option<Token>.
- **Source spans**: each token carries no position.
  A real lexer tracks `Span { start, end }`. Adding
  this means changing Token variants to include
  span fields.
- **Error recovery**: a non-ASCII byte becomes
  `Token::Error(byte)`, but lexing continues. A
  more polished lexer would collect a Vec<LexError>
  alongside.

Each of these is mechanical. Future sessions will
add them one or two at a time.

## The wire-ups

```
examples/bootstrap/lexer.rn  (+~100 lines: Token enum +
                              tokenize() + 4 helper fns +
                              kind_of() debug helper)

examples/bootstrap/main.rn   (+10 lines: demo driver that
                              tokenizes a hard-coded snippet
                              and returns the token count)

tests/codegen.rs   (+BOOTSTRAP_LEXER_RN const + 7 multi-file
                    tests covering token count, payload,
                    whitespace handling, error token, etc.)

docs/sessions/128-first-lexer-in-rune.md   (this doc)

LANGUAGE.md   (decision-log row)
README.md     (Phase 2 progress)
```

No source code changes to the Rust-side compiler.

## What's tested

Codegen (+7 from session 127's 496):

- `rune_lexer_simple_assignment` — `"let x = 1 +
  2;"` produces 8 tokens.
- `rune_lexer_empty_string_yields_only_eof` —
  empty input produces just `Token::Eof`.
- `rune_lexer_skips_whitespace_runs` — dense vs.
  spread-out inputs produce same token count.
- `rune_lexer_int_literal_value` — `Int(12345)`
  payload survives the lex.
- `rune_lexer_ident_payload_matches` — `Ident
  ("foobar")` payload matches via `==`.
- `rune_lexer_punctuation_each_char` — each of
  the 11 supported single-char ops produces one
  token.
- `rune_lexer_unknown_byte_becomes_error_token` —
  unrecognized byte (`@`) becomes `Token::Error`.

## Apparent bugs that aren't / explicitly deferred

- **Token::Ident covers keywords too.** The lexer
  doesn't distinguish `let` from `foo` —
  classification happens later (during parsing).
  Same as many real lexers; matches Rust's design.
- **Token::Int can have value 0 from either valid
  `"0"` OR from `i64::from_str` failure.** Session
  123's known limitation. Inside the lexer this
  doesn't surface because we only call from_str
  after confirming the byte run is all digits.
- **No multi-byte literal detection.** Adjacent
  ints like `12 34` produce two tokens, but `12.34`
  produces three (Int(12), Error("."), Int(34)) —
  the dot byte falls through to Error. Float
  support is a future session.
- **`continue` inside `while`** works correctly
  (session 063 added it). The lexer relies on this
  for the whitespace-skip / digit-run / ident-run
  flow control.
- **`let mut` inside helper-fn-less loops.** The
  outer `let mut i: i64 = 0` works as the position
  cursor. The `let mut j: i64 = i` inside each
  digit / ident branch is the lookahead cursor.
  Pattern matches v0.x's mut-binding rules.

## What's next

- **Session 129: Multi-char operators + keyword
  recognition.** Add `==`, `!=`, `<=`, `>=`, `&&`,
  `||`, `::`, `->`, `=>`, plus a `keyword_of(name)
  -> Option<Token>` helper that turns `"fn"`,
  `"let"`, etc. into distinct keyword variants.
- **Session 130: String + char literals.** Quote
  handling + simple escape sequences (`\n`, `\t`,
  `\\`, `\"`).
- **Session 131: Float literals + numeric suffixes.**
  Dot detection + `e` exponent + suffix scanning.
- **Session 132: Comments + Spans.** `//` and `/*
  */` skipping; per-token source positions.
- **Session 133+: The parser.** Per session 117's
  roadmap.
