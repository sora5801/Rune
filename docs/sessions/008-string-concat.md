# Session 008 — String concatenation

**Date:** 2026-05-19
**Outcome:** `+` and `+=` work on `str` operands. Codegen routes to a
runtime `rune_str_concat` that mallocs a fresh descriptor + byte buffer.
167 tests green (+9 new). `examples/greet.rn`:

```
$ rune run examples/greet.rn
Hello, world!
Hello, Rune!
Hello, Cranelift!
```

## Decision pinned: process-lifetime leak heap

Concatenation forced the heap-allocation conversation deferred from
session 007. The choice for Rune v0.x:

**Allocate, never free.** `rune_str_concat` calls `malloc` for both the
new descriptor and the new byte buffer. The OS reclaims everything at
process exit. No GC, no ARC, no manual frees in user code.

Why this is fine for v0.x:
- Rune programs today are short-lived (CLI tools, tests, examples).
- Reclamation is an orthogonal concern that can be bolted on later
  without changing the language surface — concat would still be `+`,
  the runtime would just `free` (or refcount-decrement) on dropped
  bindings.
- Avoids dragging in ARC / arena / GC infrastructure for a feature that
  works fine with a leaking malloc.

Tradeoffs accepted:
- A loop that concatenates 1 million times allocates 2 million heap
  blocks and 2 million byte buffers and never frees any of them.
- Memory usage grows monotonically.
- Programs that should run for days will run out of memory.

LANGUAGE.md "Memory model" stays Tentative; promoted from "stack-frame
arena only" to "stack-frame arena + process-lifetime leak heap." The
roadmap explicitly lists reclamation as a future graduation step.

## Architecture

### Type checker — `+` accepts `str`

```rust
BinOp::Add => {
    if matches!(t, Ty::Str) {
        return Ty::Str;
    }
    if !t.is_numeric() {
        self.error(span, ...);
        return Ty::Error;
    }
    t
}
```

Sub/Mul/Div/Mod still reject non-numeric. Compound assignment
(`+=`) gets a parallel carve-out:

```rust
let add_on_str = matches!(op, BinOp::Add) && matches!(lt, Ty::Str);
let needs_numeric = matches!(op, ...arith...) && !add_on_str;
```

### Codegen — `Add` on `Ty::Str` routes to runtime

```rust
HirBinOp::Add if matches!(ty, Ty::Str) => {
    let func_id = self.ensure_runtime_func("str_concat")?;
    let local_func = self.module.declare_func_in_func(func_id, self.builder.func);
    let inst = self.builder.ins().call(local_func, &[l, r]);
    self.builder.inst_results(inst)[0]
}
HirBinOp::Add => { /* normal iadd / fadd */ }
```

The runtime function takes two descriptor pointers and returns a fresh
descriptor pointer.

### The `e.ty` bug for `AssignOp`

While wiring `+=`, found a latent bug: `compile_binop_value` was being
called with `&e.ty` (the AssignOp expression's type, which is `Ty::Unit`)
instead of the operand type. For numeric `+=` it happened to work
because `iadd` doesn't care about type tags, but for string `+=` the
Str-routing `if matches!(ty, Ty::Str)` would fail.

Fix: pass `&rhs.ty` (which equals the variable's type since the type
checker enforces compatibility):

```rust
let new_val = self.compile_binop_value(*op, cur, r, &rhs.ty)?;
```

### Runtime

**Rust** (JIT host) at [src/codegen.rs](../../src/codegen.rs):

```rust
extern "C" fn rune_runtime_str_concat(
    a: *const RuneStr, b: *const RuneStr,
) -> *mut RuneStr {
    use std::alloc::{alloc, Layout};
    unsafe {
        let a = &*a; let b = &*b;
        let total_len = a.len + b.len;
        let desc = alloc(Layout::new::<RuneStr>()) as *mut RuneStr;
        if total_len == 0 {
            (*desc).ptr = std::ptr::null();
            (*desc).len = 0;
            return desc;
        }
        let bytes = alloc(Layout::from_size_align(total_len as usize, 1).unwrap());
        if a.len > 0 { copy_nonoverlapping(a.ptr, bytes, a.len as usize); }
        if b.len > 0 { copy_nonoverlapping(b.ptr, bytes.add(a.len as usize), b.len as usize); }
        (*desc).ptr = bytes;
        (*desc).len = total_len;
        desc
    }
}
```

**C** (AOT runtime) at [src/aot.rs](../../src/aot.rs)'s `RUNTIME_C`:

```c
#include <stdlib.h>

struct rune_str* rune_str_concat(const struct rune_str* a, const struct rune_str* b) {
    int64_t total_len = a->len + b->len;
    struct rune_str* result = malloc(sizeof(struct rune_str));
    if (total_len == 0) {
        result->ptr = NULL; result->len = 0; return result;
    }
    char* bytes = malloc((size_t)total_len);
    if (a->len > 0) memcpy(bytes, a->ptr, (size_t)a->len);
    if (b->len > 0) memcpy(bytes + a->len, b->ptr, (size_t)b->len);
    result->ptr = bytes;
    result->len = total_len;
    return result;
}
```

Both leak — neither path ever calls `free`.

### `declare_builtin` gets a new case

```rust
"str_concat" => {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    ("rune_str_concat", sig)
}
```

The JIT registers `rune_str_concat` alongside the other host fns:

```rust
builder.symbol("rune_str_concat", rune_runtime_str_concat as *const u8);
```

## Returning strings from functions — when it's safe

Strings are pointers to descriptors. Where the descriptor lives matters:

| Source | Descriptor location | Safe to return? |
| --- | --- | --- |
| String literal `"foo"` | Callee's stack frame | **No** — dangling after return |
| Concat result `a + b` | Heap (process-lifetime) | Yes |
| Function parameter `s: str` | Caller's frame | Yes (caller is still alive) |

This means `fn greet(name) -> str { "Hello, " + name + "!" }` is safe —
the result is the heap-allocated concat. But `fn make() -> str { "hi" }`
silently returns a dangling pointer. The compiler does **not** catch
this yet; documented as a known limitation in LANGUAGE.md.

A future fix is one of:
1. Always heap-allocate string descriptors (lose a small efficiency).
2. Detect "literal returned by value" at codegen and promote to heap.
3. Add a lifetime tracker that errors at the return.

(2) is the right answer eventually. Not in this session.

## Apparent bugs that aren't

- **`""` + `""` allocates** a fresh heap descriptor with `ptr = null + len = 0`.
  Slightly wasteful, but unifies the code paths.
- **Each concat in a loop allocates fresh.** No string interning, no
  small-string optimization. A loop body that builds an accumulator
  string allocates O(n) descriptors and O(n²) total bytes (because each
  concat copies both sides). Acceptable for v0.x; intern + builder type
  could come later.
- **Concat ABI uses i64 for descriptor pointers.** Same convention as
  the rest of the str codegen — pointers as i64 values. Cranelift's
  `pointer_type()` would be more correct on non-64-bit hosts; we're
  x86_64-only for now so this matches.

## Test coverage

6 new JIT tests in `tests/codegen.rs`:
- Basic concat
- Chained concat (`a + b + c + d`)
- Concat with empty strings
- Concat returned from a function
- Concat with a variable on one side
- `+=` on `mut` string

3 new AOT tests in `tests/aot.rs`:
- Print a concat result
- Concat returned from a function, printed
- Loop accumulator pattern (`acc = acc + p` in a for-loop)

Plus `examples/greet.rn`, demonstrating the loop + function-return
pattern.

## File layout changes

```
src/
├── checker.rs   (Add accepts Ty::Str; check_assign_op exception for str +=)
├── codegen.rs   (rune_runtime_str_concat host fn; JITBuilder::symbol;
                  declare_builtin "str_concat" case; compile_binop_value
                  Str-branch for Add; AssignOp uses rhs.ty)
└── aot.rs       (RUNTIME_C adds rune_str_concat + #include <stdlib.h>)
tests/
├── codegen.rs   (+6 concat tests)
└── aot.rs       (+3 concat tests)
examples/
└── greet.rn     (chained concat in a for-loop)
```

## Next session

The four candidates from session 007's bottom matrix all remain. Best
picks now:

1. **Polymorphic `print`.** Smallest scope. Unify `print(i64)` and
   `print_str(str)`. Requires a "dispatch by argument type" mechanism in
   the lowerer + checker — no language-level overloading, just builtin
   plumbing. After this, `print(42)` and `print("hi")` both work.

2. **String methods**: `.len()`, indexing, slicing. Adds method-call
   support broadly; touches all of checker / lowerer / codegen. Bigger
   than (1).

3. **Heap-allocated arrays / `Vec`-equivalent.** Same heap leak story
   as strings. Forces a small stdlib type.

4. **Bounds checks on array indexing.** Smaller; needs a panic /
   abort story.

Decisions to pin before any of these: whether `print` becomes
polymorphic or stays split, and whether arithmetic-style overloading
(integer + integer vs string + string) becomes a general pattern (and
how that interacts with traits later).
