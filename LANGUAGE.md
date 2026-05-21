# Rune — Language Design

Living document. Each section is tagged with a **status**:

- **Decided** — pinned down; code may depend on it.
- **Tentative** — leaning a direction, not yet committed; reversible.
- **Open** — actively undecided; options listed.

Last updated: 2026-05-20.

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
  descriptor + fresh byte buffer via `malloc`. Reclaimed via ARC
  (step 2 of the reclamation ladder, landed 2026-05-20). Refcount
  lives in the descriptor; literal strings use `rc = -1` as a
  sentinel so the helpers no-op on them.
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

**Status: Decided.** `Result` + `?`, like Rust. `std::Result<T, E>`
is in the prelude (session 027); the `?` operator landed session 032.

```rune
fn chain(ok: bool) -> std::Result<i64, i64> {
    let v = parse(ok)?;          // Ok -> unwrap; Err -> return early
    std::Result::Ok(v + 1)
}
```

- `expr?` requires `expr` to be a `Result`-shaped enum and the
  enclosing function to return a `Result` with a matching error type.
  It desugars (in the lowerer) to `match expr { Ok(v) => v, Err(e) =>
  return Err(e) }`.
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

**Status: Decided (inline + file-based modules).** Landed 2026-05-20.

- **Inline modules**: `mod name { items... }`. Nesting allowed.
  Items inside a module are namespaced under `name`.
- **File-based modules**: `mod name;` (no body) loads `name.rn` and
  splices its items in as though `mod name { ... }` had been written.
  Expansion is a token-stream transformation that runs between lexing
  and parsing — `mod name ;` is rewritten to `mod name { <name.rn
  tokens> }` — so the parser, resolver, and everything downstream only
  ever see inline modules. Each loaded file is lexed into a fresh,
  disjoint slice of the global byte-offset space (its spans shifted by
  a base offset) so spans stay unique; a `SourceMap` records which
  file owns which range.
- **Nested module directories**: `mod foo;` in the main file loads
  `foo.rn` beside it; a `mod bar;` *inside* a loaded `foo.rn` loads
  `foo/bar.rn`. Module paths are `/`-joined and resolved by the
  driver against the filesystem. Because `mod` always descends into a
  subdirectory, file modules form a tree — import cycles are
  structurally impossible; a depth cap guards a pathological loader.
- **Paths**: `a::b::c` walks the module tree. Resolution tries the
  path absolutely (from root) then relative to the current module.
- **`use`**: `use a::b::c;` aliases `c` into the using module's
  namespace; `use a::b::c as d;` aliases it under `d` instead;
  `use m::*;` is a glob that aliases every item of `m` visible from
  here. An explicit `use` or a local item of the same name wins over
  a glob. `pub use ...;` makes the alias a public re-export — the
  aliased key is reachable from anywhere, even when the underlying
  item is otherwise private.
- **Visibility**: `pub` is enforced **per path segment**. A non-`pub`
  item is reachable only from its declaring module and that module's
  descendants; a `pub` item is reachable anywhere. Resolving `a::b::c`
  checks `a`, `a::b`, and `a::b::c` in turn — a private *intermediate*
  module is caught, not just a private final item. A `pub use`
  re-export key short-circuits the check. `use m::*` skips items not
  visible here.
- **`EnumName::Variant`** continues to work and composes with
  module paths (`m::Color::Red`); variants inherit the enum's `pub`.
- Items inside a module reference their siblings unqualified;
  ancestors and root items are visible too (innermost-first
  lookup).
- Functions get module-mangled codegen names (`a__b__f`) so two
  modules can each declare `fn f` without a Cranelift symbol
  clash. Root `main` keeps its bare name (entry point).

**Not yet:**
- **`pub(crate)` / `pub(super)`** — visibility is all-or-nothing.
- **`mod.rs`-style directory roots**, and a `use` that names a module
  for pathing *through* it (`use a::sub;` then `sub::thing`).

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

## Traits (bounded generics + trait objects)

**Status: Decided.** Static dispatch via monomorphization landed
2026-05-20; dynamic dispatch (`dyn Trait`) 2026-05-21. Supertraits,
associated types, and generic impls (`impl<T> Trait for Box<T>`)
remain open.

The motivation: bounded generics. A plain `fn id<T>(x: T)` takes
any T but the body can do nothing T-specific. Traits attach a
constraint:

```rune
trait Display {
    fn fmt(self) -> str;
}

impl Display for Point {
    fn fmt(self: Point) -> str { "(...)" }
}

fn show<T: Display>(x: T) -> str { x.fmt() }
```

How it works (as implemented):

1. **Parser**: `trait Name { fn sig; ... }` declares method
   *signatures* (no bodies); `impl Trait for Type { ... }` provides
   them; `<T: TraitName>` / `<T: A + B>` at generic-param sites.
   `ast::GenericParam { name, bounds }`.
2. **Resolver**: `SymbolKind::Trait`. `Resolutions::trait_methods`
   maps trait sym → declared signatures; `generic_bounds` maps a
   generic param sym → its bound trait syms. Trait-impl methods
   register into the same `impl_methods` table as inherent methods.
3. **Checker**: `check_trait_impl_conformance` verifies every trait
   method has a matching impl (arity-checked). A method call on a
   bounded generic receiver (`x.fmt()` where `x: T`, `T: Display`)
   resolves through `trait_bound_method_sig`, which finds the
   method in one of `T`'s bounds.
4. **Monomorphization**: trait method calls on a generic receiver
   stay as `HirExprKind::MethodCall` through lowering (the receiver
   type is still `TypeVar`). After a generic function is specialized
   for a concrete type, `resolve_method_calls` rewrites each
   `MethodCall` whose receiver is now a concrete struct/enum into a
   direct `Call` into the impl method (looked up in
   `HirModule::impl_methods`).
5. Codegen sees only `Call` for trait methods — never a generic
   `MethodCall`. Builtin method calls (`str.len()` etc.) remain
   `MethodCall` and dispatch as before.

**Static dispatch** (the above): every trait call on a bounded
generic is resolved to a concrete function by monomorphization.
Calling a generic function with N distinct types produces N
specializations.

**Dynamic dispatch** — `dyn Trait`:

```rune
fn describe(s: dyn Shape) -> i64 { s.area() }   // any Shape
```

A `dyn Trait` value is an 8-byte pointer to a heap cell
`[fnptr_0, .., fnptr_{N-1}, data]` — the trait's method pointers
(a per-instance method table) followed by the concrete data pointer.
A concrete struct that implements the trait coerces to `dyn Trait`
at `let`, call-argument, and `return` sites (the checker records the
coercion; the lowerer wraps it in a `DynBox`). A method call on a
`dyn` receiver lowers to a `DynCall` — codegen loads the method
pointer and data pointer from the box and emits a `call_indirect`.
The `dyn` box is **ARC-managed**: its layout carries a refcount and a
drop slot (a pointer to the boxed struct's release function), so a
`dyn` local reclaims both itself and the concrete value it wraps at
scope exit, and a `dyn` temporary passed as a function-call argument
is reclaimed by the caller once the call returns (session 036).

Still open:
- **`dyn` coercion at struct-literal fields and enum payloads** —
  coercion fires at `let` / call-arg / `return` / method-argument
  positions (so `Vec<dyn T>` works), but not yet when initializing a
  struct field or enum-variant payload.
- **Supertraits** (`trait Ord: Eq`).
- **Associated types** / constants.
- **Generic impls** (`impl<T> Display for Box<T>`) — today only
  concrete-type impls (`impl Display for Point`) are supported.
- **Conformance is arity-only** — full param-by-param type checking
  with `Self` substitution is a follow-up.

## Stdlib

**Status: Decided (v0.x prelude).** The standard library is a `mod std
{ ... }` written in Rune itself, stored at `src/std.rn`, embedded into
the compiler with `include_str!` (`rune::PRELUDE`), and prepended to
every program before lexing by `rune::with_prelude`. `std::` items are
always in scope.

The two historical blockers — traits and a module system — both landed
in earlier sessions, so the prelude is now plain Rune compiled by the
exact same pipeline as user code. No special-casing.

### The v0.x prelude (`src/std.rn`)

```rune
mod std {
    enum Option<T> { Some(T), None }
    enum Result<T, E> { Ok(T), Err(E) }

    fn unwrap_or<T>(o: Option<T>, default: T) -> T { ... }
    fn is_some<T>(o: Option<T>) -> bool { ... }
    fn is_none<T>(o: Option<T>) -> bool { ... }
    fn ok_or<T, E>(r: Result<T, E>, default: T) -> T { ... }
    fn is_ok<T, E>(r: Result<T, E>) -> bool { ... }
    fn is_err<T, E>(r: Result<T, E>) -> bool { ... }

    fn min(a: i64, b: i64) -> i64 { ... }
    fn max(a: i64, b: i64) -> i64 { ... }
    fn abs(x: i64) -> i64 { ... }
    fn clamp(x: i64, lo: i64, hi: i64) -> i64 { ... }
}
```

The generic helpers are **zero-cost when unused**: the monomorphizer
only emits the specializations a program actually calls and drops the
generic originals, so a program that touches no `std::` generic pays
nothing. The four concrete i64 helpers always compile in.

### How it's wired

`with_prelude(user_src)` returns `PRELUDE + "\n" + user_src` — one
source string occupying a single span space. The compile commands
(`check`/`run`/`build`) lex the combined string; the debug commands
(`tokens`/`ast`) lex only the user file so their output reflects it
faithfully. Byte offsets in errors that point at user code are shifted
by the prelude's length — a known v0.x rough edge.

### Still hardcoded

Not everything is in the prelude:
- `print` / `print_i64` / `print_str` — host builtins.
- `Vec<T>` — generic (session 028) and namespaced as `std::Vec`, but
  still a compiler builtin rather than Rune source. Rune has no
  raw-memory primitives, so a `Vec` can't be written in `std.rn`.
- ARC primitives `weak` / `upgrade_or`.

### What's next

- Ship the prelude as an external `std.rn` file (file-based modules
  landed in session 029) rather than `include_str!`-embedding it.
- A `collections` module — `HashMap`, an iterator trait — built on
  the now-generic `Vec<T>`.
- More numeric/string helpers; lift the `Vec` element restrictions
  (`Vec<f64>`, `Vec<str>`).

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
| 2026-05-19 | Manual `free(x)` builtin + match codegen | (1) Step 1 of the reclamation ladder lands: `free(x)` polymorphic builtin dispatches to `free_vec` or `free_str` based on argument type. Rust JIT runtime reconstructs the `Layout` from the descriptor's stored len/cap; C AOT runtime uses libc `free`. Caveat documented: `free` on a literal string is UB (bytes live in `.rodata`); aliased-use is UB. (2) Match codegen: new `ast::Pattern::Path` for `EnumName::Variant` patterns; new `HirExprKind::Match` + `HirMatchArm` + `HirPattern` (Wildcard / Bind / IntLit / BoolLit / StrLit / EnumVariant). Codegen emits a sequential `brif`-chain; non-matching fallthrough calls `rune_panic_no_match`. No compile-time exhaustiveness yet — runtime backstop only. Guards and payload destructuring still deferred. |
| 2026-05-20 | Compile-time match exhaustiveness | New `Checker::check_match_exhaustiveness` runs after per-arm checks. Tracks a "catch-all seen" flag (set by `_` or unguarded bind patterns) plus per-type coverage sets for bool, enum variants, int literals, and str literals. Errors fall into two buckets: (a) **non-exhaustive** — bool with missing `true`/`false`, enum with missing variants, or any infinite type (i64/str/char/float) with no catch-all; (b) **unreachable** — any arm after a catch-all, plus duplicate patterns (same int / bool / variant / str literal twice). The `rune_panic_no_match` runtime helper stays wired as defense-in-depth; the previous AOT test that exercised it is removed because the checker now rejects the program statically. Compile-time guards are still arm-by-arm but excluded from coverage (a guarded arm can fail at runtime). |
| 2026-05-20 | Match guards + or-patterns | (1) **Guards.** Arms accept `pat if cond => body`; the guard is an `Option<HirExpr>` on `HirMatchArm`. Codegen lowers a guarded arm into two blocks: the pattern-match arm-body block (which compiles the guard, then `brif guard_val → guarded_body, else next_arm`) and the actual `guarded_body`. The exhaustiveness check excludes guarded arms from coverage entirely — a guarded `_ if cond` arm is **not** a catch-all and a guarded `Ok if cond` does not consume the `Ok` variant. (2) **Or-patterns.** New `ast::Pattern::Or { patterns }` at the arm pattern's top level; parser handles `\|` between atoms; `HirMatchArm::patterns: Vec<HirPattern>` (flat — Or is desugared away by the lowerer). Codegen extends the sequential `brif` chain: each alternative within an arm branches to the same body. Or-patterns can't contain bindings (rejected with `or-pattern can't contain a binding`); duplicates within a single arm fire the existing unreachable-arm error. Or-patterns participate fully in exhaustiveness: `match b { true \| false => ... }` is exhaustive without a catch-all. |
| 2026-05-20 | Range patterns | New `ast::Pattern::Range { lo, hi, inclusive }` accepts integer or char literal bounds (with optional unary `-` on numeric bounds); parser tries `..` / `..=` after a literal pattern atom. New `HirPattern::IntRange { lo: i64, hi: i64, inclusive }` (chars get pre-converted to codepoints by the lowerer). Checker validates bounds-type match against the scrutinee and rejects empty ranges (`10..=0`, `5..5`). Codegen emits `lo <= scrut && scrut [<\|<=] hi` as two icmps + a brif chain; signed vs unsigned icmp follows the scrutinee's integer type. Ranges contribute **nothing** to exhaustiveness — overlap and partial-coverage tracking is out of scope for v0.x — so `0..=10 => ...` alone still triggers "non-exhaustive on i64; add a `_` arm". Range patterns nest inside or-patterns (`1..=3 \| 7..=9`) and combine with guards (`0..=10 if n == 5`). Negative literal patterns work as a side effect (`-5..=-1`, and as a standalone literal `-5 => ...`). |
| 2026-05-20 | ARC (reclamation step 2) | Both heap descriptors grow an `rc: i64` field. `RuneStr` is now 24 bytes `{ ptr, len, rc }`; `RuneVec` is now 32 bytes `{ ptr, len, cap, rc }`. Heap constructors (`vec_new`, `str_concat`, `str_slice`) init `rc=1`; stack-allocated string literals use `rc=-1` as a sentinel so the runtime helpers no-op on them. New helpers `rune_retain_str/vec` and `rune_release_str/vec` in both the Rust JIT runtime and the AOT C runtime; release decs, dealloc's at zero, no-ops on the sentinel. **Codegen tracks "owned ARC locals" per scope.** A `let x: ARC = init` registers `x` iff `init` is NOT a `HirExprKind::Local` (i.e., the rhs is a fresh +1 producer — constructor call, str concat, etc.). At each block's scope exit, release every local pushed since the snapshot. At `return`, if the return value is a `Local` of ARC type emit retain first (caller gets +1), then release all locals. Block tail expressions follow the same retain-on-`Local` rule. **`free(x)` is removed** — supersedes by ARC; the resolver no longer interns it. **Known limitation in v0.x**: `let y = x;` between two ARC locals aliases without retaining — `y` is never released and the underlying alloc gets dropped to zero once `x` releases. Don't do this until ARC-on-copy lands. Stress test: 100k iterations of `let v = vec_new(); ... ` in a `while` loop stays at flat RSS. |
| 2026-05-20 | Parser precedence: postfix binds tighter than unary | Fix carried over from session 016. `parse_unary` previously wrapped its inner expression in the `Unary` node before the outer postfix loop could apply postfix operators. Result: `!f(x)` parsed as `(!f)(x)`, `-x[0]` as `(-x)[0]`. Fix: apply postfix to the inner expression *inside* parse_unary (`parse_postfix_chain` helper), then wrap. Now `!f(x) == !(f(x))`, `-x[0] == -(x[0])`, `!a.b == !(a.b)`. |
| 2026-05-20 | Char literal codegen | New `HirLit::Char(char)` variant; lower from `ast::Lit::Char` (previously fell through to `HirLit::Unit`). Codegen emits `iconst.i32(codepoint)`. Char pattern literals (`'A'`) reuse `HirPattern::IntLit` with the codepoint cast to i64 — works because the scrutinee's cranelift_type is `I32` for `Ty::Char`, so iconst+icmp narrow correctly. Unlocks char-arg functions and `c as i64`. |
| 2026-05-20 | `as` cast codegen | New `HirExprKind::Cast { expr }`; lowerer produces from `ast::Expr::Cast` (was previously `Unsupported`). Codegen dispatches by `(src_ty, dest_ty)` pair: same-size int → no-op; widening int → `sextend` / `uextend` per source signedness; narrowing → `ireduce`; int→float → `fcvt_from_sint/uint`; float→int → `fcvt_to_sint_sat/uint_sat` (saturating); float→float → `fpromote` / `fdemote`; int/char→bool → `icmp NE 0`; bool→float via i32. Char is treated as integer-shaped (i32). int→bool stays rejected by the checker, matching Rust. |
| 2026-05-20 | ARC-on-copy | Fixes the v0.x limitation from the previous ARC session. `let y: ARC = x` (where `x` is a Local of an ARC type) now **retains** `x` before binding, so `y` owns its own +1. The `init.kind == Local` discrimination on `let` is preserved — fresh +1 producers (constructors, concat, slice) still skip retain. Assignment `x = y` becomes "retain `y` if borrowed, release old `x`, store `y`" — old binding's ref is properly dropped before new binding takes its place. Compound assign `s += s2` (str concat) releases the old binding since `+` on strings always returns a fresh +1. The "don't copy ARC locals" warning from session 018 is now retracted. |
| 2026-05-20 | ARC for struct fields | Struct types containing ARC-managed fields (Vec, Str, or another such struct, transitively) participate in ARC. New `HirModule::struct_arc_fields: HashMap<SymbolId, Vec<(u32, Ty)>>` lists each ARC field's offset+type per struct; the lowerer computes it from `CheckResults::struct_layouts` with a small fixed-point pass to handle struct-of-struct cases. Codegen's `is_arc_type(ty)` and `emit_arc_call(action, ty, value)` are now struct-aware: a `Ty::Struct` call walks the listed fields, loads each, and recursively retain/release's. `compile_struct_lit` retains Local-of-ARC field initializers (per-field "is rhs a Local" rule). `compile_field_assign` releases the old field, retain the new if borrowed. Stress test: 100k iterations of `let h = Holder { v: vec_new() };` keeps RSS flat. Returning a struct still isn't supported by codegen (stack descriptor doesn't escape the frame), so the struct-return ARC path is dead code today. |
| 2026-05-20 | Weak references (design only) | Documented design; **no implementation this session**. The standard Arc/Weak split needs a "control block" layout with both `rc` (strong) and `weak_count` fields, plus the protocol "strong refs collectively count as 1 weak". Strong → 0 deallocs the payload; final weak deallocs the descriptor. The blocker is the user-facing API for `Weak<T>.upgrade()`: without generics it can't return `Option<T>`. The two workarounds (raw nullable pointer; `is_alive()` predicate without upgrade) are both ergonomic regressions vs. the eventual generic form. Decision: **defer until generics + `Option<T>` land**, then implement Weak with a clean `upgrade() -> Option<T>`. Until then, document that ARC cycles leak. |
| 2026-05-20 | Generics step 1 (parser) | Parser accepts `<T>` and `<T, U>` after item names (`fn`, `struct`, `enum`) and after type-position paths (`Vec<i64>`, `Result<i64, str>`). AST gains `generics: Vec<Ident>` on FnDecl/StructDecl/EnumDecl and `generic_args: Vec<Type>` on Path. Resolver creates `SymbolKind::TypeParam` for each generic parameter; the body scope sees them as types. Checker resolves them to a new `Ty::TypeVar(SymbolId)` (opaque). Codegen errors via the existing `cranelift_type` fallback when a TypeVar reaches it — **monomorphization (step 2) is not in this session**, so declaring a generic function works but calling it errors at codegen. The `<` token stays unambiguous at expression position because parse_path no longer eagerly consumes `<...>`; only parse_type does. |
| 2026-05-20 | Payload-bearing enum variants | Tuple variants land: `enum Opt { Some(i64), None }`. v0.x supports single-payload tuple variants only — `Pair(T, U)` is rejected with `multi-field tuple-variant destructuring not supported`. Resolver records `enum_variant_payloads: HashMap<SymbolId, Vec<Type>>` and `enum_has_payload: HashSet<SymbolId>` (any variant with payload). Layout: enums with at least one payload variant use a heap-allocated 24-byte `{ tag, payload, rc }` descriptor — RuneEnum. Unit variants of such enums use the same shape with payload=0. Tag-only enums (no payload variants) keep the i64-discriminant representation. New runtime helpers `rune_enum_new(tag, payload)`, `rune_retain_enum`, `rune_release_enum`. Construction `Variant(arg)` lowers to `HirExprKind::EnumPayloadCtor`; destructuring `Variant(x) => ...` adds `ast::Pattern::TupleVariant` + `HirPattern::EnumPayload` with single-field binding. ARC: the enum descriptor is ARC-managed (`is_arc_type(Ty::Enum) → true` when has-payload). **Limitation**: the descriptor's payload is opaque to the release helper — if you stuff a Vec into `Some(v)` and the Some descriptor drops, the Vec leaks (no per-variant destructor walk yet). Destructure the value out first if you care. |
| 2026-05-20 | Struct return-by-value | Structs are now heap-allocated by `compile_struct_lit` via a new `rune_struct_new(size)` runtime helper. Field layout unchanged (8-byte padding per field). The descriptor escapes the callee's frame, so `fn make() -> Point { Point { x: 1, y: 2 } }` works. ARC field tracking is unchanged — fields are retained on construction (Local-of-ARC rule), released by `emit_arc_call("release", Ty::Struct(...), value)` walking the per-struct ARC field map at scope exit. **Limitation**: the descriptor bytes themselves leak — there's no struct-level rc yet, only the ARC fields are tracked. For non-ARC structs (e.g., `Point { x: i64, y: i64 }`) this is pure leak per construction; for ARC structs the descriptor leaks but fields are correctly reclaimed. Adding struct-level rc + dealloc is a future cleanup. |
| 2026-05-20 | Struct descriptor rc + dealloc | Closes the v0.x struct leak. All user-defined structs now carry an `rc: i64` at offset `size` (the field-area size from the layout); `rune_struct_new(size)` mallocs `size + 8` and inits rc=1. New companion `rune_struct_dealloc(ptr, size)` frees the descriptor. is_arc_type returns true for **every** Ty::Struct. Per-struct release functions are synthesized at module compile time (`__rune_release_struct$<sym>`): decrement rc, on zero walk the ARC fields (via the same `struct_arc_fields` map as before) releasing each, then call struct_dealloc. emit_arc_call(retain, Ty::Struct) inlines `rc++`; release dispatches to the synthesized function. The function declarations are added in a new compile_module pass-0 so structs with nested struct fields can call each other. Stress test: 100k iterations of `let p = Point { x: i, y: i };` keeps RSS flat. |
| 2026-05-20 | Per-variant destructor walks for payload enums | Closes the v0.x payload-enum leak. The synthesized `__rune_release_enum$<sym>` function is generated for every enum in `enum_has_payload`. It loads the tag, switches by discriminant, and for each variant releases the ARC payload fields at their offsets, then calls `rune_struct_dealloc` to free the descriptor (the alloc + dealloc helpers are unified between structs and enums now). For tag-only enums no synthesized function is needed — they're i64 values with no heap descriptor. `rune_release_enum` runtime helper is retained as a fallback but unused in practice. Stress test: `enum Opt { Some(Vec), None }` with 100k iterations of `let v = vec_new(); let o = Opt::Some(v);` releases both the Vec and the descriptor. **Important fix**: `EnumPayloadCtor` now retains a Local-of-ARC payload arg before storing it, matching the struct-lit rule. Without this the descriptor held a non-owning reference and dropping it caused a double-free (Vec released twice). |
| 2026-05-20 | Multi-field tuple variants | Variants can now hold multiple values: `enum Pair { Both(i64, i64), Just(i64), None }`. The whole enum's heap layout sizes to its max-arity variant: `{ tag@0, payload[i]@(8 + i*8), rc@(8 + max_arity*8) }`. `enum_max_arity(sym)` is a codegen helper computed from `HirModule::enum_payload_tys`. Construction stores tag, retains each Local-of-ARC payload, stores at offset (8 + i*8). Destructure loads each payload from its offset and binds to the corresponding `(Ty, Option<SymbolId>)` in `HirPattern::EnumPayload.bindings`. Synthesized release walks the variant's ARC payload positions and releases each. Tested with `Pair::Both(3, 4)` and `Triple::T(1, 2, 3)`. **Named-field variants** (`Ok { value: T, err: E }`) are still parser-accepted but downstream-rejected; deferred to a follow-up session. |
| 2026-05-20 | Named-field enum variants | `Variant { name: val, ... }` construction (parsed as `Expr::StructLit`) now dispatches to a named-variant constructor when the path resolves to an EnumVariant symbol. New `ast::Pattern::NamedVariant { path, fields: Vec<(Ident, Pattern)> }` for destructure (with shorthand `Variant { x }` binding `x` directly). Resolver populates `enum_variant_payloads` from `VariantFields::Named` field types and `enum_variant_field_names` with the per-variant name list. Checker validates field names match the variant's declared names (rejects unknown, duplicate, missing fields). Lowerer reorders fields into declaration order before emitting `EnumPayloadCtor` / `EnumPayload`, so both `{ x: 3, y: 4 }` and `{ y: 4, x: 3 }` produce the same payload layout. Codegen reuses the existing tuple-variant machinery; the named/positional distinction disappears at the HIR level. |
| 2026-05-20 | Generics step 2 — monomorphization | New `src/monomorphize.rs` pass runs between the lowerer and codegen. Each `Call` to a generic function infers concrete types from value arg types (positional unify: each `TypeVar(t)` on the param side binds to the arg's concrete). The pass clones the generic HirFn with type substitution applied to params, return type, and the entire body (`subst_ty` / `subst_block` / `subst_expr` recursive walk). Specialized functions get fresh `SymbolId`s allocated past the resolver's max sym and mangled names like `id$$i64` / `pair$$i64$$str`. The instantiation cache `(SymbolId, Vec<Ty>) → SymbolId` keys further requests. Bodies of specialized functions are walked for nested generic calls — the worklist drains transitively. Call sites in concrete functions are rewritten to point at the specialized sym before codegen. The original generic HirFns are removed from the module (their bodies still mention TypeVar and would fail codegen). `Ty::compatible` was relaxed to treat `TypeVar` as compatible with anything, so the checker accepts generic uses without needing trait constraints. Checker's `check_call` does light TypeVar substitution on the return type so the call's apparent result type is concrete. **Constraints:** functions only — generic structs/enums aren't specialized this session (see the next row for partial support); no turbofish, no traits, no HKT. |
| 2026-05-20 | Generic struct field types (partial) | Generic structs (`struct Box<T> { value: T }`) parse, resolve, and check end-to-end. Construction (`Box { value: 5 }`) lowers correctly with the field stored at its 8-byte slot. Field access (`b.value`) compiles by treating unresolved `Ty::TypeVar` as `i64` in `compile_field_access` and `compile_field_assign` — works for all i64-shaped types (i64, str pointer, Vec pointer, struct pointer, enum descriptor pointer). **Limitations** (require carrying type args on `Ty::Struct` itself — bigger refactor): `+`/`-`/method calls on a `b.value` whose type is still `TypeVar` fail at the checker stage; passing a generic struct to a generic function can't infer T from `Ty::Struct(box_sym)` alone. Workaround: stick to i64-sized concrete fields or restructure to avoid post-access operations on TypeVar values. |
| 2026-05-20 | Full generic struct + enum types | `Ty::Struct(SymbolId)` and `Ty::Enum(SymbolId)` grow into `Ty::Struct(SymbolId, Vec<Ty>)` and `Ty::Enum(SymbolId, Vec<Ty>)`. Type args are populated by the resolver from `Path::generic_args` at type position and inferred at construction sites: `Box { value: 5 }` produces `Ty::Struct(box_sym, [i64])`, `Some(5)` produces `Ty::Enum(option_sym, [i64])`. Field-access lowering substitutes the field's `TypeVar` using `build_struct_subst(struct_sym, use_args)`; match-arm payload bindings substitute using the scrutinee's enum args. The monomorphizer's `unify` recurses into `Struct/Enum` args so `unbox<T>(b: Box<T>)` infers T=i64 from `b: Box<i64>`. `Ty::compatible` treats `Struct/Enum` with matching syms as compatible regardless of args (variant-construction sites still emit `Vec::new()` for the placeholder). Resolutions gains `struct_generics` and `enum_generics` maps tracking each item's generic-param symbols. **What this unlocks**: `Option<T>`, `Result<T, E>`, method calls on generic struct fields (`b.value.len()`), arithmetic across generic fields, generic functions taking generic structs/enums. **What's still TODO**: nothing fundamental at this layer; subsequent work moves to `Weak<T>` (now buildable), traits/bounded generics, and stdlib-level types built on these primitives. |
| 2026-05-20 | `Weak<T>` reference counting | Cycle-breaking weak refs. v0.x supports `Weak<Vec>` only; other inner types parse but error at codegen with a clear message. RuneVec grows `weak_count: i64` (40 bytes total). Initial state on `vec_new`: `rc=1`, `weak_count=1` — the strong refs collectively count as one weak. Four new runtime helpers: `rune_weak_downgrade_vec` (increments weak_count), `rune_weak_retain_vec` / `rune_weak_release_vec` for ARC-on-copy of Weak locals, `rune_weak_upgrade_vec` for the underlying try-promote, and the convenience `rune_weak_upgrade_or_vec(w, default)` that returns either a retained strong ref or a retained default. `rune_release_vec` was updated: when rc hits 0, dealloc the element array AND call `weak_release_vec` to drop the "all strong refs share one weak" slot — the descriptor only goes away when the last `Weak<Vec>` releases. New `Ty::Weak(Box<Ty>)`; new polymorphic builtins `weak(v) -> Weak<T>` and `upgrade_or(w, default) -> T`. `Weak` is a special builtin name the checker recognizes in `resolve_type` — `Weak<i64>` parses as `Ty::Weak(Box::new(Ty::Int(I64)))`. `is_arc_type` and `arc_helper_name` are extended for `Ty::Weak(_)` so scope-exit auto-release goes through the weak helpers, not the strong ones. **Limitation**: `Weak<Str>`, `Weak<Struct>`, `Weak<Enum>` rejected at checker. The control-block split for those types would mirror Vec; not done this session because the v0.x leak-tolerance argument from earlier sessions doesn't apply to weak refs (cycles require Weak, which requires control blocks per-type). |
| 2026-05-20 | Traits + bounded generics | Static-dispatch traits. New `trait` keyword; `ast::Item::Trait(TraitDecl)` holds method signatures (bodies-less); `ImplBlock` gains `trait_path: Option<Path>` so `impl Trait for Type` parses; `ast::GenericParam { name, bounds }` replaces the bare `Vec<Ident>` of generic params, parsing `<T: A + B>`. Resolver: `SymbolKind::Trait`; `Resolutions::trait_methods` (trait sym → sigs) and `generic_bounds` (param sym → bound trait syms); trait-impl methods register into the existing `impl_methods` table. Checker: `check_trait_impl_conformance` (every trait method has a matching impl, arity-checked); `trait_bound_method_sig` resolves `x.fmt()` where `x: T` and `T: Display` by searching `T`'s bounds. Monomorphizer: trait method calls on a generic receiver survive lowering as `HirExprKind::MethodCall`; after a generic fn is specialized for a concrete type, `resolve_method_calls` rewrites each `MethodCall` on a now-concrete struct/enum receiver into a direct `Call` (via `HirModule::impl_methods`). Codegen sees only `Call` for trait methods. **Static dispatch only** — no vtables, no `dyn Trait`. Open: supertraits, associated types, generic impls (`impl<T> Trait for Box<T>`), full param-type conformance (today arity-only). |
| 2026-05-20 | Module system (inline) | Inline modules: `mod name { items... }`, nestable. New `mod`/`use` keywords; `ast::Item::Mod(ModDecl)` and `Item::Use(UseDecl)`. The resolver flattens everything into the global namespace under module-qualified keys (`a::b::f`) — no separate per-module map. `current_path: Vec<String>` tracks module nesting through all three resolver passes (declare, declare-impls, resolve-bodies) plus a new pass 1.7 that resolves `use` aliases. `lookup` for a bare name tries each enclosing module prefix longest-first then root; `lookup_path` resolves multi-segment paths absolutely then relative. `resolve_path` first tries the whole path as a qualified item, then falls back to `Enum::Variant` (the leading segments naming the enum, possibly module-qualified). Functions get module-mangled codegen names (`a__b__f`) so same-named functions in different modules do not clash as Cranelift symbols; root `main` keeps its bare name. Impl methods inside a module mangle with the module prefix too. Checker and lowerer recurse into `Item::Mod`. **Not done**: file-based modules (`mod name;` loading a file), visibility enforcement (`pub` is parsed but any item is reachable by qualified path), `use` globs / renaming. |
| 2026-05-20 | Stdlib prelude | The standard library is a `mod std { ... }` written in Rune itself (`src/std.rn`), embedded into the compiler via `include_str!` as `rune::PRELUDE` and prepended to every program by `rune::with_prelude` before lexing. With traits and the module system already shipped, the prelude is plain Rune compiled by the same pipeline as user code — no special-casing. The compile commands (`check`/`run`/`build`) and both test harnesses (`run_main`, typecheck `run`) operate on the combined source; the debug commands (`tokens`/`ast`) stay on the user file only. v0.x prelude contents: `Option<T>` and `Result<T, E>` enums; generic helpers `unwrap_or` / `is_some` / `is_none` / `ok_or` / `is_ok` / `is_err`; concrete i64 helpers `min` / `max` / `abs` / `clamp`. The generic helpers are zero-cost when unused — the monomorphizer only emits called specializations and drops the generic originals — so a program that touches no `std::` generic pays nothing; the concrete helpers compile into every binary. **Monomorphizer fix shipped alongside**: `subst_expr_kind`'s `Match` arm cloned arm patterns without type-substituting them, so a specialized generic function's `match` arms kept `TypeVar(T)` binding types in `HirPattern::EnumPayload` and codegen rejected them (`type T#NN not supported`). New `subst_pattern` helper substitutes the binding `Ty`s through `subst_ty`. **Still hardcoded**: `print`, the i64-only builtin `Vec`, ARC primitives `weak`/`upgrade_or`. **Not done**: file-based modules for an external (non-embedded) stdlib, a generic `Vec<T>`, the `?` operator. Byte offsets in user-code errors are shifted by the prelude's length — a known rough edge. |
| 2026-05-20 | Generic `Vec<T>` | The builtin `Vec` becomes generic over its element type. `Ty::Vec` grows into `Ty::Vec(Box<Ty>)`; the checker reads the element type from a `Vec<T>` path (rejecting `str`, floats, and arrays — they don't fit the 8-byte element slot) and types `push`/`get` off it. `vec_new()` is a no-arg builtin so it yields a placeholder `Vec<i64>` that an annotated binding refines; `Ty::compatible` treats any `Vec` as compatible with any `Vec` (the same rule as `Struct`/`Enum` regardless of type args), and `HirLet.ty` is the binding's declared type so codegen drives every Vec operation off that. The runtime descriptor and `vec_new`/`vec_push`/`vec_get`/`vec_len` helpers are unchanged — elements live in 8-byte slots, narrow scalars widened on push / narrowed on get. **Per-element ARC release**: after monomorphization (every type concrete) `collect_vec_arc_elems` gathers the distinct ARC-managed `Vec` element types — transitively, so `Vec<Vec<S>>` records `Vec<S>` and `S` — into `HirModule::vec_arc_elem_tys`; codegen synthesizes a `__rune_release_vec$<elem>` per entry that, when the strong count is about to hit zero, walks the live elements releasing each, then hands off to the runtime `release_vec`. `push` of a borrowed (`Local`) ARC element retains it; `get` of an ARC element retains the returned copy. Exposed as `std::Vec` / `std::vec_new` — the resolver aliases the builtin under `std::` keys, the lowerer emits the `BuiltinFn`'s runtime name so the alias still calls the `vec_new` helper; the bare `Vec` / `vec_new` stay. **Parser**: `expect_generic_close` splits a `>>` (`Shr`) token in place so `Vec<Vec<i64>>` and `Weak<Vec<i64>>` parse. **Not done**: `Vec` is still a compiler builtin, not Rune source (Rune has no raw-memory primitives — pointers, `alloc`, `unsafe`); `str`/float/array element types are rejected; a generic `Vec<T>` instantiated at a rejected type isn't re-validated post-monomorphization; no `vec![...]` literal syntax. 405 tests green (+11 from session 027: 7 codegen, 4 typecheck). |
| 2026-05-20 | File-based modules | `mod name;` (no body) loads `name.rn` and splices its items in as an inline `mod name { ... }`. Expansion is a token-stream transformation (new `src/modules.rs`) that runs between lexing and parsing: `expand_modules` scans the token stream for `mod IDENT ;`, loads the file via a `loader` callback, lexes it, and rewrites the three tokens to `mod IDENT { <loaded tokens> }`. The parser, resolver, checker, and lowerer are entirely unchanged — they only ever see inline modules. Each loaded file is lexed into a fresh, disjoint slice of the global byte-offset space (token + lex-error spans shifted by a base offset past every prior file) so spans stay globally unique — the resolver and checker key `HashMap`s on `Span`, and two independently-lexed files would otherwise collide at low offsets. A `SourceMap` records each file's `label: start..end` range; the driver prints it as a note when a multi-file program has errors, since error offsets are now global. New `ModuleError` category for a missing file or an import cycle (a load-stack catches `a → b → a`). The driver's `loader` reads `<main-file-dir>/<name>.rn`; the test harnesses use an in-memory `(name, source)` map so multi-file tests need no temp files (`run_main_files`, `run_files`). v0.x: module files are flat — `mod foo;` always resolves to the main file's directory regardless of nesting depth; loaded modules see the prelude's `std::` items through the shared global namespace (the prelude is prepended to the main source only). **Not done**: nested module directories / per-file relative paths, visibility enforcement, `use` globs. 412 tests green (+7 from session 028: 4 codegen, 3 typecheck). |
| 2026-05-21 | Module system polish | Three module-system features. **Nested directories**: `mod foo;` in the main file loads `foo.rn` beside it, and a `mod bar;` inside a loaded `foo.rn` loads `foo/bar.rn` — `modules.rs`'s expander threads a `/`-terminated directory prefix so module paths are `/`-joined. Because `mod` always descends, file modules form a tree — import cycles are now structurally impossible, so session 029's load-stack cycle check is replaced by a depth cap against a pathological loader. **`use` globs**: `use m::*;` aliases every visible direct item of `m` into the using module. New `UseDecl.glob` flag; `parse_use` parses the path by hand so a trailing `::*` doesn't trip `parse_path`; the resolver enumerates `scopes[0]` keys under the module's qualified prefix and aliases each with `entry().or_insert` so an explicit `use` or local item wins over a glob. **`pub` enforcement**: the resolver records per-symbol `(declaring module path, is_pub)` in an `item_vis` table; `is_visible(sym)` is `is_pub || current_path.starts_with(decl_module)` — a non-`pub` item is reachable only from its declaring module and that module's descendants. Checked in `resolve_path` (the final symbol of every path, plus the `Enum::Variant` fallback) and in `resolve_uses`; the glob filters by it too. Variants inherit the enum's visibility; builtins/locals are absent from the table and always visible. v0.x checks only the path's final symbol — an intermediate private module isn't caught. **Ripple**: every item in `std.rn` is now `pub` (the prelude's `mod std` is referenced cross-module from user code), and session 026's module tests gained `pub` on their cross-module items. 420 tests green (+8 from session 029: 2 codegen, 6 typecheck). |
| 2026-05-21 | Module refinements: use-as, pub use, per-segment privacy | Three module-system refinements. **`use x as y`**: `UseDecl` gains `alias: Option<Ident>`; `parse_use` parses an optional `as ident` after a non-glob path; `resolve_uses` binds the import under the alias name instead of the path's last segment. **`pub use` re-exports**: `UseDecl` gains `vis: Visibility`; a `pub use` records its alias key in the resolver's `pub_reexport_keys` set, and a path that resolves to (or through) such a key skips the privacy check — so a module can re-export even an otherwise-private item under its own namespace. **Per-segment privacy**: `lookup_path` now returns the matched global-namespace key alongside the symbol; `check_path_visibility` walks every module prefix of that key — `a`, `a::b`, `a::b::c` — checking each with `is_visible`, so a private *intermediate* module is caught, not just a private final item (session 030 checked only the final symbol). The check runs in `resolve_path` (the direct-item branch and the `Enum::Variant` fallback's type path) and in `resolve_uses` (you can only `use` a path you can see); a `pub use` key short-circuits it. 427 tests green (+7 from session 030: 2 codegen, 5 typecheck). |
| 2026-05-21 | `?` operator | `expr?` for ergonomic `Result` propagation. The parser already produced `ast::Expr::Try`; this session type-checks and lowers it. **Checker** `check_try`: the operand must be a `Result`-shaped enum — `Ty::Enum(s, [T, E])` with `Ok`/`Err` variants — and the enclosing function must return a `Result` with the same enum and a matching error type; `expr?` then has type `T`. **Lowerer** `lower_try` desugars it to `match expr { Ok(v) => v, Err(e) => return Err(e) }` — a `HirExprKind::Match` built directly, with fresh binding symbols allocated past the resolver's max via a `Cell<u32>` counter on the `Lowerer`. The resolver, monomorphizer, and codegen need no `?`-specific code — it's a desugar to existing constructs. Two supporting fixes: (1) the monomorphizer's `walk_expr_collect_syms` was incomplete (missed `Match`/`Return`/most expr kinds), so the synthetic binding syms weren't counted toward the fresh-sym base — rewritten to be exhaustive; (2) `compile_match` rejected a diverging arm — a `return` arm body leaves a fresh unreachable block so `is_filled()` reads false — now an arm whose body type is `Ty::Never` terminates that block with a trap and contributes no merge value (this also fixes any `match` with a `return` in an arm). **Not done**: `?` error-type *conversion* (a `From`-style coercion) — the propagated and declared error types must match exactly. 433 tests green (+6 from session 031: 2 codegen, 4 typecheck). |
| 2026-05-21 | `dyn Trait` dynamic dispatch | Trait objects. New `dyn` keyword; `ast::Type::Dyn(Path)`; `Ty::Dyn(trait_sym)` — at runtime an 8-byte pointer to a heap cell `[fnptr_0, .., fnptr_{N-1}, data]` (the trait's method pointers — a per-instance method table — then the concrete data pointer). **Coercion**: a concrete struct that implements trait `T` coerces to `dyn T` at `let` / call-argument / `return` sites — the checker's `check_assignable` verifies the struct provides every trait method (`struct_impls_trait`) and records the coercion in `CheckResults::dyn_coercions`; the lowerer wraps the expression in `HirExprKind::DynBox`. **Dispatch**: a method call on a `dyn` receiver lowers to `HirExprKind::DynCall`; codegen loads the method pointer (slot `index`) and data pointer (slot `N`) from the box and emits a `call_indirect` with a signature built from the argument and result types. `DynBox` codegen heap-allocates the cell, takes each impl method's address with `func_addr`, and stores the data pointer; `HirModule::trait_methods` (ordered method names per trait) drives the table layout. The monomorphizer leaves `DynBox`/`DynCall` alone — all six expr-walk passes gained arms. **A per-instance method table was chosen over a shared data-object vtable** to avoid a first-time use of `DataDescription::write_function_addr`; the table is rebuilt at each coercion. **Not done**: the `dyn` box and the concrete value it wraps are never freed (a v0.x leak — no ARC for trait objects); coercion fires only at `let`/call-arg/`return`, not in `Vec` or other nested positions; object safety isn't enforced beyond "the impl exists". 439 tests green (+6 from session 032: 3 codegen, 3 typecheck). |
| 2026-05-21 | ARC for trait objects | Trait-object boxes now reclaim. The `dyn` box layout gains a **drop slot**: `[fnptr_0..fnptr_{N-1}, data, drop, rc]` — the N method pointers, the concrete data pointer, a function pointer to the boxed struct's synthesized release, and the ARC refcount (`struct_new` appends rc as before; the field area grows to `(N+2)*8`). `is_arc_type(Ty::Dyn) → true`, so a `dyn` local becomes a scope-tracked ARC local — released at scope exit, retained on copy (`let b = a`), retained when returned, exactly like a Vec/Str/struct local. A per-trait release function `__rune_release_dyn$<trait>` is synthesized (declared in `compile_module` pass 0, defined in pass 3, mirroring the struct/enum/Vec release functions): decrement the box rc; at zero, load the data and drop pointers and `call_indirect` the drop slot — the concrete struct's `__rune_release_struct$<sym>`, which reclaims the struct body and its ARC fields — then `struct_dealloc` the box itself. `compile_dyn_box` stores `func_addr` of the struct's release fn into the drop slot, and retains the boxed data when the coerced value is a borrowed `Local` read: the box owns a +1 it will later drop, and a fresh struct literal already carries that +1 while a borrow does not — the same heuristic as the `let` ARC-on-copy rule. `compile_dyn_call` is unchanged — data still lives at slot N, methods at `0..N`, the drop slot sits past data. **Entirely codegen + runtime** — no front-end change (`dyn` was already wired through the parser/resolver/checker/lowerer/monomorphizer in session 033). **Not done**: a `dyn` temporary passed as a call argument still leaks — the pre-existing convention that *every* ARC call-argument temporary leaks (callee parameters are borrowed, not owned), not specific to `dyn`; the method table remains per-instance rather than a shared static vtable. 442 tests green (+3 from session 033: 3 codegen). |
| 2026-05-21 | `Vec<dyn Trait>` | Heterogeneous trait-object collections. Three changes — sessions 033 (`dyn`) and 034 (`dyn` ARC) had already built the rest. **Checker**: `vec_element_supported` admits `Ty::Dyn` (a trait object is an 8-byte box pointer, so it fits Vec's 8-byte slot); `check_method_call` checks each argument with `check_assignable` instead of bare `Ty::compatible`, so a concrete struct argument coerces to a `dyn Trait` parameter — `v.push(Circle { .. })` on a `Vec<dyn Shape>` — and the coercion lands in `dyn_coercions`. `check_assignable` is a strict superset of `compatible` (it tries `compatible` first), so no existing method call changes behaviour; the only new acceptance is `struct → dyn`. Every method argument is now a `dyn`-coercion site. **Monomorphizer**: `is_arc_mono` admits `Ty::Dyn`, so `scan_ty_for_vec_elems` records `dyn` element types and codegen synthesizes `__rune_release_vec$dyn<N>` — releasing a `Vec<dyn Shape>` walks its elements through `emit_release_field(Ty::Dyn)` → `__rune_release_dyn$<trait>` → `__rune_release_struct$<sym>`, a three-layer reclaim. **No parser / resolver / HIR / lowerer / codegen change**: `lower_expr` already applies `dyn_coercions` by span for every expression (a method argument is wrapped in `DynBox` for free), and `compile_method_call`'s Vec arm was already generic over the element type via `cranelift_type` / `is_arc_type` / `elem_size`, all of which gained `Ty::Dyn` arms in 033/034. `push` of a fresh `DynBox` transfers its `+1` into the slot; `push` of a `dyn` local retains; `get` retains the box it returns. **Not done**: coercion still misses struct-literal field and enum-payload positions (they don't run `check_assignable`); a `get(i)` result used inline rather than bound to a `let` leaks — the call-argument/temporary leak class. 447 tests green (+5 from session 034: 3 codegen, 2 typecheck). |
| 2026-05-21 | Owned call arguments | Call-argument temporaries reclaim. Rune's calling convention is *borrowing* — a parameter is never scope-tracked, retained, or released by the callee — which is correct for a value the caller keeps owning (`use_it(v)` where `v` is a local) but leaks a value it does not (`use_it(make_vec())`, `describe(Circle { .. })`): the fresh `+1` is handed off, borrowed, then owned by nobody. The fix is caller-side — after a `HirExprKind::Call`, the caller releases each argument that is a fresh ARC temporary, i.e. every ARC argument except a borrowed `Local` read (the same fresh/borrowed split already used by `let` ARC-on-copy, `compile_dyn_box`, `push`, and struct-field assignment). The callee is unchanged (still borrows); the feature is purely additive caller-side cleanup. It composes: a function returning an ARC value always returns it `+1`-owned (the tail-escape retain), so the result SSA value is independent of any argument value and releasing an argument cannot free the result; and struct construction retains a `Local` field initializer, so storing a borrowed parameter into a returned struct still nets out. **Scope**: regular `Call` only — not `MethodCall` (`Vec::push` *consumes* its argument; releasing it would double-free the element slot), not `BuiltinCall` (`weak`/`upgrade_or` are ARC-subtle), not `DynCall`. The headline leak — a `dyn` argument to a free function — is a regular `Call`, so it is covered. **Known imperfection**: the `Local`-vs-not heuristic classifies `foo(s.arc_field)` as a fresh temporary though a field read is a borrow (`compile_field_access` does not retain), so it over-releases — a pre-existing flaw (`let x = s.arc_field` mis-tracks identically); the real fix is ARC field/index reads that retain, a follow-up. 450 tests green (+3 from session 035: 3 codegen). |
| 2026-05-21 | ARC field / index reads that retain | Reading an ARC value out of a struct field or an array element now retains it. The codebase's fresh/borrowed rule — a `Local` read is borrowed, everything else is a fresh `+1` — drives `let` ARC-on-copy, owned call arguments, struct construction, `push`, and `return`. But `compile_field_access` and `compile_index` loaded the value without retaining, so `s.arc_field` and `arr[i]` returned a *borrow* that every consumer, going by the rule, treated as a fresh `+1` — a latent double-free wherever an ARC field/element is read (`let x = s.arc_field`, `foo(s.arc_field)`, `return s.arc_field`; none tested, since each is a deterministic crash). The fix is one `is_arc_type` → `emit_arc_call("retain", ..)` guard after the load in each of the two functions: a `Field`/`Index` read now genuinely produces a fresh `+1`, the rule's assumption holds, and every consumer is correct *without changing the consumers*. `return` needs nothing — its retain is already gated to a `Local` operand, and a `Field`/`Index` return now carries its own `+1` from the read. This closes the `foo(s.arc_field)` over-release flagged as a known imperfection in session 036. **Cost**: a field/element read used as a method-call *receiver* (`s.field.len()`) now retains a temporary nothing releases — the receiver-temporary leak class (a leak, not a crash; v0.x tolerates leaks, not double-frees). Arrays additionally leak their elements regardless — `compile_array` never establishes ARC ownership and `Ty::Array` is not ARC-managed; the `compile_index` retain only makes the read memory-safe. **Not done**: receiver-temporary cleanup; enum-payload binding reads (the `match`-destructure analog). 453 tests green (+3 from session 036: 3 codegen). |
| 2026-05-21 | Receiver-temporary cleanup | A method-call receiver that is a fresh ARC temporary is now released by the caller after the call — the receiver-position mirror of session 036's owned call arguments. A method only borrows its receiver (`self` is never scope-tracked), so a fresh `expr.method()` receiver — a call result, a field/index read (retained since session 037), a `dyn` box — owns a `+1` that nobody reclaims once the call returns. The fix: after a `MethodCall`/`DynCall`, `release_receiver_temp` releases the receiver unless it is a borrowed `Local` (the same fresh/borrowed split as owned call arguments). To place the release after the call without threading cleanup through `compile_method_call`'s several early-returning arms, receiver compilation moved *out* of `compile_method_call`/`compile_dyn_call` and into the `compile_expr` arms, which compile the receiver, pass its `Value` to the helper, and release it afterward. Safe because a method never *consumes* its receiver (unlike `Vec::push`'s argument — the reason session 036 excluded method-call arguments; the receiver has no such exception), and the result is independent of the receiver (a builtin method returns a scalar or a `get`-retained element; a `dyn` method returns a `+1`-owned value). Composes with session 037: `s.field.len()` retains the field read, then releases it after the call — net zero. User/trait methods need nothing — the monomorphizer rewrites them to a direct `Call` with the receiver as the `self` argument, already reclaimed by session 036; at codegen a `MethodCall` is only a builtin (`str`/`Vec`) method and `DynCall` is trait-object dispatch. **Remaining leak classes**: enum-payload bindings, `BuiltinCall` arguments, and discarded expression-statement temporaries. 456 tests green (+3 from session 037: 3 codegen). |
