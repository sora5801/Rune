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

## Traits (design pass)

**Status: Open.** Not implemented; sized for a multi-session feature
of its own.

The motivation: bounded generics. Today `fn print<T>(x: T)` can
take any T, but the body can do nothing T-specific — there's no way
to say "T must support `.fmt()`". Traits fix this by attaching a
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

Implementation sketch:

1. **Parser**: `trait Name { fn ...; ... }` declares; `impl Trait
   for Type { ... }` implements; `T: TraitName` (and `T: A + B`) at
   generic bound sites.
2. **Resolver**: `SymbolKind::Trait` and `SymbolKind::TraitImpl`. A
   per-(trait, type) impl table.
3. **Checker**: bounded generic params type-check the body using
   the trait's declared method signatures. Calls on a bounded T
   resolve to the trait method (signature-checked but not yet
   dispatched).
4. **Monomorphization**: at each call site, instantiate the generic
   with a concrete type AND look up the trait impl for that type.
   The specialized body has method calls rewritten to direct calls
   into the impl's function.
5. **Optional**: dynamic dispatch via vtables — `Box<dyn Display>`
   etc. Not needed for the static case but useful for collections
   of mixed types.

The static-dispatch (monomorphized) path is the v0.x choice if/when
this lands. Dynamic dispatch can come later.

Blockers / open questions:
- Coherence: is `impl<T> Display for Vec<T>` allowed if another
  crate also impls `Display for Vec<i64>`? Rust's orphan rule
  handles this; Rune doesn't have crates yet.
- Trait inheritance / supertraits: `trait Ord: Eq { ... }`.
- Associated types (`type Item`) and constants: deferred.

## Stdlib (design pass)

**Status: Open.** Hardcoded builtins today; a real stdlib needs
traits + a module system.

Current "stdlib" surface:
- Primitive types and methods (`str.len`, `vec.push`, etc.).
- Builtin `print` for i64 and str.
- ARC primitives: `weak`, `upgrade_or`.

What a v1 stdlib would look like:

```rune
// std::collections
struct Vec<T> { ... }
impl<T> Vec<T> {
    fn new() -> Vec<T> { ... }
    fn push(self, x: T) { ... }
    fn len(self) -> i64 { ... }
}

struct HashMap<K: Hash + Eq, V> { ... }

// std::option / std::result
enum Option<T> { Some(T), None }
enum Result<T, E> { Ok(T), Err(E) }

// std::io
fn read_line() -> Result<str, IoError> { ... }
```

Blockers:
1. **Traits.** `Vec<T>` is hardcoded to `i64` today because
   we'd need ARC-aware traits to handle T's lifecycle generically.
2. **Module system.** `use std::Vec;` requires a parser for paths,
   a resolver that finds external items, a build system that knows
   where the stdlib lives. None of this exists.
3. **`?` operator** for ergonomic `Result` use. Easy syntactic
   desugar once `Result` is the standard.

Probable rollout order:
- (a) Traits (probably 2-3 sessions).
- (b) Convert builtin `Vec` to a user-written `Vec<T>` in the
  stdlib.
- (c) Module system (one big session).
- (d) `?` operator (small once `Result` is generic).
- (e) Grow stdlib incrementally — `HashMap`, `IO`, iterator
  adapters, etc.

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
