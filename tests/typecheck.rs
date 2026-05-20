use rune::*;

fn run(src: &str) -> (Vec<LexError>, Vec<ParseError>, Vec<ResolveError>, Vec<TypeError>) {
    let (tokens, le) = Lexer::new(src).tokenize();
    let (module, pe) = Parser::new(tokens).parse_module();
    let (res, re) = Resolver::new().resolve_module(&module);
    let cr = Checker::new(&res).check_module(&module);
    (le, pe, re, cr.errors)
}

fn check_ok(src: &str) {
    let (le, pe, re, te) = run(src);
    assert!(le.is_empty(), "lex errors: {:?}", le);
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    assert!(re.is_empty(), "resolve errors: {:?}", re);
    assert!(te.is_empty(), "type errors: {:?}", te);
}

fn check_has_error(src: &str, fragment: &str) {
    let (_le, _pe, re, te) = run(src);
    let all_msgs: Vec<String> = re
        .iter()
        .map(|e| e.message.clone())
        .chain(te.iter().map(|e| e.message.clone()))
        .collect();
    assert!(
        all_msgs.iter().any(|m| m.contains(fragment)),
        "expected an error containing {:?}, got {:?}",
        fragment,
        all_msgs
    );
}

// ---- resolver behavior ----

#[test]
fn unresolved_name_is_error() {
    check_has_error("fn main() { x }", "unresolved name `x`");
}

#[test]
fn forward_reference_works() {
    check_ok("fn a() -> i64 { b() } fn b() -> i64 { 0 }");
}

#[test]
fn shadowing_in_same_scope_allowed() {
    check_ok(r#"
        fn main() {
            let x = 5;
            let x = "hi";
            let _ = x;
        }
    "#);
}

#[test]
fn nested_shadowing() {
    check_ok(r#"
        fn main() {
            let x = 1;
            {
                let x = 2;
                let _ = x;
            }
            let _ = x;
        }
    "#);
}

// ---- numeric defaults ----

#[test]
fn unannotated_int_defaults_to_i64() {
    check_ok("fn main() -> i64 { 42 }");
}

#[test]
fn unannotated_int_does_not_default_to_i32() {
    check_has_error("fn main() -> i32 { 42 }", "i32");
}

#[test]
fn unannotated_float_defaults_to_f64() {
    check_ok("fn main() -> f64 { 3.14 }");
}

// ---- let bindings ----

#[test]
fn let_with_matching_annotation() {
    check_ok("fn main() { let x: i64 = 5; let _ = x; }");
}

#[test]
fn let_with_mismatched_annotation() {
    check_has_error(
        "fn main() { let x: i64 = true; }",
        "i64",
    );
}

#[test]
fn let_inferred_from_init() {
    check_ok("fn main() { let x = true; let _ = x; }");
}

#[test]
fn let_no_type_no_init_is_error() {
    check_has_error(
        "fn main() { let x; }",
        "neither type nor initializer",
    );
}

// ---- mutability ----

#[test]
fn assign_to_immutable_is_error() {
    check_has_error(
        "fn main() { let x = 0; x = 1; }",
        "immutable",
    );
}

#[test]
fn assign_to_mut_is_ok() {
    check_ok("fn main() { let mut x = 0; x = 1; }");
}

#[test]
fn assign_to_param_is_error() {
    check_has_error(
        "fn f(x: i64) { x = 0; }",
        "parameter",
    );
}

#[test]
fn compound_assign_respects_mutability() {
    check_has_error(
        "fn main() { let x = 0; x += 1; }",
        "immutable",
    );
    check_ok("fn main() { let mut x = 0; x += 1; }");
}

// ---- binary operators ----

#[test]
fn arithmetic_requires_numeric() {
    check_has_error(
        "fn main() { let _ = true + false; }",
        "numeric",
    );
}

#[test]
fn arithmetic_homogeneous() {
    check_ok("fn main() { let _ = 1 + 2; }");
}

#[test]
fn mixing_int_and_float_is_error() {
    check_has_error(
        "fn main() { let _ = 1 + 1.0; }",
        "mismatched",
    );
}

#[test]
fn comparison_returns_bool() {
    check_ok("fn f() -> bool { 1 < 2 }");
}

#[test]
fn logical_requires_bool() {
    check_has_error(
        "fn main() { let _ = 1 && 2; }",
        "bool",
    );
    check_ok("fn main() { let _ = true && false; }");
}

#[test]
fn bitwise_requires_integer() {
    check_has_error(
        "fn main() { let _ = 1.0 | 2.0; }",
        "integer",
    );
    check_ok("fn main() { let _ = 0xff | 0b1010; }");
}

// ---- unary ----

#[test]
fn neg_requires_numeric() {
    check_ok("fn main() { let _ = -5; }");
    check_has_error("fn main() { let _ = -true; }", "negate");
}

#[test]
fn not_requires_bool() {
    check_ok("fn main() { let _ = !true; }");
    check_has_error("fn main() { let _ = !1; }", "bool");
}

// ---- control flow ----

#[test]
fn if_condition_must_be_bool() {
    check_has_error("fn main() { if 1 { } }", "if condition");
}

#[test]
fn if_branches_must_unify() {
    check_has_error(
        "fn main() { let _ = if true { 1 } else { false }; }",
        "different types",
    );
}

#[test]
fn if_without_else_must_be_unit() {
    check_has_error(
        "fn f() -> i64 { if true { 5 } }",
        "()",
    );
    check_ok("fn main() { if true { } }");
}

// ---- polymorphic `print` ----

#[test]
fn print_accepts_int() {
    check_ok("fn main() { print(42); }");
}

#[test]
fn print_accepts_str() {
    check_ok(r#"fn main() { print("hello"); }"#);
}

#[test]
fn print_rejects_bool() {
    check_has_error(
        "fn main() { print(true); }",
        "does not yet support",
    );
}

#[test]
fn print_rejects_zero_args() {
    check_has_error(
        "fn main() { print(); }",
        "expects 1 argument",
    );
}

#[test]
fn print_rejects_two_args() {
    check_has_error(
        r#"fn main() { print(1, 2); }"#,
        "expects 1 argument",
    );
}

#[test]
fn print_as_value_is_error() {
    check_has_error(
        "fn main() { let p = print; }",
        "polymorphic builtin",
    );
}

#[test]
fn while_condition_must_be_bool() {
    check_has_error(
        "fn main() { while 1 { } }",
        "while condition",
    );
    check_ok("fn main() { while true { } }");
}

#[test]
fn for_over_array_binds_element_type() {
    check_ok(r#"
        fn main() {
            let xs = [1, 2, 3];
            for x in xs {
                let _: i64 = x;
            }
        }
    "#);
}

#[test]
fn for_over_non_iterable_is_error() {
    check_has_error(
        "fn main() { for x in 5 { } }",
        "iterate",
    );
}

// ---- function calls ----

#[test]
fn call_matching_args() {
    check_ok(r#"
        fn add(a: i64, b: i64) -> i64 { a + b }
        fn main() { let _ = add(1, 2); }
    "#);
}

#[test]
fn call_wrong_arity() {
    check_has_error(
        r#"
        fn add(a: i64, b: i64) -> i64 { a + b }
        fn main() { let _ = add(1); }
        "#,
        "argument",
    );
}

#[test]
fn call_wrong_arg_type() {
    check_has_error(
        r#"
        fn add(a: i64, b: i64) -> i64 { a + b }
        fn main() { let _ = add(1, true); }
        "#,
        "argument 2",
    );
}

#[test]
fn calling_non_function_is_error() {
    check_has_error(
        "fn main() { let x = 5; let _ = x(1); }",
        "cannot call",
    );
}

// ---- return type ----

#[test]
fn return_type_must_match() {
    check_has_error(
        "fn f() -> i64 { true }",
        "i64",
    );
}

#[test]
fn explicit_return_must_match() {
    check_has_error(
        "fn f() -> i64 { return true; 0 }",
        "i64",
    );
}

// ---- arrays ----

#[test]
fn homogeneous_array_ok() {
    check_ok("fn main() { let _ = [1, 2, 3, 4]; }");
}

#[test]
fn heterogeneous_array_is_error() {
    check_has_error(
        "fn main() { let _ = [1, true]; }",
        "earlier elements",
    );
}

#[test]
fn array_indexing_returns_element() {
    check_ok(r#"
        fn main() {
            let xs = [1, 2, 3];
            let _: i64 = xs[0];
        }
    "#);
}

// ---- end-to-end ----

#[test]
fn factorial_typechecks() {
    check_ok(r#"
        fn factorial(n: i64) -> i64 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
    "#);
}

#[test]
fn hello_rn_typechecks() {
    check_ok(r#"
        fn factorial(n: i64) -> i64 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }

        fn main() {
            let greeting = "Hello, Rune!";
            let answer = 42;
            let pi = 3.14;
            let mask = 0xff_00 | 0b1010;

            let primes = [2, 3, 5, 7, 11, 13, 17, 19];
            for p in primes {
                if p > 10 {
                }
            }
            let _ = greeting;
            let _ = answer;
            let _ = pi;
            let _ = mask;
        }
    "#);
}
