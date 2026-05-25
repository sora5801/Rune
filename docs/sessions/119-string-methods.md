# Session 119 — String methods (`.byte_at`, `.find`, `.split`)

**Date:** 2026-05-25
**Outcome:** Three new built-in methods on `str` for
byte-level / search / tokenize operations:
`.byte_at(i: i64) -> u8` (byte at index or 0
out-of-range), `.find(needle: str) -> i64` (byte
offset or -1), `.split(sep: str) -> Vec<str>`
(fresh +1 vec of pieces). Also lifts the prior
restriction so `Vec<str>` is now a valid type. The
core lexer-tier capabilities a self-hosted compiler
needs. 460 codegen + 223 typecheck tests green (+10
codegen from session 118).

```rune
fn main() -> i64 {
    let line: str = "key=value";
    let parts: Vec<str> = line.split("=");
    if parts.len() == 2 {
        let key: str = parts.get(0);
        if key == "key" { 1 } else { 0 }
    } else { 0 }
}
```

## The decisive observation

The hard problem (Vec<str>) had already been mostly
solved by sessions 067 / 074 / 105 — per-elem ARC
release walks, scan_ty_for_vec_elems recursion,
emit_release_field already handles `Ty::Str` via
`arc_helper_name`. The only block was a one-line
gate in `vec_element_supported` that explicitly
rejected str "for backward-compatibility reasons"
(the comment in session 064). Lifting that gate
just works — one test needed updating from "rejected"
to "accepted".

```rust
fn vec_element_supported(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Int(_) | Ty::Float(_) | Ty::Bool | Ty::Char
            | Ty::Str   // ← new
            | Ty::Struct(_, _) | Ty::Enum(_, _) | Ty::Vec(_)
            | ...
    )
}
```

With Vec<str> available, `.split` can return a Vec
directly. The runtime allocates a fresh +1 Vec,
pushes fresh +1 rune_str pieces; the caller binds
to a local, scope-exit triggers Vec release, which
walks slots and releases each piece. Standard ARC
flow.

### Return-shape decisions

- **`.byte_at`**: returns `u8` (the byte). Out-of-
  range returns 0 — same "no-panic, surface zero"
  policy as `rune_vec_get`. The caller checks `i <
  s.len()` if they need to distinguish "zero byte"
  from "out of range."
- **`.find`**: returns `i64` with -1 sentinel for
  "not found." `Option<i64>` would be more
  type-safe but requires builtin-method support
  for Option-typed returns (the resolve_method
  table currently doesn't have Option-shaped sigs).
  -1 is the C `strstr`-style convention and
  unambiguous (offset is always ≥ 0).
- **`.split`**: returns `Vec<str>`. Empty separator
  yields `[whole_str]` (the no-split convention) —
  the alternative ("split into individual chars")
  requires UTF-8 decoding which isn't this
  session's scope. Trailing separator yields a
  trailing empty piece, matching Rust's
  `str::split`.

### Runtime: split helper

`rune_str_split` is the meatiest runtime function —
it walks the source string, identifies separator
matches via `memcmp`, allocates each piece as a
fresh rune_str via `str_slice_owned`, and pushes
into a fresh rune_vec. The helper `str_slice_owned`
encapsulates "make a fresh +1 rune_str with bytes
copied from src[a..b]" — handles the empty-range
case (`ptr=NULL, len=0`) without allocating.

```c
struct rune_vec* rune_str_split(const struct rune_str* s,
                                const struct rune_str* sep) {
    struct rune_vec* v = rune_vec_new();
    if (sep->len == 0) {
        rune_vec_push(v, (int64_t)str_slice_owned(s->ptr, 0, s->len));
        return v;
    }
    int64_t start = 0, i = 0, last = s->len - sep->len;
    while (i <= last) {
        if (memcmp(s->ptr + i, sep->ptr, (size_t)sep->len) == 0) {
            rune_vec_push(v, (int64_t)str_slice_owned(s->ptr, start, i));
            i += sep->len; start = i;
        } else { i++; }
    }
    rune_vec_push(v, (int64_t)str_slice_owned(s->ptr, start, s->len));
    return v;
}
```

The Vec's slot ABI stores the rune_str pointer cast
to `int64_t` — same layout the existing per-V Vec
machinery expects. Per-elem release walks synthesized
at codegen monomorphize time will call `rune_release_str`
on each slot when the Vec is finally freed.

### Lexer-tier capability check

With these three methods plus session 118's file
I/O, a Rune program can now:

1. Read source code from disk (`read_file`).
2. Walk it byte by byte (`byte_at`).
3. Tokenize lines / identifiers (`split`).
4. Find delimiters / operators (`find`).
5. Compare byte ranges (`starts_with`).

That's the lexer's working set. The parser would
need recursive AST types (Tier C blocker from session
117's roadmap) before it can be written; this
session unblocks the lexer first.

## The wire-ups

```
runtime.c          (+~50 lines: rune_str_byte_at, rune_str_find,
                    rune_str_split, str_slice_owned helper)

src/codegen.rs     (+3 extern declarations,
                    +3 JIT symbol registrations,
                    +3 runtime-func signature arms,
                    +3 method dispatch arms in
                     compile_method_call)

src/checker.rs     (+3 MethodSig entries in resolve_method,
                    +1 line in vec_element_supported to allow
                     Ty::Str as an element)

tests/typecheck.rs (existing "Vec<str> rejected" test rewritten
                    to "Vec<str> accepted")

tests/codegen.rs   (+10 tests: byte_at + out-of-range, find
                    found / not-found / empty-needle, split
                    basic + trailing-sep + no-match + index +
                    for-loop iteration)
```

No HIR, lowerer, monomorphizer changes. No std.rn
changes. Pure additive — three runtime functions
plus a one-line gate lift.

## What's tested

Codegen (+10 from session 118's 450):

- `str_byte_at_returns_ascii` — "hello".byte_at(0) =
  104 ('h').
- `str_byte_at_out_of_range_returns_zero` — past-end
  returns 0.
- `str_find_returns_offset` — "hello world".find
  ("world") = 6.
- `str_find_returns_neg_one_when_absent` — needle
  missing → -1.
- `str_find_empty_needle_returns_zero` — empty
  needle matches at 0.
- `str_split_basic` — "a,b,c".split(",").len() = 3.
- `str_split_with_trailing_separator` — "a,b,"
  yields trailing empty piece (len = 3).
- `str_split_no_separator_match` — "hello".split
  ("xyz").len() = 1.
- `str_split_then_index_recovers_pieces` — indexing
  the split result rebuilds the substring "bar".
- `str_split_iterate_for_loop` — for-in over the
  split vec sums byte counts (3 × 4 = 12).

Typecheck unchanged (223 — the one rewrite replaces
the rejection with a positive control).

## Apparent bugs that aren't / explicitly deferred

- **`.chars()` not implemented.** UTF-8 decoding is
  its own session. For ASCII source code (which
  Rune itself is), `.byte_at` is sufficient. The
  bootstrap lexer can handle every Rune lexeme via
  bytes.
- **`.find` returns -1 sentinel.** Not Option<i64>.
  The MethodSig table doesn't currently support
  Option-typed returns from builtin str methods.
  Could be added — but -1 is the working convention
  for now. `s.find(x) >= 0` is the idiomatic check.
- **`.split` empty separator = no split.** The
  alternative interpretation ("yield every char")
  needs UTF-8 decoding; defer to a future char-
  iteration session.
- **No `.lines()` / `.trim()` / `.replace()`.**
  Mechanical to add — same shape as the methods
  here. Future session as the bootstrap needs them.
- **Push to existing Vec<str>.** The codegen path
  for Vec<str> via push (uextend a pointer to
  i64) was already implicitly handled by the
  existing 8-byte-slot widening logic. No new
  push-time handling needed beyond lifting the
  gate.
- **str-slice borrow vs owned.** All operations
  return fresh-owned rune_str (split's pieces,
  not slices into the source). A future "borrowed
  substring" type could avoid the per-piece
  allocation; for v0.x correctness wins.

## What's next

- **Session 120: Command-line args (`std::env::
  args() -> Vec<str>`)** — let a Rune program
  know its argv. With Vec<str> now working, this
  is the natural next builtin.
- **Session 121+**: continued Phase 1 buildout per
  session 117 roadmap.
