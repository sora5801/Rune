# Session 057 — Non-capturing closures (`|x| body`)

**Date:** 2026-05-23
**Outcome:** Closure literal syntax — `|x| body`, `|x, y| body`,
`|| body` — lands. The compiler desugars each closure into an
anonymous `fn` item at the lambda's source position, so the
expression becomes a fn-pointer value. Adapter callbacks no
longer need named `fn` items: `v.iter()` + `Map { f: |x| x * 2 }`
works. Capturing closures (`|x| x * mult` referencing an outer
local) are still rejected with a clear diagnostic — they require
env synthesis and a `Fn` trait, deferred to a follow-up. ~5
files. 534 tests green (+8 from 526).

## The decisive observation

The user's preview shows capture (`|x| x * mult`), but **capture
is the hard part of closures, not the syntax**. Without capture,
a closure literal is just an anonymous `fn` item written inline
— exactly the shape that fits the session-055 fn-pointer-value
machinery and the session-056 `Map<I, U> { f: fn(I::Item) -> U }`
field. The lowerer mints a synthetic `fn __lambda_N`, replaces
the lambda expression with `HirExprKind::Fn(lambda_sym)`, and
every downstream layer (codegen, monomorphizer, ARC) is unchanged.

This separation lets session 057 ship the *syntax* (which is what
unblocks ergonomic adapter pipelines once the capture is inlined
or factored out), with capture as a follow-up that adds a closure
struct + a `Fn` trait.

## The wire-ups

```
src/
├── ast.rs        (Expr::Closure { params: Vec<ClosureParam>,
│                  body: Box<Expr>, span };
│                  ClosureParam { name, ty: Option<Type>, span })
├── parser.rs     (parse_primary detects leading `|` or `||`;
│                  parses comma-separated params with optional
│                  `:` type annotations, terminating `|`, body)
├── resolver.rs   (Expr::Closure arm in resolve_expr mints a
│                  fn sym via `intern` keyed by closure span,
│                  scopes the params, resolves body; new
│                  closure_fn_sym + closure_params maps on
│                  Resolutions; new open_closure_spans stack so
│                  check_closure_capture rejects any path
│                  resolving to a Local/Param declared outside
│                  the innermost closure's span)
├── checker.rs    (Expr::Closure arm in check_expr; new
│                  check_closure with bidirectional inference
│                  via Option<&Ty> hint; check_expr_with_hint
│                  helper; CheckResults.closure_param_tys +
│                  closure_ret_tys; integration hooks in
│                  check_let (declared annotation as hint),
│                  check_struct_lit pass 2 (substituted field
│                  type as hint — closures deferred from pass 1
│                  so their unannotated params don't error
│                  prematurely))
└── lower.rs      (Lowerer.synthesized_fns: RefCell<Vec<HirFn>>;
                   Expr::Closure arm in lower_expr_kind builds
                   the synthetic HirFn from closure_fn_sym +
                   stashed param/ret types + lowered body and
                   pushes onto the stash; lower_module drains
                   the stash into items after lower_items)
```

The closure check is bidirectional but narrowly so: only
`check_let`, `check_struct_lit` pass 2, and (where added in
future sessions) `check_call` argument positions pass a hint.
Closures in other positions (block tail, match arm) require
type annotations: `|x: i64| x * 2`. Acceptable; documented.

`check_struct_lit`'s pass 1 had to be amended to **defer**
closure values: a `Map { iter: v.iter(), f: |x| x * 2 }` style
literal needs `iter`'s value to pin `I = VecIter<i64>` before
the closure's `f: fn(I::Item) -> U` field type substitutes to
`fn(i64) -> U` — without deferral, pass 1 would type-check the
closure with no hint and emit "needs a type annotation" for
every closure param.

## What's tested

Codegen (+5):

- `closure_non_capturing_basic` — `let f: fn(i64) -> i64 = |x|
  x * 2; f(21)` returns 42. Smallest possible closure.
- `closure_in_map_pipeline` — `Map { iter: v.iter(), f: |x| x
  * 2 }` then `for y in mapped`. The headline integration.
- `closure_in_filter_pipeline` — `Filter { iter: v.iter(), pred:
  |x| x > 2 }`.
- `closure_chain_map_filter_collect` — full pipeline with closures
  for both Map and Filter, then `collect`.
- `closure_zero_args` — `|| 42` (the parser handles the `||`
  token as a zero-arg closure delimiter when in prefix position).

Typecheck (+3):

- `closure_capture_rejected` — `let f: fn(i64) -> i64 = |x| x *
  mult` produces the explicit "captures `mult` from the
  enclosing scope; capturing closures are not yet supported in
  v0.x" diagnostic.
- `closure_arity_mismatch_rejected` — `let f: fn(i64) -> i64 =
  |x, y| x + y;` flags the arity mismatch.
- `closure_return_type_mismatch_rejected` — `let f: fn(i64) ->
  i64 = |x| true;` flags the body's bool return type vs i64.

## Apparent bugs that aren't / explicitly deferred

- **Capturing closures.** Out of scope. The resolver checks
  each path inside the closure body — any Local/Param declared
  outside the closure's span fires a clear diagnostic naming
  the captured binding. The user's headline preview `f: |x| x
  * mult` requires capture and does not compile this session.
- **No `Fn`/`FnMut`/`FnOnce` traits.** Closures don't unify
  with each other through a trait; each lambda is a distinct
  `fn` value with the same shape as a named fn item, distinguished
  only by its synthetic `SymbolId`.
- **No `move` keyword.** No move/borrow distinctions to make.
- **No `dyn Fn(args) -> ret`.** Trait objects of callables
  would need the `Fn` trait family above.
- **No higher-rank trait bounds.** Not relevant without the
  `Fn` trait family.
- **`(|x| x * 2)(3)`** requires param annotations because the
  call's callee is being inferred: `(|x: i64| x * 2)(3)`. The
  contextual hint path doesn't reach naked call-of-closure.
  Workaround documented.
- **Closures in block tail / match arm body without a let
  annotation.** Same constraint — no contextual hint reaches
  those positions, so closure params need explicit types.

## Symbol-identity bug check

- **Synthetic fn sym** is interned via the resolver's `intern`
  inserted into `scopes[0]` under a module-qualified mangled
  name (`__lambda_{N}` prefixed by the current module path),
  so codegen's module-mangler doesn't collide across modules.
  The resolver allocates the sym during pass-2 body resolution
  — early enough that the checker's signature-registration
  pass can find it.
- **Closure parameter syms** use the natural `name.span` for
  `decl_to_sym` keying, same as `resolve_fn`'s params. The
  lowerer reads `decl_to_sym[&p.name.span]` identically.
- **Capture detection by span containment**. The resolver
  pushes the closure's `span` onto `open_closure_spans` when
  entering the body, pops on exit. Any path resolving to a
  Local/Param whose declaration span lies outside the innermost
  open closure's span is a capture. Nested closures are
  handled — the inner closure's span is contained within the
  outer's, so the inner rejects captures from the outer's
  frame (correct), and from any frame outside the outermost.

## What's next

- **Capturing closures + `Fn` trait** — synthesize a struct
  per lambda site holding the captured fields; add `Fn(args) ->
  ret` trait family; adapter fields generalize to `F: Fn(...) ->
  ...` bounds.
- **`HashMap<K, V>`** — the other major collection.
- **`Range` as `RangeIter`** — unify the for-over-range codegen
  with the Iterator protocol.
- **`continue` keyword** — the last unsupported loop control-flow
  primitive.
