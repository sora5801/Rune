# Session 104 — Prelude `Numeric` impls for primitives

**Date:** 2026-05-25
**Outcome:** The prelude (`src/std.rn`) now ships
`impl Numeric for {i8, i16, i32, i64, isize, u8, u16,
u32, u64, usize, f32, f64}`. Users get
`<T: Numeric>`-bounded generic code working over
every primitive numeric type out of the box. 431
codegen + 167 typecheck tests green (+3 codegen from
session 103).

```rune
fn larger<T: std::Numeric>(a: T, b: T) -> T {
    if a.lt(b) { b } else { a }
}

fn main() -> i64 {
    let a: i32 = 7;
    let b: i32 = 12;
    larger(a, b) as i64        // 12 — works without writing impls
}
```

## The decisive observation

Session 084 added the `Numeric` trait. Session 087
lifted the "impl only on structs" resolver
restriction so primitives can have impls. The
remaining piece was just: **write the impls in
std.rn**. With session 087 in place, the bodies are
mechanical — each primitive's impl is `fn add(self,
other) -> Self { self + other }` and `fn lt(self,
other) -> bool { self < other }`. The native `+` and
`<` operators lower to `iadd` / `icmp` / `fadd` /
`fcmp` per type.

```rune
impl Numeric for i64 {
    fn add(self: i64, other: i64) -> i64 { self + other }
    fn lt(self: i64, other: i64) -> bool { self < other }
}
impl Numeric for i32 { ... }       // 11 more similar blocks
```

Twelve impl blocks, ~70 lines of std.rn. The
monomorphizer drops impls that aren't actually used
in the program (zero-cost generics) so binary size
isn't affected unless the user invokes `<T:
Numeric>` over the type.

### Why now and not session 084 / 087

Session 084 documented the deferral explicitly:

> v0.x doesn't yet support intrinsic impls for
> primitive numeric types — `impl Numeric for i64 {
> ... }` would require per-primitive intrinsic
> lowering at the impl-block layer, deferred.

Session 087 then did the lifting. By the milestone
retrospective (session 100), the only remaining piece
was the std.rn boilerplate. Session 104 ships it.

### Tests that needed rewriting

Three tests from session 087 defined their own `impl
Numeric for i64 { ... }` blocks (because the prelude
didn't yet have one). With session 104, those impls
collide with the prelude's, surfacing as "method
`add` already defined on `i64`" resolver errors. The
fix: remove the test's user-defined impls — the
prelude provides them now. The tests still validate
the same machinery (generic dispatch on a primitive
receiver) but without the test-local impl.

Two tests from session 096 (`method_receiver_hint_
primitive_impl`, `method_receiver_hint_chain_two_
literals`) needed deeper rewrites. Session 096's
receiver-hint logic fires when a method name uniquely
identifies one primitive-impl. With the prelude
shipping `Numeric` for every primitive, `.add` /
`.lt` are no longer unique — every numeric primitive
has them. The hint can't disambiguate.

Rewrote the two tests to use *inherent* impls on i32
(`impl i32 { fn shifted(self: i32) -> i32 { ... } }`
and `impl i32 { fn weighted(self: i32, w: i32) ->
i32 { ... } }`). These method names ARE unique to
the i32 anchor, so the hint flow still works. The
test premise — receiver-hint on uniquely-named
methods — is preserved; the prelude doesn't share
those names.

## The wire-ups

```
src/std.rn        (12 new impl blocks for Numeric
                   on every primitive numeric type.
                   Trait declaration comment updated
                   to reflect session 104's reality.)

tests/codegen.rs  (3 session-087 tests trimmed to
                   not redeclare the impl; 2
                   session-096 tests rewritten with
                   inherent-impl methods to preserve
                   the uniqueness-based receiver
                   hint test; +3 new tests for
                   prelude Numeric coverage on i32,
                   u64, and the per-primitive
                   monomorphization path.)
```

No checker / lower / mono / codegen / resolver
changes. The mechanism existed; this session just
populates the prelude.

## What's tested

Codegen (+3 new, -0 nominal but several tests
rewritten):

- `prelude_numeric_works_on_i32` — `larger<T:
  Numeric>` over i32 with the prelude impl.
- `prelude_numeric_works_on_u64` — same over u64.
- `prelude_numeric_via_generic_fn_dispatches_per_
  primitive` — `double<T: Numeric>` called over
  both i64 and i32 in the same program. Each call
  site picks the right primitive-anchor impl.

Existing tests rewritten:

- `numeric_impl_on_i64_*` — drop the duplicate impl
  blocks; rely on the prelude.
- `method_receiver_hint_primitive_impl` /
  `_chain_two_literals` — use inherent-impl methods
  (`.shifted`, `.weighted` on i32) so the
  uniqueness-based receiver-hint is still
  exercised.

## Apparent bugs that aren't / explicitly deferred

- **Float method dispatch on bare literals** —
  `3.14.add(2.0)` doesn't trigger session 096's
  receiver hint (the method name `.add` is no
  longer unique). The result type is f64 if the
  surrounding context provides it; otherwise
  defaults. Same shape as integer literals.
- **`.sum` / `.fold(0, ...)` generalization to
  Numeric** — the Iterator default-body methods
  in std.rn still hardcode `let total: i64 = 0;`
  for `.sum`. Generalizing to `let total: Self::
  Item = ???.zero();` would need a `Numeric::zero`
  associated constant or static method, which
  v0.x's trait system doesn't yet support.
- **NaN comparisons** — `f64`'s `lt` lowers to
  `fcmp.lt` which returns false for any NaN
  operand. Generic code over Numeric gets IEEE-754
  semantics for free; the trait doesn't promise
  total ordering. Document, don't fix in v0.x.
- **Method-name collision with user impls** —
  a user can now write `impl Numeric for MyType
  { ... }` AND have the prelude's `Numeric for i64`
  in scope; they don't collide because impl_methods
  is keyed by `(receiver_sym, method_name)` and
  the receiver syms differ.
- **HashMap<i64, f64>** and similar mixed-numeric
  containers still fail at the 8-byte-slot Vec
  check or HashMap-V check for floats. The
  pre-1.0 "floating-point Vec elements" item
  unblocks this.

## What's next

- **Floating-point Vec elements** — unblock
  `Vec<f64>` so iterator chains over floats work.
- **Cross-let const-eval** — propagate const values
  through let bindings.
- **Division-by-zero const-eval diagnostic**.
- **Self-hosted bootstrap** — long-term.
