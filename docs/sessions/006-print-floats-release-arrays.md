# Session 006 — print, floats, --release, arrays + for

**Date:** 2026-05-19
**Outcome:** Four next-session candidates landed in one go. 145 tests green
(16 new across codegen and AOT). `examples/primes.rn` prints the first 8
primes and exits with their sum.

```
$ rune build examples/primes.rn --release && ./primes.exe ; echo $?
2 3 5 7 11 13 17 19
77
```

## What shipped

| Feature | Description |
| --- | --- |
| `print(i64)` builtin | First host-provided builtin. Resolver pre-populates it; codegen routes calls to an imported `rune_print_i64` symbol — JIT registers it from a Rust fn, AOT compiles and links a small C runtime. |
| Float codegen tests | The fadd/fsub/fmul/fdiv/fneg/fcmp paths existed but were untested. Now exercised by 4 tests. |
| `--release` flag | `rune build --release` flips Cranelift's `opt_level` from `none` → `speed`. |
| Arrays + `for` loops | Array literals stack-allocated via Cranelift `StackSlot`; indexing emits address arithmetic + `load`; `for x in arr` desugars to a counter-based while loop. Forces the memory model from Open → Tentative. |

## Memory model: Open → Tentative

The "Open" memory model question becomes "Tentative — stack-frame arena"
this session. Concrete state in LANGUAGE.md:

- Arrays are stack-allocated at the point of literal. `let xs = [1,2,3]`
  reserves a Cranelift StackSlot sized `3 * sizeof(i64) = 24 bytes` with
  8-byte alignment; the binding's `Variable` holds the slot's address as
  an `i64` pointer.
- Arrays **cannot escape a function**. The codegen errors if asked to
  return or accept an array across a fn boundary. (The frontend will
  cheerfully type-check it; codegen catches the violation.)
- **No bounds checks** on indexing yet. `xs[i]` trusts `i`.
- No heap allocator; no `Vec`/`String`. Those graduate the model when
  they land.

This is the simplest sound story. Lifting to "named arenas" or "borrow
checker" remains open and isn't urgent.

## Architecture

### HIR additions

Four new `HirExprKind` variants ([src/hir.rs](../../src/hir.rs)):

```rust
BuiltinCall { name: String, args: Vec<HirExpr> },
Array { elems: Vec<HirExpr>, elem_ty: Ty },
Index { array: Box<HirExpr>, index: Box<HirExpr>, elem_ty: Ty },
For {
    local: Option<SymbolId>,    // None for `for _ in ...`
    iter: Box<HirExpr>,
    body: HirBlock,
    elem_ty: Ty,
    length: usize,              // statically known
},
```

The lowerer fills `elem_ty` and `length` from the type checker's
`expr_types` and the `Ty::Array(elem, len)` info. Codegen never has to
walk back into the resolver.

### Resolver / checker: `BuiltinFn` symbol kind

```rust
pub enum SymbolKind {
    BuiltinType(Ty),
    BuiltinFn(BuiltinFn),   // new
    Fn,
    Local { mutable: bool },
    Param,
    ...
}

pub struct BuiltinFn {
    pub name: &'static str,
    pub params: Vec<Ty>,
    pub ret: Ty,
}
```

The resolver pre-populates `print` alongside builtin types. The type
checker's `path_value_type` returns `Ty::Fn { params, ret }` for a
`BuiltinFn` so calls type-check normally. The lowerer detects the
`SymbolKind::BuiltinFn` and emits `HirExprKind::BuiltinCall` instead of
`Call` — that's the discriminator codegen needs.

### Print runtime

For both JIT and AOT, Rune programs reference the imported symbol
`rune_print_i64(int64_t)`. The two backends provide it differently:

- **JIT**: `JITBuilder::symbol("rune_print_i64", rune_runtime_print_i64 as *const u8)`
  registers an `extern "C" fn(i64)` that's a Rust `println!`. Cranelift
  resolves the import at JIT-link time to that function pointer.

- **AOT**: `src/aot.rs::link` writes a small C source (`RUNTIME_C`) to
  `<obj>.rt.c` and passes both `.o` and `.c` to the linker driver. clang
  compiles the C, links both, and produces a working executable:

  ```c
  #include <stdio.h>
  #include <stdint.h>
  void rune_print_i64(int64_t x) {
      printf("%lld\n", (long long)x);
  }
  ```

  We always include the runtime even for programs that don't call
  `print` — the linker dead-strips it. Simpler than tracking usage.

### Arrays — codegen detail

Array literals compile to a single `StackSlot` allocation followed by N
`stack_store`s:

```rust
let slot = builder.create_sized_stack_slot(StackSlotData::new(
    StackSlotKind::ExplicitSlot,
    (elems.len() as u32) * elem_size(elem_ty)?,
    3,  // align_shift: 8-byte aligned
));
for (i, elem) in elems.iter().enumerate() {
    let v = self.compile_expr(elem)?;
    builder.ins().stack_store(v, slot, (i * esize) as i32);
}
let addr = builder.ins().stack_addr(types::I64, slot, 0);
```

The expression's Cranelift value is the slot's start address. Subsequent
operations on the array (`index`, `for`) read through that pointer.

Indexing:

```rust
let offset = imul(idx, esize_const);
let elem_addr = iadd(arr_addr, offset);
let val = load(elem_cty, MemFlags::new(), elem_addr, 0);
```

For loops desugar to a counter-based while:

```
counter = 0
header: brif (counter < length), body, exit
body:
    elem = load(arr_addr + counter * elem_size)
    bind elem to user's variable
    ... user body ...
    counter = counter + 1
    jump header
exit:
```

This sidesteps needing an iterator protocol and works for any
stack-allocated array of statically-known length.

### `--release` flag

```rust
fn parse_build_args(args: &[String]) -> (Option<String>, Option<PathBuf>, bool) {
    // ... scans for `--release` flag, `-o <path>` flag, and positional input
}

let opt = if release { OptLevel::Speed } else { OptLevel::None };
aot::build_object(&mut hir, &module_name, opt)?;
```

`build_object` gained an `OptLevel` parameter (callers updated). The
existing `OptLevel` enum already had `Speed`, `None`, `SpeedAndSize`.

## What's deliberately not done

| Feature | Reason |
| --- | --- |
| Array bounds checks | Need an error-on-bounds-fail story (panic? abort? Result?). Defer until errors-as-values |
| `print(f64)`, `print(str)`, etc. | One builtin proves the wiring. Float / string printing is a stdlib design conversation |
| Returning arrays from functions | Stack-allocated → can't escape function. Heap requires allocator + memory model decision |
| Array as function parameter | Same — would need passing by reference + length. Trivial if we fix the calling convention, but the wider memory story should land first |
| Multi-dimensional arrays | `[[1,2],[3,4]]` would work syntactically; codegen would need recursive element addressing. Skipped |
| Empty arrays | Codegen errors. Type checker rejected them anyway |
| `for` over non-array iterables (ranges, etc.) | No iterator protocol designed yet |

## Test coverage added

10 new tests in `tests/codegen.rs`:
- 4 float tests (arithmetic, comparison, mul/div, negation)
- 6 array/for tests (indexing, sum, conditional, wildcard, nested,
  array-of-bools)

6 new tests in `tests/aot.rs`:
- 4 print tests (single, multiple, in-loop, computed value via fib)
- 1 AOT array sum
- 1 `--release` mode build + run

Plus `examples/primes.rn`, the new demo program using everything.

## File layout changes

```
src/
├── hir.rs       (HirExprKind: BuiltinCall, Array, Index, For added)
├── lower.rs     (lower new variants + builtin-call detection)
├── resolver.rs  (SymbolKind::BuiltinFn variant + `print` declaration)
├── checker.rs   (path_value_type handles BuiltinFn; check_assign_target
                  rejects BuiltinFn)
├── codegen.rs   (compile_builtin_call / compile_array / compile_index /
                  compile_for; rune_runtime_print_i64 host fn;
                  declare_builtin helper; elem_size helper; cranelift_type
                  handles Ty::Array)
├── aot.rs       (RUNTIME_C constant; build_object takes OptLevel; link
                  passes runtime alongside .o)
└── main.rs      (parse_build_args / derive_default_output_path; --release
                  threading)
tests/
├── codegen.rs   (+10 tests)
└── aot.rs       (+6 tests)
examples/
└── primes.rn    (new demo using print + arrays + for)
```

## Apparent bugs that aren't

- **AOT always compiles the runtime C even for programs that don't use
  `print`.** Intentional — keeps the build pipeline simple. Linker
  dead-strips unused functions, so the final `.exe` is the same size as
  before.
- **Array bounds aren't checked.** `arr[100]` on a 3-element array
  silently reads past-the-end memory. Deliberate scope cut.
- **Arrays evaluate left-to-right at literal time.** `[f(), g()]` calls
  `f` then `g`. Standard order, but worth noting since codegen happens
  inside a single `compile_array` call.
- **`--release` doesn't recompile dependencies.** Cranelift compiles user
  code with `OptLevel::Speed`; the linker invocation is unchanged.
  No `-O3` passed to clang.

## Next session

Top candidates:
1. **Strings.** Lexer already produces `Str(String)` literals; checker
   already has `Ty::Str`. Need: a string layout (Rust-like `&str` slice?
   C-like null-terminated?), `print_str` builtin, basic string concat.
   Forces design of the heap allocation story.
2. **Heap-allocated vectors.** Once strings need heap, vectors come
   along for free. Promotes memory model from "stack-frame arena" to
   "stack + named arena" (or RC, or whatever the user picks).
3. **Bounds checks on array indexing.** Modest. Pulls forward the
   panic-on-error vs Result decision.
4. **Struct field access.** Codegen needs to know struct layout
   (offsets). Cranelift's `StackSlot` covers in-frame allocations;
   field access becomes `stack_load` with a static offset. Generics
   wait for after this.

Decision to pin before strings/vectors land: how Rune handles heap
allocation. Options unchanged from LANGUAGE.md's memory-model table.
