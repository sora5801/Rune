# Session 024 — `Weak<T>` reference counting

**Date:** 2026-05-20
**Outcome:** Cycle-breaking weak references land. v0.x supports
`Weak<Vec>` end-to-end with downgrade, upgrade-or-default, and
proper control-block split. 368 tests green (+3 from session 023's
365). Traits and stdlib types are documented as deferred multi-
session features.

## The headline

```rune
fn main() -> i64 {
    let v = vec_new();
    v.push(42);

    let w = weak(v);              // Weak<Vec> — doesn't keep v alive
    let default = vec_new();
    default.push(-1);

    let r = upgrade_or(w, default);
    r.get(0)                      // 42 (v is still alive at this point)
}
```

When the underlying Vec's last strong reference drops, the element
array is dealloc'd immediately. The descriptor itself (with the
weak count) sticks around until the last Weak drops. `upgrade_or`
returns the default in that case:

```rune
fn get_weak() -> Weak<Vec> {
    let v = vec_new();
    v.push(99);
    weak(v)                       // v drops at function exit; w is now dead
}

fn main() -> i64 {
    let w = get_weak();
    let default = vec_new();
    default.push(7);
    let r = upgrade_or(w, default);
    r.get(0)                      // 7 — default
}
```

## The control-block split

The classic Arc/Weak design has two refcounts in the descriptor:
- `rc` — strong count.
- `weak_count` — weak count + 1 (the +1 represents the strong refs
  collectively).

```rust
#[repr(C)]
struct RuneVec {
    ptr: *mut i64,
    len: i64,
    cap: i64,
    rc: i64,           // strong
    weak_count: i64,   // weak + (1 if any strong)
}
```

The protocol:

| Event | Action |
| --- | --- |
| `vec_new` | `rc=1`, `weak_count=1` |
| Strong retain | `rc++` |
| Strong release | `rc--`; if `rc==0`, dealloc element array, then `weak_release` |
| `downgrade` | `weak_count++`, return same pointer |
| Weak retain | `weak_count++` |
| Weak release | `weak_count--`; if `weak_count==0`, dealloc descriptor |
| `upgrade` | if `rc > 0`, `rc++` and return; else return null |

Key insight: the descriptor is freed only when **both** rc and
weak_count are zero. Strong refs hold one "share" of weak_count
collectively (the initial +1 from `vec_new`), so the descriptor
stays alive as long as either a strong or a weak ref exists.

## What landed in code

```
src/codegen.rs
├── RuneVec gains weak_count
├── vec_new initializes weak_count=1
├── release_vec walks element array + chains to weak_release_vec
├── 4 new runtime helpers:
│   ├── weak_downgrade_vec (creates a Weak from a strong)
│   ├── weak_retain_vec    (ARC-on-copy for Weak locals)
│   ├── weak_release_vec   (drops a Weak, dealloc descriptor at zero)
│   └── weak_upgrade_vec   (returns null or retained-strong)
└── 1 convenience helper:
    └── weak_upgrade_or_vec (always returns a +1 strong)

src/aot.rs (RUNTIME_C)
└── matching C declarations + bodies

src/ty.rs
└── Ty::Weak(Box<Ty>) — Weak<T> as a language type

src/resolver.rs
├── `Weak` registered as a builtin sentinel type
└── `weak` and `upgrade_or` registered as polymorphic builtins

src/checker.rs
├── resolve_type special-cases the Weak sentinel: reads the path's
│   generic_args[0] to build Ty::Weak(inner)
├── compatible/apply_subst handle Ty::Weak
└── check_poly_builtin_call dispatches `weak` and `upgrade_or`,
    rejecting non-Vec inner types in v0.x

src/lower.rs
└── lower_poly_call adds `weak` and `upgrade_or` cases — both
    lower to BuiltinCall on the appropriate runtime helper

tests/codegen.rs
└── +3 tests: alive upgrade, dead upgrade falls back, 100k weak
    in a loop stays RSS-flat
```

## API choices

### Why `upgrade_or` instead of `upgrade -> Option<T>`?

A pure `upgrade(w: Weak<Vec>) -> Option<Vec>` would be more
ergonomic, but it'd require the runtime to construct an `Option`
enum value — which means either:
- Hardcoding `Option<T>` as a builtin enum the runtime knows about
  (couples the runtime to the language's stdlib).
- Making `upgrade` lower to `match upgrade_inner(w) { ptr ==> ... }`
  inline at every call site — works but invasive.

The convenience helper `upgrade_or(w, default)` sidesteps both: the
runtime takes a default value, returns one or the other, all as a
single `*mut Vec`. The cost is some lost flexibility — you can't
distinguish "alive with value X" from "dead, defaulting to X" — but
for the common case (caches, observers) it's fine.

A real `upgrade` returning `Option<Vec>` can land later once we
have a proper stdlib with `Option<T>` as a recognized type. For
v0.x, `upgrade_or` is enough.

### Why `Weak<Vec>` only, not Str / Struct / Enum?

Each ARC-managed type needs its own control-block split: a
`weak_count` field at a known offset, plus matching retain/release
helpers. For Vec the rc and weak fields fit cleanly at the end of
the descriptor. For Str, Struct, Enum the same applies but each
needs its own runtime helpers and the codegen needs to know which
helper to call per type.

v0.x ships only `Weak<Vec>` because that's the most useful case
(parent/child relationships in tree/graph structures usually use
Vec for the child collection). The pattern is mechanical to
extend; deferred for the same reason single-arity tuple variants
landed before multi: ship working scaffolding, broaden later.

## What's tested

3 new codegen tests:
- `weak_downgrade_upgrade_alive` — alive case, upgrade returns the
  underlying Vec's value
- `weak_downgrade_after_drop_returns_default` — dead case (strong
  ref dropped via function exit), upgrade returns default
- `weak_doesnt_keep_alive_in_loop` — 100k iterations, RSS flat
  (verifies both weak_count protocol and descriptor dealloc)

All 365 prior tests still pass.

## Apparent bugs that aren't

- **The `vec_new` weak_count starts at 1, not 0.** Standard
  pattern. Strong refs are conceptually one weak ref. Dropping the
  last strong calls `weak_release` once to drop that initial share.

- **`upgrade_or` returns the default WITH a fresh retain.** The
  caller's `default` local still owns its own +1, so both can be
  released at scope exit without double-free.

- **`Weak<i64>` errors at the checker.** `weak()` only supports
  ARC-managed types as input. i64 isn't ARC; the constraint is
  intentional.

- **A Weak<Vec> doesn't appear in `is_arc_type` differently from
  the strong Vec, but it dispatches to the weak helpers via
  `arc_helper_name`.** The `Ty::Weak(_)` arm is what tells the
  codegen "use weak_retain_vec, not retain_vec."

## Traits + stdlib — deferred (with design notes)

Both were on the original ask but each is too big for a single
session. The design notes live in
[LANGUAGE.md](LANGUAGE.md)'s new **Traits** and **Stdlib**
sections.

**Traits** (rough sketch):
- Parser: `trait Name { fn ...; }`, `impl T for Type { ... }`,
  `T: TraitName` at bound sites.
- Resolver/checker: `SymbolKind::Trait` + per-(trait, type) impl
  map; bounded generics gain the ability to call trait methods.
- Monomorphization: at instantiation, look up the impl for the
  concrete type; rewrite method calls to direct impl calls.
- Dynamic dispatch (vtables) deferred behind static.

**Stdlib** (waiting on traits + module system):
- Convert builtin `Vec` to user-written `Vec<T>` once `T`'s ARC
  lifecycle can be expressed via traits.
- Standard `Option<T>`, `Result<T, E>` enums in scope by default.
- Module system (`use std::Vec;`) — separate session.

## What's next

Order of operations for the next few sessions:

1. **Traits + bounded generics** — multi-session feature. Without
   this, the stdlib stays hardcoded.
2. **`?` operator** desugar — small once `Result<T, E>` is built
   in.
3. **`upgrade -> Option<T>`** to replace `upgrade_or` once
   `Option<T>` is a builtin or stdlib type.
4. **Weak<Str>, Weak<Struct>, Weak<Enum>** — extend the
   control-block split to the other ARC types.
5. **Module system** — for `use std::*`.
