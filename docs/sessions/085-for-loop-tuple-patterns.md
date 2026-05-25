# Session 085 — For-loop tuple patterns

**Date:** 2026-05-25
**Outcome:** `for (k, v) in m.entries() { ... }` works
directly. Session 075's `let (k, v) = kv` workaround
is no longer needed. 394 codegen tests green (+4 from
session 084).

```rune
for (k, v) in m.entries() {                  // direct
    total = total + k * v;
}

for (_, v) in m.entries() {                  // wildcard slot
    total = total + v;
}

for (k, v) in str_keyed_map.entries() {      // str keys
    total = total + k.len() * v;
}
```

## The decisive observation

All the pieces existed; this session just connected them.

### 1. `lower_for` accepts `Pattern::Tuple`

`lower_for` previously short-circuited on Pattern::Tuple
with "for-loop pattern must be an identifier or `_`". Lift
that restriction. The for-pat goes through to
`lower_for_iterator` (the Iterator-protocol path); range
and array fast paths still reject tuple patterns because
those yield scalars (`for (a, b) in 0..10` wouldn't
typecheck regardless).

### 2. `lower_for_iterator` threads the pattern into the some-arm body

Previously:

```rust
if let Some(user_sym) = local {
    some_body_stmts.push(HirStmt::Let(HirLet {
        sym: Some(user_sym),
        ty: item_ty.clone(),
        init: Some(HirExpr { kind: Local(x_sym), ty: item_ty.clone() }),
        ...
    }));
}
let body_hir = self.lower_block(body);
some_body_stmts.extend(body_hir.stmts);
```

The desugar binds the freshly extracted item `__x` to
the user's pattern symbol when the pattern is an
`Ident`. For tuple patterns, we want the same shape but
with destructure stmts in place of the single alias-let.

Session 074 already has the helper —
`expand_tuple_let_from_local(patterns, source_sym,
source_ty, out)`. Call it with the some-arm's freshly
bound `x_sym` and the item type:

```rust
if let ast::Pattern::Tuple { patterns, .. } = pat {
    self.expand_tuple_let_from_local(patterns, x_sym, &item_ty, &mut some_body_stmts);
} else if let Some(user_sym) = local {
    // existing alias-let
}
```

The destructure produces a sequence of
`let <leaf> = __x.<i>;` stmts that bind each tuple-arity
leaf to its corresponding TupleIndex. Wildcard slots
emit an evaluate-and-discard.

### 3. Latent `apply_subst_ty` gap fix

First test surfaced a codegen error: "type `T#149` not
supported." Root cause was the lowerer's `apply_subst_ty`
helper had a `_ => ty.clone()` catch-all that silently
dropped substitutions for `Ty::Tuple` / `Ty::HashMap` /
`Ty::Dyn`. Same gap session 075 patched in the checker's
`apply_subst_inner_with` — never showed up before
because no path through the lowerer needed to substitute
inside a tuple shape (let-destructure took the
`init_hir.ty` directly).

Now `for (k, v) in m.entries()` triggers it because
`item_ty` derivation walks `HashMapEntriesIter<V>`'s
`type Item = (i64, V)` binding via
`subst_struct_typevars` → `apply_subst_ty`. With V=i64
in struct_args, the Tuple arm needs to substitute the
inner `Ty::TypeVar(V)` to i64. Added explicit
element-wise arms for Tuple/HashMap/Dyn matching session
075's fix shape.

## The wire-ups

```
src/lower.rs      (lower_for accepts Pattern::Tuple;
                   lower_for_iterator takes the &ast::
                   Pattern and dispatches in the some-
                   arm body; apply_subst_ty gains
                   Tuple/HashMap/Dyn arms.)

tests/codegen.rs  (+4 tests: for-tuple-pattern over
                   i64-keyed entries, str-keyed entries,
                   with wildcard slot, nested per-key
                   lookup pattern.)
```

No changes to AST, parser, resolver, checker,
monomorphize, or codegen — the resolver already
recursed into Pattern::Tuple sub-patterns (declaring
each leaf ident as a binding sym), and the checker's
`bind_pattern` already handled Tuple-against-Ty::Tuple.

## What's tested

Codegen (+4):

- `for_tuple_pattern_over_entries` — `for (k, v) in
  m.entries()` over `HashMap<i64, i64>`. Both binds
  used in the body's arithmetic.
- `for_tuple_pattern_str_keyed_entries` — same shape
  over `HashMap<str, i64>`; k is a str, v is an i64.
- `for_tuple_pattern_with_wildcard` — `(_, v)` skips
  the key binding.
- `for_tuple_pattern_nested_lookup` — destructure
  inside the body uses both binds to drive a second
  map lookup.

## Apparent bugs that aren't / explicitly deferred

- **Nested tuple sub-patterns in for-pat** — `for ((a,
  b), c) in iter { ... }` would work structurally
  because `expand_tuple_let_from_local` recurses into
  nested Tuple sub-patterns. Not specifically tested
  because Vec doesn't allow tuple elements (8-byte-slot
  restriction blocks `Vec<(i64, i64, i64)>` and
  similar).
- **Or-patterns in for-pat** — still rejected. Same
  reason as session 082's match-tuple-pattern limit:
  Or-flattening produces multiple HirPatterns and
  for-pat needs exactly one binding shape.
- **Range / array for-pat with tuple pattern** — still
  errors because the range and array fast paths
  (`ForRange`, `For`) use `local: Option<SymbolId>`
  directly, not the Iterator-protocol desugar. Range
  yields i64 and array elements are scalars, so a
  tuple pattern wouldn't typecheck anyway.
- **`apply_subst_ty` catch-all** — the gap session 075
  patched in the checker existed in the lowerer too.
  Tuple was the surfaced case here; HashMap and Dyn
  arms were added in the same edit as defense.
  Likely no pre-085 program hit the HashMap/Dyn gaps
  because no current default-method body walks those
  shapes through this helper. Still, the catch-all
  pattern is now closed in lower.rs alongside checker.

## What's next

- **Intrinsic Numeric impls for primitives** — closes
  session 084's deferred half.
- **Method-call-position `Into` inference** — let /
  fn-arg / struct-field hints for `.into()`.
- **Cartesian-product exhaustiveness for tuple
  patterns** — session 082's deferred item.
- **Self-hosted bootstrap** — long-term.
