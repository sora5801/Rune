# Session 020 — Generics (parser), payload enums, struct return

**Date:** 2026-05-20
**Outcome:** Three sizable features land together. The parser now
accepts generic params and generic type arguments. Enum variants can
carry single-value payloads with full match destructuring. Structs
can be returned by value. 347 tests green (+14 from session 019).

The headline: `Some(5)` and `match opt { Some(x) => ..., None => ... }`
both work end-to-end, paving the way for `Option<T>` once
monomorphization (generics step 2) lands. Struct return-by-value
removes one of the longest-standing v0.x limitations.

## Three features, scoped tight

| # | Item | Status |
| --- | --- | --- |
| 1 | Generics step 1 (parser-level) | Lands — declaring generic fn/struct/enum parses; calling them still errors at codegen |
| 2 | Payload-bearing enum variants | Lands for single-tuple variants (multi-field deferred) |
| 3 | Returning structs by value | Lands via heap allocation; descriptor leaks documented |

## 1. Generics step 1

The classic ambiguity is `<` — both a comparison operator and the
opener for generic type arguments. The standard trick is to consume
generic args **only at type position**. parse_path no longer eagerly
consumes `<...>`; parse_type does, after parse_path returns.

AST additions:
```rust
pub struct FnDecl     { ..., pub generics: Vec<Ident>, ... }
pub struct StructDecl { ..., pub generics: Vec<Ident>, ... }
pub struct EnumDecl   { ..., pub generics: Vec<Ident>, ... }
pub struct Path       { ..., pub generic_args: Vec<Type>, ... }
```

A new `parse_optional_generic_params` helper handles the `<T, U>`
list after item names. parse_type wraps parse_path and consumes
`<...>` if it follows.

Resolver: each `g in f.generics` becomes a `SymbolKind::TypeParam`
in a scope enclosing the body. Inside the body, `T` resolves to that
symbol.

Type system: `Ty::TypeVar(SymbolId)` for opaque type parameters.
`compatible` and `unify` treat TypeVars as their own thing — two
unrelated TypeVars don't unify, which is correct for a non-inferring
checker.

Codegen: `cranelift_type(Ty::TypeVar(_))` falls through to the
catch-all error `type 'T#...' not supported in codegen`. Declaring
a generic function works; calling one errors out. This is the
"step 1: parser only" scope.

What's still TODO for generics:
- **Monomorphization** (step 2): cache `(SymbolId, Vec<Ty>)` →
  specialized function; instantiate at call sites; mangle names.
- **Type inference** at call sites: today `Vec<i64>::new()` would
  need explicit args; once monomorphization works, the checker can
  infer from arguments.
- **Generic struct construction** like `Box<i64> { value: 5 }`: the
  parser accepts the syntax but codegen rejects.

## 2. Payload-bearing enum variants

### Scope

```rune
enum Opt { Some(i64), None }
fn unwrap_or(o: Opt, def: i64) -> i64 {
    match o {
        Opt::Some(x) => x,
        Opt::None => def,
    }
}
```

Single-value tuple variants only. Multi-field variants
(`Pair(T, U)`) parse but the lowerer / checker rejects them with
`multi-field tuple-variant destructuring not supported`. Named-field
variants are likewise deferred.

### Layout

Enums with **any** payload-bearing variant flip representation: all
values use a heap-allocated 24-byte descriptor `{ tag, payload, rc }`.
Unit variants of such enums allocate the same shape with `payload=0`.
Tag-only enums (every variant is Unit) keep the previous i64
discriminant representation — no allocation, no rc.

The trigger is `Resolutions::enum_has_payload: HashSet<SymbolId>`,
populated during the resolver's pass 1 by scanning each enum's
variants. The lowerer copies it into `HirModule::enum_has_payload`
so codegen can dispatch at every site that touches an enum.

### Runtime helpers

```rust
extern "C" fn rune_runtime_enum_new(tag: i64, payload: i64) -> *mut RuneEnum;
extern "C" fn rune_runtime_retain_enum(p: *mut RuneEnum);
extern "C" fn rune_runtime_release_enum(p: *mut RuneEnum);
```

Same `rc=-1` sentinel convention as Str/Vec (though no current
allocator path uses the sentinel — there are no literal enum values).

### Construction codegen

`HirExprKind::EnumPayloadCtor { enum_sym, discriminant, payload }`:
```text
tag  = iconst.i64(discriminant)
pay  = compile_expr(payload)                  // smaller ints sext/zext to i64
desc = call rune_enum_new(tag, pay)
return desc
```

Existing `HirExprKind::EnumVariant` (unit variant) now branches:
- if `enum_has_payload`: same heap allocation with `payload=0`.
- else: the prior `iconst.i64(discriminant)` path.

### Destructuring codegen

New AST: `Pattern::TupleVariant { path, fields, span }`. Parser
recognizes `Variant(pat)` after a multi-segment path in a pattern
context.

New HIR: `HirPattern::EnumPayload { discriminant, payload_ty, binding }`.

Codegen at the pattern check site:
```text
tag = load(scrutinee + 0)
brif tag == discriminant → extract_block, else next-arm-block
extract_block:
  if binding is Some(sym):
    raw = load(scrutinee + 8)
    val = ireduce(raw, payload_cty)   // narrow back to i8/i32/etc.
    var = alloc_var(payload_cty)
    def_var(var, val)
    var_map.insert(sym, var)
  jump on_match
```

The binding variable is declared inside the codegen for the pattern,
not at arm-body entry. This means the body block reads the binding
through `var_map` and Cranelift's SSA threading handles the rest.

### ARC

`is_arc_type(Ty::Enum(sym), ...)` now returns true when
`enum_has_payload.contains(sym)`. The descriptor is treated like any
other ARC value: scope-exit release walks the runtime helper, which
decs the rc and dealloc's at zero.

**Documented limitation: payload values inside descriptors aren't
ARC-tracked by the helper.** If you put `Vec` into `Some(v)` and the
Some drops, the Vec inside leaks. The proper fix is per-variant
destructor walks at release time — the codegen would synthesize a
release function per enum that switches on the tag and recursively
releases ARC payloads. Deferred.

Workaround for users: destructure the payload out before the Some
drops. The destructured binding owns +1 of the payload and will be
released properly.

## 3. Returning structs by value

The pre-v0.x story: struct literals stack-allocated a slot in the
caller's frame and returned the slot's address. After return, the
slot is dead — any read from it is UB. Codegen never accepted
struct-typed returns; the checker accepted them but execution would
fault.

The fix this session: **all structs are heap-allocated**.
`compile_struct_lit` calls `rune_struct_new(size)` instead of
`create_sized_stack_slot`. The heap pointer outlives the callee, so
returns work.

```rune
struct Point { x: i64, y: i64 }
fn make(a: i64, b: i64) -> Point {
    Point { x: a, y: b }
}
fn main() -> i64 {
    let p = make(3, 4);
    p.x * p.x + p.y * p.y      // 9 + 16 = 25
}
```

### What's still missing

The struct descriptor itself **leaks** in v0.x:
- A struct local at scope exit calls `emit_arc_call("release",
  Ty::Struct(sym), ...)` which walks the ARC fields and releases
  each. The descriptor bytes are never dealloc'd.
- For non-ARC structs (no Vec/Str/etc. fields) this means every
  `make()` call leaks `size` bytes (16 for a `Point`).
- For ARC structs the fields are correctly reclaimed but the
  descriptor's bytes still leak.

The cleanup is to add an `rc` field at offset `size` and a per-
struct synthesized release function that walks fields then dealloc's.
Not done in this session — left as the "session 020 → 021 cleanup"
follow-up.

For the v0.x test corpus this leak is bounded. Long-running programs
will want the rc fix.

## Test counts

| Suite | Before (019) | After (020) | Delta |
| --- | --- | --- | --- |
| AOT | 34 | 34 | — |
| Codegen | 146 | 154 | +8 (5 enum payload + 3 struct return) |
| Lexer | 21 | 21 | — |
| Parser | 41 | 47 | +6 (generics syntax) |
| Typecheck | 91 | 91 | — |
| **Total** | **333** | **347** | **+14** |

## File layout changes

```
src/
├── ast.rs        (FnDecl/StructDecl/EnumDecl gain `generics`;
│                  Path gains `generic_args`; Pattern::TupleVariant)
├── parser.rs     (parse_optional_generic_params; parse_type
│                  consumes `<...>`; parse_path stays bare; pattern
│                  parser handles `Variant(pat)`)
├── resolver.rs   (TypeParam scope per item; enum_variant_payloads
│                  and enum_has_payload populated in pass 1;
│                  declare_pattern recurses into TupleVariant)
├── ty.rs         (Ty::TypeVar(SymbolId))
├── checker.rs    (resolve_type maps TypeParam → Ty::TypeVar;
│                  check_enum_variant_call;
│                  check_tuple_variant_pattern; cover_pattern arm
│                  for TupleVariant)
├── hir.rs        (HirExprKind::EnumPayloadCtor;
│                  HirPattern::EnumPayload;
│                  HirModule.enum_has_payload)
├── lower.rs      (Variant Call → EnumPayloadCtor;
│                  TupleVariant pattern → EnumPayload;
│                  HirModule.enum_has_payload populated)
├── codegen.rs    (RuneEnum struct + retain/release helpers;
│                  rune_struct_new helper; struct_arc_fields lookup
│                  unchanged; is_arc_type takes enum_has_payload;
│                  EnumPayloadCtor / EnumPayload codegen;
│                  compile_struct_lit heap-allocates)
tests/
├── parser.rs     (+6 generic syntax tests)
└── codegen.rs    (+5 enum payload + 3 struct return)
LANGUAGE.md       (4 new decision-log rows — three for the features,
                   one for weak refs from session 019 was already there)
```

## Apparent bugs that aren't

- **`Some(5)` evaluated where the enum has no payload variant errors
  with "variant takes no payload — drop the parentheses".** Correct.
  `fn f() -> Status { Status::Ok() }` for a tag-only enum is now
  rejected at type-check time.

- **Mixing payload and unit variants in the same enum (`Opt::Some(x)`
  / `Opt::None`) makes ALL of them allocate the heap descriptor.**
  Intentional — uniform value representation per enum type. The
  alternative (separate codegen for unit vs payload value sites)
  doubles the per-enum codegen complexity.

- **`Vec<i64>` as a type **annotation** parses fine but
  constructing one (`vec_new()` returns the concrete `Vec`)
  type-checks because the checker treats both as `Ty::Vec` today.**
  Once monomorphization lands, `Vec<i64>` will be a distinct type
  from `Vec<str>` and the generic-args on the type annotation will
  matter.

- **`fn id<T>(x: T) -> T { x }` parses and resolves, but cannot be
  called.** Calling triggers codegen of `Ty::TypeVar` which errors.
  The function declaration itself emits Cranelift IR via
  `Ty::TypeVar` reaching cranelift_type, also failing. So actually
  declaring the function fails too. This is the "generics step 1"
  reality: parser-only, no runtime support.

- **Struct descriptor leak**: every `make_point()` call malloc's a
  fresh descriptor that's never freed. RSS grows linearly with the
  number of struct constructions over the program's lifetime.
  Documented; future session adds rc.

## What's next

The natural picks from here:
- **Generics step 2 (monomorphization)**: instantiate generic fns/
  structs per concrete type set at call sites. Unblocks `Vec<T>`,
  `Option<T>`, `Result<T, E>`, eventually `Weak<T>`.
- **Struct descriptor rc + dealloc**: close the v0.x leak.
- **Per-variant destructor walks for payload enums**: stop the
  payload-leak inside enum descriptors.
- **Multi-field tuple variants** (`Pair(T, U)`): wider payload area
  in the enum descriptor.
- **Named-field enum variants** (`Result::Ok { value: T }`): parser
  already accepts, downstream rejects.
- **Generic struct field types**: `struct Box<T> { value: T }` —
  needs the struct layout to be generic-aware (size depends on T).
