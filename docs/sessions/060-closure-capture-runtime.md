# Session 060 — Closure capture (runtime)

**Date:** 2026-05-23
**Outcome:** Capturing closures now execute end-to-end. The
headline from session 057's preview compiles and runs:

```rune
let mult: i64 = 3;
let f: fn(i64) -> i64 = |x| x * mult;
f(7)   // 21
```

Each capturing lambda synthesizes a struct holding the captured
fields plus a `call` method whose body has captured `Local`
reads rewritten as `FieldAccess` on `self`. The lambda
expression at the source becomes a struct literal of the
captures; the call site dispatches via the struct's call method
(populated in `impl_methods` by session 059's resolver work).
~3 files. 542 tests green (+3 from 539).

## The decisive observation

The whole feature decomposes into "synthesize a struct" + "rewrite
the body" + "dispatch the call." Sessions 058 (generic traits)
and 059 (capture recording + synth syms minted by the resolver)
paid for the prerequisites. The remaining work was:

1. **Checker registers the synth struct's layout and the call
   method's signature** during the closure typecheck (after
   capture types are pinned by the surrounding let / struct-lit
   inference).
2. **Lowerer synthesizes the impl method's HirFn** with a body
   walk that rewrites `Local(captured_sym)` → `FieldAccess(self,
   offset, ty)`.
3. **Lowerer rewrites the closure expression** at the source
   into a `StructLit` of the captures.
4. **Lowerer dispatches `f(args)` for closure-typed `f`** via
   `Call(call_method_sym, [f, args])` instead of `IndirectCall`.
5. **Checker accepts the let-annotation-vs-closure-struct
   mismatch** when the closure satisfies the annotation's
   signature, binding the local to the actual struct type so the
   downstream call site dispatches correctly.

The annotation form (`let f: fn(i64) -> i64 = |x| ...`) is
required for now because Rune doesn't have bottom-up
type-inference for closure parameters. Once a hint flows
through (let annotation, struct field, fn arg), the param types
are pinned; the closure's value type is `Ty::Struct(closure_sym,
[])` regardless. The annotation also tells the user "this is a
callable taking i64 returning i64" — true semantically even if
the runtime is a struct dispatched via a synth method.

## The wire-ups

```
src/
├── checker.rs    (check_closure: capturing closures return
│                  Ty::Struct(closure_sym, []) and build the
│                  struct's StructLayout + the call method's
│                  fn_signature; check_call accepts callees of
│                  closure-struct type and uses the call
│                  method's signature minus the leading `self`;
│                  check_let lets a closure-struct value bind a
│                  fn-pointer-annotated local — the annotation
│                  fed inference but the binding's true type is
│                  the struct)
└── lower.rs      (lower_closure has two paths: non-capturing
                   keeps session 057's anonymous-fn item; capturing
                   synthesizes the call method's HirFn (with body
                   rewritten via rewrite_captures), pushes it to
                   `synthesized_fns`, and returns a StructLit of
                   the captured locals. New `rewrite_captures`
                   walks the body HIR replacing captured `Local`
                   reads with `FieldAccess(self, offset, ty)`.
                   Lower_expr's Call arm dispatches Local-of-
                   closure-struct callees via the existing
                   impl_methods lookup, emitting `Call(call_sym,
                   [callee, ...args])` instead of IndirectCall.)
```

The session-059 groundwork did the heavy lifting: the synth
struct sym, the call method sym, the `impl_methods` entry, and
the `impls_for` entry were all already in place. Session 060
filled in the actual code that uses them.

## What's tested

Codegen (+3):

- `closure_capture_basic` — the headline. `let mult: i64 = 3; let
  f: fn(i64) -> i64 = |x| x * mult; f(7)` returns 21.
- `closure_capture_multiple` — `|x| x * a + b` capturing two
  i64s, computes 6*5+10 = 40.
- `closure_capture_call_twice` — the closure is invoked twice
  with different args; the captured `base` remains accessible
  across calls, computes (1+10)+(2+10) = 23.

The existing test from session 059 (`closure_capture_session_059_groundwork`)
still pins the non-capturing path.

## Apparent bugs that aren't / explicitly deferred

- **The let annotation is required** for capturing closures. A
  bare `let f = |x| x * mult` errors because `x` has no
  contextual hint to pin its type. Bottom-up param inference
  (bidirectional from body usage) would lift this; punted to a
  follow-up session.
- **`|x| x * mult` directly inside `Map { f: ... }`** still
  doesn't work — the iterator-adapter pipeline keeps Map's `f`
  field typed as `fn(I::Item) -> U`, and a closure value (which
  has type `Ty::Struct(closure_sym, [])`) doesn't coerce there.
  The struct-lit field check rejects the coercion. Adding the
  coercion would need to also rewrite the field's storage type
  at construction, and then have monomorphize substitute the
  call site to dispatch via the struct's call method. Substantial;
  the Plan agent flagged it as the Map-integration tail of the
  session 058/059/060 arc. Deferred to session 061.
- **No `FnMut` or move-capture semantics.** Captures are
  read-only; the resolver rejects `self.field = ...` inside a
  closure body. v0.x keeps this constraint.
- **Captures that are themselves closures.** A closure
  capturing another closure should work in principle (the synth
  struct's field holds the inner closure struct value), but is
  not tested. The lowerer's recursive walks handle nested closure
  expressions via the existing pipeline.
- **ARC for captured values.** Capturing a `Vec<i64>` or `str`
  would carry an ARC-managed value into the closure struct; the
  existing `struct_arc_fields` machinery should pick it up
  automatically via the struct's layout, but this is not yet
  explicitly tested.

## Symbol-identity / state-leak check

The capture rewrite uses each capture sym as a HashMap key
(`capture_info: HashMap<SymbolId, (offset, ty)>`). The synth
`self` parameter sym is minted via the lowerer's `fresh_sym`
(out of resolver range), so it can't collide with any
user-written binding. The synth struct's field offsets are
`i * 8` for capture index `i`; the rewrite uses the same
indexing as the StructLit construction so the field offsets
match between writer (StructLit at the closure site) and
reader (FieldAccess in the synth call method's body).

The synth call method's `self` param has type
`Ty::Struct(closure_struct_sym, [])`. The lowerer constructs
this directly. Codegen handles it as a normal struct method —
the `self` Variable gets allocated in compile_fn's prologue
along with other params; FieldAccess reads work via the
existing struct-pointer + offset path.

## What's next

- **Session 061: closures in Map/Filter** — let the closure
  literal's `Ty::Struct(closure_sym, [])` coerce to Map's
  `f: fn(I::Item) -> U` field at construction, with the
  monomorphizer rewriting the call site to dispatch via the
  closure's call method. The truly-headline preview compiles.
- **Bottom-up closure-param inference** — lift the
  let-annotation requirement so `let f = |x| x * mult` works.
- **HashMap, Range as RangeIter, continue, From-based `?`** —
  independent.
