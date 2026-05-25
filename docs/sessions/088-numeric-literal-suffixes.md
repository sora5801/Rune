# Session 088 — Numeric literal suffixes

**Date:** 2026-05-25
**Outcome:** `10i32`, `42u64`, `3.14f32`, `0xffu8` all
lex as typed literals. The checker uses the suffix to
pin the literal's type instead of the i64/f64 default,
so non-default-typed numeric workloads no longer need
`as` casts on every constant. 406 codegen tests green
(+5 from session 087).

```rune
let a: i32 = 10i32;
let b: u64 = 42u64;
let pi: f32 = 3.14f32;
let mask: u8 = 0xffu8;

let untyped = 42;        // still defaults to i64
```

## The decisive observation

A literal's type is set in one place: `lit_type` in the
checker. Everything else — coercion against let
annotations, binop arithmetic, fn-arg matching — just
reads from there. So the entire feature reduces to:
parse a suffix at lex time, carry it through to the
literal AST node, and have `lit_type` return it.

### Lexer: scan suffix after digits

After consuming the digit body (and any `.fraction` /
`eExponent` for floats), peek for a suffix token:

```rust
fn scan_numeric_suffix(&mut self, _start: usize)
    -> (Option<IntTy>, Option<FloatTy>)
{
    let Some(first) = self.peek() else { return (None, None) };
    if first != 'i' && first != 'u' && first != 'f' {
        return (None, None);
    }
    // Collect into a buffer until a non-alphanumeric char.
    // Match the buffer against the 12 valid suffix names.
    ...
}
```

Mismatched suffixes (`10f32` on a digit-only token,
`3.14i64` on a float, an unknown suffix entirely) error
at lex time with a clear diagnostic. Suffix-shaped
identifiers that aren't real suffixes (`10foo`) leave
the digits intact and let the lexer produce an `ident`
token next — the parser will error on the resulting
juxtaposition.

Same logic fires after `int_with_radix` so `0xffu8`
works too.

### TokenKind / Lit carry the suffix

```rust
TokenKind::Int(i64)        →  TokenKind::Int(i64, Option<IntTy>)
TokenKind::Float(f64)      →  TokenKind::Float(f64, Option<FloatTy>)
Lit::Int(i64)              →  Lit::Int(i64, Option<IntTy>)
Lit::Float(f64)            →  Lit::Float(f64, Option<FloatTy>)
```

`None` means "no suffix; use surrounding hint or
default to i64 / f64." `Some(ty)` overrides.

### Checker: `lit_type` returns the suffix

```rust
fn lit_type(&self, lit: &Lit) -> Ty {
    match lit {
        Lit::Int(_, Some(ty)) => Ty::Int(*ty),
        Lit::Float(_, Some(ty)) => Ty::Float(*ty),
        Lit::Int(_, None) => DEFAULT_INT,
        Lit::Float(_, None) => DEFAULT_FLOAT,
        ...
    }
}
```

The lowerer's existing flow — pull the literal's type
from `expr_types` and emit `HirLit::Int(v, int_ty)`
with the right `IntTy` — Just Works because the type
came out of `lit_type` already.

## The wire-ups

```
src/lexer.rs      (scan_numeric_suffix helper; both
                   decimal and radix-prefixed paths
                   call it and attach the suffix to
                   TokenKind::Int / Float.)

src/token.rs      (TokenKind::Int / Float gain
                   Option<IntTy> / Option<FloatTy>;
                   Display impl updated.)

src/ast.rs        (Lit::Int / Float gain matching
                   suffix slot.)

src/parser.rs     (literal parsing threads the suffix
                   from TokenKind into Lit; negation
                   in patterns and tuple-index .N
                   handle the new shape; describe_kind
                   matches the new arity.)

src/checker.rs    (lit_type returns Ty::Int(suf) /
                   Ty::Float(suf) when Some; cover-
                   pattern and range-pattern matches
                   updated to the new tuple arity.)

src/lower.rs      (Lit::Int(v, _) / Float(v, _)
                   patterns updated; the existing
                   type-from-expr_types path produces
                   the right IntTy without further
                   change.)

tests/codegen.rs  (+5 tests: i32, u32, f32, no-suffix
                   default, hex+u8 radix-prefixed
                   suffix.)

tests/lexer.rs +  (matches updated for the new
tests/parser.rs    Int/Float arity.)
```

No resolver, monomorphize, or codegen changes —
suffixes resolve to concrete `Ty::Int(ty)` /
`Ty::Float(ty)` at type-check time and the rest of the
pipeline already handles every primitive numeric Ty.

## What's tested

Codegen (+5):

- `numeric_literal_suffix_i32` — `10i32 + 20i32` typed
  inferences without `as` casts.
- `numeric_literal_suffix_u32` — `100u32 - 30u32`.
- `numeric_literal_suffix_f32` — `3.14f32 * 2.0f32`,
  cast to i64 for the test result.
- `numeric_literal_suffix_default_unchanged` — bare
  `42` still defaults to i64 (no regression).
- `numeric_literal_suffix_hex_with_u8` — `0xffu8`
  works on radix-prefixed literals.

## Apparent bugs that aren't / explicitly deferred

- **Surrounding-hint coercion still doesn't override
  the default.** `let a: i32 = 10;` still errors
  ("i32 expected, got i64") because the literal's
  type comes from `lit_type` (i64 default) before the
  let's annotation checks compatibility. To fix
  generally would require bidirectional inference at
  the literal level — a separate session ("integer
  literal hint flow"). For now, users write `10i32`
  or `10 as i32`.
- **Suffix on a value that doesn't fit the type** —
  `1000u8` lexes fine (256 doesn't fit u8) but the
  checker doesn't reject it. Codegen happily emits
  the truncated value. Adding range-check at the
  checker would be a polish pass.
- **Char codepoint suffixes** — none planned; `'A'`
  is its own literal form.
- **Mixed-type arithmetic still errors** —
  `let a: i32 = 5i32 + 1` mixes i32 and i64 (the bare
  `1` is i64). Same surrounding-hint limitation; user
  writes `5i32 + 1i32` until literal-hint flow lands.
- **String escape suffix collision** — `"foo"i32`
  isn't a thing in Rune; lexer's string scanner
  stops at the closing `"` regardless.

## What's next

- **Cartesian-product exhaustiveness for tuple
  patterns** — session 082's deferred item.
- **Integer literal hint flow** — `let x: i32 = 10;`
  picks i32 from the annotation instead of defaulting
  to i64 (the polish-pass deferred above).
- **Same-target Into duplicate detection** — session
  086's deferred item.
- **Self-hosted bootstrap** — long-term.
