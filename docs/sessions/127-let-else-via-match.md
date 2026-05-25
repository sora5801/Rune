# Session 127 — `let-else` via match (no new syntax needed)

**Date:** 2026-05-25
**Outcome:** Rune doesn't have dedicated `let-else`
syntax, but the same semantic — bind from a pattern
OR diverge — is fully expressible via match-with-
diverging-arm. The bootstrap can use this directly;
no compiler changes needed. Fourth correction to
session 117's bootstrap roadmap. 496 codegen + 47
AOT + 223 typecheck tests green (+2 codegen from
session 126). No source changes.

```rune
fn double_or_default(o: std::Option<i64>) -> i64 {
    let v: i64 = match o {
        std::Option::Some(x) => x,
        std::Option::None => return -1,
    };
    v * 2     // only reached when Some(x); v is bound to x
}
```

## The decisive observation

Rust's `let Some(v) = opt else { return -1; };` is
*sugar* for:

```rust
let v = match opt {
    Some(v) => v,
    None => return -1,
};
```

Rune already has the desugared form. The `return`
(and `break` / `continue`) expressions have type
`Never`, which the checker unifies with the other
arm's type (`i64`). The match's result type is `i64`,
and the binding `v: i64` works.

Adding a dedicated `let-else` parser shortcut would
shave ~3 lines vs. the match form. Not a blocker —
the bootstrap parser would use one or the other
inconsistently anyway.

### Why this matters for the bootstrap

The standard parser idiom — "extract from `Option`
or bail" — appears constantly:

```rune
fn parse_let(p: &mut Parser) -> Option<Stmt> {
    let pat: Pattern = match p.parse_pattern() {
        std::Option::Some(x) => x,
        std::Option::None => return std::Option::None,
    };
    // ... continues with pat in scope
}
```

This compiles today, with `pat` in scope for the
remainder of the function. The match form is
slightly more verbose than `let-else` but
semantically identical.

For loop-internal extraction, `continue` works as
the diverging arm:

```rune
for x in items.iter() {
    let val: i64 = match check(x) {
        0 => continue,
        n => n,
    };
    process(val);
}
```

### Roadmap correction

Session 117's Phase 1 list:

> Tier C — `let ... else` for early-exit binding.

Removed. The pattern is expressible without new
syntax.

Combined with sessions 125 (Box<T>) and 126
(pattern guards), three of four Tier C items are
scratched. The last remaining Tier C is "borrowed
`&str` slices" — a perf-only optimization that
isn't a blocker either.

### Sessions 124-127 pattern

A consistent theme has emerged from the post-roadmap
investigations:

| Session | Roadmap item | Status |
|---|---|---|
| 124 | Module system at file granularity | Already shipped (session 020) |
| 125 | `Box<T>` for recursive types | Not needed (pointer semantics) |
| 126 | Pattern guards | Already shipped (session 016) |
| 127 | `let-else` | Expressible via match-with-diverge |

Phase 1's actual blocker list is now empty. The
remaining items in the README's Phase 1 roadmap
(`std::env::var`, `i64::parse`, `.chars()`) are
useful enhancements but the bootstrap can start
without them.

The bootstrap is now ready to begin. Session 117's
6-month estimate was based on Tier B/C work that
doesn't need doing.

## What's tested

Codegen (+2 from session 126's 494):

- `let_else_pattern_via_match_on_option` — the
  canonical Option → bind-or-return pattern.
  Returns 41 (42 from Some(21) doubled + -1 from
  None case).
- `let_else_via_match_with_continue` — loop-
  internal "skip this iteration" via continue
  arm. Finds the first nonzero in a Vec of
  `[0, 0, 42, 99]`, returns 42.

## Apparent bugs that aren't / explicitly deferred

- **No dedicated `let-else` keyword.** Would be a
  small parser-only feature (lowering reuses
  match). Adding it now distracts from the
  bootstrap path; the existing pattern works.
  Future session if syntactic noise becomes a
  problem.
- **No `if let`.** Same story: `match opt { Some(x)
  => ..., None => () }` works. The `if let`
  conditional binding shape would save a few
  lines in deeply-nested bind chains.
- **Trailing expression after for-loop parses
  as binop.** A `for ... { ... }` followed by
  `-1` parses as `(for_loop) - 1` because both
  parse as expressions. Workaround: assign to a
  `let mut result` inside the loop and return
  the result. Same parser quirk that Rust has
  with `match`-expression-as-statement.

## What's next

- **Session 128: Investigation pause + minor
  perf / polish session.** All Phase 1 blockers
  are now removed. Pick the smallest improvement
  with the highest bootstrap-payoff.
- **Phase 2 starts**: write the lexer in Rune.
  ~500 lines of Rune. Per session 117's plan.
