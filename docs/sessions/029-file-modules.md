# Session 029 — File-based modules

**Date:** 2026-05-20
**Outcome:** `mod name;` (no body) now loads `name.rn` from disk and
splices its items in as though `mod name { ... }` had been written. A
program can finally span multiple files. 412 tests green (+7 from
session 028's 405).

This is the other half of the "generic `Vec<T>` + file-based modules"
request — session 028 did `Vec<T>`, this one does the modules.

## The headline

`main.rn`:
```rune
mod helper;
use helper::triple;

fn main() -> i64 {
    triple(7) + helper::square(4)
}
```

`helper.rn`:
```rune
fn triple(x: i64) -> i64 { x * 3 }
fn square(x: i64) -> i64 { x * x }
```

`rune run main.rn` → `37`. Module files nest (a loaded file can
`mod` another), and a loaded file sees `std::` like any other.

## Design — token-stream splicing

The key decision: **`mod name;` is expanded at the token level, before
parsing.** `expand_modules` (new `src/modules.rs`) scans the token
stream for the triple `mod IDENT ;` and rewrites it to
`mod IDENT { <name.rn's tokens> }`.

The consequence is the nice part: the **parser, resolver, checker, and
lowerer are entirely unchanged**. They only ever see inline modules.
There's no new AST node, no `parse_mod` change, no resolver pass. The
inline-module machinery from session 026 — qualified-key namespacing,
`current_path`, module-mangled codegen names — does all the work. A
file-based module *is* an inline module after expansion.

```
mod helper ;          ─expand→     mod helper { fn triple ... }
```

The expansion is recursive — a loaded file's tokens are themselves
scanned for `mod` declarations — and the scan is a flat linear pass,
so a `mod foo;` nested inside an inline `mod a { ... }` is expanded
just the same.

## The span problem

Every `Span` in Rune is a pair of byte offsets into *the* source
string. Lex two files independently and both start at offset 0 — their
spans collide. That matters because the resolver and checker key
`HashMap`s on `Span` (`path_to_sym`, `expr_types`, ...); a collision is
a silent miscompilation, not a crash.

So each loaded file is lexed into a **fresh, disjoint slice of one
global offset space**. After lexing `name.rn`, every token span (and
every lex-error span) is shifted by a `base` offset that sits past
every file loaded so far. The main source is `0..N`; the first module
is `N+1..`, the next after that, and so on. Spans stay globally
unique, and nothing downstream needs to change.

A `SourceMap` records each file's `label: start..end`. Because error
offsets are now global, the driver prints the map as a note whenever a
multi-file program has errors:

```
note: byte offsets span multiple files —
  main.rn: 0..2533
  helper.rn: 2534..2602
```

(The prelude already shifted user offsets — a documented v0.x rough
edge. File modules extend that; the SourceMap note is the mitigation.)

## Loading, cycles, and errors

`expand_modules` takes a `loader: &dyn Fn(&str) -> Option<String>` —
a module name → source-text lookup. The driver's loader reads
`<dir-of-main-file>/<name>.rn`; the test harnesses use an in-memory
`(name, source)` map, so multi-file tests need no temp files.

A new `ModuleError` category covers:
- **missing file** — `mod ghost;` with no `ghost.rn`.
- **import cycle** — a load stack tracks the modules currently being
  expanded; `a → b → a` is caught and reported instead of looping.

On either error the offending `mod name;` expands to an empty
`mod name { }` so the rest of the program still parses.

## v0.x scope

Module files are **flat**: `mod foo;` always loads `foo.rn` from the
main file's directory, regardless of how deeply the `mod` declaration
is nested. There are no per-file relative paths and no `mod.rs`-style
directory trees — those are a follow-up. For small multi-file programs
the flat model is enough and keeps the loader trivial.

The prelude is prepended to the **main** source only; loaded modules
see `std::` through the shared global namespace, not by re-importing
it.

## Pipeline

```
src/
├── modules.rs   (NEW — expand_modules, SourceMap, ModuleError;
│                  token-stream splicing + offset rebasing)
├── lib.rs       (pub mod modules; re-exports)
└── main.rs      (load_and_expand; check/run/build/ast call it;
                  print_errors + note_source_map helpers)
```

Nothing else changed — the parser, resolver, checker, lowerer, and
codegen are untouched.

## What's tested

Codegen (+4):
- `file_module_call_across_files` — `helper::triple(7)`.
- `file_module_use_import` — `use helper::square;` then a bare call.
- `file_module_nested` — `main` → `mid` → `leaf`, each its own file.
- `file_module_uses_std` — a loaded module calls `std::max`.

Typecheck (+3):
- `file_module_resolves_ok` — a two-file program is clean.
- `file_module_missing_file_is_error` — `mod ghost;`, no file.
- `file_module_cycle_is_error` — `main → a → b → a`.

The two harnesses gained `run_main_files` / `run_files`, which take a
`&[(name, source)]` slice and build an in-memory loader. All 405 prior
tests still pass — `run_main` / `run` now route through
`expand_modules` with an empty loader (a no-op for single-file code).

## Apparent bugs that aren't

- **`mod name;` never reaches the parser.** Token-stream splicing
  rewrites it before parsing, so the parser still only knows inline
  `mod name { ... }`. That's deliberate — it's why nothing downstream
  needed to change.

- **Module files are flat.** `mod foo;` resolves to the main file's
  directory at any nesting depth — a loaded `sub.rn` declaring
  `mod bar;` still loads `<main-dir>/bar.rn`, not `<sub-dir>/bar.rn`.
  Nested directories are a follow-up.

- **Error offsets are global.** With multiple files, an error's
  `start..end` is an offset into the combined virtual space, not into
  any one file. The `SourceMap` note maps the ranges back. A proper
  fix (file id on every `Span`, or per-file local offsets in error
  output) is a larger refactor — deferred, consistent with the
  existing prelude-offset rough edge.

- **`mod foo;` twice loads `foo.rn` twice.** Each `mod` declaration is
  an independent module — `mod foo;` at the root and inside `mod a`
  give `foo` and `a::foo`, two separate namespaces. Only *cycles* are
  an error, not repeated loads.

- **The prelude is still embedded.** `std.rn` is `include_str!`-baked
  into the compiler. Now that file modules exist it *could* ship as an
  external file; that's a separate follow-up.

## What's next

- **Nested module directories** — per-file relative path resolution,
  `mod.rs`-style trees.
- **Visibility enforcement** — make `pub` mean something across module
  boundaries (parsed since session 026, still not enforced).
- **`use` globs / renaming** — `use m::*`, `use x as y`.
- **File ids on spans** — so multi-file error messages name the file
  and use file-local offsets directly.
- **Ship the prelude as an external file** instead of `include_str!`.
