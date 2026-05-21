# Session 037 — ARC field / index reads that retain

**Date:** 2026-05-21
**Outcome:** Reading an ARC value out of a struct field or an array
element now retains it. A `Field` or `Index` expression is finally a
genuine fresh-`+1` producer, which makes the codebase's
fresh/borrowed heuristic *exact* and closes a class of latent
double-frees. Two retains. 453 tests green (+3 from session 036's
450).

## The heuristic, and where it lied

Rune's ARC has one rule for telling an owned value from a borrowed
one: **a `Local` read is borrowed; everything else is a fresh
`+1`.** That rule drives `let` ARC-on-copy, owned call arguments
(session 036), struct construction, `Vec::push`, and `return`.

`compile_field_access` and `compile_index` broke it. Both *loaded*
the value and handed it back without retaining:

```rust
let val = self.builder.ins().load(cty, MemFlags::new(), recv, offset);
Ok(Some(val))
```

So `s.arc_field` and `arr[i]` returned a **borrow** — a pointer the
struct or array still owns — while every consumer, going by the
heuristic, treated the non-`Local` expression as a fresh `+1`. That
mismatch is a latent double-free everywhere an ARC field or element
is read:

- `let x = s.arc_field;` — `x` is tracked as an owner, released at
  scope exit; the struct *also* releases the field when it drops.
  The same `+1` is freed twice.
- `foo(s.arc_field)` — session 036 has the caller release the
  argument after the call; it was releasing a value `s` still owned.
- `return s.arc_field;` — the caller receives a borrow it treats as
  owned, and double-frees it.

None of these were tested — each is a deterministic crash, so a test
would have caught it. They were simply latent.

## The fix

Make the reads retain. After the load, if the value's type is ARC,
bump its refcount:

```rust
let val = self.builder.ins().load(..);
if is_arc_type(field_ty, ..) {
    self.emit_arc_call("retain", field_ty, val)?;
}
Ok(Some(val))
```

— one such guard in `compile_field_access`, one in `compile_index`.
Now a `Field` or `Index` read genuinely produces a fresh `+1`: the
read value is a new, independent owner. The heuristic's assumption
becomes true, so **every consumer is correct with no change to the
consumers** — `let`, call arguments, struct construction, `push`
all already handle "a fresh non-`Local` value" properly.

`return` needs nothing either: its ARC retain is already gated to a
`Local` operand (it retains a returned local so the value survives
the function's scope-exit releases). A `Field`/`Index` return now
carries its own `+1` straight from the read — exactly one owning
reference for the caller.

## What it costs

A field or element read used as a **method-call receiver** —
`s.field.len()`, `arr[i].method()` — now retains a temporary that
nothing releases. That is the receiver-temporary leak class, already
known and out of scope (session 036). It is a leak, not a crash;
v0.x tolerates leaks and does not tolerate double-frees, so trading
a latent double-free for a small leak is the right direction.

Arrays compound this: `compile_array` builds a stack array and never
establishes ARC ownership of its elements, and `Ty::Array` is not
ARC-managed — so an array of ARC elements leaks its elements
regardless. The `compile_index` retain does not change that; it only
makes the *read* memory-safe (no use-after-free, no double-free),
which is the bug at hand.

## What's tested

Codegen (+3):

- `field_read_retains` — `let got = h.v` co-owns the Vec with the
  field and the original binding; three owners, one free.
- `field_read_into_call` — a field read passed to a function twice;
  the read's retain and session 036's post-call release net out, so
  the first call doesn't free a Vec the field still holds.
- `index_read_retains` — `arr[1]` read into three bindings; the
  per-read retain keeps the three scope-exit releases from freeing
  the struct out from under each other.

Each crashes (double-free) without the retain and passes with it.

## Apparent bugs that aren't

- **A field/element read used as a call receiver leaks.**
  `s.field.method()` retains a temporary nothing releases — the
  receiver-temporary leak class. A follow-up (caller cleanup for
  receivers, the receiver-side analog of session 036) closes it.

- **Array elements leak.** `compile_array` stores elements into a
  stack slot without retaining, and arrays are not ARC-managed.
  Independent of this session — array ARC ownership is its own
  rough edge.

- **Enum-payload bindings aren't covered.** `match e { Some(x) => x }`
  binds a payload that is read out of the enum descriptor; whether
  that read retains is the enum analog of this session's fix, and
  is not addressed here — "field/index reads" was the scope.

## What's next

- **Receiver-temporary cleanup** — release the temporary in
  `expr.method()` when `expr` is a fresh ARC producer.
- **ARC for enum-payload bindings** — the `match`-destructure analog.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
