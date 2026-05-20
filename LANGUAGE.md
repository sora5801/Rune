# Rune — Language Design

Living document. Each section is tagged with a **status**:

- **Decided** — pinned down; code may depend on it.
- **Tentative** — leaning a direction, not yet committed; reversible.
- **Open** — actively undecided; options listed.

Last updated: 2026-05-19.

---

## Mission

Rune is a small, statically-typed, compiled, general-purpose programming
language. It leans toward systems use cases (predictable performance, no
mandatory GC, FFI to C) but isn't strictly an OS-level language. Native code
via Cranelift; compiler written in Rust.

**Non-goals:**
- Replacing Rust, Zig, or any existing production language.
- Maximum performance — optimization is deferred indefinitely.
- Backwards compatibility before a 1.0 milestone.

## Syntax

**Status: Decided.** Rust/Swift-flavored.

- `fn name(arg: Type) -> Ret { ... }` for functions.
- `let x: T = expr;` for immutable bindings; `let mut x: T = expr;` for mutable.
- Type annotations after `:`.
- Expression-oriented blocks (`{ ... }` evaluates to its last expression when
  unterminated by `;`).
- Curly-brace bodies; semicolon-terminated statements.
- `match` for pattern matching (`=>` for arms).
- `::` for paths, `->` for return type, `..` and `..=` for ranges.

## Numeric types

**Status: Decided.** Rust-like sized scalars.

| Family | Types |
| --- | --- |
| Signed integers | `i8`, `i16`, `i32`, `i64` |
| Unsigned integers | `u8`, `u16`, `u32`, `u64` |
| Pointer-sized | `isize`, `usize` |
| Floating point | `f32`, `f64` |

- **Default integer type for unannotated literals: `i64`** (pinned 2026-05-19).
- **Default float type: `f64`** (pinned 2026-05-19).
- Numeric literal suffixes (e.g. `42i64`, `3.14f32`) will be added in the lexer
  once the type checker needs them.

**Why `i64` over `i32`:** Rune targets 64-bit native via Cranelift; word-sized
default keeps array indexing and `usize` interop ergonomic. Swift made the
same choice with `Int`. Rust's `i32` default is historical (32-bit-friendly,
pre-2015 era) and forces noisy casts for any sized container.

**Why sized:** maps cleanly to Cranelift's primitive IR types. Reasonable for
a systems-leaning language; trivial to interop with C.

**Alternative rejected:** one unsized `Int` (`i64`) and one `Float` (`f64`).
Cleaner for a scripting language; awkward for bitfields, FFI, and predictable
memory layout.

## Mutability

**Status: Decided.** Immutable bindings by default; opt-in mutability with
`let mut`. Enforcement is **strict** — assignment to an immutable binding is
a type error, not a warning.

- `let x = 5;` — immutable binding. `x = 6;` is rejected.
- `let mut x = 5; x += 1;` — mutable. Compound assignment also requires `mut`.
- Function parameters are immutable in their declaration. To mutate a
  parameter's value inside the function body, rebind with `let mut`.

**Why:** matches Rust/Swift. Encourages explicit mutation. The `mut` keyword
isn't decorative — the type checker rejects writes to immutable bindings.

## Name resolution

**Status: Decided.** Lexical scoping with shadowing allowed.

- Innermost scope first, then enclosing scopes, then the module, then
  built-ins (primitive type names).
- **Shadowing within the same scope is allowed.** `let x = 1; let x = "hi";`
  rebinds `x` to a new binding (possibly with a different type). Each
  `let` creates a fresh symbol; references between the two declarations
  resolve to whichever was most recently declared.
- Forward references between top-level items work (two-pass resolution:
  collect declarations first, then resolve bodies).

**Why allow shadowing:** retains Rust's idiomatic stepwise refinement
(`let x = parse(); let x = x?;`). Banning it would also break the natural
`let mut x = ...; let x = x;` pattern for "freeze this after building it."

## Memory model

**Status: Tentative.** Stack-frame arena for short-lived values + a
process-lifetime leak heap for runtime-created strings. Ownership and
reclamation deferred.

Concrete state as of 2026-05-19:

- **Arrays are stack-allocated** at the point of literal. A `let xs = [1, 2, 3];`
  allocates a Cranelift `StackSlot` sized `3 * sizeof(i64) = 24 bytes` and the
  binding holds the slot's address. Lifetime is the function frame.
- **Arrays cannot escape a function** — no array returns, no array params
  yet. The codegen errors on either.
- **String literals** stack-allocate a 16-byte `(ptr, len)` descriptor;
  bytes live in `.rodata`.
- **String concatenation** (`+` on `str` operands) allocates a fresh
  descriptor + fresh byte buffer via `malloc`. **Never freed** —
  process-lifetime leak by design. Fine for v0.x; reclamation is a
  future ARC / arena / GC conversation.
- **No bounds checks on indexing.** `arr[i]` trusts `i`. Adding checks is
  cheap; deferred until errors-as-values are designed.

| Option | Story | Complexity | Note |
| --- | --- | --- | --- |
| Manual (C-like) | `alloc`/`free`, raw pointers | Low | Maximum freedom, easy to get wrong |
| Arena-only | One bump arena per region/frame | Low | Simple, fast, leaks until arena dies — **current direction** |
| ARC (Swift) | Implicit reference counting + weak refs | Medium | Per-op cost; cycles need user discipline |
| Borrow checker | Compile-time ownership | High | Defining feature if pursued; months of work |
| Tracing GC | Stop-the-world or concurrent collector | High | Real runtime; harder to bootstrap |

**Tentative recommendation:** continue stack arena + leak heap for v0.x.
Lift to "explicit named arenas" or "ARC" when programs grow long enough
that leaking becomes painful, or when we want arrays escaping their
function. Heap + ownership remains a v2 conversation.

Once code can actually run end-to-end with stdlib, decide whether to graduate
to ARC, ownership, or stay arena-based. Reversible.

### Reclamation roadmap

When the leak becomes painful, the realistic steps in order of effort:

1. **Manual `free(x)` builtin.** Lowest friction: a single runtime
   function that releases a `Vec`, concatenated `str`, or other
   heap-allocated value. Unsafe — use-after-free is on the user.
   Lets long-running programs reclaim memory without a runtime cost.
2. **ARC (Automatic Reference Counting).** Insert refcount fields in
   the heap descriptors for `Vec` and concat-`str`. Codegen emits
   inc/dec calls on copies, drops, and reassignments. Cycle leaks
   (mitigated later by `weak` references). Adds ~5–15% perf overhead.
3. **Arena-with-explicit-scope.** `arena foo { ... }` blocks where
   allocations go into `foo` and free at scope exit. More predictable
   than ARC for batch workloads; ergonomic enough that we already
   target this implicitly today (everything's the process arena).
4. **Borrow checker.** Compile-time ownership tracking. The defining
   feature if Rune ever pursues it; not a v0.x option.
5. **Tracing GC.** Mark-and-sweep or generational. Real runtime,
   harder to bootstrap, but ergonomic for the user. The "no thanks"
   path for a systems language unless we explicitly pivot.

Recommended order for actual implementation: 1 (manual `free`)
before 2 (ARC), because ARC's invariants (every copy increments) are
much easier to reason about once we know what "manual reclaim" looks
like. Skipping 1 and going straight to ARC is also legitimate but
risks getting the API wrong on the first try.

The current process-lifetime leak is fine for the test corpus and
example programs (`fib.rn`, `greet.rn`, `primes.rn`). It only becomes
painful for daemons, long-running tests, or programs that do
unbounded work in a single execution.

## Error handling

**Status: Tentative.** Result + `?`, like Rust.

```rune
enum Result<T, E> { Ok(T), Err(E) }

fn parse(s: str) -> Result<i32, ParseError> { ... }

let n = parse(input)?;
```

- No exceptions.
- No panics-as-control-flow.
- A `panic` exists for unrecoverable bugs only (out-of-bounds, etc.).

## Strings

**Status: Tentative.** UTF-8 throughout, one type: `str`.

Implementation as of 2026-05-19:

- `str` is a **fat pointer** — a 16-byte (`ptr`, `len`) descriptor.
- The descriptor lives on the **function's stack frame** (current memory
  model is stack-frame arena; same as arrays).
- String **literals** have their bytes embedded in the object's data
  section via `cranelift_module::declare_data`. The descriptor's `ptr`
  is a relocation to that static data; `len` is a constant.
- Strings are **immutable** in this iteration. No concat, no methods,
  no indexing-into-str (slicing UTF-8 needs char-boundary care).
- **Equality** (`==`/`!=`) is implemented — codegen routes through a
  runtime `rune_str_eq` that does length compare + memcmp.
- **Concatenation** (`+` and `+=`) is implemented — codegen routes through
  a runtime `rune_str_concat` that mallocs a fresh descriptor + fresh
  byte buffer. The result lives on the heap until the process exits.
- **`print_str(s: str) -> ()`** builtin prints the bytes followed by a
  newline. (Future: unify with `print(i64)` once we have overloading
  or generics.)

Empty strings (`""`) compile to a descriptor with `ptr = null` and
`len = 0`. The runtime checks `len == 0` before dereferencing, so
the null is safe.

**Methods on `str`:**

| Method | Returns | Notes |
| --- | --- | --- |
| `s.len()` | `i64` | Byte length (not char count). UTF-8-aware programs need to remember this. |
| `s.is_empty()` | `bool` | Equivalent to `s.len() == 0` but a single i8 compare. |

Both compile to inline IR — `s.len()` is a `load.i64` from the
descriptor's `len` field at offset 8; `s.is_empty()` does the load
followed by `icmp eq, 0`. No runtime call.

**Indexing and slicing**

- `s[i]` — reads one byte and zero-extends to `i64`. Byte-indexed (not
  char-indexed; for an ASCII-only program this is the obvious thing,
  for UTF-8 programs the byte may be a mid-codepoint continuation).
  No bounds checks today; reading past the end is undefined behavior.
- `s[a..b]` (exclusive) and `s[a..=b]` (inclusive) — heap-allocate a
  fresh substring descriptor + fresh byte buffer (consistent with
  `+`'s leak-heap model). Out-of-range bounds are **clamped** by the
  runtime: `start < 0` becomes 0, `end > s.len()` becomes `s.len()`,
  `end < start` becomes `start`. No panic.
- Only `a..b` / `a..=b` are parsed today. Prefix (`..b`), postfix
  (`a..`), and bare `..` are deferred. So `s[..5]` is currently a
  parse error; users write `s[0..5]`.
- Range expressions outside a slice index are explicitly rejected.
  `for i in 0..n { }` doesn't work yet — needs an iterator protocol.

**Not yet:**

- Owned/borrowed split (`String` vs `&str`). May never split if a single
  immutable type is good enough.
- `.bytes()`, iteration over chars, char-aware indexing.
- Raw string literals (`r"..."`), triple-quoted multi-line strings,
  interpolation (`"\(expr)"` or `f"{expr}"`).
- **Returning literal strings from functions** is unsound — the
  descriptor lives on the callee's stack frame. Returning concat
  results, slice results, or passed-in parameters is safe (the former
  two are heap-allocated, parameters live in the caller).
- Slice indexing **clamps** out-of-range bounds. Byte indexing
  **panics** on out-of-range (via `rune_panic_bounds`, same as array
  indexing). The discrepancy is intentional — slicing has a natural
  clamp; reading a single byte doesn't.

## Type system

**Status: Open.** Pinned roughly to:

- Static, nominal types.
- Type inference for local bindings; explicit return types on functions.
- User-defined `struct` (with `impl` blocks for methods) and `enum`
  (unit variants codegen as i64 discriminants; payload variants
  deferred until match codegen lands).
- Generics (parametric polymorphism) — designed below, not yet implemented.
- Traits / protocols — desirable; deferred until after generics.

### Generics roadmap

The design space for parametric polymorphism:

| Strategy | How | Tradeoff |
| --- | --- | --- |
| **Monomorphization** | Each call site `f<i64>(x)` and `f<str>(x)` compiles a separate specialized copy of `f`. Same as Rust, C++. | Linear in distinct type instantiations. Best perf. Code bloat. The pragmatic choice for systems languages. |
| **Type erasure with single repr** | All `T` becomes a pointer (or fat pointer). One copy of `f` exists. | Works only when every `T` fits the chosen repr. Misses primitives by-value. C#/Java reference types take this path. |
| **Boxing** | `T` always boxed. One copy of `f`. | Worst perf — heap-alloc per pass. Easiest to implement. |

**Recommended path: monomorphization.** Matches Rune's systems-leaning
intent, doesn't require ABI workarounds, and is what real users will
expect from a Rust/Swift-flavored language.

Work involved (rough sequence):

1. **Parser**: parse `<T>` and `<T, U>` after function/struct/enum names
   (lexer already has `<` and `>`; the parser needs to disambiguate
   from comparison). Standard trick: after a path, peek for `<` and
   try-parse as generics; on failure, rewind and treat as comparison.
2. **AST**: `FnDecl::generics: Vec<Ident>`, `StructDecl::generics`,
   etc. `Path::generic_args: Vec<Type>` for use sites.
3. **Type system**: a `Ty::TypeVar(name)` for placeholders inside
   generic bodies. Substitution at instantiation.
4. **Type checker**: when checking a generic function body, treat
   `T` as opaque. When checking a call site, infer `T` from argument
   types and substitute.
5. **Lowerer/codegen**: monomorphize. A `(SymbolId, Vec<Ty>)` keyed
   cache of compiled instantiations. Each call site triggers
   instantiation if absent. Mangled names: `f$$i64`, `f$$str`, etc.
6. **Stdlib payoff**: `Vec<T>` retires the i64-only restriction.
   `Option<T>`, `Result<T, E>`, etc. become expressible.
7. **Inference**: at minimum, infer type parameters from arguments at
   call sites. More sophisticated bidirectional inference can come
   later.
8. **Retires**: `SymbolKind::PolyBuiltinFn` (used today for `print`).
   `print` becomes a regular `fn print<T: Display>(x: T)` once traits
   exist; or special-cased away earlier.

This is a substantial multi-session effort. Step 1 (parsing) alone
needs careful integration with the existing Pratt parser because
`<` is overloaded.

**Structs (current state):**

- Stack-allocated with 8-byte-per-field padding (v0.x simplification).
- Constructed via struct literals: `Point { x: 1, y: 2 }`.
- Field access via `s.field` reads at a statically-known offset.
- Can be passed by pointer between functions in the same call chain;
  cannot escape outwards (the descriptor is stack-allocated, same
  caveat as literal strings).
- No field assignment yet.

**Vec (current state):**

- Heap-allocated growable list with a `(ptr, len, cap)` descriptor.
- `vec_new()` constructor, `.push(x)`, `.get(i)`, `.len()`.
- Element type is **i64 only** for v0.x (no generics). Becomes
  `Vec<T>` once parametric polymorphism arrives.

**Open questions:**
- Trait objects vs monomorphization. (Likely monomorphization first.)
- Higher-kinded types. (Almost certainly no.)
- Effect tracking / IO purity. (No, initially.)

## Modules and visibility

**Status: Open.** Probable shape:

- One file = one module.
- `mod name;` to declare a submodule.
- `use path::Item;` to bring into scope.
- `pub` for public visibility; private by default.

Defer concrete decisions until the parser is more than a toy.

## Builtins

**Status: Tentative.** Host-provided functions Rune programs can call
without `use` or `import`.

### Free functions

| Name | Signature | Dispatch |
| --- | --- | --- |
| `print(x)` | polymorphic over `x: i64` and `x: str` | Lowerer picks `print_i64` or `print_str` based on argument type |
| `print_i64(x: i64)` | `fn(i64) -> ()` | Direct |
| `print_str(s: str) -> ()` | `fn(str) -> ()` | Direct |

The polymorphic dispatch lives in the lowerer
(`Lowerer::lower_poly_call`) — it picks a concrete `BuiltinCall`
target. No language-level overloading; no traits. The mechanism is
called `SymbolKind::PolyBuiltinFn` and is intended to stay small until
generics or traits arrive, at which point `print` can become a regular
generic function and `PolyBuiltinFn` can retire.

### Methods on builtin types

Method resolution is type-directed: the checker looks up the method by
`(receiver_ty, method_name)` against a hardcoded table
(`checker::resolve_method`). The lowerer emits `HirExprKind::MethodCall`
and codegen dispatches based on the same `(ty, name)` pair, mostly
inline (no runtime call).

| Receiver | Method | Returns | Codegen |
| --- | --- | --- | --- |
| `str` | `len()` | `i64` | `load.i64` from descriptor offset 8 |
| `str` | `is_empty()` | `bool` | `len + icmp eq, 0` |
| `str` | `starts_with(prefix: str)` | `bool` | runtime `rune_str_starts_with` |
| `str` | `ends_with(suffix: str)` | `bool` | runtime `rune_str_ends_with` |
| `str` | `contains(needle: str)` | `bool` | runtime `rune_str_contains` |
| `[T; N]` | `len()` | `i64` | static `iconst` of `N` |
| `Vec` | `push(x: i64)` | `()` | runtime `rune_vec_push` (realloc if cap exceeded) |
| `Vec` | `get(i: i64)` | `i64` | runtime `rune_vec_get` |
| `Vec` | `len()` | `i64` | runtime `rune_vec_len` |

### Methods on user-defined types — `impl` blocks

```rune
struct Point { x: i64, y: i64 }

impl Point {
    fn magnitude_sq(self: Point) -> i64 {
        self.x * self.x + self.y * self.y
    }
}

fn main() -> i64 {
    let p = Point { x: 3, y: 4 };
    p.magnitude_sq()  // 25
}
```

- The `self` parameter is explicit and typed: `self: Point`. No
  implicit `self` keyword (yet).
- Inside the impl block, methods are regular `fn` declarations with
  any number of additional parameters.
- The resolver mangles method names (`Point__magnitude_sq` in
  Cranelift symbols) so the user-visible names can collide across
  types.
- A `(struct_sym, method_name) → method_sym` table in `Resolutions`
  drives method-call dispatch. The lowerer rewrites `p.magnitude_sq()`
  into `Call(method_sym, [p])` — `self` becomes the first argument.

Limitations:
- Only inherent impls — no traits, no generics. `impl Point` only.
- One impl block per type per program (a second redefinition errors).
- Method names within a type must be unique.
- No `pub` distinction (always public-within-the-module).

### Adding a builtin

The plumbing is uniform:
- Declare in the resolver (free functions) or add a row in
  `resolve_method` (methods).
- If the codegen path is non-trivial: add a runtime symbol in
  `codegen.rs` (Rust for JIT) and `aot.rs::RUNTIME_C` (C for AOT),
  and a case in `declare_builtin`.
- Trivial methods (loads, arithmetic) can be inlined directly in
  `compile_method_call`.

## Compilation model

**Status: Decided.**

- **Backend:** Cranelift. Two output modes:
  - `cranelift-jit` (in-memory codegen) for `rune run` — fastest feedback.
  - `cranelift-object` + external C linker driver for `rune build`,
    producing a native executable. `--release` flips Cranelift's opt
    level from `none` to `speed`.
- **ABI:** the target's default native calling convention — SystemV
  on Linux, WindowsFastcall on Windows, AAPCS on ARM. Effectively
  `extern "C"`. Trivial C interop later; a Rune-specific CC isn't
  planned for v0.x.
- **Entry point:** `fn main() -> i64`. The host calls main and prints
  the returned i64. `fn main() -> ()` will become valid once a
  `print` builtin lands.
- **Pipeline:** AST → HIR → Cranelift IR → machine code. The HIR is
  AST-shaped with types attached (not MIR-style basic blocks); features
  we can't codegen yet (method calls, field access, arrays, match,
  cast, try) are funneled into an `Unsupported` variant.
- **Optimization:** Cranelift's `opt_level = "none"` for fast compile.
  Switch to `speed` once correctness is settled.
- Whole-module codegen, no incremental compilation.
- No user-side FFI yet — will land alongside an `extern "C"` keyword
  on function items.

## Concurrency

**Status: Open.** Deliberately deferred. Threads / async are a post-v0 concern.

## Self-host

**Status: Aspirational.** Long-term goal: Rune compiles itself, written in
Rune. Far off; shouldn't influence near-term decisions.

---

## Decision log

| Date | Section | Change |
| --- | --- | --- |
| 2026-05-19 | Initial draft | Syntax decided; numerics, mutability, error handling, strings, compilation model tentative; memory model, type system, modules, concurrency open |
| 2026-05-19 | Parser implemented | Syntactic decisions pinned via implementation: Pratt precedence table, postfix `?` and `as`, `match` arm shape (`pat => expr,`), `else if` chains, expression-oriented blocks with optional trailing expression. Comparison operators currently left-associative (Rust treats them as non-associative — open). |
| 2026-05-19 | Resolver + type checker | Three previously-tentative decisions pinned: default integer type → `i64` (was `i32` placeholder); mutability enforcement → strict (immutable bindings can't be reassigned); name resolution → lexical scoping with shadowing allowed. Bottom-up monomorphic type checker landed. |
| 2026-05-19 | HIR + Cranelift codegen | Compilation model promoted to Decided. Cranelift JIT backend, target-native ABI (`extern "C"`), `fn main() -> i64` as entry. AST-shaped HIR with `Ty` on every node (over MIR/CFG). First runnable Rune: `rune run examples/fib.rn` prints `55`. |
| 2026-05-19 | AOT executables | `rune build <file>` produces a native `.exe` via `cranelift-object` + external C linker driver. Default linker discovery: `clang` → `gcc` → `cc`, overridable via `$RUNE_LINKER`. Rune's `main` is renamed internally to `__rune_main`; a synthesized `int main(void)` calls it and truncates the i64 return to a 32-bit OS exit code. `rune build examples/fib.rn && ./fib.exe; echo $?` → `55`. |
| 2026-05-19 | print + floats + --release + arrays/for | First host builtin: `print(i64)`, callable from both JIT (registered via `JITBuilder::symbol`) and AOT (embedded `RUNTIME_C` compiled inline by the linker driver). Float codegen tests landed (paths existed, now exercised). `rune build --release` maps to Cranelift `OptLevel::Speed`. Array literals stack-allocated via `StackSlot`; indexing via `iadd + load`; `for x in arr` desugars to a counter-based while loop. Memory model promoted Open → Tentative: stack-frame arena. |
| 2026-05-19 | Strings | `str` as a 16-byte (ptr, len) descriptor; descriptor on the function's stack frame, bytes in the object's data section via `cranelift_module::declare_data`. `print_str(s: str)` builtin and `==`/`!=` for strings (runtime `rune_str_eq`: length compare + memcmp). Immutable; no concat, no methods, no slicing. Empty strings use `ptr = null + len = 0`; the runtime checks `len == 0` before dereferencing. `examples/hello_world.rn` prints "Hello, world!". |
| 2026-05-19 | String concatenation | `+` and `+=` work on `str` operands. Codegen routes through a runtime `rune_str_concat` that mallocs a fresh descriptor + fresh byte buffer (process-lifetime leak; no free yet). Memory model gains "process-lifetime leak heap" for runtime-allocated strings. Concat results can be returned from functions, stored in mutable bindings, accumulated in loops. `examples/greet.rn` demonstrates `fn greet(name) -> str { "Hello, " + name + "!" }`. |
| 2026-05-19 | Polymorphic `print` | New `SymbolKind::PolyBuiltinFn` variant. `print(x)` accepts both `i64` (any int variant) and `str`; the lowerer dispatches to `print_i64` or `print_str` based on argument type. Type checker special-cases the call. The explicit-typed builtins `print_i64` and `print_str` remain available for direct use. Intentionally narrow (just `print` is poly today); revisits once generics or traits exist. |
| 2026-05-19 | Method calls + first methods | New `HirExprKind::MethodCall` variant. Type checker resolves methods via a hardcoded `resolve_method(recv_ty, name)` table. Codegen dispatches by `(recv_ty, name)` and mostly emits inline IR. Three methods land: `str.len()`, `str.is_empty()`, `arr.len()`. Mechanism extends to future methods (trivial: inline; non-trivial: route to runtime). |
| 2026-05-19 | String indexing + slicing | New `ast::Expr::Range` (and parser support for `a..b` / `a..=b` as infix at precedence (3,4), below comparison). New `HirExprKind::StrByteIndex` (inline `load.i8` + `uextend.i64`) and `HirExprKind::StrSlice` (calls runtime `rune_str_slice` — mallocs new descriptor + bytes; clamps out-of-range indices). Range expressions outside a slice-index context are an explicit type-check error. Standalone ranges and partial forms (`..b`, `a..`, `..`) are deferred. |
| 2026-05-19 | Range iter + str predicates + struct field access + Vec + impl blocks | Five features at once: (1) `for i in a..b { }` works via `HirExprKind::ForRange` — special-cased in the lowerer when the iter is a range. (2) Three new str methods via runtime calls: `starts_with`, `ends_with`, `contains`. (3) Struct field access end-to-end: new `Expr::StructLit`, parser support gated by `no_struct_lit` flag in condition position, `CheckResults::struct_layouts` with 8-byte-per-field padding, stack-slot codegen, field access via `load.<ty>` at offset. (4) `Vec` as a concrete builtin type — `vec_new()`, `.push`, `.get`, `.len`. Heap-allocated `{ptr, len, cap}` descriptor with realloc on grow. i64 elements only until generics. (5) `impl` blocks for inherent methods on structs — new `impl` keyword, `Item::Impl(ImplBlock)`, mangled method names (`Point__magnitude`), `Resolutions::impl_methods` table, lowerer rewrites `p.m()` to `Call(m_sym, [p])`. |
| 2026-05-19 | Field assignment + bounds checks + enum codegen + generics/reclamation design | Three features land; two are design-only because the implementations are multi-session. (1) Field assignment `p.x = 5` via new `HirExprKind::FieldAssign`. Checker's `check_place_root_mutable` walks `a.b.c` to its root and verifies that root is `let mut`. (2) Inline bounds checks for array and string byte indexing — `emit_bounds_check` emits `brif` + a call to `rune_panic_bounds` that prints to stderr and `exit(1)`s. Slice indexing keeps the clamp behavior. (3) Enum codegen for unit variants — `SymbolKind::EnumVariant { enum_sym, discriminant }`, resolver gains two-segment path resolution (`EnumName::Variant`), checker returns `Ty::Enum(sym)` for the value, codegen emits `iconst.i64(discriminant)`. Enables `==`/`!=` dispatch via the existing icmp path. Match codegen still deferred. (4) **Generics: design pass only.** Monomorphization is the recommended strategy; multi-step roadmap added to LANGUAGE.md's "Type system" section. (5) **Reclamation: design pass only.** Five-step ladder added to LANGUAGE.md's "Memory model" section, with manual `free` builtin as step 1 and ARC as step 2. No code change. |
