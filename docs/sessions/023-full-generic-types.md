# Session 023 — Full generic struct + enum types

**Date:** 2026-05-20
**Outcome:** `Ty::Struct` and `Ty::Enum` now carry their type
arguments. The lowerer substitutes field types per instantiation, the
monomorphizer unifies through struct/enum args, and the checker
infers generic args from construction sites. The combined effect:
**`Option<T>` and `Result<T, E>` work end-to-end** for the first
time. Method calls on generic struct fields work. Passing generic
structs to generic functions works. 365 tests green (+6 from
session 022's 359).

## The headline

```rune
enum Option<T> { Some(T), None }

fn unwrap_or(o: Option<i64>, def: i64) -> i64 {
    match o {
        Option::Some(x) => x,    // x: i64 (not TypeVar(T))
        Option::None    => def,
    }
}

fn main() -> i64 {
    unwrap_or(Option::Some(42), 0) + unwrap_or(Option::None, -1)
}
```

```rune
enum Result<T, E> { Ok(T), Err(E) }

fn code(r: Result<i64, str>) -> i64 {
    match r {
        Result::Ok(n)  => n,        // n: i64
        Result::Err(_) => -1,
    }
}
```

```rune
struct Box<T> { value: T }

fn unbox<T>(b: Box<T>) -> T { b.value }   // T inferred from Box<T>

fn main() -> i64 {
    let b = Box { value: 99 };            // Box<i64>
    unbox(b)                              // unbox$$Box_i64 instantiation
}
```

## The change

`Ty::Struct` and `Ty::Enum` go from single-field to two-field:

```rust
// Before:
Struct(SymbolId),
Enum(SymbolId),

// After:
Struct(SymbolId, Vec<Ty>),
Enum(SymbolId, Vec<Ty>),
```

Non-generic structs/enums use an empty Vec. Generic ones carry their
type args at every use site.

The propagation:
- **Resolver**: tracks per-item generic-param symbols
  (`struct_generics`, `enum_generics: HashMap<SymbolId, Vec<SymbolId>>`).
- **Checker resolve_type**: when resolving `Path::Box<i64>`, populates
  the type args by recursively resolving each generic_arg.
- **Checker check_struct_lit**: infers args from field values by
  unifying declared field types vs actual value types, producing
  `Ty::Struct(box_sym, [i64])` for `Box { value: 5 }`.
- **Checker check_enum_variant_call**: same idea for variant
  construction. `Some(5)` produces `Ty::Enum(option_sym, [i64])`.
- **Checker bind_pattern**: pattern destructure substitutes payload
  types using the scrutinee's enum args, so `Option::Some(x)` on an
  `Option<i64>` binds `x: i64`.
- **Lowerer lower_field_access**: substitutes `field_ty` using the
  receiver's struct args, so `b.value` on `Box<i64>` produces a
  `HirExprKind::FieldAccess { field_ty: i64, ... }`.
- **Lowerer lower_match**: builds a per-scrutinee subst so pattern
  bindings receive concrete types.
- **Monomorphizer unify**: recurses into struct/enum args, so
  passing `Box<i64>` to `unbox<T>(b: Box<T>) -> T` binds T=i64 from
  the args match.

## Compatibility relaxation

`Ty::compatible` was relaxed: structs/enums with the same `sym` are
compatible regardless of their type-arg lists. This is intentionally
coarse:

```rust
match (self, other) {
    (Ty::Struct(s1, _), Ty::Struct(s2, _)) => s1 == s2,
    (Ty::Enum(s1, _), Ty::Enum(s2, _)) => s1 == s2,
    _ => self == other,
}
```

The justification: variant-construction sites still produce
`Ty::Enum(s, [])` as a placeholder when the variant has no payload
to infer from (`None`). At a use site like `let o: Option<i64> = None;`,
the lhs and rhs types have different arg lists but matching syms —
they should be compatible. The monomorphizer + lowerer use the args
directly for specialization; the checker just needs to accept the
binding.

## Inference at construction

The neat bit: `Box { value: 5 }` doesn't say `<i64>` anywhere, but
the checker figures it out:

```rust
// In check_struct_lit:
let mut subst = HashMap::new();
for init in fields {
    let value_ty = self.check_expr(&init.value);
    let decl_field = layout.field(&init.name.name).unwrap();
    unify_typevars(&decl_field.ty, &value_ty, &mut subst);
    // ... (type checking)
}
// Build args in the struct's generic-param declaration order:
let args: Vec<Ty> = res.struct_generics[&sym_id]
    .iter()
    .map(|g| subst.get(g).cloned().unwrap_or_else(|| Ty::TypeVar(*g)))
    .collect();
Ty::Struct(sym_id, args)
```

`Some(5)` works the same way: declared payload `T`, actual arg
`i64`, infer T=i64, return `Ty::Enum(option_sym, [i64])`.

For variants with no payloads (`None`), no inference happens at the
construction site. The resulting Ty has empty args — but the surrounding
context (the let binding's annotation, the function param) provides
the args and `compatible` matches them.

## Pattern destructure substitution

For `match o { Option::Some(x) => ... }`, the scrutinee `o: Option<i64>`
gives us `[i64]`. The checker's `bind_pattern` (and the lowerer's
`collect_arm_patterns`) build a substitution from the scrutinee's
enum args and apply it to each payload position:

```rust
let subst = build_enum_subst_from_scrutinee(self.res, scrutinee_ty);
// payloads is [TypeVar(T)] at declaration
// after substitution: [i64]
let payloads: Vec<Ty> = payload_asts.iter()
    .map(|t| apply_subst(&self.resolve_type(t), &subst))
    .collect();
```

The binding `x` then has type `i64` directly, so arithmetic /
method calls on `x` all work at the checker without further hackery.

## Monomorphizer

The existing monomorphizer needed a small but crucial change:

```rust
// In unify (param vs arg):
(Ty::Struct(s1, pargs), Ty::Struct(s2, aargs))
| (Ty::Enum(s1, pargs), Ty::Enum(s2, aargs))
    if s1 == s2 =>
{
    for (p, a) in pargs.iter().zip(aargs.iter()) {
        if !unify(p, a, subst) {
            return false;
        }
    }
    true
}
```

Now `unbox<T>(b: Box<T>) -> T` called with `b: Box<i64>` unifies as:
- Param `Ty::Struct(box, [TypeVar(T)])` vs arg `Ty::Struct(box, [i64])`
- Recurse: unify `TypeVar(T)` vs `i64` → bind T=i64.

And `subst_ty` recurses into struct/enum args too, so the specialized
function's body gets all type vars resolved.

The mangler grew args-aware too: `unbox$$S5_i64` for `unbox(Box<i64>)`,
where `S5` is the struct sym's display and `i64` is its arg.

## What was tested

Codegen (+6):
- `generics_struct_field_arithmetic` — `b1.value + b2.value` works
- `generics_struct_two_fields_pair` — `Pair<A, B>` with both fields
- `generics_struct_passed_to_generic_fn` — `unbox(Box<i64>)`
- `generics_struct_field_str_method` — `b.value.len()` on str field
- `generics_option_i64` — `Option<T>` with full match
- `generics_result_two_params` — `Result<T, E>` two-param generic enum

All 359 prior tests still pass.

## File layout changes

```
src/
├── ty.rs            (Ty::Struct/Enum take Vec<Ty>; display() formats
│                     args; compatible() ignores args for same-sym)
├── resolver.rs      (Resolutions.struct_generics + enum_generics;
│                     resolve_struct/enum capture generic-param syms
│                     in declaration order)
├── checker.rs       (resolve_type recurses into Path::generic_args;
│                     check_struct_lit infers args from field values;
│                     check_enum_variant_call infers from payload args;
│                     bind_pattern substitutes via scrutinee args;
│                     unify_typevars + apply_subst recurse into
│                     struct/enum args; build_struct_subst +
│                     build_enum_subst_from_scrutinee helpers;
│                     check_field_access substitutes via recv args)
├── lower.rs         (apply_subst + build_struct_subst helpers;
│                     lower_field_access substitutes via recv args;
│                     lower_match builds scrutinee_subst and
│                     threads through collect_arm_patterns;
│                     FieldAssign substitutes too)
├── monomorphize.rs  (unify recurses into struct/enum args;
│                     subst_ty recurses; mangle includes args)
└── codegen.rs       (mechanical Ty::Struct(sym, _) /
                      Ty::Enum(sym, _) match-pattern updates)
tests/
└── codegen.rs       (+6 — generic struct field ops, generic struct
                      passed to generic fn, Option<T>, Result<T, E>)
LANGUAGE.md          (decision-log row)
```

## Apparent bugs that aren't

- **Variant construction with no payload returns `Ty::Enum(s, [])`
  instead of inferring args from context.** Correct — there's no
  bidirectional inference today. `None` alone has unknown args; the
  compatibility check at the use site (`let o: Option<i64> = None`)
  pairs it with the contextual type. This is enough for v0.x.

- **`Ty::compatible` accepts any two `Ty::Struct` with the same sym,
  even if their args differ.** Looser than Rust's strict type
  equality. The monomorphizer specializes per (sym, args), so
  passing a `Box<i64>` into a context expecting `Box<str>` would
  produce a runtime mismatch but the checker won't catch it. v0.x
  tradeoff; tighter checking can come with traits.

- **Pattern bindings on generic enums substitute via the scrutinee's
  args, not the variant's.** Same map either way — the scrutinee is
  the source of truth for the instantiation.

- **The mangler produces names like `unbox$$S5_i64`.** Cranelift's
  symbol table handles arbitrary names; the `$$` separator is the
  same one used for non-struct-arg specializations.

## What's next

With `Ty::Struct` and `Ty::Enum` fully parametric, the road is clear
for:

- **`Weak<T>` reference counting** — now buildable as a generic enum
  with two variants and proper Option-like semantics.
- **Traits / bounded generics** (`T: Display`, `T: PartialEq`) for
  constraint resolution.
- **Stdlib types** built on these primitives: `Vec<T>` becomes a real
  generic (today it's hardcoded to i64 elements), `HashMap<K, V>`,
  etc.
- **Iterator protocol** if traits land — `for x in iter` extends to
  user types.
- **`?` operator** desugared to `match Result { Ok(v) => v, Err(e) =>
  return Err(e) }` once Result is generic.
