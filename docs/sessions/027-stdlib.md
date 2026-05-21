# Session 027 — Standard library (prelude)

**Date:** 2026-05-20
**Outcome:** Rune has a standard library. It's a `mod std { ... }`
written in Rune itself, embedded in the compiler and prepended to every
program. `Option<T>`, `Result<T, E>`, six generic helpers over them,
and four concrete numeric helpers are now always in scope under
`std::`. 394 tests green (+10 from session 026's 384).

The two things that blocked a real stdlib for ten sessions — traits
and a module system — both shipped in 025 and 026. With those in
place, the stdlib needed no new compiler machinery at all. It's just
Rune source.

## The headline

```rune
fn main() -> i64 {
    let some = std::Option::Some(42);
    let none: std::Option<i64> = std::Option::None;
    std::unwrap_or(some, 0) + std::unwrap_or(none, -1)   // 42 + -1 = 41
}
```

```rune
fn main() -> i64 {
    std::min(3, 7) + std::max(3, 7) + std::abs(-5)       // 3 + 7 + 5 = 15
}
```

`use` works on prelude items too:

```rune
use std::unwrap_or;

fn main() -> i64 {
    unwrap_or(std::Option::Some(5), 0)                   // 5
}
```

## Design — the stdlib is just Rune

The whole stdlib is one file, `src/std.rn`:

```rune
mod std {
    enum Option<T> { Some(T), None }
    enum Result<T, E> { Ok(T), Err(E) }

    fn unwrap_or<T>(o: Option<T>, default: T) -> T { ... }
    fn is_some<T>(o: Option<T>) -> bool { ... }
    fn is_none<T>(o: Option<T>) -> bool { ... }
    fn ok_or<T, E>(r: Result<T, E>, default: T) -> T { ... }
    fn is_ok<T, E>(r: Result<T, E>) -> bool { ... }
    fn is_err<T, E>(r: Result<T, E>) -> bool { ... }

    fn min(a: i64, b: i64) -> i64 { ... }
    fn max(a: i64, b: i64) -> i64 { ... }
    fn abs(x: i64) -> i64 { ... }
    fn clamp(x: i64, lo: i64, hi: i64) -> i64 { ... }
}
```

No new AST node, no new resolver pass, no codegen change. The prelude
exercises only features that already existed: inline modules (026),
generic enums and functions (022–024), monomorphization (022), pattern
matching (015). If the stdlib needed a compiler change, that would be a
sign the feature it used wasn't really done.

### Embedding

`src/lib.rs` gains two items:

```rust
pub const PRELUDE: &str = include_str!("std.rn");

pub fn with_prelude(user_src: &str) -> String {
    format!("{}\n{}", PRELUDE, user_src)
}
```

`include_str!` bakes `std.rn` into the compiler binary at *its* build
time. There's no stdlib file to ship, no search path, no install step
— the prelude travels inside `rune.exe`.

`with_prelude` concatenates prelude + user source into **one string**.
That string is one span space: a single lex, a single parse, one
`Resolver`, one `Checker`. The prelude's `mod std` and the user's
items are siblings at the root of the same module tree, which is
exactly why `std::` resolves with zero special-casing — it's an
ordinary qualified path into a sibling module.

### Wiring into the pipeline

`src/main.rs` grows a `read_program_source` that reads a file and runs
it through `with_prelude`. The three compile commands switch to it:

| Command | Source |
| --- | --- |
| `check` / `run` / `build` | `read_program_source` — prelude + user |
| `tokens` / `ast` | `read_source` — user file only |

The debug commands deliberately *don't* prepend. `rune tokens foo.rn`
should print the tokens of `foo.rn`, not 90 lines of prelude first.

The two test harnesses get the same treatment — `tests/codegen.rs`'s
`run_main` and `tests/typecheck.rs`'s `run` both wrap their input in
`with_prelude` so every existing test runs *with* the prelude present.
All 384 prior tests still pass, which is the real proof that the
prelude resolves and type-checks cleanly: a single bad item in
`std.rn` would fail every `check_ok` test at once.

## Zero-cost when unused

The generic helpers (`unwrap_or`, `is_some`, …) cost nothing if a
program never calls them. The monomorphizer (session 022) only emits
the specializations actually reached from `main`, and **drops the
generic originals** — their bodies still mention `TypeVar` and
couldn't be codegen'd anyway. So a program that uses no `std::`
generic compiles to exactly what it did before this session.

The four concrete helpers — `min`, `max`, `abs`, `clamp` — are plain
`i64` functions with no type parameters, so they're always emitted.
Four tiny functions in every binary is the entire fixed cost of the
stdlib.

## The monomorphizer bug this shook out

Testing `std::unwrap_or(std::Option::Some(42), 0)` failed at codegen:

```
rune: codegen error: type T#44 not supported in codegen
```

`unwrap_or` is generic, and its body is a `match`:

```rune
fn unwrap_or<T>(o: Option<T>, default: T) -> T {
    match o {
        Option::Some(x) => x,
        Option::None => default,
    }
}
```

When the monomorphizer specializes a generic function it walks the
whole body with `subst_ty` / `subst_block` / `subst_expr`, replacing
`TypeVar(T)` with the concrete type. But `subst_expr_kind`'s `Match`
arm was cloning the arm patterns verbatim:

```rust
arms: arms.iter().map(|a| HirMatchArm {
    patterns: a.patterns.clone(),          // <-- types NOT substituted
    guard: ...,
    body: subst_expr(&a.body, subst),
}).collect()
```

`HirPattern::EnumPayload { bindings: Vec<(Ty, Option<SymbolId>)> }`
carries a `Ty` per binding. After specialization the `Some(x)` arm
still bound `x` at type `TypeVar(T)` instead of `i64`, and codegen
rejected the unresolved type var.

The fix is a `subst_pattern` helper. Only `EnumPayload` carries types,
so every other pattern variant clones unchanged:

```rust
fn subst_pattern(p: &HirPattern, subst: &HashMap<SymbolId, Ty>) -> HirPattern {
    match p {
        HirPattern::EnumPayload { discriminant, bindings } => {
            HirPattern::EnumPayload {
                discriminant: *discriminant,
                bindings: bindings.iter()
                    .map(|(ty, b)| (subst_ty(ty, subst), *b))
                    .collect(),
            }
        }
        _ => p.clone(),
    }
}
```

This bug was latent since session 022 — it just needed a generic
function whose body matched on a generic enum to surface it, and
nothing in the test suite did that until the stdlib. The prelude is a
good stress test precisely because it *is* generic-over-enum code.

## Pipeline

```
src/
├── std.rn      (NEW — the prelude, written in Rune)
├── lib.rs      (PRELUDE const + with_prelude fn)
├── main.rs     (read_program_source; check/run/build use it)
└── monomorphize.rs  (subst_pattern; Match arm substitutes patterns)

tests/
├── codegen.rs   (run_main wraps src in with_prelude; +7 tests)
└── typecheck.rs (run wraps src in with_prelude; +3 tests)
```

## What's tested

Codegen (+7):
- `stdlib_min_max_abs` — the three concrete helpers.
- `stdlib_clamp` — clamp above / below / within the range.
- `stdlib_option_unwrap_or` — generic `unwrap_or<T>` on `Some` and
  `None`; the test that exercises the `subst_pattern` fix.
- `stdlib_option_is_some_is_none`.
- `stdlib_result_ok_or` — `ok_or` on `Ok` and `Err`.
- `stdlib_result_is_ok_is_err`.
- `stdlib_use_import_generic` — `use std::unwrap_or;` then a bare,
  still-monomorphized call.

Typecheck (+3):
- `stdlib_min_rejects_non_int` — `std::min("a", "b")` is a type error.
- `stdlib_item_must_be_qualified` — bare `min(1, 2)` is unresolved;
  prelude items live under `std::`.
- `stdlib_option_unwrap_or_typechecks`.

All 384 prior tests still pass — now with the prelude prepended.

## Apparent bugs that aren't

- **Error byte offsets in user code are shifted.** `with_prelude`
  produces one string, so a span pointing at user code is offset by
  the prelude's byte length. Errors still point at the right *token*,
  just with inflated numbers. A real fix needs either multi-source
  span tracking or a line-directive mechanism — deferred. The debug
  commands sidestep it by not prepending at all.

- **The prelude can't be edited without rebuilding the compiler.**
  `include_str!` embeds `std.rn` at compiler-build time. That's the
  point — the stdlib ships *inside* `rune.exe`. An external,
  separately-shipped stdlib needs file-based modules, which don't
  exist yet.

- **`Vec` is still a hardcoded builtin, not `std::Vec`.** A
  user-written generic `Vec<T>` would need the monomorphizer and ARC
  to cooperate on a generic destructor (releasing `T` elements when
  `T` is itself ARC-managed). That's a real piece of work; the i64-only
  builtin `Vec` stays until it's done.

- **`std::` is not implicitly opened.** You write `std::Option::Some`,
  not `Some`. A bare `Some` doesn't resolve. An automatic `use std::*`
  in the prelude would fix this, but `use` globs aren't implemented
  (noted in session 026's "what's next"). For now, qualify or `use`.

- **`Result<T, E>` helpers never name `E`.** `ok_or` / `is_ok` /
  `is_err` are generic over `E` but their bodies only touch the `Ok`
  payload or the tag. `Result::Ok(7)` leaves `E` unbound and the
  monomorphizer is fine with it — an un-constrained type parameter
  just doesn't get a specialization axis. Not a bug; tested by
  `stdlib_result_ok_or`.

## What's next

- **Generic `Vec<T>` in `mod std`** — retires the i64-only builtin.
  Needs ARC-aware generic destructors.
- **File-based modules** — `mod name;` loading `name.rn`. Lets the
  stdlib ship as files instead of an `include_str!` blob.
- **`?` operator** — desugar `expr?` over `Result` now that `Result`
  is the standard type.
- **`use std::*` globs** — so prelude items can be unqualified.
- **More helpers** — string helpers, an iterator trait, `HashMap`.
- **Span fix** — stop the prelude length leaking into user-code error
  offsets.
