# Session 090 — Same-target Into duplicate detection

**Date:** 2026-05-25
**Outcome:** `impl Into<AppErr> for IoErr { ... }`
declared twice (or via two structurally-equivalent
target paths) is now rejected at type-check. Closes
session 086's deferred item. 411 codegen + 146
typecheck tests green (+2 typecheck, codegen unchanged).

```rune
impl std::Into<AppErr> for IoErr { fn into(self) -> AppErr { ... } }
impl std::Into<AppErr> for IoErr { fn into(self) -> AppErr { ... } }
// → type error: duplicate `impl Into<AppErr> for IoErr` —
//   a previous `impl` block already declared this conversion
```

## The decisive observation

Session 072 already stored every per-source Into impl
in `into_impls: HashMap<SymbolId, Vec<(Type, SymbolId)>>`
— that's the multi-impl disambiguation table the
checker walks at `?` sites and (since session 086) at
bare `.into()` sites with hints. The deferred piece
was the *validity* check: nothing rejected two impls
with identical targets, so the disambiguation table
ended up with duplicate target entries; first-match-wins
picked whichever appeared first in source order, with
no diagnostic.

The fix is a single checker pass that resolves each
recorded target's AST to a `Ty` and structurally
compares against earlier impls for the same source.

```rust
fn check_into_impl_duplicates(&mut self) {
    for (source_sym, impls) in self.res.into_impls /*cloned*/ {
        let mut seen: Vec<(Ty, Span)> = Vec::new();
        for (target_ast, fn_sym) in impls {
            let target_ty = self.resolve_type(&target_ast);
            for (prev_ty, _) in &seen {
                if target_ty.compatible(prev_ty) {
                    self.error(fn_span, "duplicate `impl Into<...>`...");
                    break;
                }
            }
            seen.push((target_ty, fn_span));
        }
    }
}
```

Resolving via `resolve_type` (not raw AST equality)
catches the textually-different case: `impl
Into<AppErr> for IoErr` and `impl Into<mod::AppErr>
for IoErr` end up at the same `Ty::Struct(s, _)` if
both paths resolve to the same sym. AST equality would
miss that.

Why **after** `register_signatures`: `resolve_type`
needs the resolver state (which fires at parser/resolve
time) and the checker's internal state for path
resolution to be fully populated. Running the
duplicate check as its own pass between signature
registration and body-checking gives us the right
phase ordering — and means the duplicate diagnostic
fires *before* any body type-checking that might
otherwise pollute the error stream with cascading
issues from the colliding impls.

### Why `compatible` over `==`

`Ty::compatible` returns true for the legitimate cases
(equal concrete types, `Ty::Struct(s, _)` vs the same
sym, etc.). It's also lenient on `TypeVar` — both sides
match anything — which v0.x's non-generic Into impls
never hit. For pathological future cases (`impl
Into<Box<T>> for X` with generic T), strict equality
would miss the duplicate; `compatible` flags it. That's
the conservative direction (over-report duplicates
rather than miss them).

The first impl's span isn't surfaced in the
diagnostic; pointing at the later impl is the
actionable side (remove that block). Future polish:
attach a "previously declared here" note pointing at
the first impl's span.

## The wire-ups

```
src/checker.rs    (check_into_impl_duplicates added;
                   called from check_module after
                   register_signatures, before pass 2
                   body checks. Friendly name in
                   diagnostic via res.symbol lookup
                   when target resolves to Ty::Struct
                   / Ty::Enum.)

tests/typecheck.rs  (+2 tests: duplicate-target
                     impls reject with the friendly
                     diagnostic; distinct-target
                     impls still type-check clean.)
```

No resolver, lower, mono, or codegen changes — the
resolver already tracked every per-source impl, this
session just adds the validation pass that previously
didn't exist.

## What's tested

Typecheck (+2):

- `duplicate_into_impl_rejected` — two `impl
  Into<AppErr> for IoErr` blocks → error containing
  the friendly name "duplicate `impl Into<AppErr> for
  IoErr`".
- `distinct_into_targets_accepted` — `Into<AppErr>` +
  `Into<DbErr>` on the same source → clean.

Codegen unchanged (the existing
`into_disambiguation_*` tests in session 086 still
pass — each uses distinct targets).

## Apparent bugs that aren't / explicitly deferred

- **"Previously declared at" note** — the diagnostic
  points at the later impl's `into` method span but
  doesn't currently include a secondary span for the
  first impl. The first impl's `fn_span` is kept in
  `seen` and could be surfaced as a note; deferred
  for a multi-span diagnostic pass that touches every
  "redefinition" check (impl_methods, etc.).
- **Generic-parameterized targets** — `impl
  Into<Box<T>> for X` with T as a generic on the
  impl block. The current pass uses `compatible`
  which treats TypeVar as matching anything; two
  `impl<T> Into<Box<T>> for X` blocks would flag as
  duplicate even if their bodies do different things
  per T. v0.x's Into impls don't take generic
  targets in practice, so this is a future concern.
- **Cross-module Into impls** — `impl Into<a::T>
  for X` in module a and `impl Into<b::T> for X` in
  module b where a::T and b::T are distinct types.
  Both resolve to their respective `Ty::Struct(_,
  _)`, which compare equal only if the syms match.
  If a::T and b::T are *different syms*, `compatible`
  returns false. Correct behavior.
- **`Into<Self>`** — `impl Into<IoErr> for IoErr` (a
  trivial identity conversion) flags as valid because
  there's only one such impl. If you declare two,
  the duplicate check fires. Same shape as any other
  target — no special casing needed.

## What's next

- **Integer literal hint flow** — `let x: i32 = 10;`
  picks i32 from the annotation.
- **Suffix overflow checks** — reject `1000u8`.
- **Per-arm unreachability in tuple matches** —
  session 089's deferred item.
- **Self-hosted bootstrap** — long-term.
