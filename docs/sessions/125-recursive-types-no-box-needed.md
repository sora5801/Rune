# Session 125 — Recursive types work without `Box<T>`

**Date:** 2026-05-25
**Outcome:** A correction to session 117's bootstrap
roadmap. Recursive enums (`Expr::Pair { lhs: Expr,
rhs: Expr }`) and recursive structs (`Node { next:
Option<Node> }`) compile and run correctly *today*
— no `Box<T>` wrapper needed. The roadmap listed
this as a Tier C blocker; investigation found it
was already non-blocking. Four new codegen tests
demonstrate the bootstrap-relevant patterns. 492
codegen + 47 AOT + 223 typecheck tests green (+4
codegen from session 124). No source changes.

```rune
enum Expr {
    Num(i64),
    Add { lhs: Expr, rhs: Expr },
    Mul { lhs: Expr, rhs: Expr },
}

fn eval(e: Expr) -> i64 {
    match e {
        Expr::Num(v) => v,
        Expr::Add { lhs, rhs } => eval(lhs) + eval(rhs),
        Expr::Mul { lhs, rhs } => eval(lhs) * eval(rhs),
    }
}

fn main() -> i64 {
    let lhs: Expr = Expr::Add { lhs: Expr::Num(2), rhs: Expr::Num(3) };
    let rhs: Expr = Expr::Add { lhs: Expr::Num(4), rhs: Expr::Num(5) };
    eval(Expr::Mul { lhs: lhs, rhs: rhs })   // 45
}
```

## The decisive observation

Session 117's bootstrap roadmap predicted:

> **`Box<T>` or equivalent for recursive types.** The
> AST has self-referential nodes (`Expr` contains
> `Expr`). Today Rune uses heap allocation implicitly
> for `Struct` types (which carry pointers); a
> recursive enum like `Expr::Binary { lhs: Expr,
> rhs: Expr }` currently fails because the layout
> would be infinite.

The prediction was wrong. Empirically, the recursive
enum compiles and runs. The reason:

- **Every `Ty::Struct(_, _)` value is `types::I64`
  at codegen** — a pointer to a heap-allocated body.
- **Every enum with payload is also `types::I64`** —
  a pointer to a heap-allocated descriptor (the
  discriminant + payload fields).
- **Enum variant payload fields with type
  `Ty::Enum(_, _)` or `Ty::Struct(_, _)` occupy
  8 bytes**, not the full size of the type.

So when the resolver / checker sees `Expr::Pair {
lhs: Expr, rhs: Expr }`, the `lhs` and `rhs` fields
are each laid out as a single 8-byte slot — not as
"inline a copy of Expr." Self-recursion in the
type system corresponds to *pointer indirection*
at the runtime layout, automatically.

This is the same reason `Vec<T>` doesn't need
explicit boxing for `T = Struct`: the Vec slot is
8 bytes (a pointer to the struct), and the struct's
own body lives separately on the heap.

### Rust vs Rune on this

Rust's value-semantics + monomorphization model
needs `Box<T>` for recursive enums because an
inline T inside T would have infinite size. Rust
gives the user explicit control via `Box`.

Rune's ARC-everywhere + pointer-indirection model
makes every aggregate type heap-allocated and
pointer-sized at the use site. Recursive types
"just work" with no syntax overhead, because every
aggregate's body is already heap-allocated, and
the reference is what gets stored in the parent.

The cost: a Rune `Pair { 2, 3 }` allocates two
small heap blocks (one for the Pair, one for each
Num if they're enum-variant-with-payload, though
unit variants like `Lit(i64)` store the i64 inline).
Rust's `Pair(2, 3)` is one stack-allocated value.
Rune trades flat-allocation perf for "no Box
keyword" ergonomics. For a bootstrap compiler this
is the right trade.

### Linked-list pattern works too

The same observation extends to structs that
contain `Option<Self>`:

```rune
struct Node { value: i64, next: std::Option<Node> }
```

`Node` is a heap pointer; `Option<Node>::Some(n)`
puts `n` (the Node pointer) in the Some payload,
which itself is heap-allocated. Each linked-list
cell is two heap blocks (a Node + an Option),
which is a bit fluffy but correct. The bootstrap's
intrusive lists (e.g., the symbol table or scope
chain) can use this shape directly.

### Why this is a meaningful session

The bootstrap was theorized to need a significant
type-system addition (Box<T>) to express the AST.
Verifying that no such addition is needed:

- Saves ~150-300 LOC of compiler changes (new Ty
  variant + runtime helpers + method dispatch).
- Confirms the runtime model is well-suited to
  the bootstrap.
- Adds regression-safety tests for the exact shape
  the bootstrap will use (mini-AST with eval).
- Updates the roadmap so future planning doesn't
  worry about this case.

## What's tested

Codegen (+4 from session 124's 488):

- `recursive_enum_self_referencing_variant` — basic
  case: enum with a variant whose field is the
  enum itself.
- `recursive_enum_eval_mini_ast` — full mini-AST
  with `Num`, `Add`, `Mul` variants + recursive
  `eval`. Result `(2+3)*(4+5) = 45`.
- `recursive_struct_with_option_linked_list` —
  linked-list shape: `struct Node { value, next:
  Option<Node> }` + recursive `sum`. Result
  `1 + 2 + 3 = 6`.
- `deeply_nested_recursive_enum` — 5-level-deep
  nested sum tree. Validates that the heap
  allocator + ARC release walks handle deep
  recursion correctly.

Each test constructs a recursive value, walks it
recursively via match, and returns an integer.
This is the *exact pattern* a bootstrap parser +
evaluator would use.

## Apparent bugs that aren't / explicitly deferred

- **Infinite construction.** `fn child() -> Node {
  Node { value: 99, next: child() } }` type-checks
  but stack-overflows at runtime. The compiler can't
  detect this; the user controls termination via
  `Option::None` or another base case. Same as Rust.
- **Cyclic references.** Two nodes pointing at
  each other (`a.next = b; b.next = a`) would
  create an ARC cycle that the refcount can't
  reclaim. Rune doesn't have a cycle detector;
  cyclic data structures leak. The bootstrap's
  AST is a tree, so this doesn't apply.
- **`Vec<Self>` in a struct.** A struct with `child:
  Vec<Self>` would work (Vec is pointer-sized,
  Self is pointer-sized in the Vec slot). Untested
  but should follow from the same logic.
- **`HashMap<i64, Self>`.** Same as Vec — the slot
  is i64-sized, and Self is a pointer that fits.
  Untested.
- **Bootstrap roadmap update.** Session 117's
  Tier C "Box<T> or implicit boxing" item is now
  resolved — recursive types are already supported.
  Updated below.

## The bootstrap-roadmap implication

The Phase 1 capability-buildout list shrinks by
one item. Session 117 listed:

> Tier C — language features the compiler would
> lean on:
> 8. **`Box<T>` or equivalent for recursive types.**

That item is removed. The remaining Tier C items
(pattern guards, let-else, `&str` borrowed slices)
are still standing but not blockers — the bootstrap
can be written without them, just with some
ergonomic friction.

Net effect: Phase 1's exit criterion (the
capability set needed for the Rune-in-Rune
interpreter) is closer than session 117 estimated.

## What's next

- **Session 126: Pattern guards (`p if cond =>
  ...`)** — Tier C ergonomic improvement, not a
  blocker but useful for the parser.
- **Session 127: `let ... else`** — early-exit
  binding, reduces ladder nesting in parser code.
- **Session 128+**: continued Phase 1 buildout
  per session 117 (minus the Box<T> item).
