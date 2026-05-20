# Session 015 — Compile-time match exhaustiveness

**Date:** 2026-05-20
**Outcome:** Non-exhaustive matches now error at type-check time. The
`rune_panic_no_match` runtime helper stays as defense-in-depth but
should never fire from a well-typed program. 282 tests green (+12
net — +13 typecheck for the new error cases, -1 AOT test that was
demonstrating the runtime backstop).

## What gets caught

| Shape | Result |
| --- | --- |
| Enum match covering every variant | OK |
| Enum match missing a variant | **error: `non-exhaustive match on enum X: missing arms for Y`** |
| Bool match with both `true`/`false` | OK |
| Bool match with only one branch | **error: `non-exhaustive match on bool: missing arms for ...`** |
| Int / str / float / char match with `_` arm or binding | OK |
| Int / str / float / char match without catch-all | **error: `non-exhaustive match on ...: add a _ arm`** |
| Any match where the same pattern appears twice | **error: `unreachable arm`** |
| Any arm after a catch-all | **error: `unreachable arm — an earlier arm covers everything`** |

## Algorithm

Single pass over the arms, after the per-arm type checks have run.
Two pieces of state:

```rust
let mut catchall_seen: Option<Span> = None;
let mut covered_bools: HashSet<bool> = HashSet::new();
let mut covered_variants: HashSet<u32> = HashSet::new();
let mut covered_ints: HashSet<i64> = HashSet::new();
let mut covered_strs: HashSet<String> = HashSet::new();
```

Per arm:
- If `catchall_seen` is set already, this arm is unreachable → error,
  skip.
- Otherwise inspect the pattern:
  - `Wildcard` or unguarded `Ident` → set `catchall_seen`.
  - `Literal(Bool/Int/Str)` → insert into the matching set;
    duplicate insert errors as unreachable.
  - `Path` resolving to an `EnumVariant` → insert discriminant;
    duplicate is unreachable.
- **Guarded arms** are excluded — `_ if cond => ...` doesn't catch
  all because the guard can fail. Same for `n if cond => ...`. (The
  guard syntax is parsed already; codegen of guards still deferred,
  but the exhaustiveness check treats them correctly when they
  appear.)

After the loop, if `catchall_seen` is `None`:
- `bool`: report missing `true` / `false` if either set is short.
- `enum X`: walk `Resolutions::enum_variants[X]`, report any variant
  whose discriminant isn't in `covered_variants`.
- everything else (i64, str, char, float, Vec, structs, arrays):
  always error — these domains are either infinite or otherwise
  unenumerable.

## Why the runtime backstop stays

`rune_panic_no_match` is now logically dead code from a sound checker
— but:

1. **Defense in depth.** If a checker bug ever lets a non-exhaustive
   match through, the backstop produces a debuggable error rather
   than reading past the last `brif` into whatever block follows.
2. **Cranelift IR needs a terminator** on the fallthrough block.
   `trap(TrapCode::user(2))` alone would compile to `ud2`, which
   crashes silently. The current `call panic_no_match; trap` pattern
   gives the user a stderr message before the trap.
3. **Forward compatibility.** When we add guards (`x if cond =>`),
   range patterns, or open enums later, the static check will get
   weaker in places; the backstop covers what static analysis can't
   prove.

Cost is one runtime symbol (~50 bytes of machine code in the linked
binary) and one branch per match.

## Apparent bugs that aren't

- **Guarded `_ if cond` arm isn't treated as a catch-all.** This is
  correct — the guard can return false, leaving the rest of the
  scrutinee's domain uncovered. Today the result is the user gets
  asked for either a second `_` arm or a coverage-completing arm.
  Once guards are codegen-supported, the same rule applies.
- **Wildcard `_` doesn't bind.** Matches anything, doesn't introduce
  a name. A user wanting to keep the matched value writes
  `x => ...`. The exhaustiveness check treats both `_` and
  unguarded `x` as catch-alls.
- **Match on an empty enum doesn't error.** `enum Never { }` followed
  by `match n { }` is currently allowed if `n: Never` (uninhabited).
  Rust treats this as exhaustive. Rune does too — the empty arm list
  has no missing variants. This is correct, even if surprising.
- **Duplicate enum variants in source order produce the unreachable
  error on the second appearance**, not the first. Same as Rust.

## What's still TODO for match

- **Compile-time guard codegen.** Parser already accepts `if cond`
  on arms; HIR lowering and codegen for guarded arms is the next
  feature.
- **Or-patterns** (`1 | 2 | 3 => ...`).
- **Range patterns** (`1..=10 => ...`).
- **Payload destructuring** (`Some(x) => ...`) — needs payload-
  bearing enum variants first, which need their own design pass.
- **Bigger jump tables** — for dense int patterns, replacing the
  sequential brif chain with a switch table. Profile-driven; nothing
  in our test corpus is dense enough to need it.

## File layout changes

```
src/
└── checker.rs    (check_match_exhaustiveness added; called from
                   check_match after per-arm checks)
tests/
├── typecheck.rs  (+13 tests covering exhaustive ok, non-exhaustive
                   errors per type, unreachable arms, duplicates)
└── aot.rs        (-1: the runtime-backstop test is no longer
                   reachable; replaced with a comment pointer to
                   the typecheck tests)
LANGUAGE.md       (decision log entry)
```

## Test coverage added

Typecheck (+13):
- enum exhaustive ok; missing variant errors
- enum + wildcard ok
- bool both branches ok; missing branch errors
- int with `_` ok; with binding ok; without catch-all errors
- str without catch-all errors
- duplicate enum variant unreachable
- duplicate int literal unreachable
- arm after `_` unreachable
- arm after binding catch-all unreachable

## Next session

Picking from session 014's deferred list:

- **Match guards** (`x if cond => ...`). Parser already accepts them
  in the AST; needs HIR lowering and codegen (a brif on the guard
  inside the pattern body). Small.
- **Or-patterns** (`1 | 2 | 3 => ...`). Modest; multi-pattern arms.
  Needs parser, HIR, exhaustiveness updates.
- **Payload destructuring**: `Some(x) => ...` and `Point { x, y } =>
  ...`. Bigger — requires extending HirPattern, threading bindings
  into arm scope, and (for enums) payload-bearing variant codegen.
- **ARC (reclamation step 2).** Big — touches every alloc, copy, drop.
- **Generics step 1 (parser).** Disambiguate `<T>` from comparison-`<`.
