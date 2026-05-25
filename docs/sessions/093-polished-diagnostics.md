# Session 093 — Polished diagnostics

**Date:** 2026-05-25
**Outcome:** Type-error messages now reference user-
visible names (`Point`, `Color`, `Option`, `T`) instead
of internal sym indices (`struct#83`, `enum#147`,
`T#149`). Quality-of-life pass affecting every error
message the checker emits. 417 codegen + 154 typecheck
tests green (+2 typecheck, codegen unchanged).

```rune
struct Point { x: i64, y: i64 }
fn use_point(p: Point) -> i64 { p.x }
fn main() -> i64 { use_point(42) }
// Before: "argument 1 has type `i64`, expected `struct#83`"
// After:  "argument 1 has type `i64`, expected `Point`"
```

## The decisive observation

`Ty::display` in ty.rs has no access to the resolver,
so it falls back to `struct#{id}`, `enum#{id}`,
`dyn#{id}`, `T#{id}` whenever it hits a user-defined
type or a TypeVar. This was fine for compiler internals
but became increasingly user-visible as the checker's
diagnostics matured.

Three options for the fix:

1. Pass resolver into `Ty::display` (changes API
   shape across the codebase).
2. Add a parallel `display_with(resolver)` on Ty.
3. Add a wrapper method on the checker that walks Ty
   recursively, looking up names from
   `self.res.symbols` when possible.

Option (3) — `Checker::ty_pretty(&self, ty: &Ty) ->
String` — wins because:
- The only callers that need friendly names are
  inside checker error messages (codegen errors are
  internal "type X not supported" cases).
- The checker already has `self.res` everywhere.
- The walk is identical to `Ty::display` except for
  the `Struct` / `Enum` / `Dyn` / `TypeVar` arms,
  which look up `res.symbol(id).name` instead of
  formatting `prefix#{id}`.
- Inference TypeVars (session 062's u32::MAX
  countdown) fall back to `"_"` instead of the
  4-billion-ish sym index.

### Bulk replace, two pitfalls

`99` `.display()` call sites in checker.rs needed to
switch to `self.ty_pretty(&...)`. A sed regex
`(\w+)\.display\(\)` → `self.ty_pretty(&\1)` handled
96 of them; three needed manual fixes:

1. `Ty::Tuple(elem_tys.to_vec()).display()` — the
   "lhs" wasn't a bare identifier, so the regex
   skipped it. Manual rewrite.
2. `ret_args[1].display()` (twice) — same shape,
   subscript expression as lhs. Manual rewrite.

**The trap**: the regex also matched the *ty.rs
display fallback* inside my new `ty_pretty` helper —
`_ => ty.display()` → `_ => self.ty_pretty(&ty)`. That
made `ty_pretty` infinitely recursive on every
non-special case (Bool, Char, primitives, ...) and
the first test that triggered a diagnostic (a bool
binop) overflowed the stack. Reverted that one site
manually after the sed run.

### Cleanup: redundant bespoke branch

Session 090's `check_into_impl_duplicates` had its
own struct/enum-name lookup baked in:

```rust
let target_name = match &target_ty {
    Ty::Struct(s, _) | Ty::Enum(s, _) => {
        self.res.symbol(*s).name.clone()
    }
    _ => target_ty.display(),
};
```

Now folded into the single `ty_pretty` call. The
diagnostic reads the same; one less ad-hoc
name-lookup path.

## The wire-ups

```
src/checker.rs    (new ty_pretty(&self, ty) helper
                   at ~line 2501; 96 sed-driven
                   replacements of `.display()` →
                   `self.ty_pretty(&...)` plus 3
                   manual fixes; redundant
                   into-impl-duplicate name lookup
                   folded.)

tests/typecheck.rs  (+2 tests: struct name appears
                     in argument-type error, enum
                     name same.)
```

No ty.rs changes — `Ty::display` stays as the
fallback shape for sites that don't have a checker
(codegen errors, AOT errors, etc.). The checker's
`ty_pretty` is the user-facing path.

## What's tested

Typecheck (+2):

- `diagnostics_use_friendly_struct_name` — `use_point
  (42)` on `fn use_point(p: Point)` produces "expected
  `Point`" and explicitly does NOT produce
  "struct#NN".
- `diagnostics_use_friendly_enum_name` — same for
  enums.

## Apparent bugs that aren't / explicitly deferred

- **Codegen / AOT errors still use `Ty::display`**.
  Those messages are mostly "type T not supported in
  codegen" (rare in practice); switching them needs
  threading the resolver into codegen, which is a
  bigger refactor. Reserved for a later polish pass
  when codegen-side diagnostics become more
  user-facing.
- **Type-variable diagnostics with inference syms**
  show `_` instead of a stable name. For unannotated
  closure params with no body-side concrete pin, the
  error currently reads "closure parameter `acc`
  needs a type annotation (no contextual hint and no
  body usage to infer from)" — the `acc` name comes
  from the binding ident, not the TypeVar's
  ty_pretty, so this is fine.
- **`Self` in default-body errors** stays as
  literal `Self`. ty_pretty's `Ty::SelfType` falls
  to `Ty::display` which writes "Self" — the right
  thing for user-facing context.
- **`Ty::Error` shows as `?`**. Unchanged. The
  catch-all `_ => ty.display()` covers it.
- **HashMap / Vec / Tuple display with friendly
  element types**. `HashMap<i64, Point>` now reads
  with both element types friendly; previously read
  `HashMap<i64, struct#83>`. Tested implicitly via
  the new tests but not exhaustively.

## What's next

- **Binary-op hint flow** — `a: i32; a + 1` lets the
  `1` adopt i32 from the LHS.
- **Per-arm unreachability in tuple matches** —
  session 089's deferred item.
- **Const-eval overflow checks** — `100u8 + 200u8`
  runtime overflow rejected at compile time.
- **Self-hosted bootstrap** — long-term.
