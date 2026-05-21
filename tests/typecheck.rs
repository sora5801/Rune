use rune::*;

fn run(src: &str) -> (Vec<LexError>, Vec<ParseError>, Vec<ResolveError>, Vec<TypeError>) {
    let src = with_prelude(src);
    let exp = expand_modules(&src, "<test>", &|_| None);
    let (module, pe) = Parser::new(exp.tokens).parse_module();
    let (res, re) = Resolver::new().resolve_module(&module);
    let cr = Checker::new(&res).check_module(&module);
    (exp.lex_errors, pe, re, cr.errors)
}

/// Run the front-end on a multi-file program. `files[0]` is the main
/// source (gets the prelude); the rest are `(module-name, source)`
/// pairs reachable through `mod name;` declarations.
fn run_files(
    files: &[(&str, &str)],
) -> (Vec<ModuleError>, Vec<ParseError>, Vec<ResolveError>, Vec<TypeError>) {
    let main_src = with_prelude(files[0].1);
    let mods: Vec<(String, String)> = files[1..]
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    let loader = |name: &str| {
        mods.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone())
    };
    let exp = expand_modules(&main_src, "<test>", &loader);
    let (module, pe) = Parser::new(exp.tokens).parse_module();
    let (res, re) = Resolver::new().resolve_module(&module);
    let cr = Checker::new(&res).check_module(&module);
    (exp.module_errors, pe, re, cr.errors)
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

// ---- method calls ----

#[test]
fn str_len_typechecks() {
    check_ok(r#"fn main() -> i64 { "hi".len() }"#);
}

#[test]
fn str_is_empty_typechecks() {
    check_ok(r#"fn main() -> bool { "".is_empty() }"#);
}

#[test]
fn array_len_typechecks() {
    check_ok("fn main() -> i64 { [1, 2, 3].len() }");
}

// ---- string indexing and slicing ----

#[test]
fn str_index_byte_is_integer() {
    check_ok(r#"fn main() -> i64 { "hello"[0] }"#);
}

#[test]
fn str_slice_is_str() {
    check_ok(r#"fn main() -> i64 { "hello"[0..3].len() }"#);
}

#[test]
fn inclusive_str_slice() {
    check_ok(r#"fn main() -> i64 { "hello"[0..=2].len() }"#);
}

#[test]
fn str_index_must_be_integer() {
    check_has_error(
        r#"fn main() -> i64 { "hi"[true] }"#,
        "must be an integer",
    );
}

#[test]
fn standalone_range_is_error() {
    check_has_error(
        "fn main() -> i64 { let r = 0..10; 0 }",
        "range expressions",
    );
}

#[test]
fn method_does_not_exist_on_type() {
    check_has_error(
        r#"fn main() -> i64 { (5).len() }"#,
        "no method",
    );
}

#[test]
fn method_with_wrong_arg_count() {
    // `.len()` takes no args.
    check_has_error(
        r#"fn main() -> i64 { "hi".len(1) }"#,
        "expects 0 argument",
    );
}

// ---- field assignment ----

#[test]
fn field_assignment_typechecks() {
    check_ok(r#"
        struct P { x: i64 }
        fn main() {
            let mut p = P { x: 1 };
            p.x = 5;
        }
    "#);
}

#[test]
fn field_assignment_on_immutable_is_error() {
    check_has_error(
        r#"
        struct P { x: i64 }
        fn main() {
            let p = P { x: 1 };
            p.x = 5;
        }
        "#,
        "immutable",
    );
}

#[test]
fn field_assignment_on_param_is_error() {
    check_has_error(
        r#"
        struct P { x: i64 }
        fn touch(p: P) { p.x = 5; }
        "#,
        "parameter",
    );
}

#[test]
fn field_assignment_wrong_type_is_error() {
    check_has_error(
        r#"
        struct P { x: i64 }
        fn main() {
            let mut p = P { x: 1 };
            p.x = "oops";
        }
        "#,
        "i64",
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

// ---- ARC: free(x) removed; ARC handles reclamation now ----

#[test]
fn free_is_no_longer_a_builtin() {
    // ARC supersedes the manual free(x). Calling it should now fail
    // at name resolution.
    check_has_error(
        "fn main() { let v = vec_new(); free(v); }",
        "unresolved name `free`",
    );
}

// ---- match exhaustiveness ----

#[test]
fn match_enum_exhaustive_ok() {
    check_ok(r#"
        enum Mode { On, Off, Idle }
        fn label(m: Mode) -> i64 {
            match m {
                Mode::On => 1,
                Mode::Off => 0,
                Mode::Idle => -1,
            }
        }
    "#);
}

#[test]
fn match_enum_non_exhaustive_errors() {
    check_has_error(
        r#"
        enum Mode { On, Off, Idle }
        fn label(m: Mode) -> i64 {
            match m {
                Mode::On => 1,
                Mode::Off => 0,
            }
        }
        "#,
        "non-exhaustive",
    );
}

#[test]
fn match_enum_with_wildcard_ok() {
    check_ok(r#"
        enum Mode { On, Off, Idle }
        fn label(m: Mode) -> i64 {
            match m {
                Mode::On => 1,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn match_bool_exhaustive_ok() {
    check_ok(r#"
        fn label(b: bool) -> i64 {
            match b {
                true => 1,
                false => 0,
            }
        }
    "#);
}

#[test]
fn match_bool_missing_branch_errors() {
    check_has_error(
        r#"
        fn label(b: bool) -> i64 {
            match b {
                true => 1,
            }
        }
        "#,
        "non-exhaustive",
    );
}

#[test]
fn match_int_without_catchall_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                1 => 1,
                2 => 2,
            }
        }
        "#,
        "non-exhaustive",
    );
}

#[test]
fn match_int_with_catchall_ok() {
    check_ok(r#"
        fn main() -> i64 {
            match 5 {
                1 => 1,
                2 => 2,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn match_int_with_binding_catchall_ok() {
    check_ok(r#"
        fn main() -> i64 {
            match 5 {
                0 => 0,
                n => n * 2,
            }
        }
    "#);
}

#[test]
fn match_str_without_catchall_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match "x" {
                "a" => 1,
                "b" => 2,
            }
        }
        "#,
        "non-exhaustive",
    );
}

#[test]
fn match_duplicate_enum_variant_is_unreachable() {
    check_has_error(
        r#"
        enum E { A, B }
        fn label(e: E) -> i64 {
            match e {
                E::A => 1,
                E::A => 2,
                E::B => 3,
            }
        }
        "#,
        "unreachable",
    );
}

#[test]
fn match_duplicate_int_is_unreachable() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 1 {
                5 => 1,
                5 => 2,
                _ => 0,
            }
        }
        "#,
        "unreachable",
    );
}

#[test]
fn match_arm_after_catchall_is_unreachable() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 1 {
                _ => 0,
                5 => 1,
            }
        }
        "#,
        "unreachable",
    );
}

#[test]
fn match_arm_after_binding_catchall_is_unreachable() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 1 {
                n => n,
                5 => 1,
            }
        }
        "#,
        "unreachable",
    );
}

// ---- match guards ----

#[test]
fn match_guard_typechecks() {
    check_ok(r#"
        fn main() -> i64 {
            match 5 {
                x if x > 0 => 1,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn match_guard_must_be_bool() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                x if x => 1,
                _ => 0,
            }
        }
        "#,
        "guard must be `bool`",
    );
}

#[test]
fn guarded_arm_does_not_make_exhaustive() {
    // A guarded wildcard can fail; bool match still needs full coverage.
    check_has_error(
        r#"
        fn main() -> i64 {
            match true {
                true => 1,
                _ if false => 2,
            }
        }
        "#,
        "non-exhaustive",
    );
}

// ---- or-patterns ----

#[test]
fn or_pattern_int_exhaustive_with_wildcard() {
    check_ok(r#"
        fn main() -> i64 {
            match 5 {
                1 | 2 | 3 => 1,
                _ => 0,
            }
        }
    "#);
}

#[test]
fn or_pattern_enum_exhaustive_without_wildcard() {
    check_ok(r#"
        enum E { A, B, C }
        fn label(e: E) -> i64 {
            match e {
                E::A | E::B => 1,
                E::C => 2,
            }
        }
    "#);
}

#[test]
fn or_pattern_missing_variant_errors() {
    check_has_error(
        r#"
        enum E { A, B, C }
        fn label(e: E) -> i64 {
            match e {
                E::A | E::B => 1,
            }
        }
        "#,
        "non-exhaustive",
    );
}

#[test]
fn or_pattern_duplicate_within_arm_is_unreachable() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                1 | 2 | 1 => 1,
                _ => 0,
            }
        }
        "#,
        "unreachable",
    );
}

#[test]
fn or_pattern_with_binding_rejected() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                1 | x => 1,
                _ => 0,
            }
        }
        "#,
        "or-pattern can't contain a binding",
    );
}

#[test]
fn range_pattern_int_typechecks() {
    check_ok(
        r#"
        fn main() -> i64 {
            match 5 {
                0..=9 => 1,
                _ => 0,
            }
        }
        "#,
    );
}

#[test]
fn range_pattern_char_typechecks() {
    check_ok(
        r#"
        fn main() -> i64 {
            match 'a' {
                'a'..='z' => 1,
                _ => 0,
            }
        }
        "#,
    );
}

#[test]
fn range_pattern_mismatched_to_bool_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match true {
                0..=9 => 1,
                _ => 0,
            }
        }
        "#,
        "range pattern with integer bounds doesn't match scrutinee type `bool`",
    );
}

#[test]
fn range_pattern_mismatched_char_on_int_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                'a'..='z' => 1,
                _ => 0,
            }
        }
        "#,
        "range pattern with char bounds doesn't match scrutinee type",
    );
}

#[test]
fn range_pattern_mixed_bounds_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                0..='z' => 1,
                _ => 0,
            }
        }
        "#,
        "range pattern bounds must be two integers or two chars",
    );
}

#[test]
fn range_pattern_inclusive_empty_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                10..=0 => 1,
                _ => 0,
            }
        }
        "#,
        "range pattern `10..=0` is empty",
    );
}

#[test]
fn range_pattern_exclusive_empty_errors() {
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                5..5 => 1,
                _ => 0,
            }
        }
        "#,
        "range pattern `5..5` is empty",
    );
}

#[test]
fn range_pattern_without_catchall_errors() {
    // A range covers part of i64 but not all — still needs a `_` arm.
    check_has_error(
        r#"
        fn main() -> i64 {
            match 5 {
                0..=100 => 1,
            }
        }
        "#,
        "non-exhaustive `match` on `i64`",
    );
}

#[test]
fn range_pattern_in_or_typechecks() {
    check_ok(
        r#"
        fn main() -> i64 {
            match 5 {
                1..=3 | 7..=9 => 1,
                _ => 0,
            }
        }
        "#,
    );
}

// ---- standard library (prelude) ----

#[test]
fn stdlib_min_rejects_non_int() {
    // std::min has a concrete i64 signature; a str arg is rejected.
    check_has_error(r#"fn main() -> i64 { std::min("a", "b") }"#, "i64");
}

#[test]
fn stdlib_item_must_be_qualified() {
    // Prelude items live under `std::`; a bare reference is unresolved.
    check_has_error("fn main() -> i64 { min(1, 2) }", "unresolved name `min`");
}

#[test]
fn stdlib_option_unwrap_or_typechecks() {
    check_ok(r#"
        fn main() -> i64 {
            std::unwrap_or(std::Option::Some(1), 0)
        }
    "#);
}

// ---- generic Vec<T> ----

#[test]
fn generic_vec_requires_type_arg() {
    // `Vec` is parametric — a bare `Vec` type is rejected.
    check_has_error(
        "fn f(v: Vec) -> i64 { v.len() }",
        "one type argument",
    );
}

#[test]
fn generic_vec_str_element_rejected() {
    // `str` is a 16-byte descriptor — it can't be a Vec element.
    check_has_error(
        "fn main() -> i64 { let v: Vec<str> = vec_new(); 0 }",
        "not supported",
    );
}

#[test]
fn generic_vec_push_type_mismatch() {
    // push's parameter is element-typed off the receiver.
    check_has_error(
        "fn main() -> i64 { let v: Vec<i64> = vec_new(); v.push(true); 0 }",
        "i64",
    );
}

#[test]
fn generic_vec_typechecks() {
    check_ok(r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1);
            v.get(0)
        }
    "#);
}

// ---- file-based modules ----

#[test]
fn file_module_resolves_ok() {
    let (me, pe, re, te) = run_files(&[
        ("main", "mod helper; fn main() -> i64 { helper::n() }"),
        ("helper", "pub fn n() -> i64 { 1 }"),
    ]);
    assert!(me.is_empty(), "module errors: {:?}", me);
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    assert!(re.is_empty(), "resolve errors: {:?}", re);
    assert!(te.is_empty(), "type errors: {:?}", te);
}

#[test]
fn file_module_missing_file_is_error() {
    let (me, _pe, _re, _te) =
        run_files(&[("main", "mod ghost; fn main() -> i64 { 0 }")]);
    assert!(
        me.iter()
            .any(|e| e.message.contains("cannot find module file `ghost.rn`")),
        "expected a missing-module error, got {:?}",
        me
    );
}

#[test]
fn file_module_nested_directory() {
    // `mod b;` inside `a.rn` resolves into the `a/` subdirectory.
    let (me, pe, re, te) = run_files(&[
        ("main", "mod a; fn main() -> i64 { a::b::deep() }"),
        ("a", "pub mod b;"),
        ("a/b", "pub fn deep() -> i64 { 7 }"),
    ]);
    assert!(me.is_empty(), "module errors: {:?}", me);
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    assert!(re.is_empty(), "resolve errors: {:?}", re);
    assert!(te.is_empty(), "type errors: {:?}", te);
}

// ---- module visibility + use globs ----

#[test]
fn private_module_item_rejected() {
    check_has_error(
        r#"
        mod m { fn secret() -> i64 { 9 } }
        fn main() -> i64 { m::secret() }
        "#,
        "private",
    );
}

#[test]
fn pub_module_item_visible() {
    check_ok(r#"
        mod m { pub fn ok() -> i64 { 1 } }
        fn main() -> i64 { m::ok() }
    "#);
}

#[test]
fn private_item_visible_within_module() {
    // A sibling inside the same module sees a non-pub item.
    check_ok(r#"
        mod m {
            fn helper() -> i64 { 1 }
            pub fn run() -> i64 { helper() }
        }
        fn main() -> i64 { m::run() }
    "#);
}

#[test]
fn use_glob_brings_items_into_scope() {
    check_ok(r#"
        mod m { pub fn a() -> i64 { 1 } }
        use m::*;
        fn main() -> i64 { a() }
    "#);
}

#[test]
fn use_glob_of_missing_module_errors() {
    check_has_error("use nope::*; fn main() -> i64 { 0 }", "no such module");
}

#[test]
fn use_glob_omits_private_items() {
    // The glob imports only items visible from here, so a non-pub
    // item of `m` stays out of scope.
    check_has_error(
        r#"
        mod m {
            pub fn shown() -> i64 { 1 }
            fn hidden() -> i64 { 2 }
        }
        use m::*;
        fn main() -> i64 { hidden() }
        "#,
        "unresolved name `hidden`",
    );
}

// ---- use renaming, pub use, per-segment privacy ----

#[test]
fn use_as_binds_new_name() {
    check_ok(r#"
        mod m { pub fn f() -> i64 { 1 } }
        use m::f as g;
        fn main() -> i64 { g() }
    "#);
}

#[test]
fn use_as_of_missing_item_errors() {
    check_has_error(
        r#"
        mod m {}
        use m::nope as x;
        fn main() -> i64 { 0 }
        "#,
        "unresolved import",
    );
}

#[test]
fn per_segment_private_module_rejected() {
    // `b` is a private module — a path through it is rejected even
    // though the final item `deep` is `pub`.
    check_has_error(
        r#"
        mod a {
            mod b {
                pub fn deep() -> i64 { 7 }
            }
        }
        fn main() -> i64 { a::b::deep() }
        "#,
        "private",
    );
}

#[test]
fn per_segment_pub_module_allowed() {
    check_ok(r#"
        mod a {
            pub mod b {
                pub fn deep() -> i64 { 7 }
            }
        }
        fn main() -> i64 { a::b::deep() }
    "#);
}

#[test]
fn pub_use_reexports_private_item() {
    // `pub use` of m's own private item makes it reachable as
    // `m::secret` from outside the module.
    check_ok(r#"
        mod m {
            fn secret() -> i64 { 1 }
            pub use secret;
        }
        fn main() -> i64 { m::secret() }
    "#);
}
