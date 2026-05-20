# Session 019 — Cleanup batch + ARC follow-ups

**Date:** 2026-05-20
**Outcome:** Five deferred items from session 018's TODO list land,
plus a design note on weak references. 333 tests green (+19 from
session 018's 314). The biggest piece is the ARC follow-up work —
copy-on-let now retains, struct fields participate in ARC, and the
"don't copy ARC locals" warning from session 018 is retracted.

## What's in the batch

| # | Item | Status |
| --- | --- | --- |
| 1 | Parser precedence: `!f(x)` | Fixed |
| 2 | Char literal codegen | Landed |
| 3 | `as` cast codegen | Landed |
| 4 | ARC-on-copy | Landed |
| 5 | ARC for struct fields | Landed |
| 6 | Weak references | Design pass, implementation deferred |

Each is small enough on its own that a separate session would be
overkill; bundled they make a coherent "shore up the v0.x basics"
release.

## 1. Parser precedence — `!f(x)` is `!(f(x))`

The pre-existing bug: `parse_unary` recursed into `parse_unary` for
the operand, wrapped in `Unary`, and then `parse_expr_bp`'s outer
postfix loop applied calls/index/field to the wrapped node. Result
patterns:

| Source | Before | After |
| --- | --- | --- |
| `!f(x)` | `(!f)(x)` | `!(f(x))` |
| `-x[0]` | `(-x)[0]` | `-(x[0])` |
| `!a.b`  | `(!a).b`  | `!(a.b)`  |

Fix is a single `parse_postfix_chain` helper applied inside
`parse_unary` between the inner-parse and the wrap:

```rust
if let Some(op) = op {
    self.bump();
    let inner = self.parse_unary()?;
    let inner = self.parse_postfix_chain(inner)?;   // <-- new
    let end = inner.span().end;
    Ok(Expr::Unary { op, expr: Box::new(inner), ... })
}
```

The outer postfix loop in `parse_expr_bp` then sees no postfix tokens
remaining after the unary chain, so this is the only fix needed.

## 2. Char literal codegen

`HirLit` gains a `Char(char)` variant; the lowerer fills it from
`ast::Lit::Char` (which previously lowered to `HirLit::Unit` — a
silent dead-end for char-typed values). Codegen emits one instruction:

```rust
HirLit::Char(c) => self.builder.ins().iconst(types::I32, *c as i64),
```

Char *pattern* literals reuse the existing `HirPattern::IntLit` with
the codepoint as `i64`. The pattern-check emits `iconst` of the
scrutinee's cranelift_type — `I32` for `Ty::Char` — so the comparison
narrows correctly without a separate `HirPattern::CharLit` variant.

What this unlocks:
- `let c = 'A';` and `c as i64`.
- `fn classify(c: char) -> ...` with `match` on chars.
- The session 017 char range tests can now run end-to-end (they were
  typecheck-only before).

## 3. `as` cast codegen

New `HirExprKind::Cast { expr }`. The lowerer drops `Unsupported`
and the codegen dispatches by `(src_ty, dest_ty)`:

```
                       dest
              i*    u*    f*   bool  char
       i*  | trunc/ext (sign) | fcvt_from_sint | icmp ne 0 | trunc/ext |
src    u*  | trunc/ext (zero) | fcvt_from_uint | icmp ne 0 | trunc/ext |
       f*  | fcvt_to_*_sat   | promote/demote | (n/a)    | (n/a)    |
       bool| zext             | zext + fcvt_uint | =      | (n/a)    |
       char| trunc/ext        | (n/a)            | icmp ne 0 | =     |
```

Saturating float→int (`fcvt_to_sint_sat`) matches typical static
language semantics: NaN → 0, +∞ → INT_MAX, -∞ → INT_MIN. No UB on
out-of-range conversions.

`int → bool` is intentionally rejected by the checker (matches Rust;
`if x != 0` is the explicit form). `bool → float` routes through
`uextend.i32` first since Cranelift's `fcvt_from_uint` needs an int
input.

## 4. ARC-on-copy

Session 018 documented this limitation: `let y = x;` between two
ARC locals aliased without retaining, so `y` became a dangling
pointer once `x`'s release dropped the rc to 0. The new rule is the
mirror image — when the let init is a `Local` read of an ARC type,
emit a retain so `y` gets its own +1:

```rust
if is_arc_type(&l.ty, ...) {
    if let HirExprKind::Local(_) = &init.kind {
        self.emit_arc_call("retain", &l.ty, v)?;
    }
    owns_arc = true;
}
```

The is-Local discrimination is preserved — fresh +1 producers
(constructor calls, str concat, slicing) still skip retain because
they already carry the +1.

Assignment to an ARC local now does retain-then-release:

```rust
if is_arc_type(&rhs.ty, ...) {
    if let HirExprKind::Local(_) = &rhs.kind {
        self.emit_arc_call("retain", &rhs.ty, v)?;
    }
    let old = self.builder.use_var(var);
    self.emit_arc_call("release", &rhs.ty, old)?;
}
self.builder.def_var(var, v);
```

Trace for `let mut s = "hello" + ""; s = "world" + "!";`:
- First let: concat → rc=1 ("hello"), `s` owns +1.
- Second concat → fresh rc=1 ("world!"). It's NOT a Local; no retain.
- Old `s` ("hello") releases → rc=0, dealloc.
- `s` now points to "world!", still rc=1.
- Scope exit: release `s` → "world!" dealloc.

Compound assign `s += s2`: the binop produces a fresh +1 (str concat
allocates). Release the old `s`, store the new value. Same shape as
plain assign but without the conditional retain (concat is never a
Local).

Self-assign `s = s` retains then releases on the same pointer — net
zero, no UAF.

## 5. ARC for struct fields

The structural change: `HirModule` gains a map of every struct's
ARC-managed fields:

```rust
struct_arc_fields: HashMap<SymbolId, Vec<(u32, Ty)>>,
```

The lowerer computes this with a small fixed-point pass over
`CheckResults::struct_layouts` so struct-of-struct cases work:

```rust
loop {
    let mut changed = false;
    for (sym, layout) in &self.check.struct_layouts {
        if struct_arc_fields.contains_key(sym) { continue; }
        let arc_fields = layout.fields.iter()
            .filter(|f| match &f.ty {
                Ty::Vec | Ty::Str => true,
                Ty::Struct(inner) => struct_arc_fields.contains_key(inner),
                _ => false,
            })
            .map(|f| (f.offset, f.ty.clone()))
            .collect::<Vec<_>>();
        if !arc_fields.is_empty() {
            struct_arc_fields.insert(*sym, arc_fields);
            changed = true;
        }
    }
    if !changed { break; }
}
```

Codegen-side changes:
- `is_arc_type(ty)` becomes `is_arc_type(ty, &struct_arc_fields)`,
  returning true for Vec, Str, and any struct in the map.
- `emit_arc_call(action, ty, value)`: if `ty` is `Ty::Struct(sym)`,
  walk the listed fields, load each at its offset, and recurse with
  the field type. Atomic Vec/Str fields call the existing runtime
  retain/release helpers; nested struct fields recurse again.
- `compile_struct_lit`: for each field initialized from a Local read
  of an ARC type, emit retain after the store. Fresh +1 producers
  skip retain (same is-Local rule as let).
- `compile_field_assign`: if the field is ARC, load the old value,
  release it, optionally retain the new value, then store. Same
  retain-rhs-if-Local rule.

What this enables:

```rune
struct Holder { v: Vec, n: i64 }

fn main() -> i64 {
    let mut i = 0;
    while i < 100000 {
        let v = vec_new();        // rc=1
        v.push(i);
        let h = Holder { v: v, n: 1 };
                                  // field init is Local → retain v → rc=2
                                  // h is registered as arc_local (struct contains Vec)
        i = i + h.n;
                                  // scope exit:
                                  //   release v (the outer local)  → rc=1
                                  //   release h → walk fields → release v field → rc=0 → dealloc
    }
    i
}
```

RSS stays flat across 100k iterations.

What's still NOT covered (struct ARC limitations):
- **Returning a struct.** The struct's stack slot lives in the
  callee's frame; returning a pointer to it gives the caller a
  dangling reference. This is a pre-existing limitation, not new
  with ARC. The struct-as-return ARC code path exists but is dead
  today.
- **Struct as function argument.** The caller passes a pointer to
  the caller-frame slot; the callee borrows. If the callee does
  `let h2 = the_struct_param`, that's an ARC let-copy, which now
  retains each field properly. OK.
- **Struct field types beyond Vec/Str/Struct.** Arrays, enums, etc.
  inside structs aren't ARC-tracked (they're trivially copyable or
  stack-allocated).

## 6. Weak references — design only

The standard Arc/Weak split has a **control block** layout with both
strong (`rc`) and weak (`weak_count`) counts:

```rust
struct ControlBlock {
    rc: i64,
    weak_count: i64,
}
// Plus the actual data (Vec elements, str bytes, ...) hanging off
// the same descriptor or a separate allocation.
```

Strong refs collectively count as one weak. Protocol:

- `retain` (strong): `rc += 1`.
- `release` (strong): `rc -= 1`; on 0, dealloc the **payload** (Vec
  elements, str bytes). The descriptor stays alive until weak hits 0.
- `downgrade(strong) -> weak`: `weak_count += 1`.
- `release_weak`: `weak_count -= 1`; on 0 (and rc already 0), dealloc
  the **descriptor**.
- `upgrade(weak) -> ???`: if rc > 0, `rc += 1`, return strong; else
  return *some kind of nothing*.

**The blocker is the API for `upgrade`.** Real generic Rust returns
`Option<Arc<T>>`. Without generics:

- **Raw nullable pointer** — `upgrade` returns a `Vec` value that may
  be null. Every caller has to check. Loses type-system support for
  "did the alloc survive."
- **`is_alive(w) -> bool` only** — never upgrade; just check liveness.
  Useful for some patterns (debug logging, observer cleanup) but
  not for "I want to use the object if it still exists."
- **Panicking upgrade** — `upgrade` aborts if the target is gone.
  Conservative; turns "best-effort" usage patterns into hard errors.

None of these match the Rust ergonomics. Rather than ship a regression,
the decision is to **defer weak references until generics +
`Option<T>` land**. At that point `Weak<T>` becomes a regular generic
with `upgrade(self) -> Option<T>` doing the right thing.

The control-block layout work *could* land without the user-facing
type (just to test the strong/weak protocol mechanically), but
without a way for users to construct a `Weak`, it'd be dead runtime
code. Skipped.

**Until then, ARC cycles leak.** Document it.

## Test counts

Per suite:
- AOT: 34 (no change)
- Codegen: 146 (+18 — char ×3, cast ×7, arc-on-copy ×4, arc struct ×3, plus the existing tests still pass)
- Lexer: 21 (no change)
- Parser: 41 (+1 — postfix_binds_tighter_than_unary)
- Typecheck: 91 (no change)

**Total: 333 (+19 from session 018's 314).**

## File layout changes

```
src/
├── parser.rs       (parse_unary applies parse_postfix_chain to inner;
│                    new parse_postfix_chain helper)
├── hir.rs          (HirLit::Char; HirExprKind::Cast; HirModule.
│                    struct_arc_fields)
├── lower.rs        (Lit::Char → HirLit::Char; pattern Char →
│                    HirPattern::IntLit; Cast lowered; struct_arc_fields
│                    fixed-point in lower_module)
├── codegen.rs      (HirLit::Char codegen; compile_cast dispatch;
│                    is_arc_type takes struct_arc_fields; emit_arc_call
│                    handles Ty::Struct via field walk; compile_struct_lit
│                    retains Local fields; compile_field_assign releases
│                    old / retains new; let-of-ARC retains on Local init;
│                    Assign / AssignOp retain/release)
tests/
├── parser.rs       (+1)
└── codegen.rs      (+18 — char, cast, arc-on-copy, arc struct)
LANGUAGE.md         (six decision-log rows)
README.md           (updated feature list, roadmap)
```

## Apparent bugs that aren't

- **`as` cast on a value of the same type is a no-op (no IR emitted).**
  Intentional — `let x: i64 = 5; let y: i64 = x as i64;` shouldn't pay
  for a sext/uext/whatever when nothing's converting.

- **`emit_arc_call` on a struct may emit many runtime calls per drop.**
  A 5-Vec-field struct emits 5 release calls. Could be inlined to a
  single helper `rune_release_struct(ptr, layout)` once we have struct
  metadata at runtime, but that's optimization, not correctness.

- **`fcvt_to_sint_sat` for f64→i64 of a value just outside i64::MAX
  gives i64::MAX, not a panic.** Documented as the saturating
  convention. The strict-conversion variant (`fcvt_to_sint`, traps on
  out-of-range) is also available; we'd add it as an opt-in later.

- **The ARC retain for a returned `Local` of a struct walks each
  field's retain — same as scope-exit's release.** Doubled walks per
  return aren't ideal but match the field-level granularity.

## What's still TODO

Carried over plus newly surfaced:

- **Generics + `Option<T>`** so weak refs can land cleanly. Big.
- **Payload-bearing enum variants** + destructuring patterns. Big.
- **Returning structs by value** (or by pointer with ownership
  transferred). Needs a calling convention decision.
- **Arrays escaping their function** (related to struct return).
- **Range patterns on float**? Not requested, easy to add when needed.
- **`?` operator**. Needs error-handling design.
- **Strict-conversion variant of `as`** that traps on out-of-range
  float→int.

## Next session

The natural single-feature picks:
- **Generics step 1**: parser-level disambiguation of `<T>` from
  comparison. Largest single feature; unblocks Option/Result/Weak.
- **Payload-bearing enum variants**: requires HirPattern
  destructuring + value-carrying enum codegen. Largest match-related
  feature.
- **Returning structs by value**: smallest of the "limitations" group.

Or another batch session covering several of the smaller items
(strict `as`, float-range patterns, struct-return) the same way as
this session.
