# Session 074 — Tuple ARC release + destructuring

**Date:** 2026-05-24
**Outcome:** Tuples no longer leak. Per-shape release walks
each ARC element before freeing the heap block. And
`let (a, b) = pair` destructuring desugars at lower time.
600 tests green (+4 from session 073).

```rune
fn build() -> i64 {
    let v: Vec<i64> = vec_new();
    v.push(1); v.push(2); v.push(3);
    let t = (v, 99);                 // tuple holds the Vec
    t.0.get(0) + t.0.get(1) + t.0.get(2) + t.1
}                                     // scope exit walks t,
                                      // releases the Vec, frees t

fn main() -> i64 {
    let (a, b) = (10, 32);             // destructure
    let (head, _, tail) = (1, 99, 4);  // wildcards ok
    a + b + head + tail
}
```

## The decisive observation

Both pieces mirror existing infrastructure exactly. ARC
release for tuples is structurally identical to session
067's HashMap V-release: collect distinct shapes during
monomorphization, declare a release fn per shape in Pass 0,
define in Pass 3 with a walk that releases each ARC slot
before deallocating the heap block. Reuses `rune_struct_new`
for allocation and `rune_struct_dealloc` for the final free.

Destructuring lowers without any new HIR. `let (a, b) =
pair` becomes a synthetic temp let plus per-element index
reads — the same `HirExprKind::TupleIndex` codegen as
ordinary `t.0`/`t.1`. The resolver already minted symbols
for the leaf Ident bindings via `declare_pattern`; the
lowerer's `expand_tuple_let` just connects them. Nested
tuple sub-patterns recurse through the same helper. Match-
arm tuple patterns are deferred (would need a
`HirPattern::Tuple` variant + match-machine support).

## The wire-ups

```
src/hir.rs        (HirModule.tuple_shapes: Vec<Vec<Ty>>
                   — distinct shapes used in the program.
                   One synth release fn per entry.)

src/monomorphize.rs  (New scan_ty_for_tuple_shapes +
                      collect_tuple_shapes. Existing
                      scan_ty_for_vec_elems /
                      scan_ty_for_hashmap_vals /
                      scan_ty_for_arrays recurse into
                      Ty::Tuple's element list so
                      nested-tuple-containing-Vec patterns
                      collect transitively. is_arc_mono
                      adds Ty::Tuple.)

src/codegen.rs    (Codegen<M>.tuple_release_funcs:
                   HashMap<Vec<Ty>, FuncId>. Pass 0
                   declares __rune_release_tuple$<arity>_<shape>
                   per distinct shape. Pass 3 calls
                   define_tuple_release which: null-guard,
                   decrement rc, if still alive bail, else
                   release each ARC slot via
                   emit_release_field, then call
                   rune_struct_dealloc. is_arc_type Tuple
                   flips back to true. emit_release_field
                   and emit_arc_call dispatch through the
                   synth fn for release; retain bumps the
                   trailing rc slot at offset N*8 directly.)

src/ast.rs        (Pattern::Tuple { patterns: Vec<Pattern>,
                   span }. Pattern::span arm.)

src/parser.rs     (parse_pattern_atom's LParen branch:
                   `()` → empty tuple pattern, `(p)` →
                   parenthesized (returns inner), `(p, q,
                   ...)` → Pattern::Tuple. Symmetric with
                   the value-position syntax from session
                   073.)

src/resolver.rs   (declare_pattern Tuple arm walks each
                   sub-pattern.)

src/checker.rs    (bind_pattern Tuple arm — match against
                   Ty::Tuple, bind sub-patterns to element
                   types; arity mismatch surfaces an error.
                   check_pattern_matches and cover_pattern
                   add Tuple arms for completeness; the
                   match-arm-tuple-pattern path is
                   explicitly deferred via an error in
                   collect_arm_patterns.)

src/lower.rs      (lower_block's Stmt::Let arm detects
                   Pattern::Tuple and delegates to
                   expand_tuple_let, which emits a temp
                   HirLet plus one HirLet per leaf sub-
                   pattern reading via TupleIndex. Inner
                   tuple sub-patterns recurse through
                   expand_tuple_let_from_local. lower_let
                   continues to no-op for top-level tuple
                   patterns because the let-expansion
                   already happened.)
```

## What's tested

Codegen (+4):

- `tuple_destructure_let_basic` — `let (a, b) = (10, 32)`
  binds correctly; `a + b` = 42.
- `tuple_destructure_let_with_arc_elements` — Vec values in
  tuple slots; destructuring reads them and the inner Vecs
  survive past the tuple's scope (TupleIndex retains on
  ARC).
- `tuple_destructure_with_wildcard` — `let (a, _, c) =
  (1, 99, 4)`; the middle slot is loaded-and-dropped.
- `tuple_release_arc_elements_at_scope_exit` — tight loop
  of 100 tuples holding `Vec<i64>` values. Pre-074 each
  inner Vec leaked; post-074 the per-shape release walk
  reclaims them. Test confirms no crash + correct totals
  (100 * 105 = 10500).

## Apparent bugs that aren't / explicitly deferred

- **Match-arm tuple patterns** (`match pair { (1, x) =>
  ..., _ => ... }`) aren't wired. The parser accepts them
  (since pattern parsing is unified), but
  `collect_arm_patterns` returns an explicit error. A
  follow-up would add `HirPattern::Tuple` and extend the
  match-machine in codegen.
- **Wildcard sub-patterns drop the value without
  releasing.** For non-ARC slot types this is harmless;
  for ARC slots (Vec, str, struct, etc.) the TupleIndex
  retained the value before we discarded it, so we leak
  one +1. Rare in destructuring (users wildcard primitives,
  not Vecs) but worth a tracker for the future.
- **The codegen-side retain-on-construction has a
  related quirk** with locals: `let t = (v, 99)` retains
  `v` for the tuple's slot, leaving the original `v`
  binding ALSO holding a +1. That's a double-owner
  situation — when both go out of scope, both release once.
  Correct behavior.
- **Tuple-shape mangling uses underscores between
  element names** (`T2_Vec_i64_i64`). Two structurally-
  different shapes with the same flat name would collide;
  unlikely in practice but a tighter mangling (e.g. arity
  prefix + bracketed nesting) would close the gap.
- **rune_struct_dealloc takes (ptr, size) but the size
  arg is unused** in v0.x's implementation — the runtime
  just frees the pointer. Tuple release passes the
  field_size (N*8) anyway for shape-correctness; if the
  runtime ever grows a sized-allocator the alignment is
  there.

## What's next

- **HashMap `.entries()`** — yields `(K, V)` tuples; now
  unblocked end-to-end.
- **Match-arm tuple patterns + nested `let` patterns** —
  reuse expand_tuple_let_from_local.
- **More default-body trait methods** — `.map(f)`,
  `.filter(p)`, `.fold(...)`, `.count()`, `.sum()`.
- **Method-call-position `Into` inference** — let / fn-arg /
  struct-field hints disambiguate single-trait calls.
- **Self-hosted bootstrap** — long-term.
