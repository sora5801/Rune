# Session 096 — Method-receiver hint flow

**Date:** 2026-05-25
**Outcome:** Bare-numeric-literal receivers in method
calls adopt the right primitive type when the method
name uniquely identifies a single primitive-impl
method. `3.add(x)` with `impl Numeric for i32 { fn
add(...) }` works without `3i32`. 424 codegen tests
green (+2 from session 095).

```rune
impl std::Numeric for i32 {
    fn add(self: i32, other: i32) -> i32 { self + other }
    fn lt(self: i32, other: i32) -> bool { self < other }
}

let a: i32 = 5;
let r: i32 = 3.add(a);       // 3 hints to i32 via .add
let r2: i32 = 4.add(7);      // both literals hint
```

## The decisive observation

Sessions 091, 094, 095 propagated hints into bare
numeric literals from a typed surrounding context
(let, fn-arg, struct-field, binop). The remaining
high-friction site was method-call receivers:
`3.method()` checked `3` as i64 before the method
lookup, so any `.method` only defined on i32 / u32 /
f64 would error with "method not found on i64."

The fix is a focused intercept in `check_method_call`:
when the receiver is a bare numeric literal and the
method name appears on EXACTLY ONE primitive-impl
anchor, hint the receiver to that anchor's type.

```rust
fn maybe_hint_method_receiver(
    &mut self,
    receiver: &Expr,
    method_name: &str,
) -> Option<Ty> {
    if !receiver-is-bare-numeric-literal { return None; }
    let mut candidates: Vec<Ty> = ...;
    for ((sym, name), _) in &self.res.impl_methods {
        if name != method_name { continue; }
        if let SymbolKind::BuiltinType(ty) = &self.res.symbol(*sym).kind {
            if matches!(ty, Ty::Int(_) | Ty::Float(_)) {
                candidates.push(ty.clone());
            }
        }
    }
    if candidates.len() != 1 { return None; }
    Some(self.check_expr_with_hint(receiver, Some(&candidates[0])))
}
```

If zero or multiple primitive impls have the method,
no hint fires — the receiver stays at its bottom-up
default (i64) and the existing method-lookup chain
either finds a user-struct impl or errors with the
familiar "no method `.foo` on i64."

### Why uniqueness, not surrounding-context

A more aggressive version would peek the surrounding
expected return type (from the let / fn-arg context
wrapping the method call) and pick the primitive impl
whose return type matches. That's more powerful but
needs deeper integration with check_expr_with_hint
(the method call would need to know its caller's
expected type before resolving the receiver).

The uniqueness-based approach is enough for the
practical case: user-defined intrinsic numeric impls
(session 087) typically pick one primitive per
method, and the method's name is unique across the
program. When ambiguity arises (e.g., user defines
`.add` on both i32 and u32), the user falls back to
explicit suffix (`3i32.add(x)`).

### Suffix-bearing receivers stay pinned

`3i32.add(x)` doesn't trigger the hint flow because
the lit-with-Some(suffix) doesn't match the
"bare-numeric-literal" filter. The suffix's type
flows directly into the receiver via session 088's
lit_type contract.

### Float receivers

`Ty::Float(_)` is included in the candidate filter,
so `.add` defined on f32 hints `3.0.add(x)` (or
even `3.add(x)` for the int-zero-as-float case from
session 091) to f32.

## The wire-ups

```
src/checker.rs    (check_method_call's first line
                   now consults maybe_hint_method_
                   receiver; new helper iterates
                   impl_methods looking for unique
                   primitive-anchor methods.)

tests/codegen.rs  (+2 tests: primitive-impl method
                   on i32 with bare literal
                   receiver; double-bare-literal
                   receiver + arg.)
```

No AST / parser / lower / mono / codegen changes —
the receiver's `expr_types` entry gets updated by
`check_expr_with_hint`, and the rest of the method-
dispatch pipeline reads the corrected type.

## What's tested

Codegen (+2):

- `method_receiver_hint_primitive_impl` — `3.add(a)`
  with `a: i32` and `.add` on i32. Receiver hints
  to i32; method dispatches correctly.
- `method_receiver_hint_chain_two_literals` —
  `4.add(7)` — receiver hints to i32 via uniqueness,
  arg `7` hints to i32 via session 081's
  bidirectional method-arg flow.

## Apparent bugs that aren't / explicitly deferred

- **Multiple primitive impls with same method name**
  — `impl Numeric for i32 { fn add ... }` AND
  `impl Numeric for u32 { fn add ... }` — the hint
  filter sees two candidates and returns None.
  Receiver defaults to i64, dispatch errors with the
  familiar "no method `.add` on i64." Users add a
  suffix (`3i32.add(x)`) or annotate via let-
  binding. Future: peek surrounding expected return
  type to disambiguate.
- **User-struct impls** — the filter only looks at
  primitive anchors (BuiltinType). A user struct
  with the only `.method` definition doesn't
  produce a hint because the receiver is a numeric
  literal (which couldn't dispatch to a struct
  anyway).
- **Generic struct impls** — `impl<T> Foo<T> { fn
  method(self) }` doesn't produce a primitive
  anchor; the hint filter skips. Numeric literals
  don't dispatch to generic-struct impls.
- **Chained method calls** — `3.add(a).add(b)`. The
  outer `.add` has receiver `3.add(a): i32` (a
  call expression, not a bare literal); the filter
  doesn't fire. Works because the inner call already
  resolved the receiver via this session's hint.
- **Static method calls** — `i32::from_str("5")` —
  unrelated. This session targets the
  receiver-as-bare-literal case only.

## What's next

- **Const-eval overflow checks** — reject `100u8 +
  200u8` runtime overflow at compile time.
- **Codegen-side diagnostic polish** — friendly type
  names in codegen / aot error paths.
- **Chained binop hint propagation** — `1 + 2 + a:
  i32` parses left-associatively; the inner `1 + 2`
  defaults to i64 before `a` enters the picture.
- **Self-hosted bootstrap** — long-term.
