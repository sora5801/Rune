# Rune v0.x Audit

**Date:** 2026-05-25
**Sessions covered:** 1 — 97
**Tests:** 424 codegen + 158 typecheck + 56 lexer + 144 parser + 21 AST + 40 unit (lib) = 843 total green.

A retrospective on the v0.x state: every feature category Rune
ships today, what's deferred, and the pre-1.0 priority list.
Written as the natural waypoint before bootstrap work begins —
the language is roughly complete, the compiler is roughly stable,
and from here on the work shifts from "add features" to "tighten
what's there + write Rune in Rune."

---

## Mission recap

> A small, statically-typed, compiled, general-purpose programming
> language. Native code via Cranelift; compiler written in Rust.
> Leans toward systems use cases (predictable performance, no
> mandatory GC, FFI to C). Not a Rust / Zig replacement —
> different point in the design space.

The original constraints have held: no GC, no async, no macros,
no async runtime, no async-anything. Compile times stay fast
because the language stays small.

---

## What works

### Syntax and parsing

- Rust/Swift-flavored: `fn name(arg: Type) -> Ret { ... }`,
  `let x: T = expr;`, expression-oriented blocks, `match` arms
  with `=>`, `::` paths, ranges (`a..b` / `a..=b` / open-ended
  forms).
- Comparison operators left-associative (intentional v0.x simpler
  default; tight Pratt precedence table).
- Numeric literal suffixes (`10i32`, `42u64`, `3.14f32`,
  `0xffu8`).
- Char literals (`'A'`), string literals with escapes.
- Bitwise ops, shift, modulo.
- Open ranges (`..n`, `n..`).
- `continue` and `break` flow constructs.
- Postfix `?` desugars to `match` against `Result` / `Option`.
- `as` casts: int-int, int-float, float-int (saturating),
  bool-as-int (rejected per Rust), int-as-bool (`x != 0`).

### Type system

- Primitive scalars: `bool`, `char`, `i8/16/32/64`, `u8/16/32/64`,
  `isize`, `usize`, `f32`, `f64`, `str`, `()`.
- Default integer: i64. Default float: f64. Suffixes and hint flow
  (sessions 088, 091, 094, 095, 096) override.
- Structs: positional and named fields, generic params, methods.
- Enums: unit variants, tuple variants (single-and multi-field),
  named-field variants. Generic enums.
- Tuples `(A, B, ...)`: heap-allocated like positional structs.
  Construction, indexing (`.N`), destructuring in `let`, match-arm
  patterns, for-loop patterns.
- `Vec<T>`: heap-allocated `{ptr, len, cap, rc, weak_count}`.
  Element type must fit an 8-byte slot — integers, bool, char,
  structs, enums, dyn, nested Vec. Not str, not floats, not arrays.
- `[T; N]`: heap-allocated arrays. ARC + per-element release.
- `HashMap<i64, V>` and `HashMap<str, V>`: open-addressing with
  tombstones. Key-kind tag dispatches i64 vs str at runtime.
  Per-V release walks synthesized at codegen.
- `Weak<Vec>`: control-block-shared refcount.

### Generics and traits

- Generic functions (`fn id<T>(x: T) -> T`).
- Generic structs / enums with type-args carried on `Ty`.
- Generic impls: `impl<T> Trait for Container<T>`.
- Method-level generics on trait methods: `fn map<F: Fn1<...>, U>
  (self, f: F) -> ...`. Bound-propagation cascade (sessions
  077-080) pins method-level type-params from arg shapes.
- Trait declarations with associated types (`type Item;`) and
  supertraits.
- Trait method bodies (`fn collect(self) -> ... { ... }`).
  Specialized per Self at each call site.
- Bounded generics: `<T: Trait>` with method dispatch via
  `trait_bound_method_sig`.
- Static dispatch via monomorphization; dynamic dispatch via
  `dyn Trait` (heap box + method table). Both supertrait method
  resolution and BFS-ordered flat method tables.
- Intrinsic primitive impls: `impl Numeric for i32 { fn add(...) }`
  (session 087).
- Method dispatch on primitive receivers via per-Ty anchor syms.

### Closures

- Non-capturing closure literals: `|x| x + 1` lowers to an
  anonymous fn item, becomes a `Ty::Fn` value, dispatched via
  `IndirectCall`.
- Capturing closures: synthesize a struct holding the captures
  plus a `call` method, satisfy `Fn1` / `Fn2` traits.
- Bidirectional inference: closure params take their types from
  the surrounding callable-bounded TypeVar (Fn1 / Fn2 bound) or
  a let-binding's fn-pointer annotation. Three converging hint
  sites (session 081, 086): let, fn-arg, struct-field.

### Iterators

- `Iterator` trait with `type Item` and `fn next(self) ->
  Option<Self::Item>`.
- For-loop desugar to `while-true + match next()` with
  `None => break`. For-pat accepts tuple destructuring.
- `Vec<T>` implements via `VecIter<T>`. `RangeIter` for integer
  ranges. `HashMapKeysIter<V>`, `HashMapEntriesIter<V>` for
  HashMap walks.
- Default-body methods on Iterator: `.collect()`, `.count()`,
  `.sum()`, `.min()`, `.max()`, `.filter(p)`, `.map(f)`,
  `.fold(init, f)`. All inherited by every impl through
  session 071's machinery.

### Memory model

- ARC (automatic reference counting) by default. Every heap
  descriptor carries an `rc: i64`.
- Per-struct / per-enum / per-tuple-shape / per-array / per-Vec-
  elem / per-HashMap-V release fn synthesis. Mutual recursion
  via Pass 0 declarations.
- `Weak<Vec>` cycle-breaking. Other Weak inner types planned
  but deferred (session 044 caveats).
- ARC-on-copy: `let y: ARC = x` retains.
- Insert-overwrite releases prior value (session 070).
- Strict mutability: immutable bindings reject re-assignment at
  type-check.

### Modules and visibility

- Inline `mod name { ... }` + file-based `mod name;` with
  recursive directory loading.
- `use path;`, `use path as alias;`, `use module::*;`,
  `pub use path;` re-exports.
- `pub` enforced per path segment.
- Module-mangled codegen names — same-named fns in different
  modules don't collide.
- Prelude (`mod std`) is prepended to every program; `std::Option`,
  `std::Result`, `std::Iterator`, `std::Fn1`, `std::Fn2`,
  `std::Into`, `std::Numeric` are always in scope.

### Pattern matching

- Wildcard, ident bind, literal (int / bool / str / char), enum
  variant (with payload destructure), or-patterns, range patterns,
  tuple patterns.
- Match-arm guards (`pat if cond => ...`).
- Compile-time exhaustiveness for bool, enum, and tuple
  scrutinees. Catch-all required for infinite domains (i64, str).
- Cartesian-product exhaustiveness for tuples via matrix
  specialization (Maranget's algorithm, session 089).
- Per-arm unreachability detection for tuples (session 094).

### Error handling

- `Result<T, E>` and `Option<T>` in the prelude as generic enums.
- `?` operator: desugars to `match` on Ok/Err and Some/None.
- `Into<T>` trait for `?`-site err conversion. Multi-impl
  disambiguation by surrounding context (sessions 072, 086).
- Same-target Into duplicate detection (session 090).

### Type inference and hint flow

Bare numeric literals adopt the surrounding context's type at:

- let-binding annotations (session 091): `let a: i32 = 10;`
- fn-arg / method-arg positions (sessions 081, 091): `f(10)`
- struct-field initializers (sessions 062, 091): `Holder { n: 10 }`
- binary operators with one typed operand (session 095): `a + 1`
- match-arm dispatch via `into_conversions` for `.into()`
  (session 086)
- method-call receivers when the method name uniquely identifies
  one primitive impl (session 096): `3.add(x)`
- unary `-N` on bare literals (session 091)

Suffix-bearing literals (`10i64`) override hints. Hint flow is
unidirectional within an expression — chained binops with literal
operands on the LHS-of-LHS still need parens.

### Compilation pipeline

- Lex (`src/lexer.rs`) → parse (`src/parser.rs`) → resolve
  (`src/resolver.rs`) → check (`src/checker.rs`) → lower
  (`src/lower.rs`) → monomorphize (`src/monomorphize.rs`) →
  codegen (`src/codegen.rs`).
- AOT (`rune build`): Cranelift `cranelift-object` →
  cranelift-emitted `.o` → external C linker driver invokes
  `runtime.c` and links. Discovers `clang` / `gcc` / `cc`.
- JIT (`rune run`): Cranelift JIT, registers runtime symbols via
  `JITBuilder::symbol`, returns the `__rune_main` entry as a
  function pointer.
- Single source of truth for the runtime is `runtime.c` (~1k
  lines) — used unmodified by both the JIT (compiled by
  `build.rs` into the host binary) and the AOT linker.

### Diagnostics

- Friendly type names in error messages via `ty_pretty`
  (session 093 + 097) — `AppErr` instead of `struct#83`.
- Match exhaustiveness lists missing arms by name.
- `?` mismatch suggests the right `Into<T>` impl to write.
- Numeric range-pattern mismatches reject empty ranges.

---

## What's deferred

Each deferral has a clear rationale — usually "the workaround is
fine for v0.x, the fix is a focused future session."

### Type inference

- **Chained binop hint propagation** — `1 + 2 + a: i32` parses
  left-associatively; the inner `1 + 2` defaults to i64 before
  `a` enters. Workaround: parenthesize as `1 + (2 + a)`.
- **Match-arm body hints** — `match x { _ => 1, _ => 2 }` with
  expected return i32 doesn't hint the arm bodies. Workaround:
  type the literals (`1i32`, `2i32`) or annotate.
- **Range-bound hints** — `let r = 0..10;` — both bounds default
  to i64; the resulting `RangeIter` is i64-only.
- **Assignment-op (`+=` / `-=`) hint flow** — goes through a
  different path than `check_binary`. Workaround: rewrite as `a =
  a + 1`.
- **Mixed integer/float coercion** — `let a: f32 = 3;` still
  errors (only `0` gets int-zero-as-float). Intentional — silent
  promotion would mask typos.

### Generics

- **Multi-missing-generic inference in mono** — session 078's
  fallback handles single-missing via a Fn-shaped pinned arg's
  ret. Multi-missing (e.g., `.fold_into<C: FromIter<Self::Item>>`
  with C only in the return type) gives up. No shipped method
  needs it.
- **Generic-parameterized Into targets** — `impl<T> Into<Box<T>>
  for X` — `compatible()` is over-eager on TypeVar; the duplicate
  detector might flag legitimate impls. No shipped Into impl
  uses this shape.
- **Nested tuple sub-patterns in matrix exhaustiveness** —
  `(a, (b, c))` falls into default-specialization (treated as
  infinite-domain head). Workaround: flatten the tuple or write a
  wildcard at the nested position.
- **Per-variant payload coverage in tuple matches** —
  `(Some(5), _)` and `(Some(_), _)` collapse to the same
  discriminant for exhaustiveness purposes. False-positive
  "exhaustive" for payload-specific tuple matches.

### Numerics

- **Suffix overflow checks** — `1000u8` lexes fine even though
  256 overflows u8. The bare literal gets the suffix's type but
  codegen emits the truncated value silently. Const-eval pass to
  reject these is queued.
- **Const-eval overflow** — `100u8 + 200u8` is two valid
  literals whose runtime sum overflows. Same shape as suffix
  overflow but needs evaluating expressions, not just literals.
- **`.sum` generalization across numerics** — `.sum()` is i64-
  only because the body's `total + x` needs an additive identity
  (zero) that traits can't express without const fns / static
  methods. `.min` and `.max` generalized in session 084 via
  Option<Self::Item> (the empty-iter sentinel takes the place of
  zero).
- **Float Numeric impls / NaN policy** — Vec doesn't allow f64
  elements (8-byte-slot constraint allows it in theory, parser
  rejects). When floats land in Vec, NaN semantics need a policy
  (skip / propagate / error).

### Iterators

- **Multi-arg closures beyond Fn2** — `Fn3<A, B, C, R>` etc.
  exist as cleanly mechanical extensions of Fn2; no shipped
  iterator method needs them yet.
- **`.fold_into<C: FromIter<Self::Item>>`** — needs multi-
  missing-generic inference (above).
- **Bidirectional closure hint at method-call position for
  unannotated args** — session 081 wired most of this; specific
  shapes like `.fold(0, |acc, x| acc + x)` work because the
  hint comes from F's `Fn2` bound. More aggressive: peek the
  surrounding return type for further constraint.
- **Or-patterns inside tuple sub-patterns** — `(true | false, x)`
  fails at lower with "or-pattern in tuple". The matrix algorithm
  would handle them if the lowerer expanded them.

### Memory model

- **`Weak<Str>` / `Weak<Struct>` / `Weak<Enum>`** — would mirror
  `Weak<Vec>`'s control-block split per-type. Cycles requiring
  these are rare in practice; if a real workload needs them, the
  pattern is documented in session 044.
- **HashMap struct keys** — would need fn-pointer Hash + Eq on
  the descriptor (similar to Rust's `BuildHasher`). The key_kind
  tag mechanism (i64 vs str) is fine for v0.x's two-kind story.
- **HashMap iteration during structural change** — a `.insert`
  that triggers grow during iteration invalidates the iter (no
  "iteration version" tag). Document, don't fix in v0.x.

### Diagnostics

- **Multi-span notes** — duplicate-Into-impl error points at the
  later impl but doesn't include a "previously declared at"
  secondary span. Same gap exists for impl_methods method-already-
  defined errors.
- **Free-function codegen errors** — `cranelift_type` /
  `elem_size` / `arc_helper_name` still use `Ty::display`. These
  fire on internal compiler bugs not user mistakes; the cryptic
  output is acceptable.
- **AOT linker invocation errors** — informative but not
  consistent. `gcc not found, trying clang...` style fallback
  messages would help. Low-priority polish.

### Other

- **Closure recursion** — closures can't refer to themselves by
  name (the synth fn doesn't have a self-bound name). Workaround:
  use named fns for recursive helpers.
- **`async` / coroutines / generators** — out of scope for v0.x.
  The runtime model (single-threaded, no GC, ARC-based) is fine
  for systems use cases; concurrency primitives are a 1.x-or-
  later concern.
- **FFI to non-C ABIs** — extern "C" is the only ABI. extern
  "system" / "stdcall" not supported.

---

## Pre-1.0 priorities

Before the language can claim "1.0", these items need
resolution. Listed roughly in priority order — the high-impact
ones unblock common workloads.

1. **Const-eval overflow checks** — closes the silent-truncation
   gap from session 088. Reject `100u8 + 200u8` (and `1000u8`)
   at compile time.

2. **Chained binop hint propagation** — `1 + 2 + a: i32` should
   work without parens. Pre-walk binop trees to find the
   concrete-typed leaf and propagate.

3. **Floating-point iteration** — `Vec<f64>` and similar.
   Touches Vec layout (8-byte slot already accommodates f64),
   parser (lift the "no floats in Vec" check), iterator
   protocol (RangeIter over floats? — probably stay i64-only).

4. **Full `Numeric` generalization** — `.sum() / .min() / .max()
   / .fold(init, +)` over any numeric Self::Item. Needs trait
   const fns OR static methods to express `T::zero()`.

5. **Closure recursion** — fn-pointer or struct-self trick to let
   closures reference themselves.

6. **Self-hosted bootstrap (long-term)** — rewrite the Rune
   compiler in Rune. Pre-requisite: the language is rich enough.
   By 1.0 we need recursive types, fully-typed closures, a
   reasonably-complete std (slices, iterators, Result, Option,
   String, HashMap). Most of that is already there.

7. **Performance** — currently single-threaded, no optimization
   beyond Cranelift's `OptLevel::Speed`. Performance has been
   "do the right thing for the size of program we write." Real
   workloads will surface bottlenecks (HashMap probe, ARC
   contention).

8. **Better diagnostics** — multi-span notes, hint suggestions
   ("did you mean `.into()`?"), error-recovery during parse so
   one syntax error doesn't kill the whole file.

---

## Self-hosted bootstrap

The long-term goal. Today, the compiler is in Rust (`src/*.rs`).
Eventually `src/checker.rn`, `src/lower.rn`, etc., compiled by
the Rust compiler on first build, then bootstrapped from itself.

Prerequisites:

- Rune has enough surface to express the compiler. Already
  ~there: structs, enums, generics, traits, HashMap, ARC,
  modules, iterators. Missing: tuples with arbitrary arity in
  Vec (currently 8-byte-slot constraint), efficient string ops,
  IO (file read / write, stdin / stdout).
- A `cranelift-frontend` binding in Rune. The Cranelift IR
  builder is the compiler's hottest dependency; writing a Rune-
  side wrapper is feasible but tedious.
- A test harness in Rune. The Rust-side tests (`tests/*.rs`) are
  the closest analog; they'd have to be ported.

Rough effort estimate: 6-12 months at the current pace
(roughly 100 sessions). The first concrete step would be
porting the lexer — minimal external deps, well-tested by the
existing 56-test lexer suite.

---

## Decisions that have held

A handful of early design choices have been load-bearing without
needing revisit:

- **i64 default** (decided session 1) — flat-line consistent
  across the codebase. Pinned by literal-hint flow when the
  surrounding context wants something else.
- **Cranelift backend** (decided session 1) — fast compilation,
  no LLVM dependency, native object output. The right scale
  for the project. Would be hard to swap now.
- **ARC over GC** (decided session 17) — predictable performance,
  no runtime scheduler. The cost is per-type release synthesis
  (touches every type that contains ARC fields), but it's been
  tractable across the type system's growth.
- **`mod std` written in Rune** (decided session 5x) — every
  feature visible to users is also visible to the prelude
  author. The std prelude (`src/std.rn`) is a great
  dogfooding test.
- **Monomorphization** (session 21 + carried through to today) —
  zero-cost generics. The trade-off is binary size, but binaries
  stay small because Rune programs are small.

---

## Where it's not

- Not concurrent. Single-threaded by design; no async / threads.
- Not a Rust replacement. The borrow checker, lifetime
  annotations, etc. are intentionally out of scope.
- Not interpreted. Native code via Cranelift; the JIT is
  ahead-of-time'd at run, not interpreted.
- Not stable. v0.x means breaking changes are allowed in pursuit
  of the right shape.

---

## What 1.0 looks like

When the items in "Pre-1.0 priorities" above are resolved:

- Numeric workloads work fluently with any integer / float type
  via Numeric + intrinsic impls + literal hint flow.
- Overflow is checked at compile time for const-known values and
  at runtime for dynamic (via debug-mode panics or wrapping
  semantics — TBD).
- Self-hosted: the compiler builds itself from a checked-in
  binary or a snapshot. The Rust source becomes the bootstrap,
  no longer the canonical implementation.
- API stability: no breaking changes to the prelude or syntax
  without a major-version bump.
- Documentation: full reference + standard library docs +
  tutorial.

The language design is essentially done — most 1.0 work is
quality, completeness, and self-hosting rather than new features.
