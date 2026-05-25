# Session 131 — Lexer: float literals + numeric suffixes

**Date:** 2026-05-25
**Outcome:** The bootstrap lexer recognizes float
literals (`3.14`, `1e10`, `3.14e-5`) and the
12-type numeric suffix set (`i8 i16 i32 i64 isize
u8 u16 u32 u64 usize f32 f64`). `Token::Int` now
carries `(i64, str)` (value + suffix-or-empty);
new `Token::Float(f64, str)` mirrors it. Required
adding a small Rust-side helper: `f64::from_str`
parallel to session 123's `i64::from_str`. 524
codegen + 47 AOT + 223 typecheck tests green (+7
codegen from session 130).

```rune
lexer::tokenize("let pi: f64 = 3.14e0; let n = 42i32;")
// → 15 tokens including:
//   Float(3.14, "")  // unsuffixed, type comes from binding
//   Int(42, "i32")   // suffix forces type
```

## The decisive observation

Three pieces compose:

1. **Float detection extends the digit-run.** After
   the leading integer-part digits, peek for `.`
   followed by another digit (committing to a
   float), then optionally `e`/`E` + sign + digits.
   `1.method()` (dot followed by alpha, not digit)
   stays an integer — the parser handles the dot
   as `.` token.
2. **Suffix scan after the numeric body.** Any
   alnum run starting at the numeric end gets
   checked against the suffix whitelist. If it's
   one of the 12 known suffixes, consume it; if
   not, back off so `1xyz` lexes as `Int(1)` +
   `Ident("xyz")` (rather than emit Error).
3. **`f32`/`f64` suffix forces float.** `42f64`
   has an integer body but an `f64` suffix —
   committed to `Token::Float(42.0, "f64")`.

```rune
// Pseudocode for the digit-run, extended:
if is_digit(b) {
    j = scan_digits_from(i);
    if peek == '.' && peek_next is digit { is_float = true; j = scan_dot_digits(j); }
    if peek == 'e'/'E' { is_float |= scan_exponent(j); }
    suffix = scan_alnum_from(j);   // try to consume suffix
    if !known_suffix(suffix) { j = back_off; suffix = ""; }
    if f32/f64 suffix || is_float { push Float(value, suffix); }
    else { push Int(value, suffix); }
}
```

### Rust-side: `f64::from_str`

The lexer needs to convert the parsed numeric
lexeme (without suffix) to its runtime value.
Session 123 added `i64::from_str` for integers;
this session adds the float counterpart via
`strtod`:

```c
double rune_f64_from_str(const struct rune_str* s) {
    if (s->len <= 0) return 0.0;
    char buf[64];
    size_t n = (size_t)s->len > 63 ? 63 : (size_t)s->len;
    memcpy(buf, s->ptr, n);
    buf[n] = '\0';
    char* end = NULL;
    double v = strtod(buf, &end);
    if (end == buf) return 0.0;
    return v;
}
```

Wired via the standard BuiltinFn pattern in
resolver.rs (interned as `f64::from_str` with a
`Ty::Str → Ty::Float(F64)` signature) and the
runtime-fn signature table in codegen.rs (returns
`types::F64`). Borrow-arg release gate extended to
include `f64_from_str`.

### Why mixed `Float(f64, str)` instead of separate variants

A simpler model would be:
```rune
Float32(f32, str),
Float64(f64, str),
```
But Rune doesn't have casts in match payloads. The
lexer produces `Float(parsed_f64, suffix)`; the
parser's job is to interpret the suffix and emit
the right `Ty::Float(...)`. Lexer stays
type-agnostic; suffix is just metadata.

Same shape for `Int(i64, str)` — value stored as
i64 regardless of suffix (an `i8` value still
fits), suffix string carries the type information
the parser will consult.

### The 1.method() ambiguity

`1.method()` — is `1.` a float, or `1` followed
by `.method()`? The lexer's choice: stay an
integer when the dot isn't followed by a digit.
This matches Rust's behavior and parses
correctly:

```rune
let x = 1.to_str();   // Int(1, "") + Dot + Ident("to_str") + ...
let y = 1.5;           // Float(1.5, "") + Eof
```

The check `j + 1 < n && is_digit(src.byte_at(j +
1))` is the gate.

## The wire-ups

```
runtime.c          (+~20 lines: rune_f64_from_str via strtod.)

src/codegen.rs     (+1 extern declaration,
                    +1 JIT symbol registration,
                    +1 runtime-func signature arm,
                    +1 name in the borrow-arg release gate.)

src/resolver.rs    (+1 BuiltinFn entry: f64::from_str.)

examples/bootstrap/lexer.rn  (~280 LOC, up from ~225 in session
                              130. New: Token::Int(i64, str) +
                              Float(f64, str) variants;
                              is_numeric_suffix helper; extended
                              digit-run scan covering float
                              detection, exponent, suffix.)

examples/bootstrap/main.rn   (Demo updated to exercise floats +
                              suffixes: tokenize `let x: f64 =
                              3.14; let n: i32 = 42i32;` → 15
                              tokens.)

tests/codegen.rs   (BOOTSTRAP_LEXER_RN const updated to mirror
                    the new lexer; +7 multi-file tests covering
                    basic float, exponent, int with suffix, float
                    with suffix, f32 suffix on int, empty suffix
                    on plain int, realistic let binding.)
```

## What's tested

Codegen (+7 from session 130's 517):

- `rune_lexer_float_literal_basic` — `"3.14"` →
  Float token.
- `rune_lexer_float_literal_exponent` — `"1e10"`
  → Float with value 10_000_000_000.
- `rune_lexer_int_with_suffix` — `"42i32"` →
  `Int(42, "i32")`.
- `rune_lexer_float_with_suffix` — `"3.14f64"` →
  `Float(_, "f64")`.
- `rune_lexer_int_with_f32_suffix_becomes_float` —
  `"42f32"` → `Float(42.0, "f32")`. Suffix forces
  the variant.
- `rune_lexer_int_no_suffix_has_empty_suffix` —
  plain `"5"` → `Int(5, "")`. Empty-string suffix
  payload, not a missing slot.
- `rune_lexer_float_realistic_let_binding` —
  `"let pi: f64 = 3.14;"` → 8 tokens.

## Apparent bugs that aren't / explicitly deferred

- **No hex / binary / octal literals (`0xff`,
  `0b1010`, `0o755`).** The digit-run treats
  everything as base-10. Mechanical extension:
  peek for `0x`/`0b`/`0o` prefix before the
  digit-run, route to a different scan function.
- **No underscore separators (`1_000_000`).**
  Common in modern languages but adds complexity
  to the digit-run scan. Future session.
- **No negative literal recognition.** `-3.14`
  lexes as Minus + Float(3.14, ""); the parser
  handles the unary minus. Same as Rust.
- **Suffix backoff is conservative.** `5_thing`
  doesn't lex as `Int(5, "")` + `Ident("_thing")`
  — the underscore isn't a digit and isn't alnum-
  start (it's just alnum-continue), so… actually
  `_thing` is a valid Rune ident (starts with `_`).
  The lexer would back off, then re-encounter
  `_thing` as the next token. Fine.
- **Exponent without preceding digits.** `e10`
  by itself is an Ident("e10"), not a Float —
  exponents need a digit-run before them.
- **`1.5.6` is ambiguous.** Current behavior:
  `Float(1.5, "")` + `Dot` + `Int(6, "")`. The
  parser will likely reject the `.6` as a syntax
  error. Real Rust does the same.
- **No float range check at lex time.** A literal
  too large for f64 (`1e1000`) lexes as
  `Float(+∞, "")` via strtod's HUGE_VAL behavior.
  The parser / checker would catch this — session
  108's float-literal range check runs on the
  parser-side `Lit::Float` once that's
  re-implemented.

## What's next

- **Session 132: Comments + source spans.** `//`
  line comments and `/* */` block comments;
  per-token `Span { start, end }` so the parser
  can report errors with positions. Spans are the
  last lexer feature before the parser begins.
- **Session 133+: Parser construction.** A
  Pratt-style precedence parser is the natural
  shape — Rune's existing parser uses the same
  approach.
