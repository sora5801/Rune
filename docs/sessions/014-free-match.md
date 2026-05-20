# Session 014 — Manual `free(x)` and match codegen

**Date:** 2026-05-19
**Outcome:** Step 1 of the reclamation ladder lands, plus full match
codegen including enum variant patterns. 270 tests green (+18 from
252).

## What shipped

### `free(x)` builtin — reclamation step 1

Mirrors the polymorphic `print(x)` pattern from session 009:

- Resolver registers `free` as `SymbolKind::PolyBuiltinFn("free")`.
- Checker's `check_poly_builtin_call("free")` accepts a single
  `Ty::Vec` or `Ty::Str` argument; errors with
  `"only accepts heap-allocated values"` for anything else.
- Lowerer dispatches `("free", Vec) → "free_vec"` and
  `("free", Str) → "free_str"`.
- Codegen has new `declare_builtin` cases for both.

Runtime, Rust side (JIT):

```rust
extern "C" fn rune_runtime_free_str(s: *mut RuneStr) {
    use std::alloc::{dealloc, Layout};
    unsafe {
        if s.is_null() { return; }
        let s_ref = &*s;
        if s_ref.len > 0 && !s_ref.ptr.is_null() {
            let bytes_layout = Layout::from_size_align(s_ref.len as usize, 1).unwrap();
            dealloc(s_ref.ptr as *mut u8, bytes_layout);
        }
        dealloc(s as *mut u8, Layout::new::<RuneStr>());
    }
}
```

Runtime, C side (AOT): standard libc `free(s->ptr); free(s);`.

The two backends use different allocators (`std::alloc` for Rust JIT,
libc `malloc`/`free` for AOT) but each is self-consistent — values
allocated by a JIT runtime are freed by the same JIT runtime, and
same for AOT.

**Caveat (documented):** `free` on a literal string is UB because the
bytes live in `.rodata`. Double-free and use-after-free are UB.
Users are expected to know which strings are heap-allocated (concat
results, slice results) vs literal.

### Match codegen

The full pattern-matching pipeline lands. Supported patterns:

| Pattern | Example | Semantics |
| --- | --- | --- |
| Wildcard | `_ => ...` | Always matches; no binding |
| Bind | `x => ...` | Always matches; binds scrutinee to `x` |
| Int literal | `42 => ...` | `icmp eq scrutinee, 42` |
| Bool literal | `true => ...` | Same, on i8 |
| Str literal | `"yes" => ...` | Calls `rune_str_eq` (existing runtime helper) |
| Enum variant | `Color::Red => ...` | Compares discriminant via `icmp eq` |

Not supported (deferred):
- Guards (`x if x > 0 => ...`)
- Or-patterns (`1 | 2 | 3 => ...`)
- Payload destructuring (`Some(x) => ...`) — needs payload-bearing variants
- Range patterns (`1..=10 => ...`)

#### Pattern compilation strategy

Sequential `brif` chain. No decision tree, no jump table — for `N`
arms the worst case is `N` comparisons. Fine for the arm counts
typical of user code; if it ever becomes a bottleneck the lowerer
can produce a switch when all patterns are int literals.

```
                ┌──────────────┐
scrutinee──────▶│ check arm 0  │──no──▶┌──────────────┐
                │  brif        │       │ check arm 1  │──no──▶ ...
                └──────┬───────┘       └──────────────┘
                       │yes
                       ▼
                  ┌──────────┐
                  │ body 0   │──▶ jump merge(value)
                  └──────────┘                           ┌──────────────┐
                                                         │ panic fallback│
                                                         │ (no match)    │
                                                         └──────────────┘
```

The fallback block calls a new runtime function
`rune_panic_no_match` that prints `"rune: no match arm matched"` to
stderr and `exit(1)`s. Same shape as the `panic_bounds` machinery.

#### Bind patterns

A bare identifier in pattern position binds the scrutinee value to
that name in the arm body's scope. The resolver still inserts the
binding via `declare_pattern` at the right point; the lowerer's
match arm emits `HirPattern::Bind(sym)` and the codegen materializes
a Cranelift `Variable` storing the scrutinee value.

For `match n { 0 => 0, x => x * 2 }`, the second arm always matches
(unconditional jump) and `x` is bound to `n` inside the body.

#### Path patterns

New `ast::Pattern::Path { path, span }`. The parser distinguishes
`Color::Red` (multi-segment path → Path pattern) from `red`
(single segment → Ident binding). The resolver runs `resolve_path`
on Path patterns so the existing 2-segment `EnumName::Variant`
machinery resolves the variant; the checker then validates the
variant's enum matches the scrutinee.

#### Exhaustiveness

**Not enforced statically.** v0.x rule: if no arm matches at
runtime, the program panics via `rune_panic_no_match`. The user is
expected to add `_` arms for safety; the compiler doesn't insist.

Compile-time exhaustiveness checking is a future feature. It needs:
- For bool: both `true` and `false` arms (or `_`).
- For int: `_` mandatory (infinite domain).
- For enums: all variants covered (or `_`).
- For str: `_` mandatory.

That's a reasonable next step on top of what's here.

## File layout changes

```
src/
├── ast.rs       (Pattern::Path variant; FieldInit unchanged)
├── parser.rs    (parse_pattern detects `Ident::` and parses Path)
├── resolver.rs  (declare_pattern handles Path; calls resolve_path)
├── checker.rs   (check_pattern_matches; bind_pattern handles Path)
├── lower.rs     (lower_match; lower_let handles Pattern::Path no-op;
                  lower_for rejects Path / Literal patterns)
├── hir.rs       (HirExprKind::Match, HirMatchArm, HirPattern)
├── codegen.rs   (compile_match + compile_pattern_check;
                  rune_runtime_panic_no_match host fn;
                  rune_runtime_free_str / free_vec host fns;
                  declare_builtin cases for free_str, free_vec,
                  panic_no_match)
└── aot.rs       (RUNTIME_C adds rune_free_str, rune_free_vec,
                  rune_panic_no_match)
tests/
├── codegen.rs   (+9 match tests, +3 free tests)
├── typecheck.rs (+5 free tests)
└── aot.rs       (+1 match no-arm panic test)
LANGUAGE.md      (decision log entry)
```

## Apparent bugs that aren't

- **Match without `_` may compile but trap at runtime.** Intentional
  — exhaustiveness checking isn't implemented yet. The runtime
  backstop catches the case in a debuggable way (clear error
  message, exit 1).
- **`free(literal)` is UB.** The type checker can't distinguish
  literal `str` from heap-allocated `str` (both have type `Ty::Str`).
  Documented, won't be fixed until we have a tracking story (perhaps
  a separate `OwnedStr` type, or a refcount flag).
- **Match arm binding shadows outer scope.** A `match x { y => ... }`
  binds `y` even if `y` exists in the enclosing scope. Same as Rust.

## Test coverage added

Codegen (+12):
- 3 `free` tests: free a Vec, free a concat str, free in a 1000-iteration
  loop (sanity check, doesn't crash).
- 9 match tests: int literal arms, wildcard fallthrough, binding pattern,
  enum variants, enum + wildcard, bool match, str match, match-as-statement
  with unit arms, match-in-expression-position binding to a variable.

Typecheck (+5):
- `free` accepts Vec/str; rejects i64, bool, zero args.

AOT (+1):
- Match with no matching arm exits non-zero and prints
  "no match arm matched" to stderr.

## Next session

- **Compile-time exhaustiveness checking.** Lift the runtime backstop
  to a compile error. Needs domain analysis per type.
- **Match guards** (`x if cond => ...`). Modest; arm-level conditional
  on top of pattern matching.
- **Or-patterns** (`1 | 2 | 3 => ...`). Modest; multi-pattern arm.
- **Payload destructuring** for non-unit enum variants. Bigger; needs
  payload codegen first.
- **ARC (reclamation step 2).** Replaces manual `free` with automatic
  refcount-based reclamation. Big — touches every alloc, every
  copy/move, and every drop point.
- **Generics step 1: parser.** Disambiguate `<T>` from comparison-`<`.
  Foundation for parametric polymorphism.
