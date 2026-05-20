# Session 018 — ARC (reclamation step 2)

**Date:** 2026-05-20
**Outcome:** Vec and concat/sliced str values are reclaimed
automatically at scope exit via refcount. Step 1's manual `free(x)`
builtin is removed. The minimal version of ARC — "scope-release of
fresh allocations" — handles the common case (allocate in a loop,
return from a function, drop locals at end) without leaking and
without crashing. 314 tests green (+1 net).

## What ARC means here

Every heap-allocated descriptor carries an `i64 rc` field. Three
runtime invariants:

1. **Fresh allocs start at rc=1.** Constructors (`vec_new`,
   `str_concat`, `str_slice`) initialize the field. The caller holds
   that one ref.
2. **Stack-allocated descriptors use rc=-1 as a sentinel.** Only
   string literals fall in this bucket today — the descriptor is on
   the stack, bytes are in `.rodata`. Retain/release helpers see -1
   and return immediately.
3. **Codegen pairs retains with releases.** Every fresh let-binding
   of an ARC type gets a release at scope exit; every borrowed value
   that escapes (return of a `Local`, tail expression of `Local`)
   gets a balancing retain.

```rune
fn make() -> Vec {
    let v = vec_new();    // rc=1, register v as owning local
    v.push(10);
    v.push(20);
    v                     // tail = Local-of-Vec → retain; rc=2.
                          // scope exit releases v → rc=1. Caller gets +1.
}

fn main() -> i64 {
    let v = make();       // rc=1 (received from caller of make)
    let n = v.get(0) + v.get(1);
    n
                          // scope exit releases v → rc=0 → dealloc.
}
```

## Descriptor layouts

```rust
#[repr(C)]
struct RuneStr {
    ptr: *const u8,
    len: i64,
    rc:  i64,              // -1 = literal (stack); >=1 = heap
}
// 24 bytes, 8-byte aligned.

#[repr(C)]
struct RuneVec {
    ptr: *mut i64,
    len: i64,
    cap: i64,
    rc:  i64,              // always >=1 (no Vec literals)
}
// 32 bytes, 8-byte aligned.
```

Both layouts are mirrored on the C side
([src/aot.rs](src/aot.rs) `RUNTIME_C`).

## Runtime helpers

```rust
extern "C" fn rune_runtime_retain_str(s: *mut RuneStr) {
    if s.is_null() || (*s).rc == -1 { return; }
    (*s).rc += 1;
}

extern "C" fn rune_runtime_release_str(s: *mut RuneStr) {
    if s.is_null() || (*s).rc == -1 { return; }
    (*s).rc -= 1;
    if (*s).rc > 0 { return; }
    // dealloc bytes + descriptor
}
```

`rune_retain_vec` / `rune_release_vec` are the same shape. The
sentinel guard means a literal-only program incurs one extra
load+compare per let-binding and zero allocator traffic.

**Single-threaded only.** The increment/decrement is a plain `i64`
write, not an atomic. Multi-threaded ARC needs `AtomicI64::fetch_add`
with `Ordering::Relaxed` for retain and `AcqRel` for the release-on-
zero check (the standard Arc<T> pattern). Threads aren't on the v0.x
roadmap, so non-atomic is fine for now.

## Codegen: who owns what

The codegen maintains a per-function `arc_locals: Vec<(Variable, Ty)>`
stack. Two operations:

1. **At `let x: ARC = init`**, register `x` iff `init` is NOT a
   `HirExprKind::Local`. This is the "fresh +1 producer" heuristic:
   - `let v = vec_new();` — register (Call is +1).
   - `let s = "a" + "b";` — register (BinOp on Str is +1).
   - `let s = "lit";` — register (Lit-Str produces a stack desc with
     rc=-1, but registering is harmless — release is a no-op).
   - `let y = x;` — DO NOT register. `y` aliases `x`. Limitation
     documented below.

2. **At block scope exit** (`compile_block`), release every
   `arc_local` pushed since the block's snapshot, then truncate.
   At `HirExprKind::Return`, release every active `arc_local` across
   all open scopes before emitting `return`.

### Balancing retains for escapes

A value that "escapes" a scope as the return / tail expression needs
to survive the upcoming scope-exit releases. Two escape patterns:

- **Explicit return** of an ARC `Local`:
  ```
  return v;
  ```
  Codegen emits: compile `v`, **retain v**, release all locals
  (including v — net zero on the underlying alloc), emit return.

- **Tail expression** of a block where the last stmt is
  `HirStmt::Expr(e, false)` and `e.kind` is `Local(_)` and `e.ty` is
  ARC:
  ```
  fn f() -> Vec {
      let v = vec_new();
      v
  }
  ```
  Codegen emits: compile tail → val, **retain val**, release locals
  in scope (release v → net zero), return last_val.

The two patterns share the same logic — "the caller wants +1, the
local is about to be released, so retain first." A fresh +1 producer
in tail position (`fn f() -> Vec { vec_new() }`) needs no retain
because the Call already returned +1 and was never registered as a
local.

## Function argument convention

**Borrowed.** Callers pass the raw pointer; callees do not release
params at function exit. Function params are registered in
`var_map` but **not** in `arc_locals`, so scope-exit doesn't touch
them.

This works because:
- Caller has +1 (the caller's local was registered).
- Caller passes pointer to callee.
- Callee can read/use freely (the alloc is alive — caller is keeping
  it pinned for the duration of the call).
- Callee returns. Caller's scope-exit eventually releases (rc→0,
  dealloc).

If a callee wants to **keep** an ARC param past the call, it must
retain it explicitly. v0.x doesn't have first-class refcount
manipulation, so this isn't directly expressible — but the pattern
of returning the param (covered above) works.

## Known limitations

These are real footguns; the next ARC session should address them.

1. **`let y = x;` between ARC locals doesn't retain.** Under the
   "register iff rhs is not Local" rule, `y` is not in `arc_locals`,
   so it's never released. The underlying alloc only sees x's
   release. The pointer in `y` is a dangling reference if x's release
   drops the rc to 0. **Workaround: don't copy ARC locals.** If you
   need two aliases, structure the code so the alias is consumed
   inside the same scope (e.g., pass to a function).

2. **No retain on assignment `x = y`** for ARC mut locals. Same
   issue. Effectively, ARC mut bindings aren't safe to reassign yet.

3. **No ARC for struct fields holding ARC values.** A
   `struct Pair { a: Vec, b: Vec }` constructed with two
   `vec_new()`s: both Vecs are stack-stored into the struct's slot,
   but the codegen doesn't release them on struct drop. They leak.
   Fix needs struct-aware drop glue.

4. **Cycles leak.** A Vec containing a pointer to itself (via some
   future indirection) would have a cycle. ARC alone can't detect.
   The standard fix is `weak` references; v0.x doesn't have them.

5. **Temporaries from method-chained calls in expressions** without
   a let-binding leak. `vec_new().push(5);` allocates a Vec, pushes
   into it, drops it on the floor — the +1 from `vec_new` is never
   released. Codegen would need to release un-consumed temporaries
   at the end of each expression statement. Skipped for v0.x.

6. **Non-atomic refcount.** Threads aren't supported yet; if we ever
   add them, retain/release must become atomic.

## `free(x)` is gone

The step-1 manual reclaim builtin is removed:

- Resolver no longer interns it
- Checker's polymorphic-builtin table loses the `"free"` arm
- Lowerer loses the dispatch entry
- Codegen no longer needs `rune_free_str` / `rune_free_vec` runtime
  functions (replaced by retain/release)

This is a deliberate cleanup — ARC supersedes the manual API. If a
future use case needs "drop this *now* regardless of refcount", we'd
add it back as `unsafe { release(x) }` or similar. No such use case
in the v0.x corpus.

## What's tested

Codegen (+8, -3 free-removed):
- `arc_vec_local_dropped_at_scope_exit` — basic vec_new + use + exit
- `arc_concat_str_dropped_at_scope_exit` — basic concat + use + exit
- `arc_in_loop_reclaims_steadily` — 100k iterations, RSS flat
- `arc_concat_in_loop_reclaims` — same with str concat
- `arc_return_local_vec_caller_uses_it` — fn returns local Vec,
  caller reads it
- `arc_return_local_str_caller_uses_it` — fn returns local str
- `arc_explicit_return_releases_locals` — early return through
  if-branch releases inner locals correctly
- `arc_str_literal_no_op_release` — sentinel path doesn't crash on
  100k iterations

Typecheck (+1, -5 free-removed):
- `free_is_no_longer_a_builtin` — `free(v)` now errors with
  `unresolved name \`free\``

Existing tests: all 116 prior codegen tests + 86 prior typecheck
tests + 34 AOT + 21 lexer + 40 parser still pass. The AOT tests in
particular exercise the C-runtime side of the ARC code path.

## File layout changes

```
src/
├── codegen.rs    (RuneStr / RuneVec gain rc; retain/release Rust
│                  runtime helpers replace free_str/vec; JIT symbol
│                  registrations swap; runtime fn declarations swap;
│                  compile_str_literal stores rc=-1 in a 24-byte
│                  stack desc; FnCodegen gains arc_locals; compile_
│                  block snapshots/releases at scope exit; HirLet
│                  registers ARC locals on non-Local init; Return
│                  retains Local-of-ARC, releases all)
├── aot.rs        (RUNTIME_C: rc field in both structs; retain/
│                  release C functions replace free_str/vec)
├── resolver.rs   (drop `free` builtin intern)
├── checker.rs    (drop `free` polymorphic dispatch arm)
└── lower.rs      (drop `free` poly dispatch entries)
tests/
├── codegen.rs    (-3 free tests, +8 arc tests)
└── typecheck.rs  (-5 free tests, +1 arc test)
LANGUAGE.md       (decision log row; memory-model note updated to
                   reflect ARC landed)
```

## Apparent bugs that aren't

- **Each scope's release pattern emits an inline call to the runtime
  retain/release helper, not an inline `rc -= 1`.** Cranelift could
  inline these via custom IR, but the call cost is a few cycles and
  the helper handles the sentinel check uniformly — cleaner than
  duplicating the check at every release site.

- **`let s = "lit";` registers s as an arc_local even though release
  is a no-op.** Correct: we don't statically know that the rhs of a
  let-of-Str is a literal vs a runtime-allocated str (consider
  `let s = if cond { "lit" } else { "x" + "y" };`). Always register;
  the sentinel handles literals.

- **The scope-exit release order is reverse of insertion.** That's
  intentional — LIFO matches the typical "later locals depend on
  earlier locals" relationship. For pure ARC this doesn't matter
  (no destructors), but it sets the convention for future struct
  drops or generic destructor calls.

## Next session

The big ones are the limitations above. Picking by impact:

- **ARC-on-copy** (`let y = x` retains; assignment retains+releases).
  Unlocks safe aliasing. Smallish change in `HirLet` codegen plus
  assignment codegen.
- **ARC for struct fields.** Walks struct layout, retain on
  construction with ARC fields, release on drop. Bigger; needs
  metadata in `CheckResults::struct_layouts`.
- **`weak` references** for cycle breaking. Larger design.
- **Char literal codegen** (carried over) — small.
- **Parser precedence bug `!f(x)`** (carried over) — small.
- **`as` cast codegen** (carried over) — modest.
