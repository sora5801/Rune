# Session 038 — Receiver-temporary cleanup

**Date:** 2026-05-21
**Outcome:** A method-call receiver that is a fresh ARC temporary —
`triple().len()`, `s.field.len()`, `v.get(0).area()` — is now
released by the caller once the call returns. The receiver-position
mirror of session 036's owned call arguments, and the last of the
ARC-temporary leak classes the dispatch/ARC sessions flagged. ~25
lines. 456 tests green (+3 from session 037's 453).

## The leak

Sessions 036 and 037 left one temporary unreclaimed: the **receiver**
of a method call.

```rune
triple().len()        // `triple()` Vec — borrowed by `.len()`, then leaked
v.get(0).area()       // the `dyn` box `get` returns — borrowed by `.area()`
s.field.len()         // session 037 made `s.field` retain — now it leaks
```

A method borrows its receiver (`self` is never scope-tracked). When
the receiver expression is a fresh `+1` producer — a call result, a
field or index read (retained since session 037), a `dyn` box — that
`+1` is owned by nobody once the call returns. One leak per call.

## The fix

After a `MethodCall` or `DynCall`, the caller releases the receiver
when it is a fresh ARC temporary — every ARC receiver except a
borrowed `Local`:

```rust
fn release_receiver_temp(&mut self, receiver: &HirExpr, recv_val: Value) {
    if is_arc_type(&receiver.ty, ..) && !matches!(receiver.kind, Local(_)) {
        self.emit_arc_call("release", &receiver.ty, recv_val)?;
    }
}
```

This is the same `Local`-vs-not split as owned call arguments — the
receiver position instead of the argument position.

## Where the release goes

The release has to run *after* the call, and `compile_method_call`
has several arms with early returns. Rather than thread cleanup
through every arm, receiver compilation moved **out** of the two
helpers and into the `compile_expr` arms that call them:

```rust
HirExprKind::MethodCall { receiver, method, args } => {
    let recv_val = self.compile_expr(receiver)?...;
    let result = self.compile_method_call(receiver, recv_val, method, args, &e.ty)?;
    self.release_receiver_temp(receiver, recv_val)?;
    Ok(result)
}
```

`compile_method_call` / `compile_dyn_call` now take the receiver
`Value` as a parameter. However the helper returns — early `return`
included — control lands back in the arm, which does the release.

## Why it is safe

- **A method never consumes its receiver.** `self` is a borrow.
  This is unlike `Vec::push`, whose *argument* is consumed — the
  reason session 036 excluded method-call arguments. The receiver
  has no such exception, so releasing it is unconditionally sound.

- **The result is independent of the receiver.** A builtin method
  returns a scalar (`len`, `is_empty`) or a `get`-retained element
  (which carries its own `+1`, surviving the receiver's release); a
  `dyn` method returns a `+1`-owned value. Releasing the receiver
  cannot free the result.

- **It composes with session 037.** `s.field.len()` — the field
  read retains (`s.field` → `+1`), the receiver release drops it.
  Net zero, no leak — exactly the leak session 037 named.

## User methods need nothing

The monomorphizer rewrites a method call on a concrete struct or
enum into a direct `Call`, with the receiver passed as the `self`
argument. So a user/trait-method receiver is a regular `Call`
argument — already reclaimed by session 036. At codegen, a
`MethodCall` is only ever a *builtin* method (`str`, `Vec`), and
`DynCall` is trait-object dispatch; those two are what this session
covers.

## What's tested

Codegen (+3):

- `receiver_temp_released` — `triple().len()` 200×; the fresh `Vec`
  receiver is reclaimed each call.
- `receiver_temp_dyn_call` — `shapes.get(i).area()`; the retained
  `dyn` box `get` returns is the receiver of the dynamic call and is
  released after it.
- `receiver_local_not_released` — a `Local` used as a receiver four
  times stays valid; releasing it would use-after-free.

## Apparent bugs that aren't

- **Array elements still leak.** `compile_array` never establishes
  ARC ownership of its elements and `Ty::Array` is not ARC-managed —
  unchanged, and independent of receivers.

- **Enum-payload bindings are not retained.** `match e { Some(x) =>
  .. }` reads a payload out of the descriptor; whether that bind
  retains is the destructure analog of session 037, still open.

- **Builtin-call and expression-statement temporaries.** `print(a +
  b)` leaks the concatenated string (session 036 scoped argument
  cleanup to regular `Call`, not `BuiltinCall`); a fresh ARC value
  used as a discarded `expr;` statement is likewise not released.
  Both are minor remaining classes.

## What's next

- **ARC for enum-payload bindings** — the `match`-destructure analog
  of session 037.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
