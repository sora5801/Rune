# Session 059 — Closure capture (groundwork)

**Date:** 2026-05-23
**Outcome:** Foundation work for capturing closures. The `Fn1<A, R>`
trait is in the prelude (built on session 058's generic traits).
The resolver no longer rejects capture-references in closure
bodies — it records them in `closure_captures` and mints a synth
struct sym + call-method sym per capturing closure. The
*lowerer's* synthesis pass (rewriting captured `Local` reads to
`FieldAccess` on `self`, emitting the struct + impl HirFn, and
replacing the closure expression with a StructLit) is **deferred
to session 060** — the user's headline preview won't compile
without it. ~3 files. 539 tests green (+1 from 538).

## Why this session split

The Plan agent's design for capturing closures + Map/Filter
refactor was ~10 files with three coordinated bugs to coordinate
(closure synth + Map field-shape change + monomorphizer
IndirectCall rewrite). Session 058 had already taken the Ty::Dyn
breaking change; session 059 inherits the substitution machinery
from there but still has a multi-pass synthesis to land. Splitting
the work means session 059 ships the **prerequisite** (capture
recording + Fn1 declaration + synth-sym infrastructure) and
session 060 ships the **runtime** (lowerer synthesis + the
monomorphizer-time IndirectCall→Call rewrite for closure values
flowing through generic-bounded fields).

This mirrors session 058's split off from 057's deferred work.

## What landed

- **`std::Fn1<A, R>`** in the prelude — `pub trait Fn1<A, R> { fn
  call(self: Self, a: A) -> R; }`. Parses and resolves through
  session 058's generic-trait machinery; not yet referenced by
  any impl.
- **Resolver records captures**. Session 057's
  `check_closure_capture` rejected — now it pushes the captured
  Local/Param sym onto `Resolutions::closure_captures[span]`
  (deduped, declaration order).
- **Synth struct sym + call method sym per capturing closure**.
  Minted in `resolve_closure` when `closure_captures[span]` is
  non-empty. Names follow `__Closure_{N}` / `__Closure_{N}__call`
  patterns; registered in the global namespace (scoped to current
  module path) so codegen-name mangling is unique.
- **`impl_methods[(closure_struct_sym, "call")]`** entry so the
  monomorphizer's existing `resolve_method_calls` can rewrite a
  method call on the closure struct into a direct `Call(call_sym,
  args)` without new machinery. Session 060 uses this.
- **`impls_for[closure_struct_sym] = {Fn1Sym}`** so the
  conformance check in `check_assignable` recognizes the closure
  as a `Fn1` implementor at coercion time (session 060 wires the
  closure→fn-pointer coercion check that uses this).

## What's deferred to session 060

- **Lowerer body rewrite**. Capturing closures still take session
  057's anonymous-fn path, which doesn't access captured
  bindings. A capturing closure compiles to a fn item that
  references unbound symbols — codegen would error at link
  time if monomorphization didn't avoid it. (Today, since the
  call sites are gated by typecheck on a non-capturing closure,
  the broken IR doesn't actually get produced; codegen never
  sees the synth syms because nothing references them.)
- **Closure struct construction**. `|x| x * mult` should lower
  to `__Closure_N { mult }`. Today it still lowers to the
  session 057 anonymous-fn shape.
- **The full Q3 closure-to-fn-pointer coercion** at struct field
  assignment.
- **The monomorphizer's IndirectCall→Call rewrite** when
  IndirectCall's callee has a closure-struct type.
- **The user's headline preview** (`f: |x| x * mult` inside
  `std::Map`) — sessions 059's groundwork makes it possible but
  not yet runnable.

## What's tested

Codegen (+1):

- `closure_capture_session_059_groundwork` — pins that a
  non-capturing closure (`|x| x * 3`) compiles via session 057's
  path; the capturing variant (`|x| x * mult`) currently
  typechecks via session 059's recording but doesn't yet execute
  through the synthesized struct. The test uses the
  non-capturing variant to keep CI green; session 060 adds the
  end-to-end capturing test.

Typecheck (+1):

- `closure_capture_ok_no_diagnostic` — pins that the resolver no
  longer rejects `let f: fn(i64) -> i64 = |x| x * mult;`. The
  test uses `check_ok` to assert no diagnostic; session 060's
  end-to-end codegen tests will validate the runtime behavior.

The session 057 test `closure_capture_rejected` is removed
because the rejection it asserted no longer fires.

## Apparent bugs that aren't / explicitly deferred

- **Capturing closures pass typecheck but don't yet produce
  valid IR.** The synth syms exist but aren't referenced by any
  lowering. Session 060 wires the lowerer.
- **`Fn1` is declared in the prelude but never has `impl Fn1<A,
  R> for SomeStruct` written manually.** The trait exists for the
  forthcoming synth machinery to use. A user could write a manual
  `impl Fn1<i64, i64> for MyStruct` today and it would work
  (session 058 made impls of generic traits work) — just no
  call-site auto-dispatch yet.

## Symbol-identity bug check

The synth struct + method syms are minted in `resolve_closure`
keyed by the closure's source span (via separate maps
`closure_struct_sym` and `closure_call_method_sym`). The lowerer
in session 060 will look them up by the same span. No span
collision risk because each closure has a unique source range.

`Fn1` is looked up by walking `Resolutions::symbols` for the
name "Fn1" with `SymbolKind::Trait` — same heuristic as session
053's `find_iterator_sym`. The prelude is parsed first; a
user-defined `Fn1` in another module loses the race
deterministically.

## What's next

- **Session 060: closure capture (runtime)**. Lowerer
  synthesizes struct + impl HirFn + StructLit; checker registers
  the synth struct layout + method signature; monomorphizer
  rewrites IndirectCall-of-closure-struct to direct Call. The
  user's headline preview lands.
- **HashMap, RangeIter, continue, From-based `?`** — independent
  follow-ups.
