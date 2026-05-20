# Session 009 — Polymorphic `print`

**Date:** 2026-05-19
**Outcome:** A single `print(x)` builtin now accepts both `i64`-family
integers and `str`. The dispatch is done at lowering time based on the
argument type. 176 tests green (+9 new). `examples/greet.rn` uses
`print` for both strings and the loop counter:

```
$ rune run examples/greet.rn
Hello, world!
Hello, Rune!
Hello, Cranelift!
Total greeted:
3
```

## Mechanism

Three moving pieces:

1. **`SymbolKind::PolyBuiltinFn(&'static str)`** in the resolver. The
   inner string is the polymorphic builtin's name (`"print"` today).
2. **Type checker** intercepts calls before binding the callee as a
   value:
   ```rust
   if let Expr::Path(p) = callee {
       if let Some(&sid) = self.res.path_to_sym.get(&p.span) {
           if let SymbolKind::PolyBuiltinFn(name) = self.res.symbol(sid).kind.clone() {
               return self.check_poly_builtin_call(name, args, span);
           }
       }
   }
   // ... normal Ty::Fn-based call check ...
   ```
   `check_poly_builtin_call` for `print` requires exactly one argument
   of a "printable" type (currently `Ty::Int(_)` or `Ty::Str`).
3. **Lowerer** dispatches via `Lowerer::lower_poly_call`:
   ```rust
   let dispatched = match (poly_name, arg_ty) {
       ("print", Some(Ty::Int(_))) => "print_i64",
       ("print", Some(Ty::Str))    => "print_str",
       _ => Unsupported(...),
   };
   HirExprKind::BuiltinCall { name: dispatched.into(), args }
   ```

The codegen never sees a "polymorphic call" — by the time it runs, the
HIR has a concrete `BuiltinCall` with a name that maps to a single
runtime function (`rune_print_i64` or `rune_print_str`).

## What changed in the user-facing surface

| Before | After |
| --- | --- |
| `print(42)` works | `print(42)` works |
| `print("hi")` was an error | `print("hi")` works |
| `print_str("hi")` works | `print_str("hi")` works |
| `print_i64(42)` — didn't exist | `print_i64(42)` works (alias to `print` for int) |
| `print(true)` would silently coerce-or-error | `print(true)` errors at type-check: "`print` does not yet support values of type `bool`" |
| `let p = print;` accidentally returned i64 | `let p = print;` errors: "polymorphic builtin cannot be used as a value" |

The error message for unsupported types deliberately lists what `print`
*does* support so users have a one-line hint. As more
runtime variants get added (`print_f64`, `print_bool`, ...), the
`is_printable` predicate grows alongside.

## Why this and not "real" overloading

Rust has no value-level function overloading; Rust+traits gets the
effect via dispatch. Rune has neither generics nor traits today. Real
overloading at the language level would:

- Need a resolver pass that handles multiple symbols with the same name.
- Need argument-type-directed selection at every call site.
- Need a story for how user code declares overloads.

None of that is justified by *one* polymorphic builtin. The
`PolyBuiltinFn` variant scales to a handful of host builtins; it
explicitly does **not** scale to user-declared overloads. When generics
or traits land, `print` becomes a regular `fn print<T: Display>(x: T)`
and `PolyBuiltinFn` retires.

## Internal renaming

The codegen-internal name for the i64 print runtime moved from `"print"`
to `"print_i64"`:

```rust
fn declare_builtin<M: Module>(module: &mut M, name: &str) -> Result<FuncId, _> {
    let (runtime_name, sig) = match name {
        "print_i64" => ("rune_print_i64", /* (i64) -> () */),
        "print_str" => ("rune_print_str", /* (*RuneStr) -> () */),
        ...
    };
    ...
}
```

User-facing names are unchanged for explicit calls:
- `print(42)` → lowerer emits `BuiltinCall("print_i64", ...)`
- `print_i64(42)` → resolver finds the `BuiltinFn`, lowerer emits the same.

The `BuiltinFn` symbol for `print_i64` is also new — added so users can
call it directly if they want to bypass the dispatch.

## File layout changes

```
src/
├── resolver.rs   (SymbolKind::PolyBuiltinFn variant; print → poly;
                   print_i64 BuiltinFn added alongside print_str)
├── checker.rs    (path_value_type and check_assign_target handle the
                   new variant; check_call intercepts before the
                   Ty::Fn-based code path; check_poly_builtin_call;
                   is_printable helper)
├── lower.rs      (is_poly_builtin_fn_symbol; lower_poly_call; Call
                   dispatch arm)
└── codegen.rs    (declare_builtin key renamed "print" → "print_i64")
tests/
├── typecheck.rs  (+6: print accepts int/str, rejects bool, rejects
                   wrong arity, rejects use-as-value)
└── aot.rs        (+3: mixed print int/str, print in for-loop over
                   strings, print of a concat result)
examples/
└── greet.rn      (now uses `print` over both strings and the counter)
```

## Apparent bugs that aren't

- **`print` is reserved at the language level.** Users can't define
  their own `fn print(...)` and have it shadow the builtin in the same
  scope (the resolver intern is at module-init time, before any user
  declarations). They *can* shadow it inside an inner scope, but the
  outer builtin remains accessible from sibling scopes. This matches
  how Rust treats `print!` etc. — different mechanism (macro), same
  ergonomics.
- **`print(true)` is a checker error, not a runtime no-op.** Some
  languages silently coerce booleans to "true"/"false". We don't —
  bool needs an explicit `print_bool` (not implemented yet) or `print(if
  b { 1 } else { 0 })`.

## Test coverage

6 new typechecker tests:
- `print` accepts int and str.
- Rejects bool, zero args, two args, use-as-value.

3 new AOT tests:
- Mixed `print(i64)` and `print(str)` in same `main`.
- `print(str_variable)` in a for-loop.
- `print` of a concatenation result.

## Next session

The remaining candidates from session 008's matrix:

1. **String methods**: `.len()`, indexing (`s[i]`), slicing. Requires
   method-call codegen support more broadly; touches checker, lowerer,
   codegen.

2. **Heap-allocated vectors** (`Vec<T>`). Same heap leak story as
   strings; adds a parameterized type (whether expressed as `Vec[i64]`
   or `Vec<i64>` syntactically). Pushes the language toward generics.

3. **Bounds checks on array indexing.** Smaller scope. Forces the
   panic-vs-abort decision.

4. **Struct field access.** Foundation for the stdlib's data types.
   Requires codegen knowing struct layout (offsets).

Decision to pin before any of these lands: how Rune surfaces panics
(if it gets bounds checks) — abort? print + exit? Result wrapping at
boundaries?
