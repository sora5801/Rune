# Session 007 — Strings

**Date:** 2026-05-19
**Outcome:** Rune now has working string literals, a `print_str(str)`
builtin, and `==`/`!=` on strings. 158 tests green (13 new across codegen
and AOT). `examples/hello_world.rn` does what it says.

```
$ rune build examples/hello_world.rn --release && ./hello_world.exe
Hello, world!
```

## Scope

**Decided this session:**
- `str` is a 16-byte fat-pointer descriptor `(ptr: *const u8, len: i64)`.
- The descriptor is allocated on the function's stack frame.
- String literal bytes live in the object's data section via
  `cranelift_module::declare_data`.
- Strings are **immutable**.
- `print_str(s: str) -> ()` builtin; `==`/`!=` on `str` operands.
- Empty strings use `ptr = null + len = 0`; runtime null-checks before
  dereferencing.

**Deferred:**
- Concatenation (`+`) — would need heap allocation, the next memory
  model conversation.
- Methods (`.len()`, `.bytes()`, slicing, indexing).
- Owned/borrowed split (`String` vs `&str`). May never split.
- Returning strings from functions — stack-allocated descriptor can't
  escape its frame.
- Raw / multi-line / interpolated string literals.
- Unifying `print(i64)` and `print_str(str)` into one polymorphic
  `print` — that's an overloading or traits decision.

## Layout

```
str value (Rune-side):  i64    ──► points to descriptor
descriptor (stack):     i64    ──► points to bytes (or null if empty)
                        i64        length in bytes

bytes (data section):   N bytes  (NOT null-terminated)
```

C mirror used by the runtime:

```c
struct rune_str {
    const char* ptr;
    int64_t     len;
};
```

16 bytes, 8-byte aligned. Cranelift's `StackSlotData::new(_, 16, 3)`
(align_shift=3 → 8 bytes) matches.

## Codegen walk

`HirLit::Str(text)` lowers to (in [src/codegen.rs](../../src/codegen.rs)):

```rust
// 1. Get a pointer to the bytes.
let bytes_ptr = if text.is_empty() {
    builder.ins().iconst(I64, 0)            // null for empty strings
} else {
    let data_id = module.declare_data(&format!("rune_str_{n}"),
                                      Linkage::Local, false, false)?;
    let mut desc = DataDescription::new();
    desc.define(text.as_bytes().to_vec().into_boxed_slice());
    module.define_data(data_id, &desc)?;
    let gv = module.declare_data_in_func(data_id, builder.func);
    builder.ins().symbol_value(I64, gv)
};

// 2. Build the (ptr, len) descriptor on the stack.
let slot = builder.create_sized_stack_slot(StackSlotData::new(
    StackSlotKind::ExplicitSlot, 16, 3,
));
builder.ins().stack_store(bytes_ptr, slot, 0);
builder.ins().stack_store(len_const, slot, 8);
let result = builder.ins().stack_addr(I64, slot, 0);
```

The expression's Cranelift value is `result` — a pointer to the
descriptor. Indexing/loading/equality all operate through this pointer.

## Equality

Compile-time: `compile_binop_value` checks `ty == Ty::Str` for `Eq`/`Ne`
operators and routes through a runtime helper instead of `icmp`:

```rust
HirBinOp::Eq | HirBinOp::Ne if matches!(ty, Ty::Str) => {
    let func_id = self.ensure_runtime_func("str_eq")?;
    let local_func = self.module.declare_func_in_func(func_id, self.builder.func);
    let inst = self.builder.ins().call(local_func, &[l, r]);
    let eq = self.builder.inst_results(inst)[0];
    if matches!(op, HirBinOp::Ne) {
        let one = self.builder.ins().iconst(I8, 1);
        self.builder.ins().bxor(eq, one)
    } else {
        eq
    }
}
```

Runtime (Rust for JIT, C for AOT):

```c
int8_t rune_str_eq(const struct rune_str* a, const struct rune_str* b) {
    if (a->len != b->len) return 0;
    return (int8_t)(memcmp(a->ptr, b->ptr, (size_t)a->len) == 0);
}
```

## Runtime registration

JIT registers Rust `extern "C"` fns via `JITBuilder::symbol`:

```rust
builder.symbol("rune_print_i64", rune_runtime_print_i64 as *const u8);
builder.symbol("rune_print_str", rune_runtime_print_str as *const u8);
builder.symbol("rune_str_eq", rune_runtime_str_eq as *const u8);
```

AOT links against the C runtime embedded in `aot::RUNTIME_C`. The link
step passes the `.rt.c` to clang/gcc/cc alongside the `.o`; the driver
compiles + links in one shot.

The two runtimes are kept in sync by convention — both implement the
same C ABI for the same three symbols.

## Sharp edges (intentional)

| Issue | Behavior | Reason |
| --- | --- | --- |
| Empty strings | `ptr = null + len = 0` | Cranelift's `define_data` rejects 0-byte payloads; null + length-0 guard at runtime is simpler than a sentinel 1-byte allocation |
| `from_raw_parts` precondition | Runtime returns early when `len == 0` | Rust's safety check trips on null, even for zero-length slices |
| Returning a `str` from a function | Codegen would silently dangle the descriptor pointer | Stack-allocated. Type checker doesn't catch it yet; will when we add lifetime tracking |
| Two string literals with the same content | Each gets its own `rune_str_N` data symbol | Dedup is straightforward later; not interesting at v0 |
| Indexing `arr[i]` where elem type is `str` | Returns a descriptor pointer, just like any pointer-sized element | Arrays of strings work; the `elem_size(Ty::Str) = 8` already covers it |

## Test coverage

7 new JIT tests in `tests/codegen.rs`:
- String literal compiles
- `==` same-value / different-value / different-length / empty
- `!=` works
- `str` passed as function parameter

6 new AOT tests in `tests/aot.rs`:
- Print single literal
- Print multiple literals
- Print with embedded escapes (`\t`, with CRLF normalization for Windows)
- Mixed `print(i64)` + `print_str(str)`
- Equality controls exit code
- Function with `str` parameter

158 total tests, all green.

## File layout changes

```
src/
├── hir.rs       (HirLit::Str variant added)
├── lower.rs     (lower ast::Lit::Str → HirLit::Str)
├── resolver.rs  (insert print_str builtin alongside print)
├── codegen.rs   (RuneStr struct mirror; rune_runtime_print_str /
                  rune_runtime_str_eq host fns; JITBuilder::symbol
                  registrations; compile_str_literal; ensure_runtime_func
                  helper; declare_builtin handles print_str + str_eq;
                  compile_binop_value routes Eq/Ne on Ty::Str through
                  the runtime; Ty::Str handled in cranelift_type and
                  elem_size)
└── aot.rs       (RUNTIME_C now defines rune_str + rune_print_str +
                  rune_str_eq alongside the existing rune_print_i64)
tests/
├── codegen.rs   (+7 string tests)
└── aot.rs       (+6 string tests)
examples/
└── hello_world.rn  (the classic)
```

## Next session

The most useful next step depends on the memory-model conversation:

1. **String concatenation.** Forces heap allocation. Pick: arena (named
   regions extending the existing stack-frame arena), reference counting,
   borrow checker, or a tracing GC. Whichever lands, concat naturally
   becomes `let x = a + b;` where the result is heap-allocated and
   reference-counted (or arena-rooted) for the function's lifetime.

2. **Polymorphic `print`.** Unify `print(i64)` and `print_str(str)` into
   a single `print(x)` that the type checker dispatches by argument
   type. Lighter touch — doesn't require heap, doesn't require traits.
   Just type-checker plumbing + a builtin-call dispatch in the lowerer.

3. **Struct field access.** Move from "structs are opaque types" to
   "structs have field offsets the codegen knows about." Foundation for
   the standard library's data types.

4. **Bounds checks on array indexing.** Pulls forward the panic
   semantics conversation. Smallest in scope of the four.

Decision to pin before (1) lands: the heap allocation story.
