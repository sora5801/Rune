# Session 097 — Codegen-side diagnostic polish

**Date:** 2026-05-25
**Outcome:** Codegen-side error messages now render
user types with friendly names (`AppErr`,
`HashMapKeysIter<i64>`) instead of internal sym
indices (`struct#83`, `dyn#47`). Closes session 093's
deferred half — that session covered the checker;
this one covers codegen. 424 codegen + 158 typecheck
tests green; no behavior change, only diagnostic
strings.

```text
# Before (any codegen error mentioning a user type):
method `.foo` on `struct#83` is not implemented
no `as` codegen for `struct#83` -> `enum#47`
internal: unresolved associated-type projection `T#149::Item`

# After:
method `.foo` on `AppErr` is not implemented
no `as` codegen for `AppErr` -> `IoErr`
internal: unresolved associated-type projection `T::Item`
```

## The decisive observation

`Ty::display` lives in `ty.rs` which has no access to
the resolver, so it falls back to `struct#{sym_id}` /
`enum#{sym_id}` / `dyn#{sym_id}` / `T#{sym_id}` for
any type carrying a SymbolId. Session 093 worked
around it in the checker by adding a `Checker::
ty_pretty` helper that consults `self.res`. Codegen
runs after lowering, has no `Resolutions` borrow, and
its error sites still spoke through bare
`Ty::display`.

The fix mirrors session 093's approach, just one
layer down: snapshot the user-visible sym names at
lower time into `HirModule::sym_names`, propagate
into `Codegen::sym_names` at `compile_module` entry
and into `FnCodegen::sym_names` per function, then
expose a free `ty_pretty(ty, names) -> String`
helper that walks `Ty` recursively and substitutes
names where present.

### Why a free function (not a method)

`ty_pretty` could be a method on either Codegen or
FnCodegen, but the call sites span both (and free
functions like `arc_helper_name` / `cranelift_type`
also benefit). A `fn ty_pretty(&Ty, &HashMap<...>)`
takes the names by reference so the caller picks
whichever sym_names borrow it already has. Codegen
methods pass `&self.sym_names`; FnCodegen methods
pass `self.sym_names` (already a borrow).

### Recursive walk

```rust
fn ty_pretty(ty: &Ty, names: &HashMap<SymbolId, String>) -> String {
    match ty {
        Ty::Struct(s, args) | Ty::Enum(s, args) => {
            let label = names.get(s).cloned().unwrap_or_else(|| ty.display());
            // ... format with args via ty_pretty recursion
        }
        Ty::Dyn(s, args) => format!("dyn {}", label_for(s)),
        Ty::TypeVar(s) => names.get(s).cloned().unwrap_or_else(|| ty.display()),
        Ty::Vec(elem) => format!("Vec<{}>", ty_pretty(elem, names)),
        // ... all other recursive cases (Tuple, Fn, Array, HashMap, Weak, Assoc)
        _ => ty.display(),
    }
}
```

Primitive scalars (`Ty::Int(_)`, `Ty::Float(_)`,
etc.) fall through to `Ty::display` since they
already produce friendly names. `Ty::SelfType`,
`Ty::Never`, `Ty::Error` likewise — they're already
short and clear.

### Sym name snapshot

```rust
// At lower time:
sym_names: {
    let mut names = HashMap::new();
    for (i, sym) in self.res.symbols.iter().enumerate() {
        if matches!(
            sym.kind,
            SymbolKind::Struct | SymbolKind::Enum
                | SymbolKind::Trait | SymbolKind::TypeParam
        ) {
            names.insert(SymbolId(i as u32), sym.name.clone());
        }
    }
    names
},
```

Only kinds that contribute to user-visible `Ty`
displays. `SymbolKind::Fn`, `Module`, `BuiltinFn`,
etc. never appear in a `Ty::Struct` / `Enum` / `Dyn`
position so we skip them. The full sym table is
~hundreds of entries; only the user-defined and
trait syms get snapshotted (~tens of entries
typical).

### Free-function arms unchanged

`cranelift_type`, `elem_size`, `arc_helper_name` —
free functions inside codegen.rs that emit internal
errors (e.g., "type T#NN not supported in codegen")
— still use `Ty::display`. They have no access to
`sym_names` without threading it through dozens of
call sites. Those errors usually surface when
monomorphization left a TypeVar unresolved (a
compiler bug, not a user error); the cryptic name
is acceptable for now. If a TypeVar regression hits
real user programs, threading sym_names there is a
mechanical follow-up.

## The wire-ups

```
src/hir.rs        (HirModule gains sym_names:
                   HashMap<SymbolId, String>.)

src/lower.rs      (lower_module populates sym_names
                   from res.symbols, filtering to
                   Struct / Enum / Trait /
                   TypeParam kinds.)

src/codegen.rs    (Codegen gains sym_names field;
                   compile_module copies from HIR.
                   FnCodegen gets a borrow of it.
                   New free fn ty_pretty + reflective
                   walk. Six user-visible error sites
                   switched from .display() to
                   ty_pretty(...).)
```

No AST / parser / resolver / checker / monomorphize
changes — pure plumbing + polish.

## What's tested

No new tests — this is purely a diagnostic
improvement and the existing test suite confirms no
regressions. The `try_without_into_impl_rejected` and
similar typecheck tests that match on error messages
all pass (those errors come from the checker, which
already had session 093's polish; the
codegen-side errors don't have dedicated tests
because they're rare in practice).

A smoke verification: `e.nonexistent_method()` on a
struct `AppErr` now produces:

```
type error at ...: no method `.nonexistent_method` on type `AppErr`
```

Both checker (via session 093) and any codegen-side
fallback (this session) print `AppErr` instead of
`struct#NN`.

## Apparent bugs that aren't / explicitly deferred

- **Free-function error sites** — `cranelift_type`,
  `elem_size`, `arc_helper_name` still use
  `Ty::display`. Threading sym_names through their
  signatures would touch ~15 call sites and break
  some recursion-via-cranelift_type patterns. Their
  errors fire on internal compiler bugs (an
  unresolved Ty reaching codegen) rather than user
  mistakes; the cryptic name is acceptable until a
  regression makes them user-facing.
- **AOT-specific error paths** — `src/aot.rs`
  doesn't yet use ty_pretty. The AOT compile failure
  modes are all about linker invocation, not type
  rendering, so this isn't a real gap. If AOT ever
  emits a type-aware diagnostic, it'd thread
  sym_names from the HirModule the same way.
- **Empty-args struct/enum display change** — the
  `args.is_empty()` branch previously formatted as
  `struct#83`. Now it formats as just `AppErr`
  (without any decoration). For args-non-empty,
  format is `AppErr<i64>` — same shape as before
  modulo the friendly name.
- **`dyn` display change** — was `dyn#NN<args>` or
  `dyn#NN`; now `dyn Iterator<i64>` or `dyn
  Iterator`. The `dyn ` prefix is normal Rust syntax;
  pre-097 the `#NN` was visible noise.

## What's next

- **Const-eval overflow checks** — reject `100u8 +
  200u8` runtime overflow at compile time.
- **Chained binop hint propagation** — `1 + 2 + a:
  i32` works without parens.
- **Pre-1.0 audit** — retrospective doc covering
  what works / what's deferred / what's planned for
  1.0.
- **Self-hosted bootstrap** — long-term.
