# Session 118 — File I/O builtins

**Date:** 2026-05-25
**Outcome:** Two new builtin functions: `read_file
(path: str) -> str` and `write_file(path: str,
contents: str) -> bool`. First concrete capability
step on the Phase 1 bootstrap path (session 117).
Rune programs can now read and write their own
source code or any other text file. 450 codegen +
223 typecheck tests green (+4 codegen from session
117).

```rune
fn main() -> i64 {
    let ok: bool = write_file("/tmp/hello.txt", "Hello, world!\n");
    if !ok { return -1; }
    let got: str = read_file("/tmp/hello.txt");
    got.len()    // 14
}
```

## The decisive observation

Everything Rune needs for file I/O already exists:
- `rune_str` descriptors with ptr/len/rc
- BuiltinFn registration pattern (used by print_i64,
  print_str, vec_new, etc.)
- ARC-aware codegen for fresh-+1 returns
- AOT path that links runtime.c automatically

So the session is purely additive: two C functions
in runtime.c, two extern declarations + JIT symbol
registrations in codegen.rs, two BuiltinFn entries
in resolver.rs, two arms in the runtime-func
signature table. No language changes, no checker
changes, no lower / mono / HIR changes.

```c
struct rune_str* rune_read_file(const struct rune_str* path) {
    // 1. Copy path to NUL-terminated cpath buf (max 4096)
    // 2. fopen, fseek/ftell to size, fseek back
    // 3. malloc bytes, fread, fclose
    // 4. Wrap in fresh rune_str with rc=1
    // 5. On any failure: return empty rune_str (also rc=1)
}

int8_t rune_write_file(const struct rune_str* path,
                       const struct rune_str* contents) {
    // 1. Copy path to NUL-terminated cpath
    // 2. fopen, fwrite, fclose
    // 3. Return 1 on full-write, 0 on any failure
}
```

### Error model

`Result<str, IoErr>` would be the right long-term
shape, but Rune doesn't have a `std::io::Error`
struct yet. v0.x picks the simplest possible
signals:

- **`read_file`** returns an empty `str` on failure.
  Callers test `.is_empty()` to detect failure.
  Empty file (a real edge case) also returns empty
  — indistinguishable, but acceptable v0.x compromise.
- **`write_file`** returns `bool`: true on full
  successful write, false otherwise.

Both are deliberately panic-free. A compiler / build
tool needs to handle missing files and read-only
filesystems gracefully; panicking on every error
would make this useless for real workloads.

### ARC ownership

`read_file` produces a fresh rune_str with rc=1.
The caller binds it to a local, the local owns the
+1, and ordinary scope-exit release reclaims it.
This is the same transfer pattern as `rune_str_concat`
/ `rune_str_slice`.

For `write_file`, both arguments are *borrowed*:
the runtime just reads their ptr/len fields. If the
caller passes a fresh-+1 value (a concat result,
not a Local binding), nothing in the runtime
releases it. Same gap that `print_str` has — and
session 118 extends `print_str`'s release dance
to also cover read_file / write_file:

```rust
// codegen.rs, compile_builtin_call:
if matches!(name, "print_str" | "read_file" | "write_file") {
    for (a, &v) in args.iter().zip(&arg_vals) {
        if is_arc_type(&a.ty, ...) && !matches!(a.kind, HirExprKind::Local(_)) {
            self.emit_arc_call("release", &a.ty, v)?;
        }
    }
}
```

A `read_file(some_concat_expr)` (rare but valid)
no longer leaks the concat's heap block.

### Path encoding

The runtime copies the Rune `str`'s ptr+len into a
NUL-terminated stack buffer (4096 bytes max) before
passing to `fopen`. Rune `str` isn't NUL-terminated
(by design — it carries explicit length), so this
copy is necessary. The 4096-byte cap matches Linux's
PATH_MAX; Windows tolerates this without issue.
Paths longer than 4096 bytes fail at `path_to_cstr`
and return the same empty/false signal as any other
failure. Real platform path limits (Windows ~260
for MAX_PATH, ~32k with `\\?\` prefix) are runtime-
checked by `fopen` itself.

### Binary safety

`rune_read_file` and `rune_write_file` both use
`"rb"` / `"wb"` open modes. Windows would otherwise
translate `\r\n` ↔ `\n` on text mode, breaking
binary round-trips. Rune's `str` is currently
expected to be UTF-8 (no validation), so binary
data through these functions might contain embedded
NULs / non-UTF-8 — that's fine for reading raw
files, but operations like `.starts_with` on the
content still treat them as opaque byte sequences.

## The wire-ups

```
runtime.c         (+~60 lines: rune_read_file, rune_write_file,
                   path_to_cstr helper.)

src/codegen.rs    (+2 extern declarations,
                   +2 JIT symbol registrations,
                   +2 runtime-func signature arms,
                   +1 release-arm gate extension)

src/resolver.rs   (+2 BuiltinFn registrations)

tests/codegen.rs  (+atomic counter + temp-path helper,
                   +4 tests: write-then-read round-trip,
                    read-missing-returns-empty,
                    write-to-invalid-path-returns-false,
                    content-matches-via-starts_with)
```

No checker, lowerer, monomorphizer, or HIR changes.
No std.rn changes (these are top-level builtins,
not std-namespaced).

## What's tested

Codegen (+4 from session 117's 446):

- `write_file_then_read_file_roundtrips` — write
  12 bytes, read back, confirm `.len() == 12`.
- `read_file_missing_returns_empty` — read a
  guaranteed-nonexistent path, confirm empty.
- `write_file_failure_returns_false` — write to a
  directory that doesn't exist, confirm `false`
  return.
- `read_file_content_matches_via_starts_with` —
  prove bytes survive the round-trip via prefix
  check.

Tests use a per-process atomic counter to mint
unique temp paths so parallel `cargo test` doesn't
race.

## Apparent bugs that aren't / explicitly deferred

- **No `Result<str, IoErr>`.** Intentional v0.x
  shape. Result requires a meaningful `Err` type;
  Rune doesn't have `std::io::Error`. Adding it
  along with `read_file_strict` returning
  `Result<str, IoErr>` is a future session — for
  now, callers use the empty / false signals.
- **Empty file vs missing file indistinguishable.**
  Both produce `read_file(p).is_empty() == true`.
  Real filesystems rarely have empty files in the
  compiler context (sources are non-empty); when
  Rune does need to distinguish, `file_exists(p)
  -> bool` is the natural addition.
- **Path encoding is bytes-as-UTF-8.** No Windows
  UCS-2 / wide-path support. Unicode filenames on
  Windows go through the "ANSI" codepage. Files
  under English / Latin paths work fine; CJK or
  emoji paths may need a future `read_file_utf16`
  variant or a switch to `_wfopen`.
- **No `append_file`.** `write_file` truncates
  (`"wb"`). Build a list and write once; or wait
  for the future variant.
- **Race conditions.** No atomic write, no flock.
  A compiler that compiles to a `.exe` while another
  process is reading it has the usual platform-
  defined behavior (Linux: silent overwrite of
  inode; Windows: ERROR_SHARING_VIOLATION).
- **No `stdin` / `stdout` builtins beyond `print`.**
  `read_stdin() -> str` would let Rune programs
  read piped input. Future session.
- **Path size cap (4096).** Paths longer than this
  fail. The runtime treats failure-to-fit the same
  as fopen-failure. Generous for any practical
  filesystem; could be lifted to `malloc` if
  someone hits it.

## What's next

- **Session 119: String methods (`.split`, `.find`,
  `.byte_at`, `.chars`)** — Rune programs that read
  files now need to walk their content.
- **Session 120: Command-line args (`std::env::
  args()`)** — let the program know what file to
  read.
- **Session 121+**: continued Phase 1 buildout per
  session 117 roadmap.
