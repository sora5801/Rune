# Session 124 — File-granularity modules: end-to-end coverage

**Date:** 2026-05-25
**Outcome:** The module system was already wired
(session 020-era `expand_modules` runs in the CLI's
`load_and_expand`); this session adds the test
coverage and demonstration program that prove it
works through the AOT pipeline and across struct /
enum / trait boundaries. 488 codegen + 47 AOT + 223
typecheck tests green (+3 codegen + 4 AOT from
session 123). New `examples/calculator/` directory
with a real three-file project.

```rune
// examples/calculator/main.rn
mod math;
mod stats;

fn main() -> i64 {
    let nums: Vec<i64> = vec_new();
    nums.push(1); nums.push(2); nums.push(3); nums.push(4);
    stats::sum_of_squares(nums)    // 30
}
```

```
$ cargo run -- run examples/calculator/main.rn
30
```

## The decisive observation

The bootstrap roadmap listed "module system at file
granularity" as a Tier B blocker. On closer
inspection, the machinery has been in place since
the early sessions:

- **`src/modules.rs`** has `expand_modules` — a
  token-level pre-pass that rewrites `mod name;`
  into `mod name { <name.rn tokens> }` before the
  parser runs.
- **`src/main.rs`** has `load_and_expand` — the
  CLI's filesystem-backed loader that resolves
  `mod foo;` to `foo.rn` next to the main file,
  and `mod bar;` *inside* `foo.rn` to `foo/bar.rn`.
- **The resolver / checker / lowerer / codegen**
  treat the expanded token stream as inline
  modules — no special-casing for file modules
  anywhere downstream.

What was missing wasn't infrastructure; it was
*confidence*. The existing 4 JIT tests covered
cross-file function calls + `use` imports + nested
directories + std-prelude visibility. AOT had zero
multi-file coverage, and JIT tests didn't cover
cross-file struct / enum / trait usage. This
session fills both gaps.

### AOT integration coverage

Added `build_exe_files` to the AOT test harness —
mirrors `tests/codegen.rs::run_main_files`'s
loader-closure pattern but produces a native
executable and runs it. Four AOT tests:

- `aot_multi_file_call_across_modules` — fn call
  via `helper::triple(14)`. The basic case proves
  the AOT pipeline doesn't drop module info.
- `aot_multi_file_use_import` — `use helper::square`
  alias. Verifies symbol mangling for aliased fns.
- `aot_multi_file_struct_across_modules` — define
  a struct in shapes.rn, construct + access fields
  from main. Tests cross-module *type* symbols,
  not just functions.
- `aot_multi_file_nested_directory` — `mod mid; mod
  leaf;` inside mid.rn loads `mid/leaf.rn`. Tests
  the directory-descent rule.

### Cross-file struct / enum / trait (JIT)

Three new JIT tests catching the type-level cases:

- `file_module_struct_construction` — pub struct in
  shapes.rn, construct from main with
  `shapes::Point { x, y }`.
- `file_module_enum_match` — pub enum in traffic.rn,
  match on variants from main with
  `traffic::Light::Green` paths.
- `file_module_trait_impl_across_files` —
  pub trait declared in helper.rn, struct in
  shapes.rn `impl helper::Area for Square`, main
  calls the trait method via a bounded generic
  fn. The most interesting case: traits + impls
  spanning three files.

### The example program

`examples/calculator/`:

```
main.rn   — entry point, builds a Vec<i64> and calls
            stats::sum_of_squares
math.rn   — pure helpers (add, mul, square)
stats.rn  — uses math::square + math::add to compute
            sum-of-squares over a Vec
```

Three pub fns spread across three files; main uses
two via `mod stats;` + qualified call; stats uses
math via `use math::square; use math::add;`. Total
≈40 lines of Rune. Runs via `rune run`, builds via
`rune build`.

### Why this is a Phase 1 milestone

The bootstrap compiler can't fit in one .rn file.
A realistic split looks like:

```
bootstrap/
├── main.rn      — entry: read argv, dispatch
├── lexer.rn     — tokenize a .rn source
├── parser.rn    — AST construction
├── resolver.rn  — name resolution
├── checker.rn   — type checking
├── lower.rn     — AST → IR
└── codegen.rn   — IR → .clif text
```

Six modules + main. Session 124 confirms that all
of this is supported today: each file declares its
own `pub fn`s, the others import via `use`, the
resolver / checker / codegen treat the whole
program as if it were one file at the IR level.
The bootstrap is no longer blocked on the module
system.

## The wire-ups

```
tests/aot.rs       (+build_exe_files helper that uses expand_modules
                    with a closure loader,
                    +build_and_run_files wrapper,
                    +4 multi-file AOT tests)

tests/codegen.rs   (+3 file-module tests: struct, enum, trait)

examples/calculator/  (+main.rn, +math.rn, +stats.rn — three-file
                       demo program runnable via `rune run`)

LANGUAGE.md        (decision-log row, no doc body changes —
                    existing "Modules and visibility" section
                    is comprehensive)

docs/sessions/124-file-modules-end-to-end.md  (this file)
```

No source code changes. The module system has been
working; session 124 is verification + demonstration.

## What's tested

Codegen (+3 from session 123's 485):

- `file_module_struct_construction` — `shapes::Point
  { x: 10, y: 32 }` from main; field access works.
- `file_module_enum_match` — `traffic::Light::Green`
  variant + cross-module match exhaustiveness.
- `file_module_trait_impl_across_files` — trait in
  helper.rn, impl in shapes.rn, bounded-generic
  call from main. The most cross-cutting test.

AOT (+4 from session 123's 43):

- `aot_multi_file_call_across_modules` — basic fn
  call through `mod helper;`.
- `aot_multi_file_use_import` — `use helper::name`.
- `aot_multi_file_struct_across_modules` — struct
  in a separate file, used in main.
- `aot_multi_file_nested_directory` — `foo/bar.rn`
  via nested `mod` declarations.

## Apparent bugs that aren't / explicitly deferred

- **No `pub(crate)` / `pub(super)`.** Same as
  session 020's deferral. v0.x is all-or-nothing
  on visibility.
- **No `mod.rs`-style directory roots.** A
  `foo/mod.rn` file as the root of a `foo` module
  isn't supported — `foo.rn` is the only shape.
  Idiomatic Rune is flatter than idiomatic Rust;
  this matches.
- **No `use foo;` for path-only imports.** `use`
  must alias a leaf item (fn, struct, enum,
  trait). To "use a module by name," just refer
  to it qualified: `foo::bar()` after `mod foo;`.
- **MAX_MODULE_DEPTH = 64.** Hard cap to guard
  against pathological loaders. The bootstrap is
  unlikely to nest more than 3-4 levels deep, so
  this is generous.
- **Circular `mod` impossible by construction.**
  `mod foo;` always descends into a subdirectory.
  Files form a tree, not a graph; no cycle
  detection needed.
- **The example uses `let mut total = 0;` + `total
  = add(total, ...)`.** A more idiomatic Rune form
  would use `xs.iter().fold(0, |a, x| a + math::
  square(x))` but the closure + bounded-generic
  fold path is heavier to demonstrate. The
  imperative form is clearer for showcasing
  modules.

## What's next

- **Session 125: `Box<T>` for recursive types** —
  the next Tier C blocker. Self-referential AST
  enums (`Expr::Binary { lhs: Expr, rhs: Expr }`)
  need implicit or explicit boxing.
- **Session 126: Pattern guards + `let ... else`**
  — Tier C ergonomics for the bootstrap parser.
- **Session 127+**: continued Phase 1 buildout.
