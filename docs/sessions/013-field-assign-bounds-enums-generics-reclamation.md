# Session 013 — Field assignment, bounds checks, enums + generics/reclamation design

**Date:** 2026-05-19
**Outcome:** Three features land; two get full design passes in
[LANGUAGE.md](../../LANGUAGE.md) instead of code, because they're
each multi-session efforts. 252 tests green (+17 from 235).

## What shipped

### 1. Field assignment (`p.x = 5`)

- New `HirExprKind::FieldAssign { receiver, offset, field_ty, rhs }`.
- Lowerer detects `ast::Expr::Field` as the LHS of `Assign` and emits
  this variant instead of the local-binding `HirExprKind::Assign`.
  Offset + field type come from `CheckResults::struct_layouts`.
- Checker's `check_assign_target` learned a new helper
  `check_place_root_mutable` that walks `a.b.c` down to its root
  binding and verifies that root is `let mut`. Errors are
  context-specific: "cannot assign to field of immutable binding",
  "cannot assign to field of parameter", etc.
- Codegen `compile_field_assign` mirrors `compile_field_access` —
  same offset, same Cranelift type, `store` instead of `load`.

```rune
struct Counter { value: i64 }
fn main() -> i64 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value = c.value + 2;
    c.value
}
// → 3
```

### 2. Array + string bounds checks

- New runtime `rune_panic_bounds(idx, len)` — prints
  `"rune: index N out of range for length M"` to stderr and `exit(1)`s.
  Registered as a JIT symbol; defined in `RUNTIME_C` for AOT.
- New codegen helper `emit_bounds_check(idx, length)`:
  - Builds `lo_ok = idx >= 0 && hi_ok = idx < length`.
  - `brif in_bounds` to an `ok_blk`; the panic branch calls
    `rune_panic_bounds(idx, length)` then `trap`s.
  - After the helper returns, the builder sits in `ok_blk` so the
    caller's load proceeds normally.
- Wired into `compile_index` (arrays — length from `Ty::Array`) and
  `compile_str_byte_index` (strings — length loaded from descriptor
  offset 8).
- Slice indexing (`s[a..b]`) keeps its **clamp** behavior in the
  runtime — that's a deliberate asymmetry, documented in LANGUAGE.md.

Test pattern: an in-bounds program returns its normal exit code; an
out-of-bounds program exits non-zero with the panic message on
stderr. JIT path isn't tested directly because `exit(1)` would kill
the test process — AOT subprocess captures it cleanly.

### 3. Enum codegen (unit variants)

- New `SymbolKind::EnumVariant { enum_sym, discriminant }`. Each
  variant gets its own `Symbol` at `declare_item` time; symbols sit
  outside lexical scope and are addressed via a new
  `Resolutions::enum_variants` map.
- Resolver's `resolve_path` learned two-segment paths — the only
  shape it handles today is `EnumName::VariantName`. Longer paths
  produce an explicit error.
- Type checker's `path_value_type` returns `Ty::Enum(enum_sym)` for
  an `EnumVariant` symbol. `check_assign_target` rejects assignment
  to a variant ("cannot assign to enum variant").
- New `HirExprKind::EnumVariant { discriminant }`. Lowerer routes
  resolved variant paths here.
- Codegen: `EnumVariant` emits `iconst.i64(discriminant)`. `Ty::Enum`
  is wired into both `cranelift_type` (I64) and `elem_size` (8).
- `==`/`!=` on enum values fall through to the existing `icmp` path
  because their Cranelift type is I64.

```rune
enum Mode { On, Off }
fn is_on(m: Mode) -> bool { m == Mode::On }
fn main() -> i64 {
    if is_on(Mode::On) { 1 } else { 0 }
}
```

Match codegen and payload variants stay deferred. Without `match`,
payload variants don't have a useful consumption story, so they
explicitly error at construction.

## What got designed instead

### Generics

A real implementation needs **monomorphization** — each `f<T>` call
site compiles a specialized copy of `f`. The full roadmap is in the
"Type system" section of [LANGUAGE.md](../../LANGUAGE.md): parser
disambiguation between `<` as comparison vs generic args, `Ty::TypeVar`
in the type system, substitution, instantiation cache, mangled names,
inference at call sites.

Step 1 alone (parser support for `<T>` in declarations and call sites)
is a session's worth of work because of the `<` overload. Shipping a
half-version where `<T>` parses but does nothing useful would just
create a regression target for the real implementation.

The pragmatic call: defer until there's a use case that justifies the
multi-session cost. `Vec` works fine as the concrete i64-only type
for the test corpus; `print` works fine as a `PolyBuiltinFn`. The
biggest pull on generics today is "I want `Vec<str>`" — but that's
also the test case that catches every monomorphization bug.

### Reclamation

A five-step ladder is laid out in the "Memory model" section of
[LANGUAGE.md](../../LANGUAGE.md):

1. **Manual `free(x)` builtin** — lowest friction, unsafe escape
   hatch. Probably the next step.
2. **ARC** — refcount fields in heap descriptors, codegen emits
   inc/dec on copies and drops. ~5-15% perf overhead, cycle leaks
   without `weak`.
3. **Arenas with explicit scope** — `arena foo { ... }` blocks.
4. **Borrow checker** — full ownership, multi-session.
5. **Tracing GC** — not the systems-language choice unless we pivot.

The leak doesn't bite today because programs are short-lived. It
starts to matter when someone writes a real daemon or a benchmark
loop that allocates unboundedly.

## File layout changes

```
src/
├── hir.rs       (HirExprKind::FieldAssign, EnumVariant variants)
├── lower.rs     (FieldAssign dispatch, EnumVariant lowering)
├── checker.rs   (check_place_root_mutable, path_value_type for variants,
                  EnumVariant in check_assign_target match)
├── codegen.rs   (compile_field_assign, emit_bounds_check + wire-through,
                  EnumVariant arm, panic_bounds runtime + JIT registration,
                  Ty::Enum in cranelift_type and elem_size)
├── resolver.rs  (SymbolKind::EnumVariant, declare_item registers variants,
                  resolve_path handles 2-segment EnumName::Variant,
                  Resolutions::enum_variants map)
└── aot.rs       (RUNTIME_C gets rune_panic_bounds)
tests/
├── codegen.rs   (+3 field-assignment, +5 enum codegen)
├── typecheck.rs (+4 field-assignment)
└── aot.rs       (+5 bounds-check subprocess tests, +build_and_capture_full)
LANGUAGE.md      (Reclamation roadmap subsection; Generics roadmap
                  subsection; decision log entry)
```

## Apparent bugs that aren't

- **Slicing clamps; byte-indexing panics.** Deliberate. Slicing
  `s[a..b]` has a natural clamp ("everything in range that's also in
  the string"); reading a single byte at an out-of-range index
  doesn't. Same as Rust.
- **`Color::Red` resolved by string lookup.** The resolver's
  `enum_variants` map is keyed by `(enum_sym, variant_name_string)`.
  No interning of variant names. Fine at our scale; future
  optimization if it ever shows up in profiles.
- **Match still doesn't work.** Enum variants codegen but
  `match c { Mode::On => ... }` is still `HirExprKind::Unsupported`.
  Users dispatch via `if c == Mode::On { ... }` chains today.

## Test coverage added

Codegen (+8):
- 3 field-assignment tests: basic, repeated through alias, in a loop.
- 5 enum tests: discriminant order, A == A, A != B, passed to fn,
  returned from fn.

Typecheck (+4):
- field assignment OK; immutable error; parameter error; wrong-rhs-type
  error.

AOT (+5):
- in-bounds + out-of-bounds + negative index for arrays, plus
  in-bounds + out-of-bounds for string byte indexing. All assert exit
  code + stderr content via `build_and_capture_full`.

252 tests green from 235.

## Next session

Picking from the remaining work, in roughly increasing cost:

1. **Manual `free(x)` builtin** — implement step 1 of the
   reclamation ladder. Modest. Touches the `Vec`/concat-`str` runtime
   to make `free` callable; unsafe but useful for long-running
   programs.
2. **Match codegen.** Needed to make payload-bearing enum variants
   useful. Significant — pattern compilation, exhaustiveness, jump
   tables.
3. **Generics, step 1 (parser).** Get `<T>` parsing without
   regressing comparison-`<`. The hard part is the lookahead /
   try-and-rewind logic.
4. **More string methods.** `find`, `byte_at`, `repeat`,
   `trim`. Mechanical via runtime calls; each is ~50 lines.
5. **`for x in vec` iteration.** Symmetrical with `for x in array`
   but reading through the heap descriptor. Modest.

Decisions worth pinning before (2) lands: how exhaustiveness checking
works (error / warning / `_` required at the end), and whether
`match` is required to be a value expression or can be statement-only.
