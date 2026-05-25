# Session 132 — Lexer: comments + source spans

**Date:** 2026-05-25
**Outcome:** Final lexer features. `// line` and
`/* block */` comments (with nesting) skip cleanly
during tokenization, emitting no tokens. Every
emitted token now carries a `Span { start, end }`
recording the source byte range it covers; the
return type changes from `Vec<Token>` to
`Vec<Spanned>` where `Spanned { kind: Token, span:
Span }`. The bootstrap lexer is feature-complete.
531 codegen + 47 AOT + 223 typecheck tests green
(+7 codegen from session 131).

```rune
// examples/bootstrap/main.rn
let src: str = "// the var\nlet x: f64 = 3.14; /* tail */ let n = 42i32;";
let toks: Vec<lexer::Spanned> = lexer::tokenize(src);
toks.len()    // 13 — comments contribute no tokens
```

## The decisive observation

Two orthogonal features land together because they
share the "rewrite the token loop to track an
additional bit of state" shape:

1. **Comments** add lookahead branches at the top
   of the main loop, before any token emission.
   They advance the cursor without pushing
   anything onto the output Vec.
2. **Spans** add an extra parameter to every push:
   the start position. The tokenize loop captures
   `start = i` before each successful token, then
   constructs `Spanned { kind, span: Span { start,
   end: j } }` where `j` is the post-token cursor.

### Comment scanning

```rune
// Line comment: skip everything until newline.
if b == 47u8 && peek_byte(src, i+1, n) == 47u8 {
    let mut j: i64 = i + 2;
    while j < n && src.byte_at(j) != 10u8 { j = j + 1; }
    i = j;   // leave \n for whitespace skip
    continue;
}

// Block comment: nesting-aware via depth counter.
if b == 47u8 && peek_byte(src, i+1, n) == 42u8 {
    let mut j: i64 = i + 2;
    let mut depth: i64 = 1;
    while j < n {
        if /* "/*" */ depth = depth + 1; j = j + 2; continue;
        if /* "*/" */ depth = depth - 1; j = j + 2; if depth == 0 { break; } continue;
        j = j + 1;
    }
    if depth != 0 { push Error; }
    else { i = j; }
}
```

Nesting matches Rust's behavior. `/* outer /*
inner */ still inside */` consumes both opens
before the first close terminates the outer. The
depth counter handles arbitrary nesting depth.

Unterminated block comment (EOF before depth
returns to 0) produces a single `Token::Error`
covering the rest of the source, then halts.

### Span shape

Spans wrap each token. The naïve choice would be
to add a field to every enum variant
(`Int(i64, str)` → `Int(i64, str, Span)`); the
cleaner choice is to wrap with a struct:

```rune
pub struct Span { start: i64, end: i64 }
pub struct Spanned { kind: Token, span: Span }
```

This is the shape Rust's `chumsky`, `logos`,
`pest`, and the rust-analyzer lexer all use:
a small `Spanned<T>` wrapper. Tests check
`.kind` and `.span` separately:

```rune
match toks.get(0).kind {
    lexer::Token::Int(v, suf) => { ... }
    _ => { ... }
}
```

The `spanned(kind, start, end)` helper in lexer.rn
encapsulates the wrapper construction:

```rune
fn spanned(kind: Token, start: i64, end: i64) -> Spanned {
    Spanned { kind: kind, span: Span { start: start, end: end } }
}
```

Each successful token-emission site captures
`start = i` before consuming bytes, then calls
`tokens.push(spanned(kind, start, j))` where `j`
is the post-token cursor.

### Eof span

The Eof sentinel sits at the end of the source:
`Span { start: n, end: n }`. Zero-width, marking
the position where future input would begin. The
parser uses this for "expected expression, got
EOF" diagnostics.

### Why Vec<struct-with-struct-field> works

Each Spanned holds two fields: a Token (pointer-
sized — either inline discriminant or pointer to
heap-allocated variant body) and a Span (pointer
to a heap-allocated `{ start, end }` block). Total
16 bytes inline (two i64 pointers in the heap
struct body).

The session 105 + 119 Vec<struct> machinery
handles this — Vec stores struct pointers in
8-byte slots, the per-elem release walk recurses
into struct ARC fields. Confirmed via the
`struct_in_vec` smoke test before going further.

## The wire-ups

```
examples/bootstrap/lexer.rn  (~370 LOC, up from ~280 in session
                              131. New: Span + Spanned structs,
                              spanned() helper, line + block
                              comment branches at top of
                              tokenize loop, every push call
                              now wraps in spanned(kind, start,
                              end).)

examples/bootstrap/main.rn   (Demo updated to include a line
                              comment and a block comment;
                              return type is Vec<Spanned>.)

tests/codegen.rs   (BOOTSTRAP_LEXER_RN const updated to mirror
                    the new lexer; all 28 existing tests
                    updated from Vec<Token> to Vec<Spanned> and
                    `toks.get(i)` → `toks.get(i).kind` via
                    bulk find-and-replace; +7 new tests
                    covering line comment, block comment,
                    nested block comment, unterminated block,
                    span over int lexeme, span at Eof, span
                    over multi-char op.)

docs/sessions/132-lexer-comments-spans.md   (this doc)
```

No Rust-side compiler changes.

## What's tested

Codegen (+7 from session 131's 524):

- `rune_lexer_line_comment_skipped` — `// skip` +
  source on next line tokenizes to just the
  non-comment tokens.
- `rune_lexer_block_comment_skipped` — `/* skip
  */` mid-line produces no tokens.
- `rune_lexer_nested_block_comment` — outer
  comment with inner `/* */` survives until outer
  `*/`.
- `rune_lexer_unterminated_block_comment_is_error`
  — EOF inside block comment produces Token::Error.
- `rune_lexer_span_covers_int_lexeme` — span over
  `"  42  "` is `[2, 4)`.
- `rune_lexer_span_eof_at_end` — Eof's span is
  `[n, n)`.
- `rune_lexer_span_multi_char_op` — `"=="` span
  width is exactly 2.

## Apparent bugs that aren't / explicitly deferred

- **No doc comments.** `/// outer` and `//! inner`
  doc-comment forms aren't distinguished from
  regular line comments. v0.x's parser doesn't
  consume them either; a future session could
  emit `Token::DocComment(str)` for the parser
  to attach to items.
- **No comment span tracking.** Comments produce
  no tokens, so their spans aren't recorded.
  Tools that need to format / preserve comments
  (rustfmt, refactoring) would need this; the
  bootstrap compiler doesn't.
- **Block comment counts as one Error on
  unterminated.** Rather than tokenizing what
  succeeded before the error, the lexer halts.
  Same shape as session 130's unterminated
  string — simple to implement, hostile to
  recovery. A diagnostic-quality bootstrap
  would refine this.
- **Span is just byte offsets, no line/column.**
  Computing line/column is line-counting; the
  parser / error formatter can do this on demand
  from the original source. Keeping the lexer's
  span as just byte indices keeps it lightweight.
- **`/*` inside a `// line comment`** is treated
  as part of the line comment (not opening a
  block). Correct — comments don't nest into
  each other across kinds.

## What's next

- **Session 133+: Begin the parser.** The lexer
  is feature-complete enough that the parser can
  consume `Vec<Spanned>` and produce an AST. The
  parser will be Pratt-style for expressions
  (matching the Rust-side parser's design).
  Starts with the simplest cases (atomic
  expressions, function definitions) and works
  up through statements, then items, then full
  module parsing.

## Phase 2 progress summary

```
Lexer  ✅ feature-complete (sessions 128-132)
       tokens (single + multi-char ops, keywords,
       int + float literals + suffixes, str + char
       literals + escapes, line + block comments,
       source spans)
Parser  ⏳ next
Resolver ⏳ later
Checker ⏳ later
Eval    ⏳ later
```

Lexer source is ~370 LOC of Rune across one file.
The Rust-side lexer is ~600 LOC for comparison —
the Rune lexer is ~60% of its surface area but
covers all headline Rune features. Things missing:
hex/binary integer literals, underscore digit
separators, raw strings, doc comments, multi-byte
UTF-8 char literals. None block parser work.
