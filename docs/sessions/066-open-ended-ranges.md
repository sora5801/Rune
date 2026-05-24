# Session 066 — Open-ended ranges (`..n`, `n..`)

**Date:** 2026-05-24
**Outcome:** All four range forms work — `a..b`, `a..=b`,
`..n`, `n..`, and the fully-open `..`. Missing start defaults
to 0; missing end defaults to `i64::MAX` (the sentinel for
"no upper bound" — the user is expected to break out
themselves). Three forms now compile and run:

```rune
for i in ..5 { ... }              // 0..5
for i in 5.. { if i > 10 { break; } ... }   // 5..i64::MAX
let r: std::RangeIter = 100..;    // 100..i64::MAX, drive manually
```

~3 files. 572 tests green (+4 from session 065).

## The decisive observation

The parser already had `Expr::Range { start: Option<Box<Expr>>,
end: Option<Box<Expr>>, ... }` as the AST shape — the Optional
slots were always there to allow this. But the parser only
produced them in the `lhs..rhs` infix form, with both sides
always Some. Two small parser extensions surface the open
forms:

1. **Prefix range** (`..n`, `..=n`, `..`): handled in
   `parse_unary` before falling through to `parse_primary`.
   When the next token is `..` or `..=`, consume it, then try
   to parse an end expression. The `can_start_expr` helper
   decides — if the next token is an expression-starter, consume
   it; otherwise leave end as None.
2. **Open-end infix** (`n..`): in the existing infix Range arm,
   after consuming the operator, check `can_start_expr`. If
   not, leave end as None and use the operator's span end.

The lowering paths (for-over-range fast path AND the RangeIter
struct-lit form) both already had `.map(|e| self.lower_expr(e))
.unwrap_or_else(|| ...lit zero)` for missing slots. Replace the
"zero" fallback for `end` with `i64::MAX`. (Start stays zero —
which is the right default.)

i64::MAX is the cleanest sentinel: the runtime exit check (`cur
< end`) terminates eventually, no special-case branch is
needed, and the user is expected to `break` out before then.
Iterating 2^63 elements isn't a real workload — by the time
you do, hardware ages out — so the leak of "would in principle
keep running" is bounded.

## The wire-ups

```
src/parser.rs        (parse_unary now peeks for DotDot /
                      DotDotEq at expression-start position and
                      builds Expr::Range with start=None. The
                      infix Range arm checks can_start_expr
                      before parsing the rhs; if false, leaves
                      end=None. New can_start_expr helper is a
                      conservative whitelist of tokens that can
                      legitimately start an expression.)

src/lower.rs         (Both range-lowering paths default missing
                      end to a Lit(i64::MAX) instead of Lit(0).
                      The for-over-range fast path
                      (HirExprKind::ForRange) and the RangeIter
                      struct-lit form (lower_range_as_iter) get
                      the same treatment so the two paths agree
                      on the sentinel.)
```

That's it — two files. The checker, monomorphizer, codegen,
and runtime are unchanged because nothing in the type system
cares about open vs closed: the AST shape is the same, and the
runtime sees a normal `cur < end` check.

## What's tested

Codegen (+3):

- `range_open_start_in_for_loop` — `for i in ..5 { ... }` sums
  0..5 = 10 through the for-range fast path.
- `range_open_end_with_break` — `for i in 5.. { if i > 10 {
  break; } ... }` sums 5..=10 = 45, exits via the user's
  break.
- `range_open_end_as_iter_value` — `let r: std::RangeIter =
  100..;` drives manual `.next()` calls; stops after 3.

The existing `range_iter_inclusive_form` and the
session-063 range tests still pass — open and closed forms
share the same plumbing.

## Apparent bugs that aren't / explicitly deferred

- **Inclusive-open (`n..=`) doesn't parse.** Not a real form
  in any language we'd model — an open-ended range with an
  inclusive upper bound makes no semantic sense.
- **i64::MAX overflow.** If the user actually iterates close
  to i64::MAX, `cur + 1` overflows in the next() body. v0.x's
  i64 arithmetic wraps silently (no panic). Real-world
  open-end usage breaks out way before this matters.
- **`..` as a fully-open range** parses and lowers to
  `0..i64::MAX`. Probably not useful but it falls out of the
  uniform handling.
- **The parser can't distinguish `n..` from `n.. + foo`** —
  `can_start_expr` looks at the next token only. If a binary
  expression like `n.. + foo` were written, the parser would
  treat `..` as open (no rhs) and then `+ foo` as a syntax
  error in some outer context. In practice this isn't
  ambiguous because `n..` is followed by `{`, `)`, `,`, `;`,
  etc. in the common idioms; deferred.
- **Reverse-direction ranges** (`5..1`) yield zero items, as
  before. Open-ended `5..3` (impossible to write) would too.

## What's next

- **HashMap value-release walk** — close the V-leak from
  session 064.
- **HashMap str-keys + remove + iteration** — natural
  extensions.
- **`?` on Option** — currently Result-only.
- **Trait default-method bodies** — `.collect()` as a chained
  method off iterator adapters.
- **Self-hosted bootstrap** — the long-term goal.
