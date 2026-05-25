# Session 130 — Lexer: string and char literals + escape sequences

**Date:** 2026-05-25
**Outcome:** Adds string and char literal recognition
to the bootstrap lexer. `"hello"` lexes to `Token::
Str("hello")`; `'a'` lexes to `Token::Char(97u8)`.
Escape sequences (`\n`, `\t`, `\r`, `\\`, `\"`,
`\'`) decode to their literal byte values. Errors —
unterminated strings, unknown escapes, multi-char
char literals — surface as `Token::Error`. 517
codegen + 47 AOT + 223 typecheck tests green (+7
codegen from session 129).

```rune
lexer::tokenize("let s = \"hi\n\"; let c = 'a';")
// → 11 tokens: Let, Ident("s"), Eq, Str("hi\n"),
//             Semi, Let, Ident("c"), Eq, Char(97),
//             Semi, Eof
```

## The decisive observation

Both literal kinds share a common scaffolding:
recognize the opening quote → scan forward
collecting decoded bytes → expect the closing
quote → emit the token. The variance is in the
buffer shape:

- **String**: variable-length, accumulated into a
  fresh `String` (session 121) via repeated
  `push_byte`. On close, `buf.to_str()` produces
  the immutable str payload.
- **Char**: exactly one byte (after escape decode).
  No buffer needed; the byte is stored directly in
  the `Char(u8)` variant.

Escape sequences are handled by a single
`decode_escape(e: u8) -> u8` helper:

```rune
fn decode_escape(e: u8) -> u8 {
    if e == 110u8 { return 10u8; }   // \n
    if e == 116u8 { return 9u8; }    // \t
    if e == 114u8 { return 13u8; }   // \r
    if e == 92u8  { return 92u8; }   // \\
    if e == 34u8  { return 34u8; }   // \"
    if e == 39u8  { return 39u8; }   // \'
    0u8                              // unknown
}
```

Returns 0 as the "unknown escape" sentinel — no
legitimate escape in this table decodes to 0, so
the sentinel doesn't collide. `\0` (NUL) is
intentionally omitted from the table; the bootstrap
source doesn't need embedded NULs, and leaving it
out keeps the sentinel collision-free.

### String literal scanning

```rune
if b == 34u8 {   // '"'
    let buf: String = String::new();
    let mut j: i64 = i + 1;
    let mut closed: bool = false;
    let mut had_error: bool = false;
    while j < n {
        let c: u8 = src.byte_at(j);
        if c == 34u8 { closed = true; j = j + 1; break; }
        if c == 92u8 {   // backslash
            if j + 1 >= n { break; }
            let decoded: u8 = decode_escape(src.byte_at(j + 1));
            if decoded == 0u8 {
                tokens.push(Token::Error(src[i..j+2]));
                had_error = true;
                closed = true;
                break;
            }
            buf.push_byte(decoded);
            j = j + 2;
            continue;
        }
        buf.push_byte(c);
        j = j + 1;
    }
    if !closed { tokens.push(Token::Error(src[i..n])); i = n; continue; }
    if !had_error { tokens.push(Token::Str(buf.to_str())); }
    i = j;
    continue;
}
```

Three exit conditions:
1. Closing quote found → push Str(buf.to_str()).
2. Unknown escape → push Error(badly-escaped lexeme),
   skip the Str push via `had_error` flag.
3. EOF before close → push Error(remaining source).

The `had_error` boolean is the cleanest way to
suppress the trailing Str push without a goto. v0.x
doesn't have `break label` for nested early exit.

### Char literal scanning

Three positions matter: the open quote at `i`, the
byte (or backslash) at `i+1`, and the close quote
at `i+2` (or `i+3` for escapes).

```rune
if b == 39u8 {   // '\''
    let c: u8 = src.byte_at(i + 1);
    let ch_byte: u8 = if c == 92u8 {
        // \X — decode X
        let d: u8 = decode_escape(src.byte_at(i + 2));
        if d == 0u8 { /* push Error and skip */ }
        d
    } else {
        c
    };
    let close_pos: i64 = if c == 92u8 { i + 3 } else { i + 2 };
    if close_pos >= n || src.byte_at(close_pos) != 39u8 {
        tokens.push(Token::Error(src[i..n])); i = n; continue;
    }
    tokens.push(Token::Char(ch_byte));
    i = close_pos + 1;
}
```

Validates the closing quote at the expected
position; rejects anything else (missing close,
multi-char content) as Error.

### Why u8 for Char, not char

Rune's `char` type is `i32` (Unicode codepoint).
For ASCII-range chars, `Token::Char(u8)` is enough:
the lexer covers source code, which is ASCII. The
parser-side will convert u8 → char when needed.

Multi-byte UTF-8 characters in source (e.g., `'é'`
which encodes as 0xC3 0xA9) would currently lex as
two bytes between the quotes, get rejected at the
closing-quote check, and emit Error. A future
session could add UTF-8 decode here; for the
bootstrap, ASCII suffices.

## The wire-ups

```
examples/bootstrap/lexer.rn  (~225 LOC, up from ~150 in
                              session 129. New: Token::Str + Char
                              variants, decode_escape helper, string-
                              literal scan branch, char-literal scan
                              branch.)

examples/bootstrap/main.rn   (Demo updated to exercise string +
                              char literal cases: tokenize `let s =
                              "hi\n"; let c = 'a';` → 11 tokens.)

tests/codegen.rs   (BOOTSTRAP_LEXER_RN const updated to mirror the
                    new lexer; +7 multi-file tests covering string
                    basic, string with \n escape, string with \"
                    escape, string unterminated, char basic, char
                    with escape, full statement with both.)
```

No Rust-side compiler changes.

## What's tested

Codegen (+7 from session 129's 510):

- `rune_lexer_string_literal_basic` — `"hello"`
  → `Str("hello")`.
- `rune_lexer_string_literal_with_escapes` —
  `"a\nb"` → 3 bytes (a, LF, b).
- `rune_lexer_string_literal_escaped_quote` —
  `"a\"b"` → 3 bytes including the literal quote.
- `rune_lexer_string_literal_unterminated_is_error`
  — `"hello` → Token::Error.
- `rune_lexer_char_literal_basic` — `'a'` →
  `Char(97)`.
- `rune_lexer_char_literal_escape` — `'\n'` →
  `Char(10)`.
- `rune_lexer_str_and_char_in_statement` — full
  Rune statement with both literal kinds tokenizes
  to 11 tokens.

## Apparent bugs that aren't / explicitly deferred

- **Multi-byte UTF-8 char literals.** `'é'` rejects
  as Error. Rune's `char` is i32 codepoint; future
  UTF-8 decoding session would lift the
  restriction. ASCII-only is enough for the
  bootstrap.
- **No `\xHH` / `\uHHHH` escapes.** Hex and Unicode
  escapes aren't supported. Mechanical extension
  to decode_escape (would need to read the next 2
  or 4 bytes after the leading sequence).
- **No raw strings (`r"..."`).** Adding `r` as an
  optional lead char before the quote, then
  disabling escape processing, is a future
  session.
- **`\0` (NUL byte) intentionally omitted.** The
  decode_escape table uses 0 as the unknown-escape
  sentinel, so a legitimate `\0` would collide.
  Bootstrap doesn't need NUL in source; future
  sessions could pick a different sentinel (255?).
- **Error tokens carry the source span,** not a
  decoded payload. A `"hi\q"` produces `Error("hi\q")`
  — the user sees the bad lexeme verbatim. Good for
  diagnostics.
- **String allocator pressure.** Each non-trivial
  string literal allocates a String (growing buffer)
  + a fresh str via `to_str()`. The bootstrap will
  lex tens of thousands of literals; for v0.x this
  is acceptable.
- **Unknown escape stops the string scan**, leaving
  later content (until the next close quote) unscanned.
  A more polished lexer would emit the Error,
  continue scanning the rest of the literal, then
  emit Str with whatever decoded content remained.
  v0.x's halt-on-first-error is simpler and the
  bootstrap source is unlikely to hit it.

## What's next

- **Session 131: Float literals + numeric suffixes.**
  Dot detection inside digit runs (`3.14`); exponent
  notation (`1e10`); type suffixes (`42i32`, `3.14f64`).
- **Session 132: Comments + source spans.** `//` line
  comments + `/* */` block comments; per-token
  `Span { start, end }` for parser-side error
  reporting.
- **Session 133+: Parser construction.** With the
  lexer feature-complete enough for headline Rune,
  the parser is the next bootstrap milestone.
