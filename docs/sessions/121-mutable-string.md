# Session 121 — Mutable `String` type

**Date:** 2026-05-25
**Outcome:** A new built-in type `String` for
heap-grown mutable strings. Distinct from `str`
(immutable view, possibly a compiled-in literal):
`String` is always owned, ARC-counted, with an
amortized-doubling growth strategy. Constructed via
`String::new()`; mutated via `.push_str(s)` /
`.push_byte(b)`; read back via `.to_str()` to get a
borrowed-view `str`. 469 codegen + 43 AOT + 223
typecheck tests green (+7 codegen from session 120).

```rune
fn main() -> i64 {
    let s: String = String::new();
    s.push_str("hello");
    s.push_byte(44u8);  // ','
    s.push_byte(32u8);  // ' '
    s.push_str("world");
    let result: str = s.to_str();
    result.len()    // 12
}
```

## The decisive observation

The lexer and codegen-output paths a self-hosted
compiler needs both accumulate text byte-by-byte
or piece-by-piece. The immutable `str` + `+=`
shape allocates a fresh descriptor *and* a fresh
byte buffer on every concat — O(n²) total bytes
for an n-byte build. A purpose-built mutable
String with amortized doubling is the standard
solution.

The runtime struct extends `rune_str`'s layout
with a `cap` field for tracking allocated capacity:

```c
struct rune_string {
    char*   ptr;
    int64_t len;
    int64_t cap;
    int64_t rc;
};
```

Growth follows Vec's doubling pattern: cap starts
at 8, doubles when len would exceed cap. The byte
buffer is freed by `rune_release_string` along
with the descriptor itself when rc hits 0.

### `Ty::String` is its own variant

`Ty::String` lives next to `Ty::Str` in the type
system — not a generic parameter, not a wrapper.
This is the simplest design and matches how Rust
distinguishes `String` from `&str` (semantically;
Rune doesn't have references, but the
owned-vs-view distinction is the same).

Updates to ty.rs / codegen.rs:
- New `Ty::String` variant.
- `display()` returns `"String"`.
- `mangle_ty_name()` returns `"string"`.
- `is_arc_type` returns true.
- `cranelift_type()` returns `I64` (pointer).
- `elem_size()` returns 8.
- `arc_helper_name(retain/release, Ty::String)` →
  `"retain_string"` / `"release_string"`.

### Methods

Four methods on `Ty::String`:

- `.push_str(s: str)` — append a borrowed str's
  bytes to the buffer. Read-only on the arg; the
  String owns the copy.
- `.push_byte(b: u8)` — append one byte. The lexer's
  inner loop wants this; an i64-truncated value
  would work too, but typing as u8 makes the
  intent clear (single byte).
- `.len() -> i64` — current byte length (not cap).
- `.to_str() -> str` — copy the current contents
  into a fresh immutable rune_str descriptor. The
  String is unchanged; the returned str is owned
  by the caller (rc=1, freed on scope exit).

`.to_str()` *copies* rather than returning a view
into the live buffer because the buffer could be
reallocated on a subsequent push, invalidating the
view. v0.x prioritizes correctness over the extra
copy; future work could add a borrowed-view variant
once the language has lifetimes (probably never
for Rune) or a "frozen" String mode.

### Constructor: path-qualified BuiltinFn

`String::new()` resolves via two interned paths
in the resolver: `String::new` and `std::String::new`.
This is the second nested-namespace builtin
(session 120 was the first with `std::env::args`).
Same mechanism — the resolver maps a multi-segment
path string directly to a BuiltinFn symbol.

### ARC ownership

Push methods *borrow* their arg — `push_str(s)`
reads from `s` but the String owns its own copy of
the bytes. The existing "fresh-+1 arg gets released
after a borrowing call" pattern (from print_str /
read_file / write_file) extends to push_str. The
codegen dispatch checks if the arg is a non-Local
fresh +1 (e.g., `s.push_str(other_str + "x")`) and
emits a release after the runtime call.

`.to_str()` produces a fresh +1 owned str — same
shape as `rune_str_concat` / `rune_str_slice`.
Caller binds to a local; scope exit reclaims.

`.push_byte(b: u8)` takes a value-type u8, no ARC
concern.

### Mutability and `let mut`

The String "mutates" via interior pointer:
`s.push_str(x)` modifies the descriptor's `ptr` /
`len` / `cap` fields in place, possibly reallocating
the byte buffer. The `s` binding itself never gets
reassigned, so `let s: String = String::new(); s.
push_str(x);` works without `let mut` — same
ergonomic story as `Vec` (`let v = vec_new(); v.
push(x);` works for a non-mutable binding because
the method mutates contents, not the binding).

## The wire-ups

```
src/ty.rs          (+1 Ty::String variant, +1 display arm)

runtime.c          (+~80 lines: rune_string struct + new + reserve
                    + push_str + push_byte + len + to_str + retain +
                    release)

src/codegen.rs     (+7 extern declarations + JIT symbols,
                    +7 runtime-func signature arms,
                    +1 method-dispatch arm (push_str / push_byte /
                     len / to_str),
                    +1 mangle_ty_name arm,
                    +1 is_arc_type arm,
                    +1 cranelift_type arm,
                    +1 elem_size arm,
                    +2 arc_helper_name arms)

src/resolver.rs    (+2 builtin type aliases: String, std::String,
                    +2 BuiltinFn entries: String::new, std::String::new)

src/checker.rs     (+4 MethodSig entries: push_str, push_byte,
                     len, to_str)

tests/codegen.rs   (+7 tests: new + len, push_str accumulate,
                    push_byte grow, to_str + starts_with,
                    to_str + equality, amortized growth 1000
                    bytes, std::String namespace)
```

No lowerer / monomorphizer / HIR changes — the
existing builtin-fn + method-dispatch infrastructure
absorbs everything.

## What's tested

Codegen (+7 from session 120's 462):

- `string_new_empty` — `String::new().len() == 0`.
- `string_push_str_accumulates_len` — two pushes
  yield correct combined length.
- `string_push_byte_grows` — byte-at-a-time builds.
- `string_to_str_roundtrip_via_starts_with` — proves
  content survives the String → str copy.
- `string_to_str_equality_recovers_exact_content`
  — round-trip via `==`.
- `string_amortized_growth` — 1000 push_byte calls,
  exercises ~7 internal reallocs.
- `string_via_std_namespace` — `std::String` and
  `std::String::new` work alongside the bare names.

## Apparent bugs that aren't / explicitly deferred

- **No `.push_char(c: char)`.** UTF-8 encoding of
  a 32-bit char into 1-4 bytes is a follow-on
  session. ASCII source code (which Rune itself
  is) works fine via `push_byte`.
- **No `.clear()` / `.truncate(n)`.** Mechanical to
  add: zero out len, optionally re-shrink cap.
  Future session if needed.
- **No `.into_str()` (move conversion).** `.to_str()`
  always copies because the String's buffer might
  grow. A move version would consume the String
  and reuse the buffer, but Rune's ARC + value-
  semantics model doesn't have a clean "consume"
  shape. The copy cost is acceptable for v0.x.
- **No indexing.** `s[i]` isn't supported on String
  (or str — they only support `s[a..b]` slicing
  which is a runtime fn). For byte access, convert
  via `to_str()` and use `byte_at`.
- **No `String + str` concat operator.** Use
  `push_str` for accumulation; `to_str() + other`
  for one-shot. Adding `+` as compound on String
  would be ergonomic but mixes mutable / immutable
  semantics.
- **String literals are still `str`.** `"hello"`
  lexes as `str` (rc=-1 literal). Conversion to
  String requires `let mut s = String::new(); s.
  push_str("hello");`. A `String::from(literal)`
  shortcut would help — future session.
- **ARC retain on push_str(local).** When the arg
  is a Local binding, no retain happens (the
  binding owns its +1, the call borrows). When
  the arg is a fresh +1 (e.g., a concat), the
  codegen releases after the call. Correct in
  both cases.

## What's next

- **Session 122: `String::from(s: str)` / `format!`-
  style helpers.** Make Rune-side string-building
  ergonomic. A `String::from("hello")` constructor
  would replace the new + push_str pair.
- **Session 123: integer / float formatting.** The
  bootstrap codegen needs to render i64 / f64 into
  String for IR output. `i64::to_str(self) -> str`
  + `String::push_int(b: i64)` are the natural
  additions.
- **Session 124: file-granularity modules** (the
  next Tier B blocker from session 117's roadmap).
- **Session 125+**: continued Phase 1 buildout.
