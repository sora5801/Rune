# Session 094 — Per-arm unreachability for tuple matches

**Date:** 2026-05-25
**Outcome:** Arms in a tuple match that are shadowed by
earlier arms are now diagnosed with "unreachable match
arm". Closes session 089's deferred item.
417 codegen + 158 typecheck tests green (+4 typecheck,
codegen unchanged).

```rune
match (b1, b2): (bool, bool) {
    (true, _) => 1,
    (true, true) => 2,    // error: unreachable — covered by `(true, _)`
    (false, _) => 3,
}

match (c, b): (Color, bool) {
    (Color::Red, _) => 1,
    (Color::Red, true) => 2,    // error: unreachable
    (Color::Green, _) => 3,
    (Color::Blue, _) => 4,
}
```

## The decisive observation

Session 089's matrix algorithm answered "is the whole
match exhaustive?" via Maranget's usefulness test
applied to `[_, _, ..., _]` (the all-wildcard
candidate) against the union of all arm rows. The
per-arm question — "is THIS arm useful against the
union of EARLIER arms?" — uses the same recursive
specialization machinery, just with a different
candidate.

A candidate pattern P is **useful** against matrix M
iff there exists some value V of the scrutinee type
that matches P but doesn't match any row in M. The
recursive cases:

- **0 columns**: useful iff M is empty.
- **Wildcard head**: useful iff for some constructor
  c of the head type, specialize(M, c) doesn't cover
  the tail.
- **Specific head (literal / variant)**: useful iff
  specialize(M, c) doesn't cover the tail, where c
  is the candidate's specific constructor.
- **Infinite-domain head + specific candidate**: same
  shape; specialize_default(M) drops all non-wildcard
  prior rows.

### Per-arm walk

```rust
let mut matrix: Vec<Vec<Pattern>> = Vec::new();
for arm in arms {
    if arm.guard.is_some() { continue; }
    let arm_rows = add_arm_rows(&arm.pat, arity);
    let any_useful = arm_rows.iter().any(|row| {
        tuple_matrix_is_useful(self, elem_tys, &matrix, row)
    });
    if !any_useful {
        self.error(arm.pat.span(), "unreachable match arm — ...");
        continue;  // Don't extend the matrix; an unreachable
                   // arm can't add coverage past what's there.
    }
    matrix.extend(arm_rows);
}
// Final exhaustiveness check uses the same matrix.
```

Guarded arms (`pat if cond => ...`) are excluded from
both checks — the guard can fail at runtime, so they
neither extend the matrix nor get usefulness-checked.
This matches the existing flat-coverage behavior
(session 020).

### Or-pattern interaction

Top-level or-patterns expand row-wise (session 089).
For per-arm checking: the arm is useful iff ANY of
its or-alternatives is useful against the prior
matrix. If at least one alternative is unique, the
arm is reachable through that alternative — fire no
diagnostic. If every alternative is redundant, the
whole arm is unreachable. The implementation uses
`arm_rows.iter().any(...)` which matches that
semantic exactly.

### The catch-all pre-pass folds in

Session 089 had a separate pre-pass that scanned for
top-level catch-all arms and flagged subsequent ones
as unreachable. That's now subsumed by the per-arm
usefulness check: once a catch-all arm's all-wildcard
row enters the matrix, every subsequent arm row gets
specialize_default'd into the empty tail and is
correctly flagged. The pre-pass + the bespoke
`pattern_is_catchall_for_tuple` helper are now
removed.

## The wire-ups

```
src/checker.rs    (check_tuple_match_exhaustiveness
                   rebuilt around the incremental
                   matrix walk; new
                   tuple_matrix_is_useful free
                   function next to existing
                   specialize_* helpers;
                   pattern_is_catchall_for_tuple
                   removed as dead code.)

tests/typecheck.rs  (+4 tests: unreachable after
                     wildcard arm; unreachable
                     specific after overlapping;
                     enum × bool unreachable
                     specific; no false-unreachable
                     when each arm contributes new
                     coverage.)

tests/codegen.rs   (match_tuple_pattern_with_bool_
                    elements updated — the now-
                    unreachable `_ => -1` catch-all
                    arm removed.)
```

No AST / parser / lower / mono / codegen changes —
purely a checker diagnostic enhancement.

## What's tested

Typecheck (+4):

- `tuple_match_unreachable_after_wildcard_arm` —
  `(_, _)` followed by `(true, true)` flags the
  second arm.
- `tuple_match_unreachable_specific_after_overlapping`
  — `(true, _)` covers `(true, true)`; the latter
  arm is flagged unreachable even with intermediate
  arms covering the false side.
- `tuple_match_overlapping_enum_specific_unreachable`
  — same shape with `Color × bool`.
- `tuple_match_no_false_unreachable` — sanity:
  the four-arm bool×bool full coverage doesn't
  flag any arm.

## Apparent bugs that aren't / explicitly deferred

- **Flat (non-tuple) match arms** — the existing
  `cover_pattern` flat coverage already flags
  duplicate-arm patterns at flat positions. Per-arm
  usefulness via the matrix algorithm only applies
  to tuple scrutinees. Flat-side could be migrated
  to the matrix algorithm too, but it'd duplicate
  the existing coverage logic with no functional
  benefit.
- **Nested tuple sub-patterns** — `(a, (b, c))`
  goes through the same default-specialization path
  as other infinite-domain head types (session 089
  caveat). A useful check on a nested tuple sub-
  pattern would need recursion into the nested
  shape's matrix; defer along with cartesian
  exhaustiveness for nested tuples.
- **Per-variant payload coverage** — `(Option::Some
  (5), _)` followed by `(Option::Some(5), _)` should
  flag the second arm but the current matrix collapses
  Some(_) and Some(5) to the same variant
  discriminant, missing the duplicate-payload case.
  Same caveat as session 089. A complete check
  would specialize on the payload columns too; deferred.
- **Guards on covering arms** — `(true, x) if cond
  => _, (true, x) => _` doesn't flag the second arm
  because the guarded arm doesn't extend the matrix.
  Correct behavior — when the guard fails, the
  second arm IS reachable.
- **Mixed reachable / unreachable or-alternatives**
  — `(true, true) | (true, _) => _, (true, false) =>
  _`. The first arm's `(true, _)` alternative makes
  the second arm's `(true, false)` row unreachable.
  Currently the arm-level usefulness check
  short-circuits on the first useful alternative,
  so the first arm IS reachable but the second
  one is correctly flagged. However the diagnostic
  doesn't pinpoint the redundant alternative within
  the first arm — future polish.

## What's next

- **Binary-op hint flow** — `a: i32; a + 1` lets the
  `1` adopt i32 from the LHS.
- **Const-eval overflow checks** — reject `100u8 +
  200u8` runtime overflow.
- **Codegen-side diagnostic polish** — session 093's
  deferred half.
- **Self-hosted bootstrap** — long-term.
