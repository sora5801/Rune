# Session 021 — Close ARC leaks + multi-field tuple variants

**Date:** 2026-05-20
**Outcome:** Three of the six items from session 020's TODO list
land — struct descriptor rc + dealloc, per-variant destructor walks
for payload enums, and multi-field tuple variants. The other three
(named-field enum variants, generics step 2 monomorphization,
generic struct field types) are deferred to their own sessions
since each is substantial on its own. 352 tests green (+5 from
session 020's 347).

The net effect: **both v0.x descriptor leaks from session 020 are
gone**. Structs and payload enums now release their heap memory
properly. Multi-field variants (`Pair(T, U)`, `Triple(A, B, C)`)
work end-to-end.

## Three features

| # | Item | Status |
| --- | --- | --- |
| 1 | Struct descriptor rc + dealloc | Lands — all structs now ARC |
| 2 | Per-variant destructor walks for payload enums | Lands |
| 3 | Multi-field tuple variants | Lands |
| 4 | Named-field enum variants | Deferred |
| 5 | Generics step 2 (monomorphization) | Deferred |
| 6 | Generic struct field types | Deferred (blocked on step 2) |

## 1. Struct descriptor rc + dealloc

The v0.x leak: session 020 heap-allocated struct descriptors so they
could escape the callee's frame, but never freed them. ARC fields
still got released, but the descriptor itself was process-leaked.

Fix:
- Every user-defined struct now carries an `rc: i64` at offset `size`
  (the field-area size). `rune_struct_new(size)` mallocs `size + 8`
  and inits rc=1. A companion `rune_struct_dealloc(ptr, size)` frees
  the block.
- `is_arc_type(Ty::Struct(_))` returns true unconditionally. Every
  struct binding becomes an `arc_local` and gets released at scope
  exit.
- Per-struct release functions are **synthesized** at module compile
  time (one Cranelift Function per struct, named
  `__rune_release_struct$<sym_id>`). The body:
  ```
  rc = load(ptr + size)
  if rc == -1: return        // sentinel — unused by structs today
  rc -= 1; store(ptr + size, rc)
  if rc > 0: return
  for (offset, ty) in arc_fields[sym]:
      field_val = load(ptr + offset)
      release_field(ty, field_val)   // runtime helper or nested struct fn
  call rune_struct_dealloc(ptr, size)
  ```
- `compile_module` adds a new pass 0 that declares all per-struct
  release functions up front, so a struct with nested struct fields
  can call the inner struct's release. Pass 3 then defines each body.
- `emit_arc_call(retain, Ty::Struct, val)` inlines `rc++` at offset
  `size`. Release dispatches to the synthesized function.

Stress test: 100k iterations of `let p = Point { x: i, y: i };` —
RSS stays flat.

```rust
fn define_struct_release(&mut self, sym, func_id) -> Result<(), _> {
    // signature: fn(*mut u8) -> ()
    // entry block, load rc, decrement, branch on zero,
    //   release arc fields via emit_release_field,
    //   call struct_dealloc, return
}
```

### Nested struct releases

When a struct A has a field of struct B, A's release function calls
B's release function (declared up front via Pass 0). The fixed-point
in the lowerer for `struct_arc_fields` already handles the dependency
graph: a struct is ARC-managed if it has any ARC field, including
transitively. The synthesized release for each struct walks ARC
fields one at a time; struct-typed fields just call their own
synthesized release.

## 2. Per-variant destructor walks for payload enums

The v0.x payload-enum leak: session 020's enum release helper
deallocated the descriptor without touching the payload. A
`Some(vec_new())` value dropped → Vec leaked.

Fix:
- Synthesize `__rune_release_enum$<sym_id>` for every enum in
  `enum_has_payload`. The body:
  ```
  rc = load(ptr + rc_offset)
  if rc == -1: return
  rc -= 1; store
  if rc > 0: return
  tag = load(ptr + 0)
  for each variant with ARC payloads:
      if tag == disc:
          for each ARC payload position i:
              raw = load(ptr + 8 + 8*i)
              release_field(payload_ty, raw)
          break
  call rune_struct_dealloc(ptr, field_size)
  ```
- The walking iterates payloads in declaration order. ARC payload
  positions are computed once at codegen time from
  `HirModule::enum_payload_tys`.
- The dealloc helper is the same `rune_struct_dealloc` used by
  structs; the previous `rune_enum_dealloc` is retained for backward
  compat but redundant.

### Construction retain bug fix

While verifying the destructor walks, found a separate bug in
`EnumPayloadCtor` construction: a `Local`-typed payload arg
(e.g., `Opt::Some(v)` where `v: Vec` is a local) was stored without
retaining. The enum descriptor held a non-owning reference; releasing
both the local Vec and the enum descriptor double-freed the Vec.

Fix: same retain-Local-of-ARC rule as struct field initializers.

Stress test:
```rune
enum Opt { Some(Vec), None }
fn main() -> i64 {
    let mut i = 0;
    while i < 100000 {
        let v = vec_new();
        v.push(i);
        let o = Opt::Some(v);
        i = i + 1;
    }
    i
}
```
RSS flat across 100k iterations.

## 3. Multi-field tuple variants

```rune
enum Pair { Both(i64, i64), Just(i64), None }

fn sum(p: Pair) -> i64 {
    match p {
        Pair::Both(a, b) => a + b,
        Pair::Just(a)    => a,
        Pair::None       => 0,
    }
}
```

### Layout

`{ tag, payload[max_arity], rc }`. The enum's max arity across
variants determines the descriptor size:
- `Pair { Both(i64, i64), Just(i64), None }` → max_arity=2 →
  `8 + 16 + 8 = 32` bytes.
- `Triple { T(i64, i64, i64), Empty }` → max_arity=3 → 40 bytes.

Unused payload slots (for lower-arity variants) are uninitialized
garbage; they're never read because the tag selects which slots are
valid. Releases only walk the ARC positions declared for the active
variant.

### Codegen helpers

```rust
fn enum_max_arity(sym, enum_payload_tys) -> usize {
    enum_payload_tys[sym].iter().map(|ps| ps.len()).max().unwrap_or(0)
}

// Used everywhere a payload-enum is allocated, read, or released.
let field_size = 8 + 8 * max_arity;     // payload area + tag
let rc_offset = field_size;             // rc lives right after
```

### HIR shape

```rust
// Construction
HirExprKind::EnumPayloadCtor {
    enum_sym: SymbolId,
    discriminant: u32,
    payloads: Vec<HirExpr>,      // one entry per arity position
}

// Destructure
HirPattern::EnumPayload {
    discriminant: u32,
    bindings: Vec<(Ty, Option<SymbolId>)>,
}
```

The pre-session-021 single-payload form is gone; v0.x now treats
single-tuple variants uniformly with multi-tuple variants
(`Some(x)` is just a 1-arity case).

### Pattern destructure

The pattern check produces N bindings on match, one per arity
position. Bindings of `_` skip the load and var allocation.

```text
tag = load(scrut + 0)
brif tag == disc → extract, else next_arm
extract:
  for each (payload_ty, binding) in bindings:
    if binding is Some(sym):
      raw = load(scrut + 8 + 8*i)
      val = ireduce(raw) if narrower
      def_var(sym, val)
  jump on_match
```

## What's tested (+5 codegen tests)

- `struct_descriptor_arc_in_loop` — 100k struct ctors stay RSS-flat.
- `enum_multi_field_tuple_variant` — `Pair::Both(3, 4)` destructured.
- `enum_three_field_variant` — `Triple::T(1, 2, 3)` with `_` bindings.
- `enum_payload_vec_released_on_drop` — verifies the destructor walk
  releases the inner Vec on enum drop.

Plus all 347 pre-existing tests still pass.

## File layout changes

```
src/
├── codegen.rs   (RuneEnum unchanged in struct layout; multi-arity
│                 layout computed per use site via enum_max_arity;
│                 rune_struct_new/dealloc handle both struct and
│                 enum allocations; per-struct + per-enum release
│                 functions synthesized in pass 0 declarations +
│                 pass 3 definitions; emit_arc_call inlines retain
│                 for struct + payload-enum, calls synthesized
│                 release; EnumPayloadCtor takes payloads: Vec<HirExpr>
│                 and stores at 8+i*8; pattern EnumPayload walks
│                 multi-binding extraction; is_arc_type returns true
│                 for any Ty::Struct)
├── hir.rs       (EnumPayloadCtor.payloads: Vec<HirExpr>;
│                 EnumPayload.bindings: Vec<(Ty, Option<SymbolId>)>;
│                 HirModule.struct_sizes; HirModule.enum_payload_tys)
├── lower.rs     (collects struct_sizes from CheckResults; builds
│                 enum_payload_tys per enum; tuple-variant ctor and
│                 pattern handle multi-arity)
└── checker.rs   (drops the v0.x "single-field only" rejection on
                  tuple variants; arity check stays)
tests/
└── codegen.rs   (+5 — struct loop, multi-field, three-field, vec
                  released on drop, descriptor arc in loop)
LANGUAGE.md      (4 new decision-log rows — one each for struct rc,
                  enum destructor walks, multi-field variants, and
                  the integrated description above)
```

## Apparent bugs that aren't

- **Each struct construction is one runtime call to `rune_struct_new`
  plus N stores.** The previous stack-slot construction was free
  but the descriptor couldn't escape. The runtime call costs ~30
  cycles; for the test corpus this is fine.

- **Synthesized release functions are emitted even for structs that
  have no ARC fields.** Correct — they still need to dealloc the
  descriptor. The body is short (no field walk).

- **The enum descriptor's payload slots beyond a variant's arity are
  uninitialized garbage.** Correct and harmless — the slots are
  never read because the tag determines which slots are live for
  each variant.

- **`EnumPayloadCtor` retains every Local-of-ARC payload arg.**
  Necessary fix uncovered while testing destructor walks. Without
  it, dropping the enum descriptor would release a non-owned
  payload pointer that some other local also held.

- **`rune_enum_dealloc` and `rune_release_enum` are still wired
  through `declare_builtin` but never called from generated code.**
  Backwards-compat aliases; harmless.

## Deferred — picking up next session(s)

1. **Named-field enum variants** (`Ok { value: T, err: E }`). The
   parser already accepts the syntax; the resolver collects the
   field types as `VariantFields::Named(_)` but downstream treats
   them as Unit (with a Vec::new() payload). To land:
   - Resolver: populate `enum_variant_payloads` from Named field
     types in declaration order; record field names per variant.
   - Checker: dispatch `Variant { name: val }` (parsed as
     `Expr::StructLit`) by checking whether the path resolves to
     a variant — if so, reorder fields into declaration order and
     produce a variant ctor.
   - Pattern: add `Pattern::NamedVariant { path, fields: Vec<(name,
     Pattern)> }`. Lowerer reorders by name into declaration order.
   - Codegen: same as tuple variants once the payloads vec is in
     declaration order.

2. **Generics step 2 (monomorphization)**. Each Call site to a
   generic fn infers concrete types from arguments; the codegen
   instantiates a specialized HirFn with TypeVars substituted, caches
   `(SymbolId, Vec<Ty>) → FuncId`, mangles names like `id$$i64`,
   recursively monomorphizes calls inside the specialized body. The
   checker needs a substitution pass for the generic body's
   internal types. This is multi-session work.

3. **Generic struct field types** falls out of monomorphization —
   `struct Box<T> { value: T }` becomes `Box$$i64` etc. The layout
   computation already exists; just needs T resolved per
   instantiation.

## What this session deliberately is not

- **Not** a full v1.x. Even after closing the descriptor leaks,
  there are still memory paths that leak (untested code paths,
  panics, etc.). The test corpus stress-tests the common patterns
  and they're flat.

- **Not** trying to optimize. Inline retain + synthesized release
  each emit a chunk of IR per call. A future pass could inline the
  per-struct release for small structs; we don't bother today.

## Next session

The natural pick is **named-field enum variants** (small extension
to the tuple-variant work) followed by **generics step 2**, which
is the unlock for everything generic.
