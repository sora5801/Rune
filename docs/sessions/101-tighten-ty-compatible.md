# Session 101 — Tighten `Ty::compatible` at Into sites

**Date:** 2026-05-25
**Outcome:** Added `Ty::compatible_strict` — same as
`compatible` but without the TypeVar-as-wildcard and
Assoc-as-opaque special cases. Replaced `compatible`
with `compatible_strict` at the three Into-related
call sites (session 086's `.into()` disambiguation,
session 086's `?`-site disambiguation, session 090's
duplicate-target detection). Closes the design-debt
item from session 100's milestone retrospective. No
behavior change for v0.x (today's non-generic Into
impls); strictness shows up only when generic targets
land. 424 codegen + 162 typecheck tests green; no
regressions.

## The decisive observation

Session 100's retrospective named this:

> `TypeVar` as compatible-with-anything in
> `Ty::compatible`. Session 047-era decision to make
> generic inference cheap. Has caused subtle
> over-acceptance: session 086's
> `try_into_disambiguation` (`compatible(target,
> expected)`) over-matches when targets are generic.
> Session 090 (duplicate-target detection) has the
> same issue.

The Into sites compare two ALREADY-RESOLVED concrete
types (an impl's target `Ty` vs the surrounding
expected `Ty`). They don't need TypeVar-as-wildcard.
But changing `compatible` globally would break dozens
of other call sites that DO need it (generic-body
checks, fn-arg unification against TypeVar params,
etc.).

The fix is split: keep `compatible` lenient for the
unification-flavored callers, add `compatible_strict`
for the Into-flavored ones.

### What `compatible_strict` does

```rust
pub fn compatible_strict(&self, other: &Ty) -> bool {
    if self.is_error() || other.is_error() ||
       self.is_never() || other.is_never() {
        return true;
    }
    match (self, other) {
        // TypeVar matches only against the same TypeVar.
        (Ty::TypeVar(a), Ty::TypeVar(b)) => a == b,
        // Struct / Enum match by sym + recursively strict args.
        // Empty args remain the "placeholder" sentinel from
        // bare variant construction (`None` produces
        // `Enum(option, [])`).
        (Ty::Struct(s1, a1), Ty::Struct(s2, a2))
        | (Ty::Enum(s1, a1), Ty::Enum(s2, a2))
            if s1 == s2 =>
        {
            a1.is_empty() || a2.is_empty() ||
            (a1.len() == a2.len() &&
             a1.iter().zip(a2).all(|(x, y)| x.compatible_strict(y)))
        }
        // Containers / Fn / Weak / Array / Assoc all recurse
        // strictly. Assoc is NOT opaque under strict; two
        // different `Self::Item` projections from different
        // base types are now distinct.
        ...
    }
}
```

The Assoc case is the subtle one. `compatible` treats
any Assoc as compatible with anything (so a
projection that hasn't resolved yet flows freely
through `?` / `.into()` site checks). Under strict
mode, two Assoc projections must match by name AND
base. The Into sites don't see projections in
practice (impl target types are concrete at lookup
time), so this tightening doesn't observe any
difference today. If a future generic Into
(`impl<T: Iterator> Into<T::Item> for X`) lands,
strictness flags the right shape.

### What stays lenient

The original `compatible` keeps its semantic
unchanged for ~20 other call sites in `checker.rs`:

- Function body type-check vs return type — generic
  bodies have TypeVar bodies; `compatible` accepts
  them against any TypeVar-or-concrete declared
  return.
- Argument unification at call sites — fn-arg
  typevar params accept any concrete via the
  wildcard semantic.
- Match arm body unification with the scrutinee /
  match return type.
- Variant payload binding against expected types in
  pattern-coverage checks.

Switching these to strict would break generic inference
across the codebase. The deferred refactor for v0.x is
to thread an explicit substitution map through these
sites (Maranget-style, the same shape session 089 used
for tuple exhaustiveness) — but that's a bigger
session.

## The wire-ups

```
src/ty.rs         (new Ty::compatible_strict; original
                   Ty::compatible unchanged.)

src/checker.rs    (3 sites switched: try_into_
                   disambiguation, check_into_impl_
                   duplicates, check_try's Into
                   conversion lookup.)
```

No AST / parser / resolver / lower / mono / codegen
changes. Pure type-system refinement.

## What's tested

The existing tests (session 086's
`into_disambiguation_*`, session 090's
`duplicate_into_impl_rejected` /
`distinct_into_targets_accepted`) all pass unchanged
— confirming the strictness doesn't regress today's
non-generic Into impls.

No new tests this session: every concrete shape that
`compatible` accepts at the three Into sites,
`compatible_strict` also accepts. The new function is
future-proofing; its observable difference will
appear when generic Into targets land.

## Apparent bugs that aren't / explicitly deferred

- **The other ~20 `compatible` call sites still
  lenient.** Tightening them would require threading
  an explicit substitution map (so a TypeVar matches
  only against its pinned target) — a multi-site
  refactor. The Into sites were the highest-leverage
  ones because they compare resolved-against-resolved
  rather than unification-flavored. Future session
  could do the threading.
- **Same-target Into duplicate test (session 090's
  `duplicate_into_impl_rejected`) still passes.**
  Same shape both ways: two `impl Into<AppErr> for
  IoErr` are `Ty::Struct(app_err_sym, [])` on both
  sides; `compatible_strict` matches by sym just
  like `compatible` did.
- **`distinct_into_targets_accepted` still passes.**
  Two impls with different syms (AppErr vs DbErr)
  fail both `compatible` and `compatible_strict` (sym
  mismatch); no over-acceptance.
- **`?`-site Into disambiguation** — the test
  `try_without_into_impl_rejected` and the various
  one-impl `?`-paths all pass.
- **Strict on placeholder-args** — `compatible_strict`
  on `Ty::Struct(s, [])` vs `Ty::Struct(s, [i64])`
  returns true (the empty side is the placeholder
  from `None` / variant-construction). Otherwise
  `Some(5)` (`Enum(option, [i64])`) wouldn't match
  the `None` placeholder pattern (`Enum(option, [])`)
  in the same match. The if-else unify check at
  session 020 era depends on this. Strict preserves
  it.

## What's next

- **Compound const-eval overflow** — `100u8 + 200u8`
  errors at compile.
- **Chained binop hint propagation** — `1 + 2 + a:
  i32` works without parens.
- **Floating-point Vec elements** — unblock numeric
  workloads on f64.
- **Self-hosted bootstrap** — long-term.
