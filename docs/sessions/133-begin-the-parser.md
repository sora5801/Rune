# Session 133 — The parser begins

**Date:** 2026-05-25
**Outcome:** First Rune-in-Rune parser module
(`examples/bootstrap/parser.rn`). Consumes the
session 128-132 lexer's `Vec<Spanned>` and produces
an `Expr` AST via Pratt-style precedence climbing.
Atom + unary-neg + binary ops (`+`, `-`, `*`, `/`,
`%`) with correct associativity. End-to-end demo
parses `(1 + 2 * 3) - (10 / 5)` and an inline
evaluator returns 5. 541 codegen + 47 AOT + 223
typecheck tests green (+10 codegen from session
132).

```
$ cargo run -- run examples/bootstrap/main.rn
5
```

```rune
// examples/bootstrap/main.rn
mod lexer;
mod parser;

fn main() -> i64 {
    let src: str = "(1 + 2 * 3) - (10 / 5)";
    let toks = lexer::tokenize(src);
    let p = parser::new_parser(toks);
    let ast = parser::parse_expr(p);
    parser::eval(ast)   // 7 - 2 = 5
}
```

## The decisive observation

Pratt-style precedence climbing is short. The core
loop is:

```rune
fn parse_bp(p: Parser, min_bp: i64) -> Expr {
    let lhs = parse_unary(p);
    parse_binops(p, lhs, min_bp)
}

fn parse_binops(p: Parser, lhs: Expr, min_bp: i64) -> Expr {
    let bp = binop_bp(peek(p));
    if bp == 0 || bp <= min_bp { return lhs; }
    let op_tok = advance(p);
    let op = binop_of(op_tok.kind);
    let rhs = parse_bp(p, bp);
    let combined = Expr::Binary { op, lhs, rhs };
    parse_binops(p, combined, min_bp)   // continue parsing right-side ops
}
```

Each operator has a binding power; the recursive
`parse_bp` only consumes operators whose bp is
strictly greater than the surrounding `min_bp`.
Left-associativity falls out automatically because
recursive calls pass `bp` (not `bp+1`), so equal-
precedence ops chain via the outer loop rather than
nesting.

### AST shape

Three node kinds:

```rune
pub enum Expr {
    Num(i64),
    Ident(str),
    Neg(Expr),
    Binary { op: BinOp, lhs: Expr, rhs: Expr },
}
pub enum BinOp { Add, Sub, Mul, Div, Mod }
```

`Expr` is recursive (a Binary's lhs and rhs are
themselves Exprs). Session 125 confirmed recursive
enum payloads work without `Box<T>` — each Expr
value is a pointer, so the recursion is
pointer-indirected at the runtime layout.

### Cross-module Token reference

The parser refers to `lexer::Token::Plus` etc. The
key wrinkle: `mod lexer;` at the top of parser.rn
would look for `parser/lexer.rn` (per session 020's
nested-module-into-subdirectory rule). Instead,
main.rn declares both `mod lexer;` and `mod parser;`
as siblings; parser.rn references `lexer::*` paths
absolutely, relying on the resolver's "try absolute
then relative" path lookup (session 124's tests
exercise this exact shape).

### Why a Parser struct (not just position)

```rune
pub struct Parser {
    tokens: Vec<lexer::Spanned>,
    pos: i64,
}
```

Wrapping the position with the token vector keeps
the cursor and the data together. `peek(p)` reads
`p.tokens.get(p.pos).kind`; `advance(p)` returns
the current and bumps `p.pos`. The struct mutates
via interior pointer (same pattern as session 121's
String, session 132's Spanned) — `let p = new_parser
(toks); parse_expr(p);` works without `let mut`.

### Inline evaluator for test coverage

Each codegen test parses an expression and
evaluates it via `eval(ast) -> i64`. This is a
shape-check by proxy: if `1 + 2 * 3` parses to the
wrong precedence tree, eval returns 9 instead of 7.
The eval function handles each AST shape:

```rune
pub fn eval(e: Expr) -> i64 {
    match e {
        Expr::Num(v) => v,
        Expr::Ident(_) => 0,         // no environment yet
        Expr::Neg(inner) => 0 - eval(inner),
        Expr::Binary { op, lhs, rhs } => {
            let l = eval(lhs); let r = eval(rhs);
            match op { Add => l + r, ... }
        }
    }
}
```

Ten codegen tests cover atom (int + paren), unary
neg, basic addition, precedence (mul over add),
left-associativity (10 - 3 - 2 = 5), paren
override, mixed complex expression, mod op,
negation-of-paren.

### What's not here

Tracking against the lexer's coverage:

- ✅ Atom: integer literals, identifiers, parens
- ✅ Unary minus
- ✅ Binary arithmetic (+ - * / %)
- ⏳ Comparison ops (`==` `!=` `<` `<=` `>` `>=`)
- ⏳ Logical ops (`&&` `||` `!`)
- ⏳ Bitwise ops (`&` `|` `^` `<<` `>>`)
- ⏳ Function calls `f(a, b)`
- ⏳ Field access `obj.field` and method calls
- ⏳ Indexing `arr[i]`
- ⏳ Casts `x as T`
- ⏳ Range `a..b`
- ⏳ Struct literals
- ⏳ Closures `|x| x + 1`
- ⏳ String / char / float literal atoms (only Int)
- ⏳ Block expressions `{ stmts; expr }`
- ⏳ if / match / while / for / let
- ⏳ Items (fn / struct / enum / trait / impl)
- ⏳ Error reporting with spans

Each is mechanical to add. The session-133 parser
is ~180 LOC of Rune; the Rust-side parser is ~2000
LOC — about 9% of surface area, but the Pratt core
is the load-bearing piece.

## The wire-ups

```
examples/bootstrap/parser.rn  (+~180 lines: BinOp + Expr enums,
                               Parser struct + new_parser
                               constructor, peek/advance
                               cursor, binop_bp / binop_of
                               tables, parse_bp + parse_binops
                               + parse_unary + parse_atom, and
                               an inline eval for testing.)

examples/bootstrap/main.rn   (Demo updated to wire lexer →
                              parser → eval and return 5
                              for `(1 + 2 * 3) - (10 / 5)`.)

tests/codegen.rs   (+BOOTSTRAP_PARSER_RN const + run_bootstrap_
                    eval helper + 10 multi-file tests covering
                    atom / paren / unary / addition / precedence
                    / associativity / mixed-complex / mod /
                    negation-of-paren.)
```

No Rust-side compiler changes.

## What's tested

Codegen (+10 from session 132's 531):

- `rune_parser_atom_int` — `"42"` evaluates to 42.
- `rune_parser_atom_paren` — `"(7)"` evaluates to 7.
- `rune_parser_unary_neg` — `"-5"` evaluates to -5.
- `rune_parser_addition` — `"1 + 2"` evaluates to 3.
- `rune_parser_precedence_mul_over_add` — `"1 + 2
  * 3"` evaluates to 7 (Mul binds tighter).
- `rune_parser_left_associative_add` — `"10 - 3 -
  2"` evaluates to 5 (left-associative; not 9).
- `rune_parser_parens_override_precedence` — `"(1
  + 2) * 3"` evaluates to 9.
- `rune_parser_mixed_complex` — `"(1 + 2 * 3) -
  (10 / 5)"` evaluates to 5.
- `rune_parser_mod_op` — `"17 % 5"` evaluates to 2.
- `rune_parser_negation_of_paren` — `"-(2 + 3)"`
  evaluates to -5.

## Apparent bugs that aren't / explicitly deferred

- **Unexpected token in atom returns sentinel.**
  `parse_atom` falls through to `Expr::Num(-1)` on
  unexpected tokens. Real error recovery / diagnostic
  reporting is future work — for now, malformed
  input produces a misleading AST but doesn't
  crash.
- **No `Eof` handling beyond the loop check.** A
  parse that runs past the tokens would panic in
  `p.tokens.get(p.pos)`. Adding a proper "expected
  EOF but got X" diagnostic is straightforward
  with the existing peek + advance.
- **Spans not propagated to AST.** Each Expr
  variant could carry a Span field; the parser
  could record the span covering each subexpression.
  Sessions 134+ will add this once error reporting
  begins.
- **Ident evaluates to 0** in the inline eval.
  Identifiers aren't bound yet (no environment).
  A future session adds scope handling.
- **No comparison / logical / bitwise / shift in
  the binop table.** Each adds one row to
  `binop_bp` and `binop_of` plus a corresponding
  `BinOp` variant. Mechanical.
- **`parse_atom` doesn't handle `Token::Float(...)`
  / `Token::Str(...)` / `Token::Char(...)`.** Same
  shape as Int — add variants to Expr (Lit::Float
  / Lit::Str / Lit::Char) and one match arm each.
  Session 134.
- **No `let mut p` needed** — the Parser struct
  mutates via interior pointer (`p.pos = p.pos +
  1` inside `advance` works because struct field
  assign isn't a binding reassignment). Same as
  Vec.push.

## What's next

- **Session 134: Comparison + logical ops + more
  literal kinds.** Wire `==`, `!=`, `<`, `<=`, `>`,
  `>=`, `&&`, `||` into the Pratt table; add
  Float / Str / Char / Bool atoms.
- **Session 135: Function calls + field access.**
  `f(args)` and `obj.field` are postfix operations
  in Pratt parlance — add a postfix-handling loop
  to parse_atom's continuation.
- **Session 136+: Block expressions + control flow.**
  `if`, `match`, `while`, `for`, `let` statements.
  This is where the parser starts looking like a
  real one.

## Phase 2 progress

```
Lexer    ✅ feature-complete (sessions 128-132)
Parser   ⏳ in progress
   - 133: expression skeleton (atom, unary, binops)
   - 134-???: comparison + logical, calls, control flow, items
Resolver ⏳ later
Checker  ⏳ later
Eval     ⏳ later
```
