# Session 003 — Resolver + Type Checker

**Date:** 2026-05-19
**Outcome:** Front end complete. Name resolution + monomorphic type checker
landed in two new modules, 40 integration tests added (98 total green).
`rune check examples/hello.rn` reports `ok`.

## Goal

Walk the AST and answer two questions:
1. **What does each identifier refer to?** (resolver)
2. **What's the type of each expression, and is the program consistent?**
   (type checker)

Concrete bar: `examples/hello.rn` passes `rune check` without errors.

## Three decisions pinned this session

| Decision | Choice | Was |
| --- | --- | --- |
| Default integer type | **`i64`** | placeholder `i32` |
| Mutability enforcement | **Strict** — assignment to immutable is a type error | Tentative |
| Same-scope shadowing | **Allowed** — `let x = ..; let x = ..;` rebinds | Open |

LANGUAGE.md statuses updated to **Decided** for all three.

### Why `i64` over Rust's `i32`

Rune targets 64-bit native code via Cranelift. Word-sized default means
indexing into arrays (`usize`) and calling OS APIs doesn't require constant
casts. Swift picked the same default for `Int`. Rust's `i32` choice is
historical — pre-2015, when 32-bit was the dominant client target.

## File layout added

```
src/
├── ty.rs        (new — semantic types: Ty, IntTy, FloatTy, SymbolId)
├── resolver.rs  (new — Symbol, SymbolKind, Resolutions, Resolver)
└── checker.rs   (new — TypeError, CheckResults, Checker)
tests/
└── typecheck.rs (new, 40 tests)
```

Plus updates to `src/lib.rs`, `src/main.rs` (new `rune check` subcommand),
and a one-line `Hash` derive on `Span` so it can key the type tables.

## Architecture

Resolver and type checker are **separate passes**. Each runs over the same
AST and produces a parallel data structure keyed by span. This keeps each
phase testable in isolation and avoids the resolver needing to know about
types.

```
Module ──▶ Resolver ──▶ Resolutions { symbols, path_to_sym, decl_to_sym }
   │                          │
   └────────────────┬─────────┘
                    ▼
              Checker ──▶ CheckResults { expr_types, fn_signatures, ... }
```

### Semantic types — `src/ty.rs`

Separate from `ast::Type` (which is a *syntactic* path expression).
Source-level `i64` is `ast::Type::Path(...)`; resolved, it becomes
`Ty::Int(IntTy::I64)`.

```rust
enum Ty {
    Bool, Char, Int(IntTy), Float(FloatTy), Str, Unit,
    Array(Box<Ty>, usize),
    Fn { params: Vec<Ty>, ret: Box<Ty> },
    Struct(SymbolId), Enum(SymbolId),
    Never,   // for return / break / continue
    Error,   // sentinel to silence cascade errors
}
```

`Ty::Error` compares **compatible with everything**. That's a deliberate
choice: once a subexpression has had an error reported, follow-on errors
that mention it would just be noise.

`Ty::Never` similarly unifies with any type. It's how we say "this branch
diverges so the type comes from the other arm" — useful for
`if cond { return x; } else { y }` where the if's type is `y`'s type.

### Resolver — `src/resolver.rs`

Two-pass:
1. **Declare** every top-level item (`fn`, `struct`, `enum`, `const`) into
   the global scope. Now forward references work — `fn a() { b() }
   fn b() {}` is fine.
2. **Resolve** each item's body. Identifiers in expressions and types are
   looked up against the scope chain (innermost first).

Built-in type names get inserted as `SymbolKind::BuiltinType(Ty)` at
construction time. When the checker later wants to know what `i64` means,
it follows the resolved path to the symbol and reads the embedded `Ty`.

Scope chain is a `Vec<HashMap<String, SymbolId>>`. `enter_scope` pushes,
`exit_scope` pops. `lookup` walks from the back. **Shadowing** falls out
naturally: `intern` overwrites the entry in the topmost scope, but the
old `Symbol` stays in `symbols` keyed by its declaration span — so
references that resolved to it earlier are still valid.

### Type checker — `src/checker.rs`

Single bottom-up walk. Each `check_expr` returns the expression's `Ty` and
records it in `expr_types[span]`.

Pre-pass before walking bodies: gather function signatures so calls in any
order work. The checker also gathers `const` declared types in the same
pre-pass, since they may be referenced from any function.

Mutability is checked in `check_assign_target`: an assignment's LHS must
be `Expr::Path` resolving to a `SymbolKind::Local { mutable: true }`.
Anything else is rejected with a context-specific message
("cannot assign to immutable binding", "to parameter", "to const",
"to function", "to type").

### Things deliberately deferred

| Feature | Note |
| --- | --- |
| Struct field type checking | Resolver knows struct names; field types not threaded yet |
| Enum variant typing | Same — variants resolve as names, but pattern-matching them is later |
| Method call dispatch | Stubbed out with a "not yet type-checked" error |
| Field access | Same |
| `?` operator | No `Result` story yet, so `?` is stubbed |
| Generics | No `<T>` parsing yet |
| Range expression types | Parser stub, not checker-aware |
| Closure types | Closures aren't parsed |
| String indexing | `xs[0]` on `str` is treated as no-op for now |

These are flagged in `check_expr_inner` and emit explicit "not yet
type-checked" errors so the user knows it's an unimplemented feature
rather than a parser bug.

## Apparent bugs that aren't

- **`Ty::Error` accepts everything.** Intentional — we don't want one
  malformed expression to cascade into 20 spurious errors.
- **Comparison operators allow `char` operands.** `let _ = 'a' < 'z'` is
  accepted. Rust does the same.
- **`int + float` is a hard error**, not an automatic widening. There's no
  implicit numeric coercion. If you want it, write `(x as f64) + y`.

## `rune check` output

The new CLI subcommand chains lex → parse → resolve → check. On clean code
it prints `ok` and exits 0; on errors it prints each diagnostic with its
byte span and exits non-zero. Pipeline command for a quick sanity test:

```
$ rune check examples/hello.rn
ok
```

## Next session

**HIR + Cranelift codegen.** Lower the AST to an HIR that's friendlier to
codegen (resolved types attached to every node, control-flow desugared,
no `Ty::Error` paths), then walk it producing Cranelift IR. Goal: run a
Rune `main()` that adds two integers and exits with the result as the
status code.

Decisions to pin first:
- HIR shape — duplicate AST with type annotations, or a flatter
  CFG-style IR?
- ABI for Rune functions — `extern "C"` by default, or a Rune-specific
  calling convention?
- Entry point — `main()` returning `()` or `i32`? Cranelift's `cranelift_jit`
  vs ahead-of-time `cranelift_object`?
