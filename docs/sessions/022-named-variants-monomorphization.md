# Session 022 — Named variants + monomorphization

**Date:** 2026-05-20
**Outcome:** Three features from session 021's TODO list land —
named-field enum variants, generics step 2 (monomorphization), and
partial generic struct field types. Calling `id<T>(x: T) -> T` with
concrete arg types now produces specialized i64/str/etc. versions
automatically. 359 tests green (+7 from session 021's 352).

This closes out the "all six from session 020's deep dive" arc:
parser fix + char codegen + as casts + ARC-on-copy + ARC structs
(019), struct rc + enum destructors + multi-field variants (021),
named variants + generics step 2 + generic struct fields (022).

## Three features

| # | Item | Status |
| --- | --- | --- |
| 1 | Named-field enum variants | Lands |
| 2 | Generics step 2 (monomorphization) | Lands — functions only |
| 3 | Generic struct field types | Partial — i64-sized fields work |

## 1. Named-field enum variants

```rune
enum Result { Ok { value: i64 }, Err { code: i64 } }

fn unwrap_or(r: Result, def: i64) -> i64 {
    match r {
        Result::Ok { value }    => value,
        Result::Err { code: _ } => def,
    }
}

fn main() -> i64 {
    let a = Result::Ok { value: 42 };
    let b = Result::Err { code: 7 };
    unwrap_or(a, 0) + unwrap_or(b, -1)
}
```

Construction reuses the `Expr::StructLit` AST node — the parser
doesn't need a new shape for `Variant { name: val }`. The checker's
`check_struct_lit` dispatches based on what the path resolves to:
- `SymbolKind::Struct` → existing struct-lit path.
- `SymbolKind::EnumVariant` → new `check_named_variant_lit`.

`check_named_variant_lit` validates names against the variant's
declared field names (rejects unknown, duplicate, missing). The
lowerer reorders user-provided fields into declaration order and
emits the same `HirExprKind::EnumPayloadCtor` that tuple variants
use. So `Pt { y: 4, x: 3 }` and `Pt { x: 3, y: 4 }` produce
identical HIR.

Destructure is a new AST node:
```rust
Pattern::NamedVariant {
    path: Path,
    fields: Vec<(Ident, Pattern)>,
    span: Span,
}
```

The parser recognizes both forms:
- `Variant { name: pat, ... }` — explicit per-field pattern.
- `Variant { name, ... }` — shorthand: `name` binds the field
  directly to a local of the same name.

The lowerer reorders by name → declaration order and emits
`HirPattern::EnumPayload` with the corresponding `bindings`.

Resolver additions:
- `Resolutions::enum_variant_field_names: HashMap<SymbolId, Vec<String>>`
  — declared field names per named variant.
- `enum_variant_payloads` now populated from `VariantFields::Named`
  as well as `Tuple` (previously empty for Named).

## 2. Generics step 2 — monomorphization

The big unlock. New pass in `src/monomorphize.rs` runs between the
lowerer and codegen.

### Inference

```rust
fn unify(param: &Ty, arg: &Ty, subst: &mut HashMap<SymbolId, Ty>) -> bool {
    match (param, arg) {
        (Ty::TypeVar(t), concrete) => match subst.get(t) {
            None => { subst.insert(*t, concrete.clone()); true }
            Some(prev) => prev == concrete,
        },
        (a, b) => a == b,
    }
}
```

Positional: walk the generic's param types alongside the call's
argument types. Each `TypeVar(t)` on the param side binds `t`. A
second occurrence must match. No bidirectional inference, no trait
constraints. Calls with arity mismatch produce `None`; the checker
already flagged them.

### Specialization

```rust
fn subst_fn(f: &HirFn, subst: &HashMap<SymbolId, Ty>) -> HirFn {
    HirFn {
        sym: f.sym,
        name: f.name.clone(),
        generics: f.generics.clone(),
        params: f.params.iter().map(|p| HirParam {
            sym: p.sym, name: p.name.clone(),
            ty: subst_ty(&p.ty, subst),
        }).collect(),
        ret_ty: subst_ty(&f.ret_ty, subst),
        body: subst_block(&f.body, subst),
    }
}
```

`subst_ty` / `subst_block` / `subst_expr` recursively walk the HIR
replacing `Ty::TypeVar(t)` with the bound concrete type. The body's
locals keep their original SymbolIds (they're scoped to the
function and don't clash across instantiations).

The specialized function gets:
- A fresh `SymbolId` allocated past the resolver's max sym.
- A mangled name via `mangle()`: `id$$i64`, `pair$$i64$$str`, etc.
- `generics: Vec::new()` so it's no longer "generic".
- Its body fully substituted.

### Cache + worklist

```rust
cache: HashMap<(SymbolId, Vec<Ty>), SymbolId>,
worklist: Vec<(SymbolId, Vec<Ty>)>,
```

Initial pass scans every concrete function's body, recording
`(generic_sym, inferred_args)` pairs into the worklist. The drain
loop creates each specialization, scans IT for further generic
calls (recursive instantiation), and continues until empty.

After draining, a final rewrite pass walks every concrete function
and replaces `Call.callee` with the cached specialized sym, so
codegen sees direct calls to specializations.

### Checker support

The checker had to learn to accept TypeVars. Two changes:

1. **`Ty::compatible`** treats `TypeVar` on either side as compatible
   with anything. Without this, `arg: i64` couldn't pass through a
   `param: T` slot — the checker would reject the call before the
   monomorphizer could see it.

2. **`check_call`** does light substitution on the return type. For
   `fn id<T>(x: T) -> T` called with `id(5)`, the apparent result
   type becomes `i64` instead of `TypeVar(T)`. This means downstream
   `let n: i64 = id(5);` typechecks naturally.

```rust
// Inside check_call after gathering arg_tys:
let mut subst = HashMap::new();
for (param, arg) in params.iter().zip(&arg_tys) {
    unify_typevars(param, arg, &mut subst);
}
apply_subst(&ret, &subst)
```

### What works

```rune
fn id<T>(x: T) -> T { x }
fn first<T, U>(a: T, b: U) -> T { a }

fn pair_first<T>(a: T, b: T) -> T {
    let r = id(a);     // recursive: instantiates id$$T₁
    r
}

fn main() -> i64 {
    let n = id(7);              // id$$i64
    let s = id("hello");        // id$$str (separate instantiation)
    first(99, "ignored");       // first$$i64$$str
    pair_first(10, 20);         // pair_first$$i64 + id$$i64
    n + s.len()
}
```

Each unique `(generic_sym, type_args)` combination produces one
specialized function. Re-uses across call sites share the cached
specialization.

### What doesn't (deliberate v0.x scope)

- **Generic structs/enums** aren't fully specialized — `Ty::Struct(s)`
  is just a SymbolId, not parameterized. Operations that need the
  field's concrete type (`.len()`, `+`, method calls) on a generic
  field fail at the checker. See the next section.
- **No turbofish** (`f::<T>()`). All inference is from value-arg
  types.
- **No traits / constraints.** `T: Display` doesn't parse; there's
  no way to require a method on `T`.
- **HKT** is out of scope.

## 3. Generic struct field types (partial)

```rune
struct Box<T> { value: T }

fn main() -> i64 {
    let b = Box { value: 42 };
    b.value
}
```

This works. The trick: `compile_field_access` and
`compile_field_assign` treat `Ty::TypeVar(_)` as `types::I64` for
the cranelift load/store. Since v0.x uses 8-byte-per-field padding,
the slot is always 8 bytes regardless of the actual field type.
For all i64-shaped types — i64, str pointer, Vec pointer, struct
pointer, enum descriptor pointer — this is exactly right.

For narrower types (bool, i8, i16, i32, char, f32), the load reads
garbage in the upper bytes. The checker wouldn't actually allow
those operations because the field's type is still `TypeVar`, but
the limitation is real.

### Why it isn't full

The full fix requires `Ty::Struct(SymbolId, Vec<Ty>)` — embedding
the generic args into the struct type. Then:
- `let b = Box { value: 5 };` would have `b: Ty::Struct(box, [i64])`.
- `b.value` would resolve the field type using the receiver's type
  args, producing `Ty::Int(I64)` instead of `TypeVar(T)`.
- Passing `b: Box<i64>` to `unbox<T>(b: Box<T>) -> T` would unify
  `Box(box, [i64])` against `Box(box, [TypeVar(T)])` and bind T.

That refactor touches every Ty::Struct comparison site — the
checker's type matching, the lowerer's field lookup, the codegen's
ARC tracking. Material rewrite, deferred to its own session.

## Test counts

| Suite | Before (021) | After (022) | Delta |
| --- | --- | --- | --- |
| AOT | 34 | 34 | — |
| Codegen | 160 | 167 | +7 (2 named-variant, 4 generics fn, 1 generics struct) |
| Lexer | 21 | 21 | — |
| Parser | 47 | 47 | — |
| Typecheck | 91 | 91 | — |
| **Total** | **352** | **359** | **+7** |

## File layout changes

```
src/
├── ast.rs              (Pattern::NamedVariant)
├── parser.rs           (named-variant destructure in
│                        parse_pattern_atom — `Variant { name: pat }`
│                        + shorthand `Variant { name }`)
├── resolver.rs         (Resolutions.enum_variant_field_names;
│                        Named variants now populate
│                        enum_variant_payloads + field_names;
│                        declare_pattern recurses NamedVariant)
├── checker.rs          (check_named_variant_lit dispatches in
│                        check_struct_lit when path is variant;
│                        check_named_variant_pattern;
│                        bind_pattern reorders by name;
│                        cover_pattern accepts both variant patterns;
│                        check_call substitutes TypeVar in ret;
│                        Ty::compatible accepts TypeVar both ways;
│                        unify_typevars + apply_subst helpers)
├── hir.rs              (HirFn.generics: Vec<SymbolId>)
├── lower.rs            (lower_struct_lit dispatches EnumPayloadCtor
│                        when path is variant; lower NamedVariant
│                        pattern via field-name reorder; lower_fn
│                        records generics)
├── ty.rs               (Ty derive Eq + Hash for cache keys; compat
│                        rule for TypeVar)
├── codegen.rs          (compile_field_access / compile_field_assign
│                        treat TypeVar as i64)
├── lib.rs              (mod monomorphize)
├── monomorphize.rs     (NEW — pass that walks for generic calls,
│                        infers type args, clones+substitutes generic
│                        HirFn, names with $$ separators, drains
│                        worklist, rewrites call sites)
└── main.rs             (monomorphize_module between Lowerer and
                         Codegen for both rune run and rune build)
tests/
└── codegen.rs          (+7 — 2 named variant, 4 generics fn, 1
                         generics struct field)
LANGUAGE.md             (4 new decision-log rows)
```

## Apparent bugs that aren't

- **`Ty::TypeVar` is "compatible with anything".** Coarser than Rust's
  trait-based compatibility but matches our no-traits design. Once
  traits land, this widens into proper constraint resolution.

- **Specialized functions all live in the same module — names get
  long for nested generics.** `outer$$i64$$inner$$str` is fine; it's
  a SymbolId-equivalent string. Cranelift's symbol table handles it.

- **The mangler uses `$$` (double-dollar) as a separator instead of
  a single character.** Avoids collision with user identifiers that
  contain `$` (which Rune's lexer doesn't accept) — but using a
  reserved sentinel is robust.

- **Inference is positional and one-shot.** No bidirectional matching,
  no constraint propagation, no integer-literal flow inference. If
  you call `id(5)` and `id` has T param, T = the literal's default
  i64. Fine for v0.x.

- **Generic struct field codegen widens TypeVar to i64.** Not type-
  correct in the strict sense, but the only types you'd reasonably
  put in a generic field today (i64, str, Vec, struct, enum) are
  all 8-byte-pointer-or-direct. Narrower fields will get wrong
  upper bits. Documented.

## What's next

The big v0.x gaps that remain:
- **Full generic struct types** (`Ty::Struct(SymbolId, Vec<Ty>)`)
  to unlock method calls on generic fields, generic struct passing
  through generic functions, etc.
- **Generic enums** — same parametric trick for enum values would
  let `Option<T>`, `Result<T, E>` work properly.
- **Traits** for bounded generics (`T: Display`, `T: PartialEq`).
- **`Weak<T>`** for cycle breaking (now actually buildable once
  `Option<T>` exists).
- **`?` operator** (try) once Result is generic.

With monomorphization landed, the door is open for a real stdlib.
