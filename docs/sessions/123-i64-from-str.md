# Session 123 — `i64::from_str(s: str)`

**Date:** 2026-05-25
**Outcome:** A path-qualified builtin
`i64::from_str(s: str) -> i64` that parses a decimal
string into an integer. The inverse of session
122's `i64::to_str`. The bootstrap lexer's "convert
an integer-literal lexeme to a runtime value" path
now compiles. 485 codegen + 43 AOT + 223 typecheck
tests green (+8 codegen from session 122).

```rune
fn main() -> i64 {
    let line: str = "answer=42";
    let parts: Vec<str> = line.split("=");
    let value_str: str = parts.get(1);
    i64::from_str(value_str)        // 42
}
```

## The decisive observation

The runtime already has `strtoll(buf, &end, 10)`
from libc — full-range parsing with sign handling.
The only Rune-side work is copying the str's bytes
into a NUL-terminated stack buffer (same shape as
session 118's `path_to_cstr` for fopen). Plus a
path-qualified BuiltinFn entry in the resolver
(under `i64::from_str`) so the user writes
`i64::from_str(s)`.

```c
int64_t rune_i64_from_str(const struct rune_str* s) {
    if (s->len <= 0) return 0;
    char buf[32];
    size_t n = (size_t)s->len > 31 ? 31 : (size_t)s->len;
    memcpy(buf, s->ptr, n);
    buf[n] = '\0';
    char* end = NULL;
    long long v = strtoll(buf, &end, 10);
    if (end == buf) return 0;   // no digits consumed
    return (int64_t)v;
}
```

### Return-shape decision: i64 with `0` on failure

The cleanest choice would be `Option<i64>` — type-
safe "did it parse?" signal. But `Option` is a
generic enum whose `SymbolId` is only known *after*
std.rn is parsed, while the resolver's BuiltinFn
registration runs at `Resolver::new()` time. Adding
deferred BuiltinFn registration is a deeper
refactor; v0.x picks the simpler convention: return
`i64` with `0` on parse failure.

Two failure modes:
- **Empty input** → 0.
- **Non-digit characters at start** → 0 (strtoll
  returns `end == buf`).

A failure value of 0 collides with `"0"` itself.
For the bootstrap, this is acceptable because the
lexer *pre-validates* lexemes — by the time
`from_str` is called, the input is known to be a
sequence of digits (possibly with a leading `-`).
Casual misuse like `i64::from_str("hello")` returns
0 rather than panicking; the user is expected to
check input validity beforehand when it matters.

A future session can add `i64::parse(s: str) ->
Option<i64>` once the resolver supports deferred
BuiltinFn registration.

### Path encoding limit (31 bytes)

The stack buffer is 32 bytes; `strtoll` needs 1
byte for the NUL terminator. So lexemes up to 31
characters parse correctly. An i64's decimal
representation is at most 20 characters (`-` +
"9223372036854775808"), so 31 is comfortably
within bounds — anything longer is necessarily an
out-of-range value, and strtoll would return
`LLONG_MAX` / `LLONG_MIN` with errno=ERANGE which
we ignore (return the clamped value silently). For
the bootstrap that matches "user wrote a huge
literal" behavior — wrap silently.

### Reuses session 118's borrow-arg release pattern

The runtime fn borrows the str arg (reads ptr+len,
doesn't retain or store). If the caller passes a
fresh-+1 str (e.g., `i64::from_str(line.split("=").
get(1))`), the codegen would leak it without
intervention. Extended the existing release-gate
in `compile_builtin_call` to cover `i64_from_str`
alongside `print_str` / `read_file` / `write_file`
/ `string_from`.

## The wire-ups

```
runtime.c          (+~20 lines: rune_i64_from_str via strtoll)

src/codegen.rs     (+1 extern declaration,
                    +1 JIT symbol registration,
                    +1 runtime-func signature arm,
                    +1 name in the borrow-arg release gate)

src/resolver.rs    (+1 BuiltinFn entry: i64::from_str)

tests/codegen.rs   (+8 tests: positive, negative, zero,
                    multi-digit, empty → 0, non-digit → 0,
                    to_str→from_str round-trip, split→from_str
                    realistic lexer pattern)
```

No checker / HIR / lowerer / monomorphizer / std.rn
changes.

## What's tested

Codegen (+8 from session 122's 477):

- `i64_from_str_positive` — `"42"` → 42.
- `i64_from_str_negative` — `"-17"` → -17.
- `i64_from_str_zero` — `"0"` → 0.
- `i64_from_str_large` — `"9999999"` → 9999999.
- `i64_from_str_empty_returns_zero` — empty string
  → 0 (parse-failure convention).
- `i64_from_str_non_digit_returns_zero` — `"abc"`
  → 0.
- `i64_from_str_roundtrip_via_to_str` — n → to_str
  → from_str → n. Cross-session test (122 + 123).
- `i64_from_str_after_split` — `"answer=42".split
  ("=").get(1)` → i64::from_str → 42. Lexer-pattern
  test combining 119 + 123.

## Apparent bugs that aren't / explicitly deferred

- **`"0"` and parse failure both return 0.**
  Documented limitation. The bootstrap pre-validates
  lexemes, so the lexer's flow doesn't hit this
  ambiguity. A future `Option<i64>`-returning
  variant (`i64::parse`) would resolve it cleanly.
- **No hex / binary / octal parsing.** Only base 10.
  `0x...` lexemes would need a separate variant or
  a "detect prefix" mode.
- **No leading-zero rejection.** `"007"` parses to
  7 (strtoll's default behavior). Some parsers
  reject leading zeros as a stylistic choice;
  Rune doesn't.
- **Out-of-range clamps silently.** `"9999999999999999999999"`
  returns `LLONG_MAX` without an error signal. The
  bootstrap's literal-overflow check happens at
  the integer-literal lexer level (session 092 /
  099 / 102), not at from_str. For runtime-supplied
  strings (file contents, CLI args), the caller
  validates beforehand.
- **Other widths (`u8::from_str`, `i32::from_str`,
  etc.)** not registered. Same path as `i64::to_str`
  in session 122 — generalize via Numeric trait
  later. For now, `(i64::from_str(s) as i32)` is
  the explicit downcast.
- **Whitespace.** `strtoll` skips leading whitespace
  silently; trailing characters after digits are
  not consumed and don't trigger an error here.
  `"42 trailing"` parses to 42. Idiomatic Rune
  trims before calling.

## What's next

- **Session 124: module system at file granularity.**
  Tier B blocker from session 117 — bootstrap
  needs to span lexer.rn, parser.rn, etc.
- **Session 125: `Box<T>` for recursive types** —
  Tier C from session 117 — needed for the AST.
- **Session 126+**: continued Phase 1 buildout.
