# Session 040 — Match-scrutinee temporaries

**Date:** 2026-05-21
**Outcome:** A `match` scrutinee that is a fresh ARC temporary —
`match make_opt() { .. }` — is now reclaimed. The scrutinee is
scope-tracked, so it is released at the merge block, and on the way
out by `release_all_arc_locals` if an arm diverges. The last of the
ARC-temporary leak classes. ~16 lines. 462 tests green (+3 from
session 039's 459).

## The leak

`compile_match` computed `scrutinee_val` and never released it:

```rune
match make_opt() { Some(x) => x.len(), None => 0 }
```

`make_opt()` is a fresh enum descriptor. The match reads its tag,
binds payloads out of it, runs an arm — and then drops it on the
floor. One leak per match over a temporary.

## The constraint

The scrutinee cannot simply be released right after it is compiled.
Arm pattern bindings — the `x` in `Some(x)` — are **borrows into
the scrutinee's payload slots** (session 039: a payload binding is
not retained). The scrutinee must outlive every arm body. Its
release belongs *after* the arms.

## The fix: scope-track the scrutinee

When the scrutinee is a fresh ARC temporary (`is_arc_type` and not a
borrowed `Local`), `compile_match` materializes it in a `Variable`
and pushes it onto `arc_locals` — the same machinery a `let` binding
uses. Two release paths then cover it, and a given run takes exactly
one:

- **Merge block** — after the arms, `release_arc_locals_to(snapshot)`
  drops the scrutinee, then `arc_locals` is truncated. This is the
  fall-through path.
- **A `return`-diverging arm** — `Return` codegen already calls
  `release_all_arc_locals`, which now finds the scrutinee in
  `arc_locals` and drops it on the way out.

A `Local` scrutinee (`match opt { .. }`) is left alone — it is owned
by its own binding and released at that scope; it already outlives
the match.

## Why both paths

The `?` operator desugars to `match expr { Ok(v) => v, Err(e) =>
return Err(e) }` — the `Err` arm **diverges via `return`**. For
`foo()?` on a fresh `foo()`, a merge-only release would leak the
scrutinee on every error. Scope-tracking puts it on the `return`
path too.

## Composing with session 039

`match make_bag() { Bag::Full(x) => x, Bag::Empty => .. }` — the
payload escapes the arm. Session 039 retains it on the way out, so
the match value is a fresh `+1` independent of the scrutinee
descriptor. The merge then releases the scrutinee — freeing the
descriptor and dropping its payload-ownership — and the escaped
value survives on the retain session 039 added. The two fixes
interlock exactly.

## What's tested

Codegen (+3):

- `match_scrutinee_temp_released` — `match make_bag(..) { .. }` 200×,
  fall-through; the scrutinee is released at the merge.
- `match_scrutinee_payload_escapes` — the payload escapes the match
  and the scrutinee is a temporary; escape retain and scrutinee
  release net out.
- `match_scrutinee_returning_arm` — an arm `return`s; the scrutinee
  is reclaimed by `release_all_arc_locals`, not the merge.

## Apparent bugs that aren't

- **Array elements still leak.** `compile_array` builds a stack
  array and never establishes ARC ownership of its elements, and
  `Ty::Array` is not ARC-managed. This is a structural gap — arrays
  do not participate in ARC at all — not a stray temporary, and is
  the one reclaim hole left.

- **`weak` / `upgrade_or` arguments are deliberately not cleaned.**
  They alias their argument's control block.

## Status

With call arguments (036), field/index reads (037), receivers
(038), enum-payload escapes / `print` arguments / discarded
statements (039), and the `match` scrutinee (040), every ARC value
that flows through an expression position is now accounted for. The
only reclaim hole left is array elements — a structural gap, since
arrays are not ARC-managed.

## What's next

- **ARC for array elements** — give arrays the same per-element
  release `Vec` has, or make array element reads/writes ARC-aware.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
