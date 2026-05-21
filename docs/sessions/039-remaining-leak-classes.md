# Session 039 — Remaining leak classes

**Date:** 2026-05-21
**Outcome:** The three ARC-temporary leak classes session 038 left
open are closed: an enum payload that escapes a `match` arm, a fresh
string handed to `print`, and a fresh value dropped by a discarded
expression statement. Three small codegen changes. 459 tests green
(+3 from session 038's 456).

## 1 — Match-arm escape retain

`match opt { Some(x) => x }` binds `x` to a payload loaded out of
the enum descriptor. That binding is a **borrow** — codegen loads it
without retaining and without scope-tracking it, exactly like a
function parameter. Fine for use *inside* the arm; broken when the
binding **escapes** as the arm's value:

```rune
fn unwrap_or<T>(o: Option<T>, d: T) -> T {
    match o { Some(x) => x, None => d }   // arm yields a borrow
}
```

The `match` expression then yields a borrowed pointer, which `let r
= match ..`, `return`, and call arguments all treat as a fresh `+1`.
For an ARC `T` that double-frees — and `unwrap_or` on a `Vec` is an
ordinary thing to write.

The fix: `compile_match` retains a borrowed-`Local` arm body before
the merge jump — the arm-body analog of `compile_block`'s existing
tail-escape rule for block tails. The payload binding stays a
borrow; only the escape point takes a `+1`. Not enum-specific — it
equally fixes `match n { 0 => some_vec, _ => other_vec }`.

## 2 — `print` arguments

Session 036 reclaimed fresh ARC arguments after a regular `Call` but
excluded `BuiltinCall`: `weak` and `upgrade_or` *alias* their
argument's control block, so releasing it would dangle. `print`,
though, only **reads** its argument — `print("a" + "b")` leaked the
concatenated string.

The fix: `compile_builtin_call` releases a fresh ARC argument after
a `print_str` call. An allowlist of one — the borrowing builtin.
`print_i64` has no ARC argument; `weak` / `upgrade_or` stay
excluded.

## 3 — Discarded expression statements

```rune
make_vec();   // value computed, thrown away
```

An expression statement with a trailing `;` discards its value. A
fresh ARC value there owns a `+1` nobody reclaims. The fix:
`compile_stmt`, for a semicolon-terminated `Expr` statement,
releases the value when it is a fresh ARC temporary (not a borrowed
`Local` — `v;` where `v` is a binding leaves `v` to its owner).

## Why one session

The three interlock. A discarded `match opt { Some(x) => x };` needs
**both** the match escape retain (so the match yields a real `+1`)
**and** the discarded-statement release (to reclaim it). #3 without
#1 would release a borrow — a double-free. And #3 is sound only
because session 037 (field/index reads retain) together with #1 now
guarantee that *every* non-`Local` ARC expression is a genuine `+1`.

## What's tested

Codegen (+3):

- `enum_payload_escape_retained` — the `unwrap_or` shape, an ARC
  payload extracted through a helper, 200×; without the escape
  retain the Vec is freed three ways at scope exit.
- `discarded_statement_temp_released` — `make();` discarded 200×,
  then a discarded `Local` (`keep;`) that must stay valid.
- `print_arg_temp_released` — a fresh concatenation handed to
  `print` is reclaimed; a `Local` argument is passed twice and
  remains valid.

## Apparent bugs that aren't

- **A `match` scrutinee that is a fresh temporary leaks.** `match
  make_opt() { .. }` never releases the `make_opt()` enum — the
  receiver-temporary analog for `match`, not addressed here. Arm
  bindings borrow into the scrutinee, so its release belongs at the
  merge point; a follow-up.

- **`weak` / `upgrade_or` arguments are deliberately not cleaned.**
  They alias their argument's control block; releasing a fresh
  argument would free a block the returned `Weak` / strong ref still
  points at.

- **Array elements still leak.** `compile_array` never establishes
  ARC ownership — unchanged since it was first noted.

## What's next

- **Match-scrutinee temporaries** — release a fresh `match` /
  destructure scrutinee at the merge point.
- **`dyn` coercion at struct-literal fields and enum payloads.**
- **Supertraits, associated types, generic impls.**
