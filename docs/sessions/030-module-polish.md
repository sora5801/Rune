# Session 030 — Module system polish

**Date:** 2026-05-21
**Outcome:** Three features finish the module system: nested module
directories, `pub` visibility enforcement, and `use` globs. 420 tests
green (+8 from session 029's 412).

The module system is now coherent end to end — files nest by
directory, `pub` actually controls access, and `use m::*` works.

## Nested module directories

Session 029 kept module files flat: `mod foo;` always loaded `foo.rn`
from the main file's directory, at any nesting depth. Now they nest:

```
main.rn        mod a;          → loads a.rn
a.rn           mod b;          → loads a/b.rn
a/b.rn         mod c;          → loads a/b/c.rn
```

The expander (`modules.rs`) threads a `/`-terminated **directory
prefix** through `expand_stream`: `""` for the main file, `"a/"`
inside a loaded `a.rn`, `"a/b/"` inside `a/b.rn`. A `mod X;` at prefix
`P` resolves to the module path `P + X`; that module's own children
get prefix `P + X + "/"`. The `loader` callback receives the
`/`-joined path (`"a/b"`); the driver joins it against the main file's
directory.

A nice consequence: because `mod` *always* descends into a
subdirectory, module paths strictly grow down every branch — **file
modules form a tree, and import cycles are structurally impossible.**
Session 029's load-stack cycle check is now dead code; it's replaced
by a depth cap (`MAX_MODULE_DEPTH`) that only guards against a
pathological `loader`.

## `pub` visibility enforcement

`pub` has been parsed since session 026 but never enforced — any item
was reachable by its qualified path. Now it means something.

The rule: **a non-`pub` item is visible only from its declaring
module and that module's descendants; a `pub` item is visible
anywhere.** The resolver records, per symbol, `(declaring module
path, is_pub)` in an `item_vis` table, and:

```rust
fn is_visible(&self, sym) -> bool {
    match self.item_vis.get(&sym) {
        None => true,                              // builtin / local
        Some((decl_mod, is_pub)) =>
            *is_pub || self.current_path.starts_with(decl_mod),
    }
}
```

`current_path.starts_with(decl_mod)` is the "declaring module or a
descendant" test — `["a","b"]` starts with `["a"]`, so code in `a::b`
sees `a`'s private items; the crate root `[]` starts with nothing, so
it can't reach into a module without `pub`.

The check runs in `resolve_path` (on the final symbol of every path,
plus the `Enum::Variant` fallback) and in `resolve_uses`. A failure is
a resolve error: `` `secret` is private to module `m` ``. Bare names
always pass — `lookup` only finds things in enclosing modules or the
root, all ancestors — so the check only ever bites a qualified path
reaching *into* a module.

**v0.x scope**: only the path's *final* symbol is checked. `m::f`
with `f` `pub` but `m` itself private is allowed — per-segment
privacy is a follow-up. Enum variants inherit their enum's visibility.

### Ripple

Turning enforcement on broke everything that referenced a non-`pub`
item across a module boundary:

- **`std.rn`** — the prelude is a `mod std { ... }`, referenced from
  user code at the root. Every item is now `pub` (and `pub mod std`).
- **Session 026's module tests** — `mod math { fn square }` etc.
  gained `pub` on the items used as `math::square` from outside.

That's expected churn — the same shape as session 028's `Vec` →
`Vec<i64>` migration.

## `use` globs

`use m::*;` aliases every item of `m` into the using module:

```rune
mod m {
    pub fn one() -> i64 { 1 }
    pub fn two() -> i64 { 2 }
}
use m::*;
fn main() -> i64 { one() + two() }   // 3
```

- **Parsing**: `UseDecl` gains a `glob: bool`. `parse_use` parses the
  path by hand — a trailing `::*` would otherwise trip `parse_path`'s
  `expect_ident` on the `*`.
- **Resolution**: `resolve_use_glob` finds the module's qualified key
  (absolute, then relative to each enclosing module), then enumerates
  `scopes[0]` keys under `<key>::` with no further `::` — the
  module's direct items — and aliases each into the using module.
- **Precedence**: aliases go in with `entry().or_insert`, so a local
  item or an explicit `use` of the same name wins over a glob.
- **Visibility**: the glob filters by `is_visible`, so `use m::*`
  from outside `m` brings in only `m`'s `pub` items.

## Pipeline

```
src/
├── ast.rs       (UseDecl.glob)
├── parser.rs    (parse_use — hand-rolled path + `::*`)
├── modules.rs   (dir-prefix threading; depth cap replaces cycle check)
├── resolver.rs  (item_vis table; is_visible / visibility_error;
│                 resolve_use_glob; checks in resolve_path/resolve_uses)
└── std.rn       (every item `pub`)
```

The checker, lowerer, and codegen are untouched — visibility and globs
are entirely resolve-time, nesting is entirely expansion-time.

## What's tested

Codegen (+2): `use_glob_imports_fns`, `use_glob_imports_struct`.

Typecheck (+6): `private_module_item_rejected`,
`pub_module_item_visible`, `private_item_visible_within_module`,
`use_glob_brings_items_into_scope`, `use_glob_of_missing_module_errors`,
`use_glob_omits_private_items`. Plus `file_module_nested_directory`
replaced session 029's now-impossible cycle test.

Nesting is also exercised by the codegen `file_module_nested` test
(main → mid → mid/leaf). All 412 prior tests still pass — session
026's module tests after their `pub` migration.

## Apparent bugs that aren't

- **Import cycles can't happen.** With nested directories `mod`
  always descends, so the module tree can't loop. The depth cap is
  the only runaway guard, and it only fires for a degenerate loader.

- **Only the final path segment is privacy-checked.** `m::f` where
  `f` is `pub` but `m` is a private module resolves fine. Catching
  that needs per-segment checks — deferred. The common case (a
  private helper inside an otherwise-used module) is enforced.

- **`pub` is all-or-nothing.** There's no `pub(crate)` /
  `pub(super)`; an item is either module-private or fully public.
  Fine for v0.x.

- **A glob can shadow another glob.** `use a::*; use b::*;` with a
  name in both — the first glob's binding wins (`or_insert`). No
  ambiguity error. Explicit `use` and local items always beat globs.

- **`std.rn` items are all `pub` now.** Required — the prelude's
  `mod std` is referenced from user code, which lives at the root,
  outside `std`.

## What's next

- **Per-segment privacy** — check intermediate modules in a path.
- **`use x as y`** renaming, **`pub use`** re-exports.
- **`pub(crate)` / `pub(super)`** — finer visibility.
- **Ship the prelude as an external file** now that nested file
  modules exist.
