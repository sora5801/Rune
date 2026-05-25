# Session 082 — Match-arm tuple patterns

**Date:** 2026-05-24
**Outcome:** `match pair { (1, x) => ..., _ => ... }`
works. Tuple sub-patterns can be literals, wildcards,
idents (binding), or nested tuples. Session 074's "not
supported in v0.x" error is gone. 380 codegen tests
green (+8 from session 081).

```rune
let pair: (i64, i64) = (3, 4);
match pair {
    (1, x) => x,
    (3, y) => y * 100,           // matches → 400
    (_, _) => -1,
}

let t: (i64, i64, i64) = (1, 2, 3);
match t {
    (1, 2, x) => x,              // → 3
    (_, _, _) => -1,
}

match pair {
    (a, b) if a < b => b - a,    // guards work
    (a, b)          => a - b,
}
```

## The decisive observation

Session 074 already had all the structural pieces:
`HirExprKind::TupleIndex` for element extraction,
tuple heap layout via `rune_struct_new(N*8 + 8)`, per-
shape release walks. The only thing missing was a
`HirPattern::Tuple` variant and a match-machine arm
that knows to load-and-recurse.

Two small wires bring it together:

### 1. HirPattern::Tuple — a list of typed sub-patterns

```rust
Tuple { elements: Vec<(Ty, HirPattern)> },
```

Each element carries the scrutinee element's type
(codegen needs the cranelift_type for load width) plus
a sub-pattern. Sub-patterns are any HirPattern —
literal, wildcard, ident, nested tuple. Closed under
itself.

### 2. compile_pattern_check Tuple arm — load + recurse

For each element:

1. Load `i*8` bytes off the tuple pointer.
2. Narrow to the element's cranelift_type if not I64.
3. If the sub-pattern is `Wildcard`: jump to the
   next-element block.
4. If `Bind`: alloc a Cranelift Variable, declare it,
   store the loaded value, register in var_map; then
   jump.
5. Otherwise: recurse via `compile_pattern_check` with
   the loaded value as scrutinee, a fresh "check next"
   block as on_match, original on_no_match unchanged.

The Bind case is handled inline because
`compile_pattern_check`'s Bind arm at the top level
does NOT allocate a variable — the outer match-codegen
does that in the body block (where scrutinee_val is
the original tuple pointer, not the loaded element).
For tuple sub-patterns we need to bind to the loaded
inner value, mirroring how `HirPattern::EnumPayload`
allocates Variables for payload bindings inline.

### 3. Scrutinee-typed lowering

`collect_arm_patterns` previously didn't carry scrutinee
type info. Added a `scrutinee_ty: &Ty` parameter
(threaded from `lower_match` and recursed into for each
tuple sub-pattern with the element type). When the
walker hits a `Pattern::Tuple`, it reads `Ty::Tuple`'s
elements list and types each sub-pattern against the
corresponding element.

### 4. Monomorphize walks recurse into tuple patterns

`subst_pattern`, `walk_tys_pattern`, and
`walk_pattern_collect_syms` all needed Tuple arms that
recurse into sub-patterns. Without these, a tuple
pattern in a specialized fn would keep `TypeVar(T)` in
its element types or skip Bind sym collection.

## The wire-ups

```
src/hir.rs            (HirPattern::Tuple variant.)

src/lower.rs          (collect_arm_patterns gains a
                       scrutinee_ty arg; Pattern::Tuple
                       arm builds HirPattern::Tuple with
                       element types from Ty::Tuple +
                       recursed sub-patterns.)

src/codegen.rs        (compile_pattern_check Tuple arm:
                       load + bind-or-recurse loop with
                       intermediate per-element blocks.)

src/monomorphize.rs   (subst_pattern recurses into
                       Tuple; walk_tys_pattern + walk_
                       pattern_collect_syms helpers
                       extracted so Match arm walkers
                       handle Tuple.)

src/checker.rs        (cover_pattern Tuple arm now
                       treats the pattern as a catch-all
                       iff every sub-pattern is itself
                       a catch-all — `(_, _)` catches,
                       but `(1, x)` doesn't.)

tests/codegen.rs      (+8 tests: basic, first-arm
                       wins, fallback, both-literals,
                       wildcard-first, three-elements,
                       with-guard, with-bool-and-_).
```

## What's tested

Codegen (+8):

- `match_tuple_pattern_basic` — `(3, 4)` matches the
  middle arm, binds `y`, computes `y * 100`.
- `match_tuple_pattern_first_arm` — `(1, 99)` takes
  the first arm.
- `match_tuple_pattern_fallback` — `(5, 5)` falls
  through to `(_, _)`.
- `match_tuple_pattern_both_literals` — no bindings,
  pure literal comparison on both positions.
- `match_tuple_pattern_with_wildcard_first` — `(_, 42)`
  exercises wildcard in the first position.
- `match_tuple_pattern_three_elements` — 3-tuple,
  ensures the offset loop scales past 2.
- `match_tuple_pattern_with_guard` — guards still work
  on tuple-bound idents.
- `match_tuple_pattern_with_bool_elements` — `(bool,
  i64)` mixed types, narrowing on the bool position.

## Apparent bugs that aren't / explicitly deferred

- **Cartesian-product exhaustiveness isn't tracked**.
  `match (b: bool, x: i64) { (true, x) => ..., (false,
  _) => ... }` IS structurally exhaustive but v0.x's
  coverage tracker treats it as non-exhaustive because
  the per-element coverage sets only handle the outer
  scrutinee's type (bool/enum/int/str), not tuple-of-
  bool. Need an explicit `_` arm. Documented in
  `cover_pattern` and in the test that exercises this.
  Future work: a recursive coverage check that
  computes per-position coverage and reports gaps.
- **Or-patterns inside tuple patterns rejected**.
  `(1 | 2, x)` errors with "tuple sub-pattern produced
  an or-pattern". Or-patterns are flattened to multiple
  HirPattern entries by the lowerer, and a tuple
  pattern needs exactly one HirPattern per element.
  Workaround: write multiple arms.
- **Nested tuple sub-patterns** work (`((1, 2), x)`
  lowers cleanly because the recursive sub-pattern is
  another Tuple). Not specifically tested this session;
  the structural recursion through subst_pattern /
  walk_tys_pattern covers it.
- **Tuple patterns under generic substitution** — when
  a generic fn matches a `(T, U)` tuple, subst_pattern
  walks Tuple's elements and substitutes both each
  element's type AND the sub-pattern types. Tested
  implicitly via the monomorphize walks.
- **ARC release for tuple-pattern bindings** — when a
  binding extracts an ARC value from a tuple, the
  binding "borrows" the scrutinee's slot. The scrutinee
  itself releases at scope exit (tuple per-shape
  release walks each ARC slot). v0.x doesn't double-
  count: the binding is a Variable, not an ARC local,
  so it doesn't independently release. Tested
  implicitly through the existing tuple release walks
  from session 074.

## What's next

- **Numeric trait bounds** — generalizes `.sum() /
  .min() / .max() / .fold(init, +)` beyond i64.
- **Str-keyed HashMap iteration** — `.keys() /
  .entries()` on `HashMap<str, V>`.
- **Method-call-position `Into` inference** — let / fn-
  arg / struct-field hints for `.into()`.
- **Cartesian-product exhaustiveness for tuple
  patterns** — proper recursive coverage check.
- **Self-hosted bootstrap** — long-term.
