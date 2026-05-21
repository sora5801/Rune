# Session 026 — Module system (inline)

**Date:** 2026-05-20
**Outcome:** Inline modules land. `mod name { items... }`, nested
modules, `use a::b::c;` imports, and multi-segment path resolution
all work end-to-end. Functions in different modules with the same
name no longer collide. 384 tests green (+9 from session 025's 375).

This is the last structural blocker before a real stdlib — a
`mod collections { ... }` can now hold a self-contained `Vec<T>`.

## The headline

```rune
mod math {
    fn square(x: i64) -> i64 { x * x }
    fn quad(x: i64) -> i64 { square(square(x)) }   // sibling, unqualified
}

mod a { fn f(x: i64) -> i64 { x + 1 } }
mod b { fn f(x: i64) -> i64 { x + 2 } }            // no collision with a::f

use math::quad;

fn main() -> i64 {
    quad(2) + a::f(0) + b::f(0)                     // 16 + 1 + 2
}
```

Nested modules:
```rune
mod outer {
    mod inner {
        fn deep(x: i64) -> i64 { x * 100 }
    }
    fn mid(x: i64) -> i64 { inner::deep(x) + 1 }
}
```

## Design — flat qualified namespace

Rather than a tree of per-module symbol tables, the resolver
flattens everything into the **global namespace** under
**module-qualified keys**:

- `fn f` at root → key `"f"`.
- `fn f` in `mod m` → key `"m::f"`.
- `fn f` in `mod a { mod b }` → key `"a::b::f"`.

Since identifiers can't contain `::`, qualified keys never collide
with bare identifiers. The resolver carries `current_path:
Vec<String>` — the module names it's currently inside — through all
passes.

### Lookup

A **bare name** `name` tries each enclosing module prefix,
longest-first, then root:
```
inside mod a { mod b }:  a::b::name → a::name → name
```
This gives the "innermost wins, ancestors and root visible"
semantics. Builtins (`i64`, `print`, ...) live at root under bare
keys, so they're found by the final attempt.

A **multi-segment path** `a::b::c` (`lookup_path`) tries the joined
key absolutely (`"a::b::c"`) then relative to each enclosing module
prefix.

### resolve_path

```
1. Try the whole path as a qualified item (lookup_path).
2. Else, if >= 2 segments: treat segments[..n-1] as an enum type
   path and the last as a variant — `m::Color::Red` works.
3. Else: "unresolved name/path" error.
```

`Enum::Variant` resolution composes with module paths: the enum can
itself be inside a module.

## Codegen name mangling

Two modules can each declare `fn f`. They'd both want the Cranelift
symbol `f` — a clash. So functions get a module-mangled **codegen
name**:

- root `fn f` → `"f"` (so `main` stays `main`, the entry point).
- `mod m { fn f }` → `"m__f"`.
- `mod a { mod b { fn f }}` → `"a__b__f"`.

The mangled name is stored in `Symbol.name`; codegen uses it
directly when declaring the Cranelift function. Structs/enums/etc.
keep bare names — they never produce a Cranelift symbol.

Impl methods inside a module mangle with the module prefix too
(`m__Point__push`), so `impl Point` in two modules doesn't clash.

## Pipeline

```
src/
├── token.rs    (mod, use keywords)
├── ast.rs      (Item::Mod(ModDecl), Item::Use(UseDecl))
├── parser.rs   (parse_mod, parse_use; mod/use added to item-start
│                and statement-item sets + error recovery)
├── resolver.rs (SymbolKind::Module; current_path; intern_item with
│                qualified keys; mangled_fn_name; lookup +
│                lookup_path rewritten; resolve_path multi-segment;
│                declare_items / declare_impls / resolve_uses /
│                resolve_items all recurse into modules; new pass
│                1.7 for use aliases)
├── checker.rs  (collect_struct_layouts + register_signatures
│                recurse into modules; check_item handles Item::Mod)
└── lower.rs    (lower_items recurses into modules — functions are
                 emitted flat with their already-mangled names)
```

The four resolver passes:
1. **declare_items** — intern every item under its qualified key.
2. **declare_impls** — impl methods (recurses into modules).
3. **resolve_uses** (new) — resolve each `use` path, alias the final
   segment into the using module's namespace.
4. **resolve_items** — resolve bodies.

`use` is pass 1.7 — after every item is declared (so the target
exists) but before bodies are resolved (so they can see the alias).

## `use`

`use a::b::c;` resolves `a::b::c` to a symbol, then inserts an alias
`<current_module>::c → that_symbol` into the global namespace. A
bare reference to `c` from within the using module then resolves
through the normal qualified-lookup path.

## What's tested

Codegen (+6):
- `module_qualified_call` — `math::square(7)`.
- `module_use_import` — `use math::cube;` then bare `cube(3)`.
- `module_intra_module_call` — a function calls its sibling
  unqualified.
- `module_nested` — `outer::inner::deep` two levels deep.
- `module_same_fn_name_no_collision` — `a::f` and `b::f` coexist.
- `module_struct_and_enum` — `m::Point { ... }` and
  `m::Kind::Tall` in a match.

Parser (+3): `parses_module`, `parses_nested_module`, `parses_use`.

All 375 prior tests still pass.

## Apparent bugs that aren't

- **`pub` is parsed but not enforced.** Any item is reachable by its
  qualified path regardless of `pub`. Real privacy checking
  (accessible from the declaring module + descendants, or anywhere
  if `pub`) is a documented follow-up. The module system's primary
  value — namespacing, `use`, no codegen collisions — works without
  it.

- **No file-based modules.** `mod name;` (without a body, loading
  `name.rn`) isn't supported. Inline `mod name { ... }` is the v0.x
  form. File loading needs IO plumbing in the driver and a
  multi-file test story — orthogonal to the resolution work done
  here.

- **The resolver flattens to a single namespace with qualified
  keys** rather than a tree of `HashMap`s. Simpler to implement and
  the `::`-joined keys can't collide with identifiers. A tree would
  be needed for, e.g., reflecting a module's contents; not needed
  for v0.x resolution.

- **`current_path` is threaded through every resolver pass.** A
  module's items are declared, impl-resolved, use-resolved, and
  body-resolved each with `current_path` set so qualified keys and
  relative lookups line up.

- **Mangled names show up in some error messages** for functions
  inside modules (`m__f`). Same tradeoff as impl methods
  (`Point__magnitude`) since session 012. Acceptable for v0.x.

## What's next

- **File-based modules** — `mod name;` loads `name.rn`. Driver IO +
  module-path-to-file mapping + multi-file tests.
- **Visibility enforcement** — make `pub` mean something across
  module boundaries.
- **`use` globs** (`use m::*`) and renaming (`use x as y`).
- **Stdlib** — now fully unblocked. A `mod std { mod collections {
  struct Vec<T> { ... } } }` can hold a real generic `Vec`,
  `Option<T>`, `Result<T, E>`, an iterator trait, etc. The
  remaining piece for an *external* stdlib (shipped separately from
  user code) is file-based modules.
