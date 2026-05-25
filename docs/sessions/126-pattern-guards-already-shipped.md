# Session 126 — Pattern guards (already shipped, session 016)

**Date:** 2026-05-25
**Outcome:** Another correction to session 117's
bootstrap roadmap. Pattern guards (`pat if cond =>
body`) shipped in session 016 (2026-05-20). The
roadmap listed them as a Tier C item to add. They're
already production-quality with 7 codegen tests + 3
typecheck tests. Session 126 adds 2 more codegen
tests demonstrating bootstrap-specific patterns
(keyword classification via guarded Ident arms, AST
eval with guard-driven optimization) and updates
the roadmap. 494 codegen + 47 AOT + 223 typecheck
tests green (+2 codegen from session 125). No source
changes.

```rune
enum Token {
    Ident(str),
    Number(i64),
    Punct(str),
}

fn classify(t: Token) -> i64 {
    match t {
        Token::Ident(name) if name == "fn" => 1,
        Token::Ident(name) if name == "let" => 2,
        Token::Ident(name) if name == "if" => 3,
        Token::Ident(_) => 100,    // catch-all for non-keyword idents
        Token::Number(n) if n < 0 => -1,
        Token::Number(_) => 200,
        Token::Punct(_) => 300,
    }
}
```

## The decisive observation

Pattern recognition: the bootstrap roadmap doc
(session 117) was written without inspecting the
existing test suite or session history. Sessions
124 (modules), 125 (recursive types), and now 126
(pattern guards) have all found "Tier B/C blockers"
to be already-implemented features. This pattern
suggests the roadmap should be re-checked against
the actual codebase before treating any item as a
blocker.

Pattern guards have been complete since session 016,
which added them alongside or-patterns. The parser
already reads `pat if cond` in match arms; the AST's
`MatchArm` struct has a `guard: Option<Expr>` field;
the checker type-checks the guard as a bool
expression; the lowerer emits the guard check after
the pattern match, falling through to the next arm
if guard fails; the exhaustiveness check excludes
guarded arms from coverage (a guarded `Status::Ok`
arm doesn't make `Ok` covered — the same pattern
unguarded still has to appear).

The session-016 doc explicitly demonstrates the
keyword-recognition example:

```rune
match s {
    Status::Ok => 0,
    Status::Pending if retry => 1,   // guard
    Status::Pending => 2,            // fallback
    Status::Failed => -1,
}
```

### What was already covered (sessions 016 + 094)

- `match_guard_int_positive` — basic `n if cond`.
- `match_guard_fails_falls_through` — fall-through
  when guard returns false.
- `match_guard_uses_binding` — guard inspects a
  bound name from the pattern.
- `match_enum_with_guard` — guard on enum variant.
- `or_pattern_with_guard` — `a | b | c if cond`.
- `range_pattern_with_guard` — `1..=10 if cond`.
- `match_tuple_pattern_with_guard` (session 089) —
  guard on tuple pattern.
- `match_guard_typechecks` (typecheck) — guard's
  type is checked.
- `match_guard_must_be_bool` (typecheck) — non-bool
  guard rejected.
- `guarded_arm_does_not_make_exhaustive` (typecheck)
  — exhaustiveness gate.

### Session 126's contribution

Two new codegen tests that capture the *bootstrap-
relevant* uses, not previously exercised in the
test suite:

- `match_guard_bootstrap_keyword_classification` —
  Token enum with Ident(str) carrying a name;
  guards dispatch on string equality to recognize
  keywords. Mirrors what a real lexer would do
  when classifying identifier lexemes as keywords
  vs. plain identifiers.
- `match_guard_with_recursive_enum` — combines
  session 125's recursive enums with guards. The
  guard *recursively evaluates* a sub-expression
  before deciding which arm runs — a pattern an
  optimization pass might use. Result: an `Expr::
  Sum { lhs, rhs }` whose lhs evaluates to 0 gets
  shortcut to just `eval(rhs)`.

Together with the existing 10 tests, the coverage
spans every bootstrap-relevant pattern: keyword
recognition, range matches, guard-driven AST
optimization, guard interaction with exhaustiveness.

### Roadmap correction

Session 117's Phase 1 Tier C item list:

> 9. Pattern guards (`p if cond => ...`).

Removed. Pattern guards already work.

Combined with session 125's Box<T> removal, two of
the four Tier C items are scratched. The remaining
ones (`let ... else`, borrowed `&str` slices) are
nice-to-haves, not blockers.

## What's tested

Codegen (+2 from session 125's 492):

- `match_guard_bootstrap_keyword_classification` —
  Token::Ident("fn") / "let" / "foo" + Token::
  Number(-5) / 42 all routed via guards. Sums to
  302.
- `match_guard_with_recursive_enum` — recursive Sum
  expression; guard `if eval(lhs) == 0` short-
  circuits to `eval(rhs)`. Returns 7.

## Apparent bugs that aren't / explicitly deferred

- **Per-arm unreachability on nested patterns.**
  An arm like `Expr::Sum { lhs: Expr::Num(0), rhs }`
  followed by a more general `Expr::Sum { lhs, rhs }`
  triggers a false-positive "unreachable arm"
  diagnostic (the exhaustiveness checker only
  tracks top-level variant coverage, not nested
  pattern refinement). Worked around in the test
  by hoisting the discrimination into a guard
  (`if eval(lhs) == 0`) rather than nesting the
  pattern. Same shape as session 094's deferred
  "nested tuple sub-patterns fall into default
  specialization" item. A future session could
  extend the matrix algorithm to nested patterns;
  for now, guards are the workaround.
- **Guard order matters.** `Token::Ident(name) if
  name == "fn"` before `Token::Ident(_)` works.
  Reversed order would make the second always
  match first, falling through to never. Same
  semantics as Rust.
- **Guards can have side effects.** Nothing
  prevents `match x { _ if launch() => 0, _ => 1 }`
  where `launch()` returns bool. Idiomatic Rune
  avoids this; the language doesn't forbid it.

## What's next

- **Session 127: `let ... else`** — Tier C
  ergonomic improvement for early-exit binding.
  Probably also already-shipped or trivially
  expressible; investigation first.
- **Session 128: `i64::parse(s) -> Option<i64>`**
  — the inverse pair to `from_str` that uses
  Option for type-safe parse failure.
- **Session 129+**: continued Phase 1 buildout.
