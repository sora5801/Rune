# Session 072 — `?` on Option + multi-impl Into

**Date:** 2026-05-24
**Outcome:** Two related improvements to ? operator semantics.
The `?` operator now works on `Option<T>` (returns `None` on
`None`, unwraps on `Some`). And a source error struct that
implements `Into<A>` AND `Into<B>` now disambiguates correctly
at each `?` site — the checker picks the impl whose target
matches the surrounding fn's err type. 593 tests green (+3 from
session 071).

```rune
// ? on Option:
fn get() -> Option<i64> { Option::Some(42) }
fn use_get() -> Option<i64> {
    let v: i64 = get()?;           // None propagates, Some unwraps
    Option::Some(v + 8)
}

// Multi-impl Into disambiguation:
struct IoErr   { code: i64 }
struct AppErr  { tag: i64 }
struct WireErr { kind: i64 }
impl std::Into<AppErr>  for IoErr { fn into(self: IoErr) -> AppErr  { ... } }
impl std::Into<WireErr> for IoErr { fn into(self: IoErr) -> WireErr { ... } }

fn into_app()  -> Result<i64, AppErr>  { let v: i64 = read()?; ... }
                                       //         ^ picks Into<AppErr>
fn into_wire() -> Result<i64, WireErr> { let v: i64 = read()?; ... }
                                       //         ^ picks Into<WireErr>
```

## The decisive observation

Both fixes touch the same code path (check_try) but address
orthogonal issues. The `?`-on-Option extension is the simpler
of the two: same desugar shape as Result, just with `Some(v) =>
v, None => return None` instead of `Ok/Err`. No conversion exists
(no err type to convert) so check_try can return early once it
spots an Option scrutinee.

Multi-impl Into is more interesting. Pre-072 the `into` method
was looked up via `impl_methods[(source, "into")]` — a single
entry, silently overwritten when the same source struct had
multiple Into impls. The fix records per-impl target types in
a new `into_impls: HashMap<SymbolId, Vec<(Type, SymbolId)>>`,
keyed by the source struct sym; check_try walks the list,
resolves each target's AST type, and matches against the
surrounding fn's err type. The chosen fn sym is recorded
directly in `try_conversions` (previously held the *source*
sym; now holds the *fn sym* itself), so the lowerer makes the
call with zero further lookup.

The resolver gets one ergonomic concession: duplicate `into`
method names on the same source struct no longer error, but
ONLY when the impl is for `Into<T>`. Non-Into impls keep the
strict "method already defined" check. Codegen-side, the
mangled fn name appends `__{impl_span_start}` for Into impls
so Cranelift's symbol table doesn't reject the duplicates.

## The wire-ups

```
src/ast.rs        (no change)

src/parser.rs     (no change)

src/resolver.rs   (new `into_impls: HashMap<SymbolId, Vec<(Type,
                   SymbolId)>>` field. declare_impl detects Into
                   impls by checking the trait sym's name against
                   "Into", extracts the target from the trait
                   path's first generic arg, pushes
                   (target_ast, into_fn_sym) onto the source's
                   list. Duplicate-method check skipped for Into
                   impls. Mangled fn name disambiguated by impl
                   span start so multiple `into` methods on the
                   same struct don't collide at the Cranelift
                   level.)

src/checker.rs    (check_try gains an `option_shape` detection
                   before the `result_shape` path; on hit, the
                   surrounding fn's return type is checked
                   against the same Option enum sym, then early-
                   return with the unwrapped ok type. The Result
                   path's err-mismatch branch now walks
                   `into_impls[source]`, resolves each
                   target AST type, picks the first that matches
                   the surrounding fn's err type. Records the
                   chosen fn sym in try_conversions; the borrow
                   on current_return is cloned to avoid clashing
                   with the mutable self in resolve_type.)

src/lower.rs      (lower_try dispatches on Option vs Result at
                   the scrutinee's Ty. lower_try_option builds
                   the `Some(v) => v, None => return None`
                   match, mirroring lower_try's structure. The
                   Result branch's conversion path simplifies:
                   try_conversions now holds the fn sym
                   directly — no impl_methods lookup needed.)
```

## What's tested

Codegen (+3):

- `try_op_on_option_some_unwraps` — `Some(42)?` produces 42.
- `try_op_on_option_none_propagates` — `None?` short-circuits
  the surrounding fn back to `None`.
- `try_op_with_multi_into_picks_right_target` — `IoErr` has
  Into impls for both `AppErr` and `WireErr`; two distinct
  fns use `?` to convert to each target. Both work; pre-072
  one would have been silently misrouted.

## Apparent bugs that aren't / explicitly deferred

- **No Into-target inference yet at value site.** A bare
  `let a: AppErr = some_io_err.into();` still goes through
  the single-method lookup and picks whichever Into impl
  was registered last. Only `?` disambiguates. Lifting this
  is future work — would need a hint-based method-call
  resolution similar to closure inference.
- **Option's `?` has no `?`-style conversion analog.** A
  `Result<T, E>?` in a fn returning `Option<U>` errors with
  "the `?` operator can only be used in a function returning
  a `Result`." Rust supports cross-conversion via `Try`
  trait machinery — out of scope for v0.x.
- **Into-impl mangled names append a span offset.** Stable
  for a given source layout but not portable across
  reformats. Codegen-only concern — user-facing names
  unaffected.
- **Two Into impls with structurally-equivalent targets**
  (e.g., two `Into<AppErr>` differing only by trait args)
  pick the first match by source order. v0.x has no
  ambiguity error here — the first impl wins silently.

## What's next

- **More default-body trait methods** — `.map(f)`, `.filter(p)`,
  `.fold(...)`, `.count()`, `.sum()`.
- **Tuple types** — unblocks HashMap `.values()` / `.entries()`.
- **Method-call-position Into inference** — explicit target
  hints from let / fn arg / struct field.
- **Self-hosted bootstrap** — long-term.
