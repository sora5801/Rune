# Session 100 — Milestone retrospective

**Date:** 2026-05-25
**Sessions covered:** 1 — 99 (Rune started 2026-05-19,
six days ago).

A reflective doc at the 100-session mark. `V0X-AUDIT.md`
catalogs *features* (what works, what's deferred);
this one catalogs *lessons* (what I'd do again, what I'd
do differently, what surprised me). Docs-only.

---

## What worked structurally

### The "decisive observation" framing

Every session doc opens with a paragraph that names the
key insight before the wiring: "feature X reduces to
threading data Y through site Z." Reading them back,
the framing is the most valuable artifact. It forced me
to find the right abstraction before writing code, and
it makes the diff feel small even when the typing was
large.

Most sessions, once the observation was named, the code
became mechanical. Sessions where I started typing
without the observation produced larger diffs and more
revisions.

### Two-layer mirroring

When a feature touched both the checker and the
codegen, the second layer almost always mirrored the
first. The checker establishes the *shape* (does this
type-check, what's the inferred type, what's the
diagnostic?), then codegen reads the same shape from
`expr_types` and emits the right IR. Examples:

- Session 088 (literal suffixes) only needed lit_type
  change in checker; codegen consumed the right
  IntTy automatically.
- Session 097 (codegen-side diagnostic polish)
  re-ran session 093's checker-side `ty_pretty`
  pattern in codegen with `sym_names` propagated
  through HIR.
- Session 087 (intrinsic Numeric impls) added
  primitive_anchors in the resolver and
  primitive_anchor lookup in the checker / lower /
  codegen — same shape, three layers.

The corollary: if a feature feels like it needs deep
codegen surgery, the checker probably hasn't done
enough yet. Push more work into types.

### Trait default-method bodies as the leverage point

Session 071 (`fn collect(self: Self) -> Vec<Self::Item>
{ ... }` as a default-body method on Iterator) was the
single highest-ROI session. The decisive observation
("a default body is a generic free function in
disguise") meant zero monomorphizer changes. The
sessions that followed (076 `.count` `.sum`, 077-080
method-level generics + `.filter`/`.map`, 084 `.min`
`.max`, 080 `.fold`) all rode on the same machinery.
By session 084 the iterator method chain
`v.iter().filter(p).map(f).fold(0, |a, x| a + x)`
worked end-to-end without any new monomorphization
support.

### The matrix algorithm reuse

Session 089 implemented Maranget's usefulness test for
tuple-pattern exhaustiveness via four free functions
(`specialize_bool`, `specialize_enum_disc`,
`specialize_default`, `tuple_matrix_is_exhaustive`).
Session 094 added per-arm unreachability detection —
one new function (`tuple_matrix_is_useful`) next to
the existing ones, sharing the same specialize_*
helpers. Adding one concept cost one function.

This is what "good factoring" looks like in
retrospect: the second user of the abstraction is the
test that the abstraction was real.

### The hint-flow framework

`check_expr_with_hint` (introduced in session 062 for
struct-lit-field closure hints) became the universal
"feature wants to flow context inward" mechanism.
Every later session that added a new hint source
(let-binding 062, fn-arg 081, struct-field 062,
binop 095, method-call receiver 096, `.into()` 086,
unary-neg 091, literal 091, integer hint 099)
added one match-arm at the top of the function. The
caller didn't change, the callees mostly didn't
change.

### Single source of truth for the runtime

`runtime.c` is compiled once into the host (`build.rs`)
for JIT symbol registration AND linked directly by AOT
builds. No duplication, no drift. When a feature
needed a runtime function (HashMap, ARC helpers,
Weak), it landed in `runtime.c` and both pipelines
saw it on next rebuild. Decided session 045; held
through ~50 follow-on sessions.

---

## Decisions I'd make again

- **i64 default for unannotated integer literals.**
  Friction-free for the systems-leaning code most
  Rune programs are. Suffix flow (session 088) +
  hint flow (091, 094, 095, 096, 099) covers the
  cases where i64 isn't what users want.

- **Cranelift over LLVM.** Fast compilation,
  predictable feature surface, no version-pinning
  pain. The downside (less optimization) hasn't
  bitten because Rune programs don't need extreme
  optimization.

- **ARC over GC.** Predictable performance, no
  scheduler. The per-type release synthesis was more
  work than expected (every new type that contains
  ARC fields needed wiring) but tractable. By
  session 074 the pattern was so mechanical that
  adding tuple ARC release (a brand-new type
  category) took one session.

- **Prelude (`mod std`) written in Rune itself.**
  Forced the language to be expressive enough to
  write its own collection types. Surfaced
  limitations early (sessions 062, 071, 077-080 all
  pushed back from std.rn limitations).

- **Monomorphization over runtime polymorphism for
  generics.** Zero-cost generics. The trade-off
  (binary size) hasn't been a problem because Rune
  programs are small.

- **Document every deferral.** Every session doc has
  an "Apparent bugs that aren't / explicitly
  deferred" section. Reading them back, I can
  reconstruct *why* a limitation exists, not just
  *that* it exists. Worth its weight ten times
  over.

---

## Decisions I'd make differently

- **The 8-byte-slot Vec restriction.** Decided early
  to keep Vec simple: every element must fit one
  i64-sized word. Made floats, arrays, and tuples
  not fit as Vec elements. Lifting it (eventually a
  pre-1.0 session) requires per-element-size codegen
  paths. If I'd done variable-width Vec from the
  start, sessions 042 (arrays), 073-075 (tuples),
  and the upcoming float-Vec work would be one
  unified mechanism.

- **`TypeVar` as compatible-with-anything in
  `Ty::compatible`.** Session 047-era decision to
  make generic inference cheap. Has caused
  subtle over-acceptance: session 086's
  `try_into_disambiguation` (`compatible(target,
  expected)`) over-matches when targets are
  generic. Session 090 (duplicate-target detection)
  has the same issue. The right answer is
  probably "compatible-modulo-known-subst" rather
  than "compatible-treating-vars-as-wildcards."

- **The `current_self_param` slot in the checker.**
  Added session 078 to make `Self::Item` resolve
  to a substitutable typevar inside trait default
  bodies. It works but feels like a side-channel
  — the checker would be cleaner if the resolver
  represented `Self::Item` with a sym directly
  instead of through a stash.

- **Mangled fn names via span-start hash for Into
  multi-impl.** Session 072's `IoErr__into__{span.start}`
  is a workaround for "two `into` fns on one struct
  collide at Cranelift's symbol table." A cleaner
  fix would be a proper module-level symbol
  table that handles overload-shaped collisions
  without span-strings in names.

- **Lex-time numeric suffix recognition.** Session
  088 added suffix parsing inline in `number()`.
  A separate `parse_numeric_suffix` token after
  the digit body would have been cleaner — the
  current approach uses a peek-loop that's a bit
  scratchy.

---

## Surprises

### Positive

- **How small the cross-cutting refactors stayed.**
  ~100 sessions added types, traits, generics,
  closures, HashMap, tuples, iterators, modules,
  ARC, `?`/Into, default methods, exhaustiveness,
  literal suffixes, hint flow. Almost all as
  additions. The biggest refactor was probably
  session 086's struct-lit pass-1 deferral
  extension, which was ~10 lines.

- **The hint-flow framework's compositionality.**
  Sessions 062, 081, 086, 091, 095, 096, 099 all
  added one match-arm to `check_expr_with_hint`.
  Each session's tests passed without breaking
  earlier sessions'. The framework absorbed the
  load.

- **Test stability.** 843 tests across 100 sessions,
  almost no flake. The few regressions (session
  079's check_binary opaque pass-through, session
  085's `apply_subst_ty` Tuple gap) were caught at
  the per-session test run and fixed in the same
  session. The test suite became a real safety net.

- **Diagnostic polish came easier than expected.**
  Sessions 093 and 097 added friendly type names
  in errors. The mechanism (snapshot sym names,
  thread through to format) was straightforward
  once the abstraction (`ty_pretty(ty, names)`)
  was right.

- **Bound propagation cascade landing in one session.**
  Sessions 077-080 wrestled with this iteratively,
  but the final shape (recursive walk of bound
  args, unify positionally with the concrete Fn /
  closure-struct's call sig) was clean. The mono-
  side single-missing-generic fallback (session
  078) ended up unused but its presence
  documents the corner case.

### Negative

- **`apply_subst_inner_with`'s catch-all dropping
  Tuple subst.** Session 075 found that the
  checker's substitution helper had a `_ =>
  ty.clone()` arm that silently lost Tuple /
  HashMap / Dyn substitutions. Session 085 then
  found the same gap in the lowerer's `apply_subst_ty`.
  Both fired only when a feature happened to
  exercise the gap. Lesson: exhaustive matches on
  Ty are worth the verbosity.

- **The closure-as-struct synthesis is intricate.**
  Sessions 058-061 introduced closures; the
  capturing-struct mechanism with synthesized
  call methods, generic-fn-impl-for-closure, and
  Fn1/Fn2 traits is correct but tangled. Hard to
  reason about without re-reading the per-session
  docs. Probably fine for v0.x; for self-hosted
  bootstrap, this is the hardest piece to port to
  Rune itself.

- **Test runtime crept up.** Session 1 ran tests in
  ~0.2s; session 100 takes ~5s. Mostly the
  codegen tests (each runs the full pipeline). Not
  bad, but the trend matters for self-hosted
  bootstrap, where the compiler tests itself.

- **Some session docs are dense.** Sessions 077-080
  (bound propagation cascade) are walls of text
  because the design space was tangled. They're
  useful as references but not enjoyable as reads.
  Lesson: a complex topic should have a "summary
  table" up front before the prose dive.

---

## Patterns that emerged

- **Per-shape release synthesis.** First introduced
  for Vec<T> in session 023, then echoed for
  Array (session 043), HashMap V (session 067),
  Tuple (session 074), Array nested (session 042).
  Each new type category that holds ARC fields
  follows the same template: collect distinct
  shapes during monomorphization, declare a
  release fn per shape in Pass 0, define in Pass 3
  with a walk over the ARC slots.

- **Bidirectional inference via `check_expr_with_hint`.**
  Introduced session 062, expanded by every
  subsequent type-inference session. The
  shape never changed: same function signature,
  add a new match-arm at the top, fall through to
  `check_expr` for non-hinted cases.

- **The "session N closes session M's deferred X"
  pattern.** ~half the sessions explicitly closed a
  prior session's deferred item. The deferred-with-
  rationale documentation made these straightforward
  to find when revisiting.

- **The big-leverage session.** Once per ~10
  sessions, one session unlocked many downstream
  features. Session 048 (generic impls), session
  071 (trait default bodies), session 088 (numeric
  literal suffixes), session 091 (literal hint
  flow). These sessions feel small while
  implementing but show up as the foundation for
  many later sessions.

- **The "v0.x scope cut."** Most sessions defer
  something explicitly with a clear future hook.
  Examples: full Numeric trait deferred from
  session 084 to a Numeric-trait session;
  cartesian exhaustiveness deferred from session
  082 to session 089. The deferrals weren't
  arbitrary — each was named, and the future
  session usually had the precise observation
  already documented.

---

## What's still surprising

After 100 sessions:

- **How much the language design held up.** The
  syntactic decisions from session 1 (i64 default,
  Rust-flavored syntax, `match` arms, expression-
  oriented blocks) never needed revision. The
  type-system decisions from sessions 5-25 (Ty
  shape, monomorphization, ARC) also held. The
  things that needed revision were specific
  implementation choices (TypeVar compatibility,
  the 8-byte-slot Vec restriction), not language
  shape.

- **How small the compiler stayed.** ~6.5k lines
  in src/checker.rs (the biggest), ~4.5k in
  codegen.rs, ~3k in lower.rs, ~2.5k in
  resolver.rs, ~2k in parser.rs. Total ~25k lines.
  The runtime is another ~1k in runtime.c.
  Cranelift handles the heavy lifting; Rune is the
  language part, not the optimization or
  scheduling part.

- **How much of the work was finding the right
  framing rather than typing.** Sessions where
  the framing came naturally took 1-2 hours;
  sessions where it didn't took 4-6 hours. The
  decisive observation isn't optional — it's the
  speed-limiting step.

- **The pre-1.0 horizon feels reachable.** Reading
  V0X-AUDIT.md's "Pre-1.0 priorities" section,
  every item has a clear plan and rough effort
  estimate. The biggest single piece (self-hosted
  bootstrap) is the only one that's months rather
  than sessions.

---

## The next 100 sessions

The shape of the work changes:

- **Less feature-adding, more tightening.** Most of
  the surface is there. The remaining items are
  edge cases and polish.
- **More cross-cutting refactors.** Some of the
  decisions I'd make differently above (8-byte-slot
  Vec, TypeVar compatibility) will need fixing
  before 1.0.
- **Self-hosted bootstrap starts.** Probably session
  ~130 for the first Rune-in-Rune attempt at the
  lexer. By session ~200, the parser. By session
  ~300, the checker. Etc.
- **Performance work emerges.** Currently the
  compiler is fast, the runtime is fast-enough.
  Real workloads will eventually surface
  bottlenecks. ARC's atomic ops, HashMap probe
  performance, struct field layout.

I'll keep the per-session doc + LANGUAGE.md row +
README roadmap update workflow. It's worked. The
output isn't always efficient, but it's always
documented.

100 sessions to a coherent language. Onward.
