# Session 011 — String indexing + slicing

**Date:** 2026-05-19
**Outcome:** `s[i]` reads one byte; `s[a..b]` and `s[a..=b]` heap-
allocate a substring. Range expressions parse and check. 210 tests
green (+18 new). `examples/slice.rn`:

```
$ rune run examples/slice.rn
Hello, world!
13
world
world
72
```

(Lines 3 and 4 come from `s[7..12]` and `s[7..=11]` — same substring
expressed two ways. Line 5 is `s[0]`, ASCII 'H'.)

## Decision: slice is a heap copy, not a view

The interesting design question: when `s[a..b]` produces a `str`, does
it point into `s`'s bytes (zero-copy) or own a fresh copy?

**Chose: fresh heap copy.** Same allocator strategy as concat (leak
heap, process-lifetime). Tradeoffs:

| Option | Pros | Cons |
| --- | --- | --- |
| Zero-copy view | No allocation; fast | Lifetime entanglement — slice can't outlive source. Needs a lifetime tracker we don't have. |
| **Heap copy** ✓ | Slice is independent; safe to return from functions or store anywhere | Allocation + memcpy per slice; leak heap holds onto the bytes |

For v0.x the heap copy is the right call. When (if) reclamation lands —
ARC, arena, or a borrow checker — we can revisit and introduce a
zero-copy "borrowed str" type alongside the owned one.

## Range expressions

The parser already had `..` and `..=` tokens (lexer session 001) but
never parsed them. Now they're infix at precedence (3, 4), below
comparison (9, 10) but above assignment (2, 1). Left-associative,
which is irrelevant for any real program — `a..b..c` is a type error
anyway.

Only `a..b` and `a..=b` are supported. Prefix (`..b`), postfix (`a..`),
and bare `..` are deferred. They'd need a separate prefix-operator
parser entry; doable but not yet motivated.

Range expressions only typecheck inside a slice-index context. Outside
that, the checker emits:

> range expressions are only allowed as a slice index (e.g. `s[a..b]`) —
> `for i in 0..n` and bare ranges aren't supported yet

This keeps the door open for a real iterator protocol later without
half-implementing it now.

## Architecture

### AST

```rust
pub enum Expr {
    ...
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
        span: Span,
    },
    ...
}
```

`Option` on both sides anticipates partial forms; today both are
always `Some` because the parser only produces the full `a..b` form.

### HIR

Two new variants — string indexing and slicing get their own:

```rust
HirExprKind::StrByteIndex { str_val: Box<HirExpr>, index: Box<HirExpr> },
HirExprKind::StrSlice {
    str_val: Box<HirExpr>,
    start: Box<HirExpr>,
    end: Box<HirExpr>,
    inclusive: bool,
},
```

Existing `HirExprKind::Index` stays for arrays. The lowerer
dispatches based on receiver type:

```rust
ast::Expr::Index { receiver, index, .. } => {
    let recv = lower_expr(receiver);
    if matches!(recv.ty, Ty::Str) {
        if let Expr::Range { start, end, inclusive, .. } = &**index {
            return StrSlice { str_val, start, end, inclusive };
        }
        return StrByteIndex { str_val, index };
    }
    // ... array path ...
}
```

### Codegen

**Byte index** is inline (no runtime call):

```rust
let recv = compile_expr(str_val)?;          // descriptor pointer
let i    = compile_expr(index)?;            // i64
let ptr  = load.i64 recv + 0;               // bytes pointer
let addr = iadd ptr, i;
let byte = load.i8 addr + 0;
let r    = uextend.i64 byte;                // zero-extend to i64
```

**Slice** routes to a runtime function:

```rust
let recv = compile_expr(str_val)?;
let start = compile_expr(start)?;
let end_raw = compile_expr(end)?;
// For inclusive, fold `end+1` at compile time so the runtime only sees
// half-open ranges.
let end = if inclusive { iadd end_raw, 1 } else { end_raw };
call rune_str_slice(recv, start, end)
```

The runtime `rune_str_slice` mallocs, clamps out-of-range indices, and
copies. Provided in both flavors:
- **Rust** (JIT host): `rune_runtime_str_slice` in `codegen.rs`.
- **C** (AOT): in `aot::RUNTIME_C`, alongside the existing
  `rune_str_concat`.

Both clamp with `start.max(0).min(s.len)` and `end.max(start).min(s.len)`.
Zero-length result returns a descriptor with `ptr = null + len = 0`,
matching the empty-string convention from session 008.

## Type checker

`check_index` gained a Range branch and a Str branch. The full
dispatch logic:

```
if index is Range:
    type-check start/end if present (both must be integer)
    if receiver is Ty::Str → return Ty::Str
    else → error "cannot slice value of type ..."
else:
    type-check the index as integer
    if receiver is Ty::Array(elem, _) → return *elem
    if receiver is Ty::Str → return Ty::Int(I64)
    else → error "cannot index value of type ..."
```

Standalone `Expr::Range` in `check_expr_inner` emits the explicit
"only inside a slice" error and returns `Ty::Error`.

## File layout changes

```
src/
├── ast.rs       (Expr::Range variant, span method updated)
├── parser.rs    (InfixKind::Range(bool); DotDot/DotDotEq at (3,4))
├── resolver.rs  (Expr::Range arm in resolve_expr)
├── hir.rs       (StrByteIndex, StrSlice variants)
├── lower.rs     (Index dispatch on receiver type; Range outside index
                  lowers to Unsupported)
├── checker.rs   (check_index Range/Str branches; Range standalone error;
                  check_expr_inner Range arm)
├── codegen.rs   (compile_str_byte_index, compile_str_slice;
                  rune_runtime_str_slice; JITBuilder::symbol; "str_slice"
                  case in declare_builtin)
└── aot.rs       (RUNTIME_C adds clamp_i64 + rune_str_slice)
tests/
├── parser.rs     (+3: exclusive range, inclusive range, slice index)
├── typecheck.rs  (+5: str index returns i64, slice returns str,
                  inclusive form, non-integer index error, standalone
                  range error)
├── codegen.rs    (+9: byte indices, exclusive/inclusive slices, empty
                  slice, clamping, len of slice, slice of concat)
└── aot.rs        (+1: print(s[7..12]))
examples/
└── slice.rn      (demo of len, slice, byte index)
```

## Apparent bugs that aren't

- **Out-of-range slice doesn't panic.** `"abc"[0..100]` returns `"abc"`,
  silently clamped. This is a deliberate choice for v0.x — we don't yet
  have a story for runtime errors. When panics arrive, slicing will
  probably stay forgiving (to match Python's behavior) and indexing
  will get a hard check.
- **Out-of-range byte indexing reads garbage.** `s[99]` on a 5-byte
  string loads past the end of the buffer. Undefined behavior; not
  guarded until bounds checks land.
- **UTF-8 boundaries are not checked.** `"héllo"[1]` reads the first
  byte of the `é` multibyte sequence (0xC3, 195 in decimal). For
  ASCII strings this matches intuition; for UTF-8 it's a footgun. We
  lean into it explicitly: Rune's `str[i]` is byte-indexed, like
  Rust's `&str` byte access.
- **`for i in 0..n { }` doesn't work.** The type checker errors:
  "range expressions are only allowed as a slice index". We need an
  iterator protocol first. Workaround: `for i in [0, 1, 2, ..., n-1]
  { }`, or a while loop.

## Test coverage

Parser (3): exclusive, inclusive, slice index.

Typecheck (5): byte index returns int; slice returns str; inclusive
slice; non-integer index errors; standalone range errors.

Codegen / JIT (9): byte indices first/last/via-var; basic slice;
inclusive slice; empty slice; out-of-range clamping; slice length;
slice of a concat result.

AOT (1): `print(s[7..12])` prints the substring.

Total: 210 tests, all green.

## Next session

The remaining strings-and-arrays story:

1. **Iterator protocol so `for i in 0..n { }` works.** Requires a
   minimal Iterator interface (next() returning Option) — pulls
   generics or traits forward.

2. **Heap-allocated `Vec<T>`-like type.** Same allocator strategy as
   concat / slice. The bigger question is whether `Vec` is generic
   (`Vec<i64>` distinct from `Vec<str>`) or always erased (a single
   `Vec` type holding pointers).

3. **More string methods**: `starts_with`, `ends_with`, `contains`,
   `byte_at(i)`, `find(needle) -> i64`. Mechanical — runtime-routed.

4. **Struct field access.** Codegen needs to know struct layout.
   Foundation for proper stdlib data types.

5. **User-defined methods via `impl` blocks.** Retires the hardcoded
   `resolve_method` table.

Pinning decision before (1) lands: does Rune get traits (Rust-style)
or interfaces (Go-style) for the iterator protocol? Or is the
iterator protocol baked-in (like Python's `__iter__`) until generics
arrive?
