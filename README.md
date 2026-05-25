# Rune

A small, statically-typed, compiled programming language. Written in Rust,
targeting native code via Cranelift.

## Status

**Pre-alpha — full pipeline lex → parse → check → codegen, both JIT and AOT.**
Generics (monomorphized), traits, inline modules, ARC heap reclamation, and an
embedded Rune-written standard library all work. `--release` AOT supported.

```
$ rune build examples/hello_world.rn --release && ./hello_world.exe
rune: linked with clang -> hello_world.exe
Hello, world!
$ rune run examples/greet.rn
Hello, world!
Hello, Rune!
Hello, Cranelift!
greeted (count, total bytes):
3
42
$ rune build examples/primes.rn --release && ./primes.exe ; echo $?
rune: linked with clang -> primes.exe
2 3 5 7 11 13 17 19
77
```

## Implementation state

### Lexer — done

- Keywords: `let`, `mut`, `fn`, `return`, `if`, `else`, `while`, `for`, `in`,
  `break`, `continue`, `true`, `false`, `struct`, `enum`, `match`, `pub`,
  `const`, `as`
- Identifiers (ASCII)
- Integer literals: decimal, hex (`0x`), binary (`0b`), octal (`0o`); `_`
  digit separators
- Float literals: fractional part with optional `e`/`E` exponent
- String literals with `\n \t \r \\ \' \" \0` escapes
- Char literals with the same escape set
- All single- and multi-char operators: `+ - * / %`, `== != < > <= >=`,
  `&& || !`, `& | ^ ~ << >>`, `-> => :: .. ..=`, `+= -= *= /= %=`, `? .`
- Delimiters: `( ) { } [ ]`, `, ; :`
- Line comments and **nested** block comments
- UTF-8 source input
- Byte-offset spans on every token
- Error recovery
- 21 integration tests

### Parser — done

- Items: `fn`, `struct`, `enum`, `const`, `trait`, `impl` /
  `impl Trait for Type`, `mod`, `use` (with optional `pub`)
- Statements: `let`, expression statements, items inside blocks
- Expressions: literals, paths, parenthesized, unary, all binary operators
  with Pratt precedence, assignment and compound assignment, postfix
  (call/method/field/index/`?`/`as`), block expressions, `if`/`else if`/`else`,
  `while`, `for ... in ...`, `match` with arms and guards, `return`,
  `break`, `continue`, array literals
- Patterns: wildcard, identifier (with `mut`), literal (with optional
  unary `-` on numeric literals), path (`EnumName::Variant`),
  tuple-variant destructure (`Some(x)`), or-patterns (`a | b | c`),
  and ranges (`lo..hi`, `lo..=hi`)
- Types: paths with optional generic args, nestable — a `>>` token
  is split so `Vec<Vec<i64>>` / `Weak<Vec<i64>>` parse (`Vec<i64>`,
  `Result<i64, str>`, `Vec<Vec<i64>>`)
- Generic type parameters on `fn`/`struct`/`enum`/`impl` items.
  Generic functions — and the methods of an `impl<T> Foo<T>` — are
  monomorphized per concrete instantiation
  (`id$$i64`, `pair$$i64$$str`). `Ty::Struct` and `Ty::Enum` carry
  type args, so `Box<i64>`, `Option<T>`, `Result<T, E>` etc. all
  work end-to-end.
- **Traits + bounded generics**: `trait Display { fn fmt(...); }`,
  `impl Display for Point { ... }`, `fn show<T: Display>(x: T)`.
  Static dispatch — trait method calls in a bounded-generic body
  are resolved per-specialization by the monomorphizer. Supports
  generic `impl<T> Trait for Foo<T>` blocks; supertraits
  (`trait Dog: Animal`) with transitive method lookup; associated
  types (`type Item;` in the trait, `type Item = i64;` in the
  impl, `Self::Item` in method signatures); `T::Item`
  projection through a type parameter — the projection is resolved
  to the impl's binding at monomorphization, so the iterator-protocol
  shape `fn next<T: Iterator>(x: T) -> T::Item` compiles; and
  **generic trait declarations** (`trait Producer<T> { fn make(...)
  -> T; }`) where the trait's generic args substitute through
  every method signature at the dyn call site.
- **Iterator protocol** — the prelude declares
  `trait Iterator { type Item; fn next(self: dyn Iterator) -> Option<Self::Item>; }`.
  A user struct that implements `Iterator` is iterable through
  `for x in iter { ... }`; the lowerer desugars to a
  `while true { match iter.next() { Some(x) => ..., None => break } }`
  loop. Composes with bounded generics — `fn count<T: Iterator>(it: T)`
  works for any concrete implementor. `break` and `continue` are
  real control-flow constructs, threaded through codegen's per-loop
  exit/continue stacks with ARC-local cleanup at the snapshot.
  `Vec<T>` joins the protocol through `v.iter() -> std::VecIter<T>`,
  integer ranges (`a..b`, `a..=b`) join via `std::RangeIter` (so
  they flow into `Map { iter: 0..10, ... }`), plus prelude adapters
  `std::Map<I, F, U>` and `std::Filter<I, P>` (with `F: Fn1<I::Item,
  U>` / `P: Fn1<I::Item, bool>` — any callable: named fns,
  non-capturing closures, OR capturing closures), and
  `std::collect<T: Iterator>(it: T) -> Vec<T::Item>` — the pipeline-
  style `collect(Filter { iter: Map { iter: v.iter(), f: |x: i64|
  x * mult }, pred: |y: i64| y > min })` works end-to-end including
  captures. Bound-arg propagation in the checker pins Map's `U`
  from the closure's return type via the `F: Fn1<I::Item, U>`
  bound (no need to mention U directly in a field).
  **Iterator default methods** — `.collect()`, `.count()`, `.sum()`,
  `.min()`, `.max()`, `.filter(p)`, `.map(f)`, `.fold(init, f)` are
  declared as default-body methods on the `Iterator` trait, so every
  implementor inherits them. The pipeline reads as a method chain
  end-to-end with unannotated closures: `v.iter().filter(|x| x > 1)
  .map(|x| x * 10).fold(0, |a, x| a + x)` — the checker synthesizes
  a `Ty::Fn` hint at each method-call position from F's `Fn1` /
  `Fn2` bound and uses it to bind closure params bidirectionally.
  `.min()` and `.max()` return `Option<i64>::Some(best)` over non-
  empty iterators (i64-only until a `Numeric` trait lands).
  `.filter` and `.map` take any callable (named fn, non-capturing
  closure, or capturing closure); `.fold` uses `Fn2<U, Self::Item,
  U>` for its (acc, next) -> acc binary closure.
- **Function-pointer values + closures (capturing)** — named
  `fn` items are first-class values; closure literals `|x| body`
  / `|x, y| body` / `|| body` lower to anonymous fn items
  (non-capturing) or to a synthesized struct holding the
  captured fields plus a `call` method (capturing). The prelude
  declares `trait Fn1<A, R> { fn call(self: Self, a: A) -> R; }`.
  Closure-param annotations are inferred from any of three
  sources: an explicit `let f: fn(i64) -> i64 = |x| ...`, a
  callable-bounded generic context (`let m = std::Map { iter:
  ..., f: |x| x * mult }` — F's `Fn1<I::Item, U>` bound supplies
  the shape), or the body's own usage (`let f = |x| x * mult` —
  the binop with `mult: i64` pins x to i64).
- **`dyn Trait`** — dynamic dispatch. A concrete type coerces to a
  trait object (a boxed method table); `s.method()` on a `dyn`
  dispatches through it via `call_indirect`. The box is ARC-managed —
  it carries a refcount and a drop slot, so a `dyn` local reclaims
  itself and the value it wraps at scope exit. `Vec<dyn Trait>`
  collects trait objects of different concrete types. **`dyn Sub`
  exposes supertrait methods**: the box's method table is laid out
  flat (Sub's methods first, then each supertrait in BFS order), so
  a value of type `dyn Dog` can call both `Dog`'s and `Animal`'s
  methods.
- **Modules**: inline `mod name { items... }` (nestable) and
  file-based `mod name;` (loads `name.rn`; nested — `mod bar;` inside
  `foo.rn` loads `foo/bar.rn`). `use a::b::c;` imports, `use x as y;`
  renaming, `use m::*;` globs, `pub use` re-exports. `pub` is enforced
  per path segment — a non-`pub` item (or a private intermediate
  module) is private to its module and descendants. Functions get
  module-mangled codegen names so same-named functions in different
  modules don't collide.
- Generic type parameters with optional trait bounds
  (`<T>`, `<T: Display>`, `<T: A + B>`)
- Error recovery at item-starting keywords
- 54 integration tests

### Resolver — done

- Two-pass: declare top-level items, resolve bodies. Forward references
  between items work.
- Built-in type names pre-populated (`bool`, `char`, `str`, `i8`–`i64`,
  `u8`–`u64`, `isize`/`usize`, `f32`/`f64`).
- Lexical scoping with same-scope shadowing allowed.
- Module-qualified namespacing; `use` imports, renaming (`as`),
  globs, and `pub use` re-exports; per-segment `pub` visibility
  enforcement (a non-`pub` item is reachable only from its module
  and descendants).

### Type checker — done

- Primitives + array types, inferred or written as `[T; N]`.
- Unannotated integer literals default to `i64`; floats to `f64`.
- `let` checks annotation vs initializer; mutability is strictly enforced.
- Arithmetic / comparison / logical / bitwise / unary checked.
- `if`/`else` branches unify; `while`/`if`-without-else require unit body.
- Function calls check arity and argument types.
- `as` casts allowed between numeric / bool / char / integer pairs.
- **Match**: per-arm pattern type checking, compile-time exhaustiveness
  for `bool` / enum, "missing `_` arm" for infinite domains, unreachable-
  arm detection across arms and within or-patterns. Guards must be `bool`
  and don't contribute to exhaustiveness coverage.
- 130 integration tests.

### HIR + Cranelift codegen — done

- AST-shaped HIR (`src/hir.rs`) with `Ty` on every node; paths resolved to
  `SymbolId`. Unsupported variants funneled into `Unsupported(msg)`.
- Lowering pass at `src/lower.rs`.
- Cranelift codegen (`src/codegen.rs`) generic over `Module` —
  parameterized backend used by both JIT and AOT paths.
- Covers:
  - Integers (i8/i16/i32/i64 + unsigned + isize/usize), floats (f32/f64),
    bool — arithmetic, comparison, bitwise, shifts, unary.
  - Short-circuit `&&`/`||`, `if`/`else` (expression form, `else if`
    chains), `while`, `let` with mutability, assignment and compound
    assignment.
  - Rune-to-Rune function calls (forward references, recursion, mutual
    recursion), early `return`.
  - **Array literals** heap-allocated as refcounted blocks (escape-
    safe — returnable, struct-storable), **indexing** via address
    arithmetic + `load`, **`for x in arr`** desugared to a
    counter-based while loop.
  - **String literals** as a 16-byte (ptr, len) descriptor — bytes in
    the object's data section, descriptor on the function's stack.
    **`==`/`!=`** for strings via runtime `rune_str_eq`.
    **`+` / `+=`** (concatenation) via runtime `rune_str_concat` that
    mallocs a fresh descriptor + buffer; result is process-lifetime
    heap (no free yet).
  - Host builtins: **polymorphic `print(x)`** dispatches on argument
    type to `print_i64` (for any int) or `print_str` (for `str`).
    Explicit-typed variants `print_i64` and `print_str` remain callable
    directly. All runtime symbols come from one source — `runtime.c`,
    linked into the binary for the JIT and compiled by the AOT linker.
  - **Method calls** dispatch on `(receiver_ty, method_name)`. First
    three methods: `str.len()` and `str.is_empty()` (inline `load` +
    optional `icmp`), `arr.len()` (static constant from the array's
    type). The mechanism extends to future methods.
  - **String indexing** (`s[i]`) reads one byte and zero-extends to
    `i64`. **String slicing** (`s[a..b]`, `s[a..=b]`) heap-allocates a
    fresh substring (clamps out-of-range bounds, never panics).
  - **Range iteration**: `for i in 0..n { }` and `for i in 1..=n { }`
    work via a counter-based loop.
  - **String predicates**: `s.starts_with(p)`, `s.ends_with(p)`,
    `s.contains(p)` via runtime calls.
  - **Structs** with field access: `struct Point { x: i64, y: i64 }`,
    constructed via `Point { x: 1, y: 2 }`, accessed via `p.x`,
    **returned by value** from functions. Heap-allocated with a
    refcount; per-struct synthesized release walks ARC fields and
    dealloc's the descriptor. 8-byte-per-field padding (v0.x
    simplification).
  - **`impl` blocks** for inherent methods on structs:
    `impl Point { fn magnitude_sq(self: Point) -> i64 { ... } }`.
    Methods dispatched at lowering time; `self` becomes the first
    argument of a regular Cranelift function with a mangled name.
  - **`Vec<T>`** — a generic heap-allocated growable list:
    `vec_new()`, `.push(x)`, `.get(i)`, `.len()`, exposed as
    `std::Vec`. Elements occupy 8-byte slots; a `Vec` of ARC-managed
    elements (structs, payload enums, nested `Vec`) reclaims them
    through a codegen-synthesized per-element-type release. Still a
    compiler builtin — Rune has no raw-memory primitives.
  - **`HashMap<K, V>`** — a runtime-backed open-addressing hashmap
    (linear probing, 75% load factor, initial cap 8, doubles on
    grow). `hashmap_new()` or `hashmap_str_new()` for i64 / str
    keys; `.insert(k, v)`, `.get(k)`, `.contains_key(k)`, `.len()`,
    `.remove(k)`, `.keys()`. V is any 8-byte-fitting type. ARC-
    managed at the descriptor, per-value (codegen-synth release
    walk), AND per-key for str-keyed maps (runtime owns the str
    ARC). Two distinct str descriptors with the same byte content
    hash to the same slot and compare equal (memcmp). Tombstone-
    based remove keeps probe chains correct; .keys() yields a
    HashMapKeysIter that skips empty + tombstoned slots.
  - **Enums** with `EnumName::Variant` path syntax. Tag-only variants
    represent as i64; **payload variants** (`Some(i64)`, `Err(str)`,
    `Pair(i64, i64)`) flip the whole enum to a heap-allocated
    `{ tag, payload[max_arity], rc }` descriptor sized to its max
    variant arity. ARC-managed: per-enum synthesized release walks
    the active variant's ARC payloads on drop. Multi-field tuple
    variants work; named-field variants (`Ok { value: T }`) work
    with field-by-name construction (`Variant { name: val }`) and
    destructure (`Variant { name }` / `Variant { name: pat }`).
    Full **`match`** support: literal/path/wildcard/ident patterns,
    **tuple-variant destructure** (`Some(x) => ...`),
    **or-patterns** (`A | B | C => ...`), **range patterns**
    (`lo..hi` / `lo..=hi` on integer or char scrutinees, including
    negative literal bounds), and **guards** (`pat if cond => body`).
    Non-exhaustive matches and unreachable arms are compile-time
    errors; a runtime `rune_panic_no_match` backstop stays wired as
    defense in depth.
  - **`?` operator** — `expr?` propagates errors: the lowerer
    desugars it to `match expr { Ok(v) => v, Err(e) => return Err(e) }`.
    The checker requires a `Result`-shaped operand and an enclosing
    function returning a `Result`. When the err types differ, the
    `?` looks for an `impl std::Into<TargetErr> for SourceErr` and
    auto-converts via `err.into()` before the return.
  - **ARC reclamation (step 2 of the reclamation ladder).** Vec and
    concat/sliced str descriptors carry a refcount; codegen tracks
    "owned ARC locals" per scope and emits release calls at scope
    exit. Returning a local of ARC type retains first so the caller
    gets +1. String literals use a `rc=-1` sentinel so the runtime
    helpers no-op on them. **ARC-on-copy** (let y = x retains) and
    **ARC for struct fields** (Vec / str fields participate, retain
    on construct, release on drop) both work. **`Weak<Vec>`** for
    cycle breaking — control-block split with separate strong/weak
    counts; `weak(v)` downgrades, `upgrade_or(w, default)` promotes
    or falls back. Non-atomic (single-threaded only).
  - **`as` casts** between numeric / char / bool with sign-aware
    extend, saturating float→int, and float widening/narrowing.
- ABI: target-native (effectively `extern "C"`).
- 249 JIT codegen tests + 40 AOT tests.

### AOT executables — done

- `rune build <file> [--release] [-o out]` produces a native executable
  via `cranelift-object` + an external C-style linker driver.
- `src/aot.rs`: `build_object` renames Rune's `main` to `__rune_main`,
  emits a synthesized `int main(void)` that calls it and truncates the
  i64 return to the i32 exit code. `link` writes the single-source
  runtime (`runtime.c`) to a `.rt.c` file and passes it to the linker
  driver alongside the `.o` — drivers compile and link in one shot.
  The same `runtime.c` is compiled into the `rune` binary by
  `build.rs` for the JIT.
- Linker discovery: `clang` → `gcc` → `cc`; `$RUNE_LINKER` overrides.
- `--release` sets Cranelift's opt level to `speed`; default is `none`
  for fast iteration.
- Output: `<input-stem>.exe` on Windows, `<input-stem>` elsewhere.
  `-o <path>` overrides.

**Not yet codegen'd:** any construct without a backend lowering
emits `Unsupported(msg)` at lowering, with a clear error if reached.

### Standard library — done

- A `mod std { ... }` prelude written in Rune itself (`src/std.rn`),
  embedded into the compiler with `include_str!` and prepended to
  every program. `std::` items are always in scope — no install step,
  no search path; the prelude ships inside the compiler binary.
- `std::Option<T>` (`Some` / `None`) and `std::Result<T, E>`
  (`Ok` / `Err`).
- Generic helpers over them: `unwrap_or`, `is_some`, `is_none`,
  `ok_or`, `is_ok`, `is_err`.
- Concrete i64 helpers: `min`, `max`, `abs`, `clamp`.
- Generic helpers are zero-cost when unused — the monomorphizer drops
  any specialization the program never calls.
- The compile commands (`check`/`run`/`build`) prepend the prelude;
  the debug commands (`tokens`/`ast`) don't, so their output reflects
  only the user's file.

## Roadmap

1. Intrinsic `Numeric` impls for primitive types — lifts the
   `impl on i64/i32/f64/...` resolver restriction
2. Numeric literal suffixes — `10i32`, `3.14f32`
3. Cartesian-product exhaustiveness for tuple patterns
4. Self-hosted bootstrap (long-term)

## Planned syntax

Rust/Swift-flavored. Expression-oriented, statically typed with inference,
immutable by default.

```rune
fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() -> i64 {
    fib(10)
}
```

## Build

```
cargo build
cargo test
```

## CLI

```
rune tokens <file.rn>                       # dump tokens
rune ast <file.rn>                          # parse and dump the AST
rune check <file.rn>                        # parse, resolve names, type-check
rune run <file.rn>                          # JIT-compile and execute `main() -> i64`
rune build <file.rn> [-o out] [--release]   # AOT-compile to a native executable
```

`rune build` requires a C-style linker on PATH. The discovery order is
`clang` → `gcc` → `cc`. Override with `RUNE_LINKER=<name>`. `--release`
maps to Cranelift's `OptLevel::Speed`.

## Documentation

- [LANGUAGE.md](LANGUAGE.md) — language design decisions (living document)
- [docs/sessions/](docs/sessions/) — per-session technical deep dives

## License

MIT (see [LICENSE](LICENSE)).
