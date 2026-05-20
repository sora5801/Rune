# Session 004 — HIR + Cranelift codegen

**Date:** 2026-05-19
**Outcome:** First runnable Rune. `rune run examples/fib.rn` JIT-compiles
and prints `55`. 23 new codegen integration tests; 121 total green.

## Goal

Take type-checked Rune source to executing machine code. Concrete bar: a
recursive `fib(n)` returning the 10th Fibonacci number, computed by native
x86-64 produced by Cranelift, called from the Rust host via `transmute`.

## Three decisions pinned this session

| Decision | Choice |
| --- | --- |
| HIR shape | **AST-shaped with `Ty` attached.** Paths resolved to `SymbolId`. Unsupported features funneled into an `Unsupported(msg)` variant so lowering can complete and codegen reports the gap clearly. |
| Function ABI | **Target-native** (`SystemV` / `WindowsFastcall` / AAPCS — i.e. `extern "C"`). Trivial future C interop; no Rune-specific calling convention until at least v0.x. |
| Entry point + run | **`fn main() -> i64`**, JIT-compile via `cranelift-jit`, host calls main and prints the i64. AOT-to-object deferred to a later session. |

LANGUAGE.md "Compilation model" promoted from Tentative → Decided.

## Architecture

```
AST ─── Lowerer ───▶ HIR ─── Codegen ───▶ Cranelift IR ───▶ machine code
        (lower.rs)        (codegen.rs)
```

### HIR — `src/hir.rs`

Same tree shape as AST, but:
- `Path` collapses to `Local(SymbolId)` / `Fn(SymbolId)`.
- Every `HirExpr` carries its `Ty`.
- `Assign` and `AssignOp` LHS is restricted to a `SymbolId` (no
  index/field/deref places yet).
- `Call` callee is a `SymbolId` (no first-class functions yet).
- `&&` / `||` are split into a `Logical` variant (codegen needs branch-based
  short-circuiting; lumping them with `Binary` would be wrong).
- Anything we can't codegen lowers to `HirExprKind::Unsupported(msg)`. The
  lowerer never fails; codegen fails with a useful message if it touches an
  unsupported node.

### Codegen — `src/codegen.rs`

Single struct `Codegen` owns a `JITModule`. Per-function compilation:

1. **Pass 1** — declare every function in the module (with full signature).
   Forward references resolve.
2. **Pass 2** — define each body via a `FunctionBuilder`.

For each function:
- Create an entry block with parameters mapped to Cranelift block params.
- Allocate a `cranelift_frontend::Variable` per Rune local/param. The
  `Variable` abstraction does SSA + phi-insertion automatically — we just
  `def_var`/`use_var` and Cranelift figures out block params.
- Walk the HIR producing IR.

#### Control-flow patterns

`if cond { a } else { b }` (expression form, produces a value):

```
brif cond, then_blk, else_blk
then_blk:  ... ; jump merge(a_val)
else_blk:  ... ; jump merge(b_val)
merge(result):
```

`while cond { body }`:

```
        jump header
header: brif cond, body_blk, exit
body:   ... ; jump header
exit:
```

Short-circuit `a && b` lowers as `if a { b } else { false }` — same shape as
the if-expression but with synthesized constants. `a || b` is the dual.

#### The "block is filled" check

After emitting a terminator (`return_` / `jump` / `brif`) the current
Cranelift block can't accept more instructions. Cranelift's
`FunctionBuilder::is_filled` is **private** as of 0.115, so we query the
underlying layout directly:

```rust
fn is_filled(&self) -> bool {
    let Some(blk) = self.builder.current_block() else { return true; };
    let Some(last) = self.builder.func.layout.last_inst(blk) else { return false; };
    self.builder.func.dfg.insts[last].opcode().is_terminator()
}
```

That's the one bit of Cranelift internals we touch. Everything else is via
the public `FunctionBuilder` API.

#### Early return

`return` emits `return_(...)` then switches to a fresh unreachable block.
That keeps the IR well-formed for any code the lowerer might still emit
after the return (e.g., in `if cond { return 1; } 0`, the `0` needs a block
to live in even though it's unreachable after the return).

#### Calling conventions

Cranelift's `JITBuilder::with_isa` uses the host's native ISA, which carries
its own default `CallConv`. We don't override it. On x86-64 Linux/Windows
that maps to SystemV / WindowsFastcall — both compatible with C, so we can
`transmute` a function pointer to `extern "C" fn() -> i64` from the host.

### `rune run` host runtime

`src/main.rs::cmd_run`:

1. Lex → parse → resolve → type-check. Any errors → exit non-zero.
2. Lower to HIR.
3. Compile via Cranelift JIT.
4. Look up `main` (must be `Fn` symbol with that exact name).
5. `get_finalized_function(main)` → raw `*const u8`.
6. `transmute` to `extern "C" fn() -> i64` and call.
7. `println!` the result.

The `JITModule` stays alive for the call (it owns the executable memory).

## What's not in codegen yet (deliberate)

| Feature | Why deferred |
| --- | --- |
| Float arithmetic | Wired through `compile_lit` / `compile_binop_value` but no integration tests yet; defer until a stdlib forces it |
| Strings | No string runtime / allocator. Needs a story for owned vs borrowed slices first |
| Arrays | Needs stack allocation (`StackSlot`) or heap; revisit with memory model decision |
| `for` loops | Lowering needs iterator protocol or array indexing — pick one first |
| `match` | Pattern compilation is a session unto itself |
| Struct / enum values | Aggregate ABI is non-trivial; the type checker already treats them as opaque |
| Method calls, field access | Needs struct codegen first |
| `?` operator | Needs a `Result` story |
| `as` casts | Codegen path exists, but needs careful semantics (truncate vs sign-extend) and tests |
| AOT executables | `cranelift-object` instead of `cranelift-jit`, plus linker invocation |

Every one of these emits `HirExprKind::Unsupported(...)` at lowering and a
`CodegenError("unsupported in codegen: ...")` if compilation reaches it.

## Apparent bugs that aren't

- **Test programs intentionally use division-by-zero inside `&&` / `||`** to
  prove short-circuiting works:
  ```rune
  let safe = false && (10 / 0 > 0);  // never evaluates the rhs
  ```
  If short-circuit broke, Cranelift would emit a divide trap and the JIT
  would crash. It doesn't — the test runs cleanly.
- **`main` returning `()` is rejected at runtime, not type-check time.**
  The checker allows any `main` signature; `cmd_run` enforces `() -> i64`
  by `transmute`'ing to that type. A future improvement is a dedicated
  check that errors if the signature is wrong.

## File layout added

```
src/
├── hir.rs        (new — HirModule, HirItem, HirFn, HirExpr, ...)
├── lower.rs      (new — AST → HIR pass)
└── codegen.rs    (new — HIR → Cranelift JIT)
tests/
└── codegen.rs    (new — 23 end-to-end tests)
examples/
└── fib.rn        (new — first runnable Rune)
```

Cargo.toml gains five Cranelift dependencies (`cranelift`, `cranelift-jit`,
`cranelift-module`, `cranelift-frontend`, `cranelift-native`, all at
`0.115`). First clean build takes a couple of minutes; incremental rebuilds
are seconds.

`Span` derived `Hash` since the type tables key on it.

## Test coverage

23 end-to-end tests in `tests/codegen.rs`:

- Literal return, arithmetic, division/modulo
- Unary negation, bitwise ops
- `let` binding, `let mut` + assignment, compound assignment, shadowing
- `if`/`else` as expression, `if` with comparison condition
- `else if` chains
- `while` loop with accumulator
- Short-circuit `&&` and `||` (verified by dividing by zero in the rhs)
- Function calls — simple, forward reference, recursive factorial,
  recursive fib, mutual recursion
- Early `return`

## Next session

Pick one of:

1. **AOT compile to `.o` + link.** `cranelift-object` instead of
   `cranelift-jit`. Produces a real executable; opens the door to a
   self-hosting story eventually. Needs a linker (`cc` or `link.exe`).
2. **`print` builtin + I/O.** Host registers a Rune-callable `print(x: i64)`
   so Rune programs can do something with their values without going
   through the return code. Foundation for a stdlib.
3. **Arrays + `for` loops.** Adds heap or stack-array allocation, makes
   the `for ... in arr` syntax actually work. Pulls a memory-model
   decision forward.

Decisions to pin before any of these are picked: target output format
(executable vs library?), Cranelift opt level for release builds, and
where `print` lives (host-provided builtin vs Rune source).
