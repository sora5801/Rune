# Session 084 — `Numeric` trait + polymorphic `.min` / `.max`

**Date:** 2026-05-24
**Outcome:** `.min()` and `.max()` return `Option<Self::
Item>` instead of hardcoded `Option<i64>`. The new
`std::Numeric` trait gives user types a path into
generic numeric code (`<T: Numeric>` bounds with
`.add` / `.lt`). Intrinsic impls for primitive types
still deferred — that needs `impl on primitives` which
the resolver currently rejects. 390 codegen tests green
(+4 from session 083).

```rune
// .min / .max now Self::Item-typed.
v.iter().min()                                    // Option<i64> still, because Self::Item = i64
v.iter().max()

// Numeric trait, usable by user structs.
pub trait Numeric {
    fn add(self: Self, other: Self) -> Self;
    fn lt(self: Self, other: Self) -> bool;
}

struct Money { cents: i64 }
impl std::Numeric for Money {
    fn add(self: Money, other: Money) -> Money {
        Money { cents: self.cents + other.cents }
    }
    fn lt(self: Money, other: Money) -> bool {
        self.cents < other.cents
    }
}

fn smaller<T: std::Numeric>(a: T, b: T) -> T {
    if a.lt(b) { a } else { b }
}
```

## The decisive observation

Two pieces; the harder piece (intrinsic primitive impls)
stays deferred until a separate session.

### 1. `.min` / `.max` go polymorphic

Bodies previously typed `best: Option<i64>` and
`Option::Some(x)` where `x: Self::Item`. At spec time
when `Self::Item = i64`, this works. When `Self::Item`
is anything else, the `Option::Some(x)` assignment to
an `Option<i64>` slot fails.

Switch the explicit `Option<i64>` annotations to
`Option<Self::Item>`. The body's `<` / `>` comparisons
already flow through session 079's
`Ty::Assoc`-opaque pass-through, so type-check accepts
the projection. At spec time, mono pins Self::Item to a
concrete type and codegen emits the right `icmp` /
`fcmp` instructions.

For i64-iterators (the only practical numeric iterator
today — `Vec<i64>` and `RangeIter`), the return type
remains `Option<i64>` in practice. Existing tests still
pass because `Self::Item = i64` flows through.

### 2. `std::Numeric` trait for user types

```rune
pub trait Numeric {
    fn add(self: Self, other: Self) -> Self;
    fn lt(self: Self, other: Self) -> bool;
}
```

Two methods: addition + less-than ordering. Enough for
the typical "scalar-shaped" use case. User structs
implementing Numeric can be passed through
`<T: Numeric>` bounded generic fns — calls like
`a.add(b)` and `a.lt(b)` dispatch through the bound via
session 050-054's trait-bound method machinery.

### 3. Intrinsic primitive impls — still deferred

`impl std::Numeric for i64 { ... }` errors with
"`i64` is not a struct; `impl` can only be applied to
structs (for now)". Lifting that restriction
(per-primitive intrinsic impls that lower the trait
methods to native ops) is a larger session — would
need:

- Parser/AST: accept primitive type paths as impl-block
  targets.
- Resolver: a parallel `impl_methods_for_primitive` map
  keyed by `Ty` (not `SymbolId`).
- Codegen: lower the impl-method body to the native op
  (e.g., `Numeric::add` on i64 → `iadd`).
- Monomorphize: dispatch `<T: Numeric>` calls where T
  pins to a primitive through the parallel map.

Deferred until a focused session — same complexity
class as the runtime's structKey-Hash+Eq deferred from
session 069.

Practical consequence: `<T: Numeric>` works for user
struct types today; for i64 / i32 / f64 etc., users
fall back to direct ops (`a + b`, `a < b`) or wrap the
primitive in a struct.

## The wire-ups

```
src/std.rn        (.min / .max bodies use
                   Option<Self::Item> instead of
                   Option<i64>. New Numeric trait
                   with `add` and `lt` methods.)

tests/codegen.rs  (+4 tests: numeric_trait_user_struct,
                   numeric_trait_combined_add_and_lt,
                   iterator_min_polymorphic_return_
                   type, iterator_max_polymorphic_
                   return_type.)
```

No changes to checker, lower, monomorphize, or codegen.
The polymorphism flows through existing infrastructure
(session 079's opaque pass-through, session 050's
trait-bound method dispatch).

## What's tested

Codegen (+4):

- `numeric_trait_user_struct` — `struct Money` impls
  Numeric; a `<T: Numeric>` generic fn dispatches `.lt`
  through the bound and returns the smaller Money.
- `numeric_trait_combined_add_and_lt` — `<T: Numeric>`
  fn calls `.add` to combine two Money values.
- `iterator_min_polymorphic_return_type` — `.min()` on
  Vec<i64>.iter() still returns `Some(smallest)`; the
  return type is now `Option<Self::Item>` but resolves
  to `Option<i64>` at spec time.
- `iterator_max_polymorphic_return_type` — same for
  .max.

## Apparent bugs that aren't / explicitly deferred

- **No intrinsic Numeric impls for primitives**.
  `impl Numeric for i64` requires lifting the
  "impl can only be applied to structs" resolver
  restriction. Deferred — needs parser/AST/resolver/
  codegen/mono changes coordinated. Users who want
  numeric polymorphism today wrap primitives in
  structs.
- **`.sum` still i64-only**. Generalizing needs an
  additive-identity ("zero") which v0.x traits can't
  express (no trait const fns or `T::zero()` static
  method). Could be worked around with `.fold(init, +)`
  where the user supplies the zero.
- **`.min` / `.max` over `f64` Self::Item** — would
  work structurally (the body's `<` resolves to
  `fcmp` at spec) but Vec doesn't support f64 elements
  (8-byte-slot constraint allows f64 in theory; v0.x
  parser/codegen rejects). Separate issue.
- **NaN policy for future float iterators** — Rust's
  `min` skips NaN via `partial_cmp` returning `None`;
  Rune's `.min` would surface NaN-vs-other as
  comparison-fails-silently (fcmp's NaN handling
  follows IEEE-754). Pin a policy when floats land.
- **Numeric supertrait for trait-method dispatch on
  generic Self** — a method on `dyn Numeric` would
  need a vtable + boxing; same as session 052's
  dyn-with-default machinery, but no immediate use
  case (most numeric code is generic, not dyn).

## What's next

- **Intrinsic Numeric impls for primitives** — the
  "real" version of this session's deferred work.
- **For-loop tuple patterns** — `for (k, v) in
  m.entries()` directly.
- **Method-call-position `Into` inference**.
- **Cartesian-product exhaustiveness for tuple
  patterns**.
- **Self-hosted bootstrap** — long-term.
