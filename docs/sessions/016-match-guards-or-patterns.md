# Session 016 — Match guards + or-patterns

**Date:** 2026-05-20
**Outcome:** Two related match features land together. Arms accept
`pat if cond => body` and `a | b | c => body`. Both interact correctly
with the compile-time exhaustiveness check from session 015. 299 tests
green (+17 new — 9 codegen, 8 typecheck).

## Surface syntax

```rune
fn describe(n: i64) -> str {
    match n {
        0 => "zero",
        x if x < 0 => "negative",
        1 | 2 | 3 => "small",
        _ => "other",
    }
}

enum Status { Ok, Pending, Failed }

fn label(s: Status, retry: bool) -> i64 {
    match s {
        Status::Ok => 0,
        Status::Pending if retry => 1,   // guard
        Status::Pending => 2,            // fallback when retry is false
        Status::Failed => -1,
    }
}
```

Two things to notice. First, the `Status::Pending` arm appears twice
— the checker doesn't flag this as unreachable because the first
occurrence is guarded. Second, `Status::Pending if retry => 1` does
**not** count `Pending` as covered for exhaustiveness; the unguarded
arm below it still has to exist.

## HIR shape

`HirMatchArm` carries a vector of patterns plus an optional guard:

```rust
pub struct HirMatchArm {
    /// One or more alternative patterns. With or-patterns the arm fires
    /// on the first match; without, the Vec has exactly one entry.
    pub patterns: Vec<HirPattern>,
    /// Optional guard `if cond` — checked after pattern match succeeds.
    /// Guarded arms don't count as catch-alls for exhaustiveness.
    pub guard: Option<HirExpr>,
    pub body: HirExpr,
}
```

The flat `Vec<HirPattern>` means or-patterns are erased by the time
codegen runs — the lowerer's `collect_arm_patterns` walks an `ast::Or`
recursively and appends each leaf. A simple arm (`Status::Ok =>`) has
`patterns.len() == 1`; an or-pattern (`1 | 2 | 3 =>`) has 3.

## Parser

The trick is to keep `|` out of `parse_pattern_atom` so individual
positions (struct fields, `let` patterns, future destructuring) get
the leaf grammar without accidentally accepting `|`. Split:

```rust
fn parse_pattern(&mut self) -> ParseResult<Pattern> {
    let first = self.parse_pattern_atom()?;
    if !self.check(&TokenKind::Pipe) { return Ok(first); }
    // collect alternatives, flattening any nested Or
    let mut patterns = match first {
        Pattern::Or { patterns: ps, .. } => ps,
        other => vec![other],
    };
    while self.eat(&TokenKind::Pipe) {
        let next = self.parse_pattern_atom()?;
        match next {
            Pattern::Or { patterns: more, .. } => patterns.extend(more),
            other => patterns.push(other),
        }
    }
    Ok(Pattern::Or { patterns, ... })
}
```

Or is only meaningful in arm position; `let mut a | b = ...` would
parse but the lowerer rejects it with "let pattern must be ident or
`_`". Same for `for x | y in ...`.

## Resolver + checker

The resolver's `declare_pattern` recurses into `Pattern::Or` so any
ident inside a sub-pattern declares a symbol. The checker then rejects
that case: or-patterns can't contain bindings, since the bound symbol
would have an ambiguous declaration site across alternatives. The
error is `or-pattern can't contain a binding`.

Two coverage rules combine for guards + or:

1. **Guarded arms contribute nothing to coverage.**
   `cover_pattern` early-returns when `guarded == true`. This is what
   lets `Status::Ok if cond => ..., Status::Ok => ...` work — the
   first arm's `Ok` is not inserted into `covered_variants`, so the
   second arm doesn't fire the "unreachable" error.

2. **Or-patterns recurse.** `Pattern::Or { patterns }` walks each
   alternative through the same `cover_pattern` machinery. This means
   `true | false => ...` marks both bool values covered without
   needing a catch-all, and `Color::Red | Color::Green | Color::Blue
   => ...` covers all three variants of a 3-variant enum.

```rust
fn cover_pattern(&mut self, pat: &Pattern, guarded: bool, ...) {
    if guarded { return; }       // (1)
    match pat {
        Pattern::Or { patterns, .. } => {
            for sub in patterns {
                self.cover_pattern(sub, guarded, ...);   // (2)
            }
        }
        Pattern::Literal { lit: Lit::Bool(b), .. } => {
            if !covered_bools.insert(*b) {
                self.error(..., "unreachable arm — `true`/`false` was already covered");
            }
        }
        // ... other arms unchanged from session 015
    }
}
```

Duplicates within a single or-pattern (`1 | 2 | 1 => ...`) still
trigger the unreachable error — the second `1` re-inserts and fails.

## Codegen

The session-014 match codegen emitted a sequential `brif` chain: each
arm has one pattern check, and on mismatch the control flow falls
through to the next arm's check block. Two extensions:

**Or-patterns.** For an arm with `N` patterns, emit `N` checks.
Pattern `i < N-1` branches to the body on match, or to a fresh "try
next pattern in this arm" block on mismatch. Pattern `N-1` branches
to the body on match, or to the *next arm's* check block on mismatch.

```text
arm 0 (1 | 2 | 3 => body0):
  check_blk_0:
    icmp scrut, 1; brif eq → body0, neq → alt_1
  alt_1:
    icmp scrut, 2; brif eq → body0, neq → alt_2
  alt_2:
    icmp scrut, 3; brif eq → body0, neq → check_blk_1   // next arm
  body0:
    ...result...
    jump merge
```

**Guards.** After the body block is entered (and any Bind has been
declared), if the arm has a guard, compile the guard expression and
`brif guard_val → guarded_body, else → next_arm_blk`. The `guarded_body`
block runs the actual arm body.

```text
arm 1 (Status::Pending if retry => 1):
  check_blk_1:
    icmp scrut, 1 /* Pending discriminant */
    brif eq → body1, neq → check_blk_2
  body1:
    // (no Bind to declare here)
    retry_val = use_var(retry)
    brif retry_val → guarded_body, → check_blk_2
  guarded_body:
    iconst 1; jump merge
```

The "guard failed → next arm" jump is the key piece. It restores the
sequential semantics that match expects: a guarded arm that doesn't
fire is equivalent to that arm not existing for the remaining arms.

Binding rule: a Bind pattern can only appear when the arm has
**exactly one pattern**. The codegen guards on `arm.patterns.len() ==
1 && matches!(arm.patterns[0], HirPattern::Bind(_))` before defining
the variable. This is safe because the checker rejects Bind inside
or-patterns at type-check time.

The fallback block (no arm matched) still calls `rune_panic_no_match`
and traps — defense in depth, as in session 015.

## What's tested

Codegen (+9):
- `match_with_int_guard` — guarded arm fires when condition true
- `match_guard_falls_through` — guarded arm misses, later arm catches
- `match_guard_uses_binding` — binding visible inside guard expression
- `match_enum_with_guard` — earlier guarded variant + later unguarded
  fallback covering same variant
- `or_pattern_int` — `1 | 2 | 3 => body` matches each alternative
- `or_pattern_enum` — `A | B => ...` on enum variants
- `or_pattern_exhaustive` — full domain coverage via or, no `_`
- `or_pattern_with_guard` — or-pattern + guard on the same arm
- `or_pattern_bool_exhaustive` — `true | false => ...` is exhaustive

Typecheck (+8):
- `match_guard_typechecks` — well-formed guard accepted
- `match_guard_must_be_bool` — `if 5` rejected
- `match_guard_does_not_make_exhaustive` — guarded catch-all still
  triggers non-exhaustive error
- `or_pattern_int_exhaustive_with_wildcard` — `1 | 2 | _ => ...`
- `or_pattern_enum_exhaustive_without_wildcard` — `A | B | C => ...`
- `or_pattern_missing_variant_errors` — `A | B => ...` on 3-variant enum
- `or_pattern_duplicate_within_arm_is_unreachable` — `1 | 2 | 1 => ...`
- `or_pattern_with_binding_rejected` — `x | y => ...` is an error

## File layout changes

```
src/
├── ast.rs         (Pattern::Or { patterns, span })
├── parser.rs      (parse_pattern split into pipe-handling outer +
│                   atom inner)
├── resolver.rs    (declare_pattern recurses into Or)
├── checker.rs     (bind_pattern recurses; check_pattern_matches
│                   rejects Bind inside Or; cover_pattern extracted
│                   from check_match_exhaustiveness as a recursive
│                   helper, early-returns on guarded)
├── hir.rs         (HirMatchArm now { patterns: Vec, guard, body })
├── lower.rs       (lower_match builds new shape; collect_arm_patterns
│                   flattens Or; let/for reject Or patterns)
└── codegen.rs     (compile_match emits alt-chain per arm + optional
                    guard brif between body entry and the actual body)
tests/
├── codegen.rs     (+9 tests)
└── typecheck.rs   (+8 tests)
LANGUAGE.md        (decision log entry)
```

## Apparent bugs that aren't

- **`x if false => ...` is accepted by the parser but never reaches
  the body.** The exhaustiveness checker still excludes it from
  coverage; the codegen still emits the dead branch. Compile-time
  guard *value* analysis (constant folding to detect dead arms) isn't
  attempted — Rust doesn't do it for arbitrary expressions either.

- **`Status::Ok | Status::Ok => ...` is unreachable on the second
  variant, not the whole arm.** Same convention as duplicate arms
  across the match.

- **Or-patterns flatten lazily.** `(a | b) | c` and `a | (b | c)` and
  `a | b | c` all produce the same flat `Vec<HirPattern>`.

## Workarounds in this session

The pre-existing parser precedence bug `!f(x) → (!f)(x)` (unary `!`
binds tighter than the postfix call) bit one test. The fix belongs
in the parser — `parse_unary` needs to consume postfix operators
before wrapping in `Unary`, or equivalently the precedence of unary
operators needs to be lower than the call precedence. Out of scope
for a match-feature session; the test was rewritten to avoid `!` on a
function call. Added to the deferred list below.

## What's still TODO for match

- **Parser bug — `!f(x)` parses as `(!f)(x)`.** Found while testing
  or-pattern exhaustiveness on bool. Small fix; deferred to its own
  session for clarity.
- **Range patterns** (`1..=10 => ...`). Modest — parser already has
  `..` and `..=` as infix operators; needs an `ast::Pattern::Range`
  variant and codegen as a chained icmp.
- **Payload destructuring** (`Some(x) => ...`, `Point { x, y } => ...`).
  Bigger — payload-bearing enum variants need their own design pass,
  and destructuring binds inside patterns means rethinking the
  binding rule that currently sits at `patterns.len() == 1`.
- **Or-pattern bindings via type-unification** (`Ok(x) | Err(x) => ...`).
  Both alternatives must bind `x` to the same type. Possible later;
  needs payload destructuring first.
- **Switch tables for dense int matches.** Profile-driven; not needed
  by the current test corpus.

## Next session

From the standing list, the natural picks are:

- **Range patterns.** Small, complements or-patterns nicely.
- **Parser precedence fix for `!f(x)`.** Small standalone session.
- **Generics step 1 (parser).** Bigger; disambiguates `<T>` from
  comparison-`<`.
- **ARC reclamation step 2.** Big; touches every alloc, copy, drop.
- **Payload destructuring** is the natural follow-up to all the match
  work but needs the payload-variant design first.
