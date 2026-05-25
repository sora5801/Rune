# Session 122 — `String::from(s: str)` + `i64::to_str()`

**Date:** 2026-05-25
**Outcome:** Two ergonomic builders for the codegen-
output path: `String::from(s: str) -> String`
constructs a fresh mutable String pre-populated with
a str's bytes; `(n: i64).to_str() -> str` renders an
integer as a decimal string. Together they make the
"build IR text" pattern compact: `let buf = String::
from("x = "); buf.push_str(n.to_str()); ...`. 477
codegen + 43 AOT + 223 typecheck tests green (+8
codegen from session 121).

```rune
fn main() -> i64 {
    let buf: String = String::from("x = ");
    let n: i64 = 42;
    buf.push_str(n.to_str());      // appends "42"
    buf.len()                      // 6
}
```

## The decisive observation

The lexer / parser side of the bootstrap (sessions
118-119) only needs *reading* operations on strings.
The codegen-output side needs *writing*: build text
like `"v3 = iadd.i32 v1, v2\n"` mixing literal
fragments with rendered integers. Session 121's
`String` covered the buffer; session 122 adds the
two conversions that turn the buffer into a
practical builder.

Two pieces, both ~40 lines of C + one new method-
dispatch arm each:

### `String::from(s: str)`

Equivalent to `String::new()` followed by
`push_str(s)`, in one allocation:

```c
struct rune_string* rune_string_from(const struct rune_str* s) {
    struct rune_string* out = rune_string_new();
    if (s->len > 0) {
        rune_string_reserve(out, s->len);
        memcpy(out->ptr, s->ptr, (size_t)s->len);
        out->len = s->len;
    }
    return out;
}
```

Reuses the session-121 `rune_string_reserve` helper
for the initial cap. The single `memcpy` replaces
push_str's per-call buffer-bookkeeping. Surfaced
under both `String::from` and `std::String::from`
in the resolver (same shape as session 121's
`String::new` / `std::String::new`).

### `i64::to_str()`

A method on `Ty::Int(IntTy::I64)` that renders the
value as a decimal `str`. Uses `snprintf("%lld", v)`
into a 32-byte stack buffer (plenty for any i64:
20 digits + sign + NUL = 22), then copies the
digits-only span into a fresh +1 `rune_str`.

```c
struct rune_str* rune_i64_to_str(int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof(buf), "%lld", (long long)v);
    struct rune_str* r = malloc(sizeof(struct rune_str));
    r->rc = 1;
    if (n == 0) { r->ptr = NULL; r->len = 0; return r; }
    char* bytes = malloc((size_t)n);
    memcpy(bytes, buf, (size_t)n);
    r->ptr = bytes;
    r->len = n;
    return r;
}
```

The method is added in checker.rs's `resolve_method`
table alongside the existing `Ty::Str` /
`Ty::String` arms; codegen.rs's compile_method_call
gets one new arm for `(Ty::Int(IntTy::I64), "to_str")`.

### Why only i64

Other integer types (i8 / i16 / i32 / u8 ... / u64
/ isize / usize) all have natural `.to_str()`
semantics — but adding 12 separate runtime functions
is a lot of duplication. v0.x picks the path of
least surprise: only i64 has a built-in. Users who
need other widths write `(my_i32 as i64).to_str()`
or `(my_u8 as i64).to_str()`. The `as`-cast is
explicit (no silent conversion), which fits Rune's
"loud" type model.

A future session could generalize via the existing
`Numeric` trait + a per-primitive `.to_str()`
default body. The bootstrap probably won't need
more than i64 — line numbers, byte offsets, register
indices are all naturally i64.

### Why not `f64::to_str()`

Float formatting introduces precision questions
("how many digits?", "scientific notation?",
"trailing zeros?") that aren't relevant to the
codegen-output path (Cranelift IR uses hex floats
for unambiguous round-trip). Deferred.

## The wire-ups

```
runtime.c          (+~40 lines: rune_string_from + rune_i64_to_str)

src/codegen.rs     (+2 extern declarations,
                    +2 JIT symbol registrations,
                    +2 runtime-func signature arms,
                    +1 method dispatch arm for (Ty::Int(I64),
                     "to_str"))

src/resolver.rs    (+2 BuiltinFn entries: String::from,
                     std::String::from)

src/checker.rs     (+1 MethodSig entry: (Ty::Int(IntTy::I64),
                     "to_str") -> Ty::Str)

tests/codegen.rs   (+8 tests: from-literal, from + push_str,
                    from-empty, to_str positive / negative /
                    zero / equality round-trip,
                    builder-pattern usage with both new APIs)
```

No HIR / lowerer / monomorphizer changes. No
std.rn changes.

## What's tested

Codegen (+8 from session 121's 469):

- `string_from_literal` — `String::from("hello").
  len()` is 5.
- `string_from_then_push_str_appends` — combine
  with push_str.
- `string_from_empty_str` — empty input → empty
  String.
- `i64_to_str_positive` — `12345.to_str().len()`
  is 5.
- `i64_to_str_negative_has_minus_sign` — `(-42).
  to_str()` is 3 bytes including the leading `-`.
- `i64_to_str_zero_is_one_byte` — `0.to_str()` is
  "0".
- `i64_to_str_content_matches_via_equality` —
  `123.to_str() == "123"` proves byte content.
- `i64_to_str_into_string_builder` — realistic
  "build `x = 42`" pattern; verifies the two new
  APIs compose.

## Apparent bugs that aren't / explicitly deferred

- **Only i64 has `.to_str()`.** Other widths via
  `as i64` cast. Generalizing to all integer types
  needs a `Numeric::to_str` trait method or per-
  width builtins — future session.
- **No `f64::to_str()`.** Float formatting is
  multi-decision (digits, notation, trailing
  zeros). Defer until needed.
- **No `i64::from_str(s: str)`.** Parsing the
  inverse direction (str → i64) is the natural
  follow-on but a separate session. The bootstrap
  lexer would need `from_str` to convert integer
  literals.
- **No hex / binary / octal formatting.** v0.x's
  `to_str` is decimal only. Cranelift IR uses
  decimal for integer constants, so this fits.
  `0x`/`0b` variants would help debug output;
  future.
- **No padding / alignment.** No `format!`-style
  width / precision. Concatenation via push_str
  is the manual path.
- **`String::from` always allocates.** Even
  `String::from("")` mallocs a descriptor (cap=0).
  Could be optimized but allocator-noise is the
  consistent v0.x behavior.

## What's next

- **Session 123: `i64::from_str(s: str) -> i64`** —
  parse the inverse direction. The bootstrap lexer
  needs this to convert integer-literal lexemes
  into runtime values.
- **Session 124: module system at file granularity.**
  Tier B blocker from session 117 — the bootstrap
  needs to span many `.rn` files.
- **Session 125+**: continued Phase 1 buildout.
