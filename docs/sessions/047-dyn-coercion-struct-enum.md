# Session 047 — `dyn` coercion at struct fields and enum payloads

**Date:** 2026-05-21
**Outcome:** A concrete struct now coerces to `dyn Trait` at two
more sites — a struct-literal field initializer and an enum-variant
payload. With these, every assignment position coerces. Two checker
lines and one `lower.rs` line. 483 tests green (+4 from 479).

## The two remaining sites

`dyn Trait` coercion — concrete struct → `dyn Trait` — is recorded
by the checker's `check_assignable` and applied by the lowerer,
which wraps the coerced expression in a `DynBox`. Sessions 033 and
035 wired it at `let` bindings, call arguments, `return`, and
method-call arguments. Two assignment positions still used a bare
`compatible` check, so a concrete struct could not coerce there:

```rune
struct Holder { shape: dyn Shape }
enum Maybe { Has(dyn Shape), Empty }

Holder { shape: Circle { r: 2 } }   // struct-literal field
Maybe::Has(Circle { r: 3 })         // enum-variant payload
```

## The fix

`check_struct_lit` and `check_enum_variant_call` now check the
value against the field / payload type with `check_assignable`
instead of `compatible` — the same one-line swap session 035 made
for method arguments. `check_assignable` records the coercion keyed
by the value's span; the lowerer already applies `dyn_coercions` to
every expression, so it wraps these with no lowerer change. Codegen
needs nothing — a `dyn` box is an 8-byte pointer, exactly a struct
field slot or an enum payload slot.

## One ARC wire-up

A struct's `dyn` field must be released when the struct drops, or
the box leaks. `field_ty_is_arc` (`lower.rs`) now selects `Ty::Dyn`,
so `define_struct_release` walks the field through
`emit_release_field`'s `dyn` arm (session 034). Enum `dyn` payloads
needed nothing — `define_enum_release` selects payloads by
`is_arc_type`, true for `dyn` since session 034.

This closes the `dyn` coercion-site story: every assignment
position — `let`, call argument, `return`, method argument, struct
field, enum payload — now coerces.

## What's tested

Codegen (+2): `dyn_struct_field`, `dyn_enum_payload` — each loops
200× so the per-iteration release walk shows a leak or double free.

Typecheck (+2): `dyn_coercion_at_struct_field_and_enum_payload`
(positive), `dyn_field_non_implementor_rejected` (a struct that
does not implement the trait is rejected at a `dyn` field).

## Apparent bugs that aren't

- **No dangle concern.** Unlike an array field (session 042), a
  `dyn` value carried in a struct or enum does not dangle on escape
  — a `dyn` box is already a heap pointer.

- **`Weak` struct fields are still not walked.** `field_ty_is_arc`
  now covers `Vec` / `Str` / struct / array / `dyn`, but not
  `Weak` — pre-existing, and `Weak` is niche.

## What's next

- **Supertraits, associated types, generic `impl`s.**
- **A `collections` module** — `HashMap<K, V>`, an iterator
  protocol.
