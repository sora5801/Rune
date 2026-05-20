# Session 012 — Five features at once

**Date:** 2026-05-19
**Outcome:** 235 tests green (+25 new). `examples/methods.rn`:

```
$ rune run examples/methods.rn
Hello, my name is Alice!
...and she is an adult.
found Alice in the greeting
sum of ages:
89
sum of squares 1..=5:
55
```

The user asked for all five remaining candidates from session 011's
deep dive in one shot. Each got a minimum-viable cut.

## 1. Range iteration: `for i in 0..n { }`

Special-cased in the lowerer rather than implementing a real iterator
trait. New `HirExprKind::ForRange { local, start, end, inclusive, body }`.
The lowerer detects `ast::Expr::Range` in the for-loop's iter position
and emits `ForRange` instead of `For`.

Codegen does a counter-based while loop: init counter to start, loop
while `counter < end`, increment by 1 each iteration. Inclusive ranges
fold `end + 1` at compile time so the loop body is identical for both
forms.

Type-checker hint: range endpoints must be integers. The standalone
range error from session 011 still fires for `let r = 0..10;` —
ranges remain illegal outside `for ... in ...` and slice indices.

## 2. More string methods

Three new runtime-routed methods: `starts_with`, `ends_with`, `contains`.
All share the same `(str, str) → bool` shape. One row in
`resolve_method`, one match arm in `compile_method_call`, three new
keys in `declare_builtin`, three runtime symbols (Rust for JIT, C in
`RUNTIME_C` for AOT).

While wiring these up, fixed a latent double-evaluation in
`compile_method_call` — the existing eager-args loop pre-compiled the
args for side effects only; the new methods needed the values too.
Pre-compiled args now go into a `Vec<Value>` that all arms share.

## 3. Struct field access

The bulkiest item — touches every phase.

**AST.** New `Expr::StructLit { path, fields, span }`. New
`FieldInit { name, value }`. `Expr::Field` already existed but used to
lower to `Unsupported(...)`.

**Parser.** A `Path` followed by `{` is now a struct literal — provided
`no_struct_lit == false`. That flag is new alongside `no_block_expr` and
gets set in condition position (`if`/`while`/`for`/`match` heads) so
`if Foo { ... }` still parses as `if Foo { body }`, not as
`if (Foo { ... }) { body }`. Same trick Rust uses.

**Type checker.** `CheckResults::struct_layouts` is built in pass 1
alongside fn signatures. Layout is a `Vec<StructLayoutField>` of
`(name, ty, offset)` tuples plus a total `size`. v0.x simplification:
every field gets 8 bytes regardless of width — avoids dealing with
alignment until variable widths matter. `check_struct_lit` validates
all fields are present (no defaults yet), no duplicates, and types
match the declaration. `check_field_access` resolves the receiver to
`Ty::Struct(_)`, looks up the field, returns its type.

**HIR.** New `HirExprKind::StructLit { sym, fields: Vec<(u32, HirExpr)>, size }`
and `HirExprKind::FieldAccess { receiver, offset, field_ty }`. The
lowerer reorders user-provided fields into declaration order so codegen
can iterate them sequentially.

**Codegen.** Struct literals allocate a stack slot of the layout's
`size` bytes, store each field at its offset, return the slot's
address. Field access compiles the receiver (a pointer to the slot)
and emits `load.<field_ty>` at the static offset. `cranelift_type` and
`elem_size` handle `Ty::Struct(_)` as a pointer.

Same dangling-pointer caveat as literal strings: a function that
returns a struct literal returns a pointer to a destroyed stack frame.
Pass-through parameters and concat-results-of-strings stay safe.

## 4. `Vec`

A single concrete `Vec` type rather than `Vec<T>` — no generics today.
Element type is implicitly `i64`. Becomes `Vec<T>` once parametric
polymorphism arrives.

New `Ty::Vec` variant. Registered as a builtin type alongside `i64`,
`str`. Codegen treats it as an i64 pointer (same shape as `Ty::Str`).

Builtin function `vec_new() -> Vec` constructs an empty vector. Three
methods:

| Method | Codegen |
| --- | --- |
| `push(x: i64)` | runtime call; reallocs (cap doubles) on grow |
| `get(i: i64) -> i64` | runtime call; OOB returns 0 instead of panic |
| `len() -> i64` | runtime call (could be inlined later) |

The descriptor is `{ ptr: *mut i64, len: i64, cap: i64 }` — 24 bytes,
heap-allocated via `malloc` (process-lifetime leak like the rest of
v0.x's heap story).

`push` doesn't require the Vec binding to be `mut`. The Vec value is a
pointer; the pointer is immutable, but the data behind it is. Like
`const Vec*` in C. The same flexibility means `let xs = vec_new();
xs.push(1);` works. We may revisit if explicit interior mutability
becomes important.

## 5. `impl` blocks

The biggest design win: inherent methods on user-defined structs.

```rune
struct Point { x: i64, y: i64 }
impl Point {
    fn magnitude_sq(self: Point) -> i64 {
        self.x * self.x + self.y * self.y
    }
}
```

**Lexer / parser.** New `impl` keyword. `parse_impl` consumes
`impl Path { fn... fn... }`. Visibility on impl methods is parsed but
ignored.

**Resolver.** Two-and-a-half passes now:
1. Declare top-level items (skip impls).
2. For each impl, resolve the type path against existing structs and
   register each method in a new `impl_methods` table indexed by
   `(struct_sym, method_name) → method_sym`. Methods get a mangled
   name (`Point__magnitude_sq`) so they can coexist in the symbols
   vec without colliding with similarly-named user fns.
3. Resolve all bodies, including method bodies (which are just
   regular `FnDecl`s).

**Checker.** Method bodies go through `check_fn` like regular
functions. The method-call resolution now checks both the hardcoded
builtin table and the `impl_methods` map. For user methods, the
externally-visible signature drops the `self` parameter (it's filled
in by the lowerer).

**Lowerer.** When an `ast::Expr::MethodCall` resolves to a user
method, it's rewritten into `HirExprKind::Call { callee: method_sym,
args: [receiver, ...original_args] }`. The HIR `MethodCall` variant
stays for inline builtin methods.

**Codegen.** Nothing new — user methods are just Cranelift functions
with the mangled name as their export name.

Constraints accepted for v0.x:
- Only inherent impls. No traits.
- One impl block per type.
- Self parameter must be written explicitly with its type:
  `self: Point`. No implicit `self`.
- Method names within a type must be unique.

## File layout changes

```
src/
├── token.rs    (+ TokenKind::Impl)
├── ast.rs      (+ Item::Impl, ImplBlock, Expr::StructLit, FieldInit)
├── parser.rs   (+ parse_impl; struct-lit in primary; no_struct_lit flag)
├── resolver.rs (+ impl_methods; declare_impl; resolve impl bodies)
├── checker.rs  (+ struct_layouts, build_struct_layout, check_struct_lit,
                  check_field_access; user_method_sig fallback;
                  impl methods in check_module + check_item)
├── hir.rs      (+ ForRange, StructLit, FieldAccess variants)
├── lower.rs    (+ range special-case in lower_for; struct-lit and field
                  access lowering; user-method-call rewrite to Call;
                  impl methods flattened into HirModule items)
├── ty.rs       (+ Ty::Vec variant)
├── codegen.rs  (+ Vec runtime fns + JITBuilder registrations;
                  str predicate runtime fns; compile_struct_lit /
                  compile_field_access / compile_for_range;
                  Ty::Vec/Struct in cranelift_type + elem_size)
└── aot.rs      (+ rune_vec_* and rune_str_starts_with/ends_with/contains
                  in RUNTIME_C)
tests/codegen.rs (+25: 5 range, 8 str predicates, 4 struct, 4 Vec, 4 impl)
examples/
└── methods.rn  (composes all five features)
```

## Apparent bugs that aren't

- **Struct fields are always 8 bytes wide.** A `struct { active: bool }`
  uses 8 bytes for a 1-byte field. Wasteful for memory-sensitive code;
  trivial to fix when widths matter — just sum and align in
  `build_struct_layout`.
- **Vec elements are i64.** A `Vec` of strings or structs would need
  generics. The type system accepts `Vec` as a value type but doesn't
  know its element type. `push` and `get` are hardcoded i64.
- **`p.push(...)` on a struct named `Vec`** would shadow the builtin if
  the user defines `struct Vec { ... }` with an `impl` block. The
  resolver would prefer the user's struct. We don't yet have namespaces.
- **`impl Foo` before `struct Foo` works** because of the two-pass
  declaration. Forward references between impls and structs are fine.

## Test coverage

25 new codegen tests:
- 5 for range iteration (exclusive, inclusive, negative bounds,
  variable bounds, empty-when-equal).
- 8 for string predicates (starts_with/ends_with/contains, true/false
  variants, empty needle, self-match).
- 4 for structs (basic, mixed field order, passed to fn, with bool
  field).
- 4 for Vec (push/get/len, grow past initial cap, empty-get returns 0,
  passed to fn).
- 4 for impl blocks (with self, with args, multiple methods, returns
  concat).

Total: 235 tests, all green.

## Next session

The remaining v0.x agenda:

1. **Field assignment** — `p.x = 5` for mutable struct bindings. Needs
   the LHS-place machinery from arrays/Vec.
2. **Generics**: `Vec<T>`, `print<T>` — pulls in monomorphization and
   probably traits.
3. **Enum codegen** — variants, `match` arms compiling, tagged-union
   layout.
4. **Bounds checks** + panic semantics — opens the runtime-error
   conversation.
5. **Reclamation** — process-lifetime leak has been good enough for v0.x
   but long-running programs will want it.

The biggest design conversation ahead is **generics vs traits**: how
much polymorphism we want before declaring v1.0, and which mechanism
gets us there.
