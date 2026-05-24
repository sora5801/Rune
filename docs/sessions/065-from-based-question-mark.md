# Session 065 — From-based `?` (via Into)

**Date:** 2026-05-24
**Outcome:** The `?` operator now auto-converts errors when the
inner result's error type doesn't match the surrounding
function's. The user implements `Into<TargetErr>` on each
source error type and the conversion lands automatically at the
`?` site:

```rune
struct IoErr { code: i64 }
struct AppErr { code: i64 }
impl std::Into<AppErr> for IoErr {
    fn into(self: IoErr) -> AppErr {
        AppErr { code: self.code + 1000 }
    }
}
fn inner() -> std::Result<i64, IoErr> {
    std::Result::Err(IoErr { code: 7 })
}
fn outer() -> std::Result<i64, AppErr> {
    let v: i64 = inner()?;        // IoErr → AppErr at the `?`
    std::Result::Ok(v + 1)
}
// outer() returns Err(AppErr { code: 1007 })
```

~3 files. 568 codegen + typecheck tests green (+3 from session
064).

## The decisive observation

Rune's standard convention is methods take `self` as the first
parameter, so `Into` (which mirrors Rust's `Into`) fits the
existing trait-method shape directly:

```rune
pub trait Into<T> {
    fn into(self: Self) -> T;
}
```

vs Rust's `From<T>` which would have an associated function
`fn from(t: T) -> Self` (no `self`) — Rune's checker doesn't
support no-`self` trait methods cleanly today. Same semantic
"convert from X to Y", inverted shape.

The `?` operator's lowering was already a match-expansion (Ok →
bind, Err → return Err); the From-based conversion is just
"wrap the err binding in a `.into()` call before reconstructing
Err." The checker records a `try_conversions: HashMap<Span,
SymbolId>` (the source err type's sym) at each `?` site that
needs conversion; the lowerer reads it and emits the call when
present.

The into method's return type is recovered from its
`fn_signatures` entry (keyed by the method's symbol span) — so
the converted payload's `Ty` is concrete, and codegen lays out
the Err variant's payload slot correctly.

## The wire-ups

```
src/std.rn           (pub trait Into<T> { fn into(self: Self) ->
                      T; } — single method, takes self, returns
                      the trait's T parameter.)

src/checker.rs       (CheckResults + Checker gain
                      `try_conversions: HashMap<Span, SymbolId>`.
                      check_try's err-type-mismatch arm now tries
                      to recover via Into: if the source err's
                      sym has an `impl_methods[(sym, "into")]`
                      entry, record the source sym at the `?`
                      span and accept. Otherwise error with
                      "implement `Into<TargetErr>` for `SourceErr`
                      to convert at the `?` site." — actionable
                      diagnostic that names exactly what to write.)

src/lower.rs         (lower_try takes the `?` span as a new arg.
                      When check_try recorded a conversion at
                      this span, the Err arm's payload becomes a
                      Call to the source's into method with the
                      err binding as the lone arg. The call's Ty
                      is pulled from the method's fn_signatures
                      entry (its ret type, which is the target
                      err type after the impl's substitution).
                      When no conversion is recorded, the payload
                      stays as the err binding directly — same
                      behavior as before session 065.)
```

## What's tested

Codegen (+2):

- `try_from_based_conversion_ok` — `outer` calls `inner_ok()?`
  where the err types differ but the result is Ok. Typecheck
  has to accept the mismatch via the Into impl; runtime returns
  Ok(43).
- `try_from_based_conversion_err` — same setup, but inner_err
  returns Err. The `?` calls `IoErr.into()` to produce an
  `AppErr { code: 1007 }`; outer() returns that wrapped in
  Err. main reads the converted code via match.

Typecheck (+1):

- `try_without_into_impl_rejected` — `?` with mismatched err
  types and no Into impl errors with "Into" in the message.

## Apparent bugs that aren't / explicitly deferred

- **Single Into impl per source type.** `impl_methods` keys
  methods by name alone — if `IoErr` impls both `Into<AppErr>`
  and `Into<DbErr>`, the lowerer picks whichever Into impl
  was registered first, with no way to disambiguate against
  the surrounding fn's err type. The checker still accepts
  the `?`, so the diagnostic doesn't catch the multi-impl
  case. Fix: key impl_methods by `(source_sym, trait_args,
  method_name)` instead of `(source_sym, method_name)`.
  Deferred.
- **No Self-type Into-from-Into derivation.** Rust auto-derives
  `Into<T>` for any `From<T>` impl. We don't have From at all
  (no-self trait methods), so this is moot — but mentioning so
  future readers don't expect the symmetry.
- **No `?` on Option.** Only `Result`. The lowerer's
  Result-shape check is hardcoded to look for `Ok` and `Err`
  variants; extending to Option would need separate handling.
  Deferred.
- **No conversion for the err's *inner generics*.** If
  `IoErr<T>` is generic and you want `Into<IoErr<U>>`, the
  impl_methods lookup would need to handle the generic args.
  v0.x errors don't commonly carry generics; deferred.
- **The conversion happens in the err arm body**, not at
  evaluation of the original expression. So if `inner()` has
  side effects that depend on Result construction order, they
  fire before the conversion — same as before, just noting it.

## What's next

- **HashMap value-release walk** — close the V-leak from
  session 064.
- **str keys for HashMap**, **HashMap remove + iteration** —
  natural extensions.
- **Open-ended ranges** (`..n`, `n..`) — still pending from
  session 063.
- **Trait default-method bodies** — would let `.collect()`
  chain off iterator adapters.
