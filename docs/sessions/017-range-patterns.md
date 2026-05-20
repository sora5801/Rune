# Session 017 — Range patterns

**Date:** 2026-05-20
**Outcome:** `lo..hi` and `lo..=hi` work as match patterns on integer
and char scrutinees. Bounds are literals with optional unary `-` on
numeric sides; mixed-type and empty ranges error at type-check time.
Range patterns nest inside or-patterns and combine with guards. 313
tests green (+14 — 5 codegen, 9 typecheck).

## Surface syntax

```rune
fn bucket(n: i64) -> i64 {
    match n {
        0..=9       => 1,
        10..=99     => 2,
        100..=999   => 3,
        -100..=-1   => -1,        // negative literal bounds work
        _           => 0,
    }
}

fn label(n: i64) -> i64 {
    match n {
        1..=3 | 7..=9      => 1,  // inside or-patterns
        0..=10 if n == 5   => 99, // with guards
        _                  => 0,
    }
}
```

Exclusive `0..10` (not including 10) and inclusive `0..=10` both
parse. The same `..` and `..=` tokens used as infix range operators
in expressions now also appear in pattern position; the parser
disambiguates by context (we're inside `parse_pattern_atom`, so we're
clearly parsing a pattern).

## What the parser does

After `parse_pattern_atom` consumes a literal — possibly negated —
it peeks for `..` or `..=`. If present, it consumes the operator and
parses a second literal as the upper bound:

```rust
| TokenKind::Minus => {
    let (lit, s) = self.parse_pattern_lit()?;
    if let Some(inclusive) = self.peek_range_op() {
        self.bump();
        let (hi, hi_span) = self.parse_pattern_lit()?;
        return Ok(Pattern::Range { lo: lit, hi, inclusive, ... });
    }
    Ok(Pattern::Literal { lit, span: s })
}
```

`parse_pattern_lit` is a small helper that accepts an optional `-`
before an `Int` or `Float` literal and folds it into the literal value
(`Lit::Int(v) → Lit::Int(-v)`). Unary `-` on a non-numeric literal is
rejected with `unary - is only valid on numeric literals in patterns`.

A free side effect: bare negative literal patterns like `-5 => ...`
now also work, because the same code path produces them when there's
no `..` following.

## Pattern type-check

`check_range_pattern` enforces three rules:

1. **Bound types must agree.** Both bounds are `Lit::Int`, or both
   are `Lit::Char`. `0..='z'` errors with `range pattern bounds must
   be two integers or two chars`.
2. **Bound type must match scrutinee.** Int bounds require an integer
   scrutinee; char bounds require `Ty::Char`. Otherwise:
   `range pattern with integer/char bounds doesn't match scrutinee
   type X`.
3. **Range must be non-empty.** Inclusive requires `lo <= hi`,
   exclusive requires `lo < hi`. `10..=0` errors with `range pattern
   '10..=0' is empty (lo must be <= hi)`.

Range patterns introduce **no** bindings, so `bind_pattern` is a no-op
on them, and the resolver's `declare_pattern` short-circuits as well.

## Exhaustiveness

Range patterns contribute **nothing** to the exhaustiveness coverage
sets. The reasoning:

- For integers, the domain is up to 2^64 values; tracking which
  intervals are covered by which arms would need an interval tree
  and overlap detection.
- For chars, the domain is the Unicode scalar set; same issue at a
  smaller scale.
- Rust historically didn't track range coverage either (although
  exhaustive_patterns is closing the gap).

Consequence: a match with only a range and no `_` arm still errors
with "non-exhaustive on i64; add a `_` arm", even if the range
visibly covers a huge subset. Acceptable for v0.x.

In `cover_pattern`:

```rust
Pattern::Range { .. } => {
    // Ranges cover a subset of an infinite domain; we don't track
    // partial coverage. They neither contribute to exhaustiveness
    // nor cause duplicate-arm errors against literals or other
    // ranges.
}
```

This means **no** unreachable-arm errors fire for `0..=10, 5 => ...`
(both arms remain "alive"), and **no** duplicate-arm errors for
`0..=10, 0..=10 => ...`. The cost is one extra branch at runtime
when the second arm is genuinely dead. Tracking can come later.

## HIR shape

```rust
pub enum HirPattern {
    Wildcard,
    Bind(SymbolId),
    IntLit(i64),
    BoolLit(bool),
    StrLit(String),
    EnumVariant { discriminant: u32 },
    IntRange { lo: i64, hi: i64, inclusive: bool },
}
```

Only one new variant. Chars are pre-converted to their codepoint
(`Lit::Char(c) → lo = c as i64`) by the lowerer's `lit_to_int_bound`,
so codegen only ever sees i64 bounds.

## Codegen

The pattern-check block emits `lo <= scrut && scrut [<|<=] hi` as
two icmps separated by a brif. On a match, jump to the body; on
either no-match, jump to `on_no_match` (which is the next arm's
check block, or the next or-pattern alternative).

```rust
let lo_ok = icmp(le_cc, lo_v, scrutinee);
let check_hi = create_block();
brif(lo_ok, check_hi, [], on_no_match, []);
switch_to_block(check_hi);
let hi_ok = icmp(if inclusive { le_cc } else { lt_cc }, scrutinee, hi_v);
brif(hi_ok, on_match, [], on_no_match, []);
```

Signed vs unsigned icmp is chosen by the scrutinee type — `i8/i16/
i32/i64/isize/char` use signed, `u8/u16/u32/u64/usize` use unsigned.
Char is signed-OK because all valid Unicode scalars (≤ U+10FFFF) fit
in the non-negative half of `i32`.

Negative literal patterns (`-5 => ...`) ride on `iconst.i64(-5)`,
which Cranelift sign-extends correctly across the various integer
widths.

## What's tested

Codegen (+5):
- `range_pattern_inclusive_in_middle` — boundary values land in the
  expected bucket (0, 9, 10, 99, 100, 999)
- `range_pattern_exclusive_excludes_upper` — `0..10` doesn't match 10
- `range_pattern_negative_bounds` — `-100..=-1` works
- `range_pattern_in_or_pattern` — `1..=3 | 7..=9 => ...`
- `range_pattern_with_guard` — `0..=10 if n == 5 => ...` ordering

Typecheck (+9):
- `range_pattern_int_typechecks` — `0..=9` on i64 scrutinee
- `range_pattern_char_typechecks` — `'a'..='z'` on char scrutinee
- `range_pattern_mismatched_to_bool_errors` — int range on bool errors
- `range_pattern_mismatched_char_on_int_errors` — char range on int
- `range_pattern_mixed_bounds_errors` — `0..='z'` rejected
- `range_pattern_inclusive_empty_errors` — `10..=0` rejected
- `range_pattern_exclusive_empty_errors` — `5..5` rejected
- `range_pattern_without_catchall_errors` — `0..=100 => ...` alone
  still needs `_`
- `range_pattern_in_or_typechecks` — `1..=3 | 7..=9` accepted

## File layout changes

```
src/
├── ast.rs         (Pattern::Range { lo, hi, inclusive, span })
├── parser.rs      (parse_pattern_lit helper for `-Int|-Float|Lit`;
│                   parse_pattern_atom peeks .. / ..= after a literal)
├── resolver.rs    (declare_pattern no-op on Range)
├── checker.rs     (check_range_pattern: bounds-type / scrutinee
│                   match / non-empty; bind_pattern no-op; cover_pattern
│                   no-op for ranges)
├── hir.rs         (HirPattern::IntRange)
├── lower.rs       (lit_to_int_bound helper; collect_arm_patterns
│                   builds IntRange; let/for reject Range)
└── codegen.rs     (compile_pattern_check IntRange: two icmps + brif,
                    signed vs unsigned per scrutinee type)
tests/
├── codegen.rs     (+5 tests)
└── typecheck.rs   (+9 tests)
LANGUAGE.md        (decision log entry)
```

## Apparent bugs that aren't

- **`0..=10, 0..=10 => ...` doesn't fire an unreachable error.**
  Correct under the current design — range coverage isn't tracked.
  The second arm is dead at runtime, costing one branch.

- **`0..=10, 5 => ...` accepts the `5 => ...` arm even though `5` is
  already covered.** Same reason. The literal-coverage set doesn't
  cross-check against ranges.

- **Char ranges accept any `char` scrutinee but can't actually run
  yet.** Char *literals* lower to `HirLit::Unit` today
  ([lower.rs:340](src/lower.rs:340)), so passing a char as a function
  argument fails codegen. The type-check path works; the codegen path
  needs `HirLit::Char` first. Tracked as a follow-up.

- **`as u32` casts don't work yet.** Codegen has an explicit
  `Unsupported("as cast")` arm. The unsigned codegen branch for
  range patterns is exercised in the test corpus only via the i64
  fallthrough — adding `as` codegen will unlock end-to-end u32 tests.

## What's still TODO for match

- **Char literal codegen** (`HirLit::Char`) so char scrutinees can
  reach the runtime.
- **`as` cast codegen** so the unsigned-int range path can be tested
  end-to-end.
- **Range overlap / coverage tracking** for unreachable-arm detection.
- **Payload destructuring** (`Some(x) => ...`) — still needs the
  payload-bearing variant design pass.
- **Parser precedence bug** — `!f(x)` still parses as `(!f)(x)`;
  carried over from session 016's deferred list.

## Next session

Natural picks from the standing list:

- **Char literal codegen.** Unlocks char ranges, char comparisons,
  and char-keyed match arms end-to-end. Small.
- **Parser precedence fix for `!f(x)`.** Small.
- **`as` cast codegen.** Modest; needs sign/zero-extend dispatch per
  source/dest pair.
- **Generics step 1 (parser).** Bigger; disambiguates `<T>` from
  comparison-`<`.
- **ARC reclamation step 2.** Big; touches every alloc, copy, drop.
