# Session 036 — Owned call arguments

**Date:** 2026-05-21
**Outcome:** Call-argument temporaries reclaim. An ARC value passed
to a function — a struct literal, a call result, a `dyn` box — that
no binding owns is now released by the caller once the call returns.
This closes the leak the trait-object sessions kept flagging
(`describe(Circle { .. })` and its kind). One change, in the `Call`
codegen arm. 450 tests green (+3 from session 035's 447).

## The leak

Rune's calling convention is **borrowing**: a function parameter is
never scope-tracked, never retained on entry, never released on
exit — the callee just reads it. That is correct for a value the
caller keeps owning:

```rune
let v = make_vec();
use_it(v);          // `v` borrowed; the `let` still owns it
```

But it leaks a value the caller does *not* keep:

```rune
use_it(make_vec());          // the Vec has a +1, and no binding owns it
describe(Circle { r: 10 });  // the dyn box, and the Circle — same
```

`make_vec()` returns a fresh `+1`. It is handed to `use_it`, which
borrows it and returns. Now nothing holds a reference to it, and
nothing ever released it. Leak — one per call, for every ARC
argument that is a temporary.

## The fix

After a `Call`, the caller releases each argument that is **a fresh
ARC temporary** — every ARC argument except a borrowed `Local` read:

```rust
HirExprKind::Call { callee, args } => {
    // ... compile args into arg_vals, emit the call ...
    for (a, &v) in args.iter().zip(&arg_vals) {
        if is_arc_type(&a.ty, ..) && !matches!(a.kind, HirExprKind::Local(_)) {
            self.emit_arc_call("release", &a.ty, v)?;
        }
    }
}
```

`Local`-vs-not is the same fresh/borrowed split the codebase already
uses for `let` ARC-on-copy, `compile_dyn_box`, `push`, and struct-
field assignment. A `Local` argument stays owned by its binding,
which releases it at that scope's exit; everything else — a struct
literal, a call, a `dyn` box, a string concat — carries a `+1` that
becomes the caller's to drop the moment the call returns.

The callee is **unchanged**. It still borrows. The whole feature is
caller-side cleanup — purely additive, no convention change.

## Why it composes

- **The result is independent of the arguments.** A function that
  returns an ARC value always returns it `+1`-owned — a tail
  `Local` is retained by `compile_block`'s tail-escape rule, a fresh
  producer carries its own count. The returned SSA value is distinct
  from any argument value, so releasing an argument can never free
  the result.

- **Returning an argument still works.** `fn id(v: Vec) -> Vec { v }`
  — the tail `v` is retained on the way out, so `id(make_vec())`
  yields `rc = 1` *after* the caller drops the argument temporary.

- **Storing an argument still works.** `fn wrap(v: Vec) -> Holder {
  Holder { v: v } }` — struct construction retains a `Local` field
  initializer, so `Holder.v` gets its own `+1`; the caller then
  drops the argument temporary back to that single owner.

## Scope: regular calls only

The release fires for `HirExprKind::Call` — direct function calls.
Deliberately not:

- **`MethodCall`** — `Vec::push` *consumes* its argument (the `+1`
  transfers into the element slot). Releasing a `push` argument
  would double-free it.
- **`BuiltinCall`** — `print` borrows, but `weak` / `upgrade_or`
  have ARC-subtle semantics; a blanket rule is unsafe and the
  payoff is small.
- **`DynCall`** — trait-object method calls; same borrowing story,
  left for a follow-up.

The headline leak — a `dyn` argument to a free function — is a
regular `Call`, so it is covered.

## What's tested

Codegen (+3):

- `call_arg_dyn_temp_released` — `describe(Circle { .. })` 200×; the
  boxed `dyn` argument is reclaimed each call, a double free crashes.
- `call_arg_vec_temp_released` — the argument is a call result
  (`vlen(triple())`) 200×; a fresh `Vec` temporary, released.
- `call_arg_local_not_released` — a `Local` passed three times stays
  valid; releasing it post-call would use-after-free on call two.

A leak is invisible to a functional test; a double free or
use-after-free crashes the JIT'd process. The loop tests drive
hundreds of alloc/release cycles, and the `Local` test proves the
borrowed path is left alone.

## Apparent bugs that aren't

- **A field read passed as an argument over-releases.** The
  `Local`-vs-not heuristic classifies `foo(s.arc_field)` as a fresh
  temporary, but a field read is a *borrow* — `compile_field_access`
  does not retain. So the caller would release a value `s` still
  owns. This is a pre-existing imperfection of the heuristic, not
  introduced here: `let x = s.arc_field` already mis-tracks the same
  way. The real fix is making ARC field (and index) reads retain a
  `+1` copy — a separate follow-up. v0.x programs pass locals and
  fresh producers, not bare ARC field reads, as arguments.

- **Method / builtin / dyn-call argument temporaries still leak.**
  Out of scope this session (see *Scope* above) — `push` consuming
  its argument is the blocker for a blanket method-call rule.

- **A call *result* used inline still leaks.** `triple().len()`
  drops the `triple()` Vec unreleased — that is a receiver
  temporary, not an argument temporary.

## What's next

- **ARC field / index reads that retain** — makes the fresh/borrowed
  heuristic exact, and unlocks safe method/dyn-call argument
  cleanup.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
