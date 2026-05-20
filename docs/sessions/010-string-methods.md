# Session 010 — Method calls + first string methods

**Date:** 2026-05-19
**Outcome:** Method-call infrastructure wired through HIR, checker, lowerer,
and codegen. Three methods land — `str.len()`, `str.is_empty()`,
`arr.len()`. 192 tests green (+16 new). The greet example now uses
`g.len()` to total greeting bytes:

```
$ rune run examples/greet.rn
Hello, world!
Hello, Rune!
Hello, Cranelift!
greeted (count, total bytes):
3
42
```

## Mechanism

Three moving pieces (mirroring the polymorphic-print pattern from
session 009):

1. **`HirExprKind::MethodCall { receiver, method, args }`** in HIR.
   Parser already produced `ast::Expr::MethodCall`; previously lowered
   to `Unsupported(...)`. Now lowers properly.
2. **Type checker** has a hardcoded method table:
   ```rust
   fn resolve_method(recv: &Ty, name: &str) -> Option<MethodSig> {
       match (recv, name) {
           (Ty::Str,         "len")      => Some(MethodSig { params: vec![], ret: i64 }),
           (Ty::Str,         "is_empty") => Some(MethodSig { params: vec![], ret: bool }),
           (Ty::Array(_, _), "len")      => Some(MethodSig { params: vec![], ret: i64 }),
           _ => None,
       }
   }
   ```
   `check_method_call` resolves, validates arity + argument types,
   returns the declared return type. Unknown methods error with
   `no method `.X` on type `Y``.
3. **Codegen** dispatches by `(receiver.ty, method)`. Trivial methods
   are inlined — no runtime call:
   ```rust
   (Ty::Str, "len") => {
       // Descriptor layout: { ptr @ 0, len @ 8 }
       Ok(Some(self.builder.ins().load(types::I64, MemFlags::new(), recv_val, 8)))
   }
   (Ty::Str, "is_empty") => {
       let len = self.builder.ins().load(types::I64, MemFlags::new(), recv_val, 8);
       let zero = self.builder.ins().iconst(types::I64, 0);
       Ok(Some(self.builder.ins().icmp(IntCC::Equal, len, zero)))
   }
   (Ty::Array(_, length), "len") => {
       // Length is in the type — runtime receiver value isn't even read.
       Ok(Some(self.builder.ins().iconst(types::I64, *length as i64)))
   }
   ```

For methods with non-trivial semantics (future: `str.starts_with`,
`str.byte_at`, etc.), the codegen path can route to a runtime function
via the existing `ensure_runtime_func` helper. No infrastructure change
needed; just add the runtime symbol and add a case.

## Why a hardcoded table

The cleanest design for v0.x. Tradeoff cost:
- New method on a builtin type = one row in `resolve_method` + one arm
  in `compile_method_call`.
- New type with methods (user-defined `impl`) = needs a proper method
  resolution scheme. That's a future session.

`PolyBuiltinFn` (session 009) and this method table are both stopgaps
that retire when generics or traits arrive. They're small enough to
delete cleanly later — no big abstractions to undo.

## Method receiver is a place expression for now

`x.len()` evaluates `x` once. Side effects in the receiver are still
side-effected (we always call `compile_expr(receiver)` first). Arguments
are also evaluated even though the current methods ignore them
(reserved for forward compatibility).

Returning the array length without reading the receiver is a minor
optimization the codegen does opportunistically — the `iconst(length)`
ignores the receiver's value but still emits its IR so side effects
land.

## File layout changes

```
src/
├── hir.rs       (HirExprKind::MethodCall variant added)
├── lower.rs     (lower ast::Expr::MethodCall → HIR variant)
├── checker.rs   (check_method_call, MethodSig, resolve_method)
└── codegen.rs   (compile_method_call dispatching on (recv_ty, method))
tests/
├── codegen.rs    (+9 method tests)
├── typecheck.rs  (+5 method tests: typechecks pass, errors on unknown
                  method and wrong arg count)
└── aot.rs        (+2: print(s.len()) and print(arr.len()))
examples/
└── greet.rn     (now uses g.len() to total greeting bytes)
```

## Apparent bugs that aren't

- **`s.len()` returns byte count, not character count.** For ASCII
  this matches intuition; for UTF-8 with multibyte sequences (e.g.,
  "héllo"), `.len()` is 6 not 5. Same as Rust's `&str::len`.
- **`[1,2,3].len()` doesn't read the array.** Length is in the type;
  codegen emits a static `iconst`. The receiver is still compiled to
  preserve side effects, but the resulting Value is discarded.
- **`is_empty()` returns an i8.** Rune's bool is one byte. Anyone
  inspecting the generated assembly will see a `cmp` + `set*` sequence,
  not a single-bit operation. That's fine; Cranelift's `icmp` returns
  the canonical i8 representation we use everywhere.

## Test coverage

Codegen (9):
- `"hello".len()` → 5
- `"".len()` → 0
- `"".is_empty()` → true, `"hi".is_empty()` → false
- `s.len()` on a let-bound string
- `(a + b).len()` on a concat result
- `arr.len()` on a literal array, and used in arithmetic
- Plus a placeholder test for UTF-8 byte semantics (using ASCII for now;
  multibyte test waits on `\u{...}` lexer support)

Typecheck (5):
- `.len()` and `.is_empty()` on str typecheck.
- `arr.len()` typechecks.
- `(5).len()` errors with "no method".
- `"hi".len(1)` errors with "expects 0 argument".

AOT (2):
- `print("hello".len())` outputs `5`.
- `print([10, 20, 30].len())` outputs `3`.

## Next session

The remaining strings story:

1. **String indexing / slicing.** `s[i]` returns one byte (i64 widened
   from u8); `s[a..b]` returns a slice of the original. The slice needs
   a strategy: does it allocate a new descriptor pointing into the old
   bytes (zero-copy, lifetime tied to original), or copy out fresh?
   Forces the lifetime question we've been avoiding.

2. **More string methods.** `starts_with`, `ends_with`, `contains`,
   `byte_at` — runtime-routed, mechanical. Doesn't force any new
   design decisions.

3. **Heap-allocated vectors.** Same heap leak story as concat. Adds a
   parameterized type (`Vec<T>` or `Vec[T]`?) — touches generics.

4. **Bounds checks on array indexing.** Pulls forward the panic-vs-
   abort decision. Smallest scope.

5. **User-defined methods via `impl`.** Bigger — requires storing
   per-type method tables. Foundation for moving past the hardcoded
   builtin table.

Decision to pin before (1) lands: slice lifetime semantics — is `s[a..b]`
zero-copy (then it can't outlive `s`), or always a heap-allocated copy?
