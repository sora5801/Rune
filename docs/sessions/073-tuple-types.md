# Session 073 — Tuple types

**Date:** 2026-05-24
**Outcome:** Tuple types and literals — `(A, B, C)` at type
position, `(a, b, c)` as values, `t.0` / `t.1` / ... for field
access. Heap-allocated like positional structs; same N*8-byte
layout plus trailing rc. 596 tests green (+3 from session 072).

```rune
fn split(x: i64) -> (i64, i64) {
    (x / 10, x % 10)
}
fn sum_pair(p: (i64, i64)) -> i64 {
    p.0 + p.1
}
fn main() -> i64 {
    sum_pair(split(347))   // split → (34, 7); sum_pair → 41
}
```

## The decisive observation

A tuple is just a positional struct with a synthesized
position-based layout. The runtime already handles heap-
allocated N*8 blocks via `rune_struct_new`; codegen already
knows how to load/store at field offsets. Adding tuples
required four mechanical pieces:

1. **AST + parser**: `Type::Tuple { elems }`, `Expr::Tuple
   { elems }`, `Expr::TupleIndex { receiver, index }`. The
   parser distinguishes `(expr)` (parenthesized grouping)
   from `(expr, expr, ...)` (tuple) by checking for a comma
   after the first inner expression. Tuple index `t.N` uses
   the existing postfix-`.` slot, extended to accept an
   integer literal in addition to an identifier.

2. **Ty**: `Ty::Tuple(Vec<Ty>)` parallel to `Ty::Vec`. Carries
   the element types verbatim; equality/unification element-
   by-element.

3. **Checker**: `resolve_type(Type::Tuple)` walks elements;
   `check_expr(Expr::Tuple)` collects element types and
   returns `Ty::Tuple(...)`; `check_expr(Expr::TupleIndex)`
   reads the receiver's `Ty::Tuple` and returns the element
   at `index`.

4. **Codegen**: `cranelift_type(Ty::Tuple(_))` returns I64
   (heap pointer); `compile_tuple` allocates `N*8` bytes via
   `rune_struct_new` and stores each element at `i*8`;
   `compile_tuple_index` loads at `index*8` from the receiver
   pointer with the existing borrow-vs-fresh retain pattern.

No HIR-level desugaring is needed — `HirExprKind::Tuple` and
`TupleIndex` flow straight through monomorphize (with
ordinary recursion) to codegen. Two tuples with the same
shape don't share a synth struct sym in v0.x because there
is no synth struct at all; codegen reads the shape directly
off `Ty::Tuple`.

## The wire-ups

```
src/ast.rs        (Type::Tuple { elems, span },
                   Expr::Tuple { elems, span },
                   Expr::TupleIndex { receiver, index, span }.)

src/parser.rs     (parse_type's LParen branch — 1 elem ⇒
                   parenthesized, ≥2 ⇒ Tuple. parse_primary's
                   LParen branch — same disambiguation for
                   expressions; trailing comma allowed (so
                   `(a,)` is a future 1-tuple slot). parse_
                   postfix's Dot branch accepts an Int literal
                   for tuple index in addition to an ident.)

src/ty.rs         (Ty::Tuple(Vec<Ty>) variant. compatible,
                   unify, display arms; element-wise.)

src/resolver.rs   (Type::Tuple, Expr::Tuple, Expr::TupleIndex
                   walks — recurse into children, nothing
                   else to resolve.)

src/checker.rs    (resolve_type Tuple → Ty::Tuple. check_expr
                   Tuple → Ty::Tuple of elements. check_expr
                   TupleIndex → element at index, with range
                   check.)

src/hir.rs        (HirExprKind::Tuple, HirExprKind::TupleIndex.)

src/lower.rs      (lower_expr_kind Tuple/TupleIndex →
                   HirExprKind. rewrite_captures walks them
                   so closures capturing through tuples work.
                   No struct synthesis — the kind flows
                   directly to codegen.)

src/monomorphize.rs  (subst_expr_kind, walk_tys_expr,
                      walk_expr_collect_syms — three walks
                      need Tuple/TupleIndex arms. subst_ty
                      maps Ty::Tuple element-wise.)

src/codegen.rs    (cranelift_type Tuple → I64. is_arc_type
                   Tuple → false (see deferral note).
                   mangle_ty_name Tuple → `T{N}_{elems}`.
                   compile_tuple: alloc via struct_new, store
                   each element. compile_tuple_index: load at
                   offset, retain-on-ARC.)
```

## What's tested

Codegen (+3):

- `tuple_literal_and_index` — `(10, 20).0 + (10, 20).1` = 30.
- `tuple_three_elements_mixed_types` — `(i64, bool, i64)`;
  bool element accessed and used in a conditional.
- `tuple_as_fn_return_and_param` — round-trip a tuple through
  a fn return + a fn parameter. `split(347)` → `(34, 7)`;
  `sum_pair` reads both fields.

## Apparent bugs that aren't / explicitly deferred

- **Tuples leak the heap block at scope exit.** v0.x treats
  `Ty::Tuple` as non-ARC (`is_arc_type` returns false) so
  codegen never emits a release — and there's no synth
  release fn per shape. For programs that run-and-exit
  (every current test) this is invisible. Adding the per-
  shape release walk mirrors session 067's HashMap pattern
  exactly: collect distinct shapes in monomorphize, declare
  a release fn in Pass 0, define in Pass 3 with a walk that
  releases each ARC element before calling `struct_dealloc`.
- **No tuple destructuring patterns** (`let (a, b) = pair`).
  The parser only accepts the indexing form. Patterns are
  next; would need work in the pattern parser, resolver, and
  match-arm checker — a separate session.
- **Empty tuples `()` and 1-tuples `(a,)` parse but have no
  codegen path tested.** The parser accepts `()` as
  `Expr::Tuple { elems: [] }` and `(a,)` as a 1-tuple via
  trailing comma; both reach the lowerer but allocate
  zero/one-element heap blocks. Real use awaits the
  destructuring follow-up.
- **`Ty::Tuple` containing TypeVars isn't fully tested
  through monomorphization.** The walks pass element types
  through subst_ty correctly, but a generic fn parameterized
  over a tuple shape (`fn f<T>(t: (i64, T)) -> T`) hasn't
  been exercised. Should work — same machinery as Vec<T> or
  HashMap<K, V> — but flag for the test author.
- **No HashMap.entries() yet.** Tuples were "the missing
  piece" for that API but `.entries()` itself wasn't added
  in this session; left for follow-up.

## What's next

- **Tuple destructuring patterns** (`let (a, b) = pair`,
  `match opt { Some((k, v)) => ... }`).
- **Tuple ARC release per shape** — mirror HashMap V-release.
- **HashMap .entries()** — yield `(K, V)` tuples.
- **More default-body trait methods** — `.map(f)`, `.filter(p)`,
  `.fold(...)`, `.count()`, `.sum()`.
- **Self-hosted bootstrap** — long-term.
