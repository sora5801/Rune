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
fn tuple_match_unreachable_after_wildcard_arm() {
    // Session 094: a wildcard tuple arm covers everything; any
    // subsequent specific arm is unreachable.
    check_has_error(
        r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (true, false);
            match pair {
                (_, _) => 1,
                (true, true) => 2,
            }
        }
        "#,
        "unreachable match arm",
    );
}

#[test]
fn tuple_match_unreachable_specific_after_overlapping() {
    // `(true, _)` covers all (true, *) values; a later `(true,
    // true)` is shadowed.
    check_has_error(
        r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (true, false);
            match pair {
                (true, _) => 1,
                (true, true) => 2,
                (false, _) => 3,
            }
        }
        "#,
        "unreachable match arm",
    );
}

#[test]
fn tuple_match_overlapping_enum_specific_unreachable() {
    // `(Color::Red, _)` covers all (Red, *); a later `(Color::
    // Red, true)` is shadowed.
    check_has_error(
        r#"
        enum Color { Red, Green, Blue }
        fn main() -> i64 {
            let p: (Color, bool) = (Color::Red, true);
            match p {
                (Color::Red, _) => 1,
                (Color::Red, true) => 2,
                (Color::Green, _) => 3,
                (Color::Blue, _) => 4,
            }
        }
        "#,
        "unreachable match arm",
    );
}

#[test]
fn tuple_match_no_false_unreachable() {
    // Sanity: when each arm contributes new coverage, none
    // should be flagged.
    check_ok(
        r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (true, false);
            match pair {
                (true, true) => 1,
                (true, false) => 2,
                (false, true) => 3,
                (false, false) => 4,
            }
        }
        "#,
    );
}

#[test]
fn diagnostics_use_friendly_struct_name() {
    // Session 093: type errors should reference the struct's
    // source name instead of the internal sym index.
    let (le, pe, re, te) = run(
        r#"
        struct Point { x: i64, y: i64 }
        fn use_point(p: Point) -> i64 { p.x }
        fn main() -> i64 { use_point(42) }
        "#,
    );
    assert!(le.is_empty() && pe.is_empty() && re.is_empty());
    let msgs: Vec<String> = te.iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("expected `Point`")),
        "expected `Point` in diagnostic, got {:?}",
        msgs
    );
    assert!(
        !msgs.iter().any(|m| m.contains("struct#")),
        "diagnostic should not contain `struct#NN`, got {:?}",
        msgs
    );
}

#[test]
fn diagnostics_use_friendly_enum_name() {
    let (le, pe, re, te) = run(
        r#"
        enum Color { Red, Green, Blue }
        fn use_color(c: Color) -> i64 { 0 }
        fn main() -> i64 { use_color(true) }
        "#,
    );
    assert!(le.is_empty() && pe.is_empty() && re.is_empty());
    let msgs: Vec<String> = te.iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("expected `Color`")),
        "expected `Color` in diagnostic, got {:?}",
        msgs
    );
    assert!(
        !msgs.iter().any(|m| m.contains("enum#")),
        "diagnostic should not contain `enum#NN`, got {:?}",
        msgs
    );
}

#[test]
fn suffix_overflow_u8_rejected() {
    // Session 092: `1000u8` doesn't fit u8 (max 255); rejected
    // at type-check.
    check_has_error(
        "fn main() -> i64 { let x: u8 = 1000u8; x as i64 }",
        "out of range for `u8`",
    );
}

#[test]
fn suffix_overflow_i8_positive_rejected() {
    // `200i8` overflows i8 (max 127).
    check_has_error(
        "fn main() -> i64 { let x: i8 = 200i8; x as i64 }",
        "out of range for `i8`",
    );
}

#[test]
fn suffix_overflow_negative_signed_min_accepted() {
    // `-128i8` IS valid (i8 range is -128..=127). Make sure the
    // negated-range check accepts the lower bound.
    check_ok("fn main() -> i64 { let x: i8 = -128i8; x as i64 }");
}

#[test]
fn suffix_overflow_negative_one_past_min_rejected() {
    // `-129i8` is one past i8::MIN; rejected.
    check_has_error(
        "fn main() -> i64 { let x: i8 = -129i8; x as i64 }",
        "out of range for `i8`",
    );
}

#[test]
fn suffix_overflow_negative_unsigned_rejected() {
    // Negation of unsigned-suffixed literal is invalid regardless
    // of magnitude.
    check_has_error(
        "fn main() -> i64 { let x: u8 = -5u8; x as i64 }",
        "out of range for `u8`",
    );
}

#[test]
fn suffix_overflow_in_range_accepted() {
    // 255 fits u8 exactly; 127 fits i8 exactly.
    check_ok("fn main() -> i64 { let x: u8 = 255u8; x as i64 }");
    check_ok("fn main() -> i64 { let x: i8 = 127i8; x as i64 }");
}

#[test]
fn const_eval_add_overflow_rejected() {
    // Session 102: `100u8 + 200u8` const-evals to 300 which
    // overflows u8.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: u8 = 100u8 + 200u8;
            a as i64
        }
        "#,
        "literal `300` is out of range for `u8`",
    );
}

#[test]
fn const_eval_mul_overflow_rejected() {
    // `100i8 * 2i8` const-evals to 200 which overflows i8.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: i8 = 100i8 * 2i8;
            a as i64
        }
        "#,
        "literal `200` is out of range for `i8`",
    );
}

#[test]
fn const_eval_unsigned_underflow_rejected() {
    // `5u8 - 10u8` const-evals to -5 which doesn't fit u8.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: u8 = 5u8 - 10u8;
            a as i64
        }
        "#,
        "literal `-5` is out of range for `u8`",
    );
}

#[test]
fn const_eval_in_range_accepted() {
    // Sanity: a const-eval'd binop whose result fits the type
    // compiles cleanly.
    check_ok(
        r#"
        fn main() -> i64 {
            let a: u8 = 50u8 + 100u8;
            let b: i8 = 10i8 * 5i8;
            (a as i64) + (b as i64)
        }
        "#,
    );
}

#[test]
fn cross_let_const_eval_overflow_rejected() {
    // Session 106: const values flow through immutable let
    // bindings. `let a = 100u8; let b = 200u8; a + b` now
    // const-evals to 300 and gets caught by session 102's
    // range check — previously was a runtime wrap.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: u8 = 100u8;
            let b: u8 = 200u8;
            let c: u8 = a + b;
            c as i64
        }
        "#,
        "literal `300` is out of range for `u8`",
    );
}

#[test]
fn cross_let_const_eval_in_range_accepted() {
    // The complementary path: bindings whose sum fits the
    // type compile cleanly.
    check_ok(
        r#"
        fn main() -> i64 {
            let a: u8 = 50u8;
            let b: u8 = 100u8;
            let c: u8 = a + b;
            c as i64
        }
        "#,
    );
}

#[test]
fn cross_let_const_eval_skipped_for_mutable_binding() {
    // Mutable bindings don't get tracked — a `let mut a` could
    // be reassigned, invalidating any recorded value. The check
    // doesn't fire, so this compiles even though 100 + 200 = 300
    // would overflow u8 at runtime.
    check_ok(
        r#"
        fn main() -> i64 {
            let mut a: u8 = 100u8;
            let b: u8 = 200u8;
            let c: u8 = a + b;
            c as i64
        }
        "#,
    );
}

#[test]
fn cross_let_const_eval_chains_through_binding() {
    // The recorded value is the const-eval'd result of the
    // init, so a chain `let a = 1; let b = a + 2; let c = b * 3`
    // pins c to 9 — and `let d: u8 = c + 250u8` overflows.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: u8 = 5u8;
            let b: u8 = a + 1u8;
            let c: u8 = b + 250u8;
            c as i64
        }
        "#,
        "literal `256` is out of range for `u8`",
    );
}

#[test]
fn cross_let_const_eval_negation_through_binding() {
    // Negation flows through too: `-a` where `a` is const-tracked
    // const-evals to `-value`.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: i8 = 100i8;
            let b: i8 = -a - 100i8;
            b as i64
        }
        "#,
        "literal `-200` is out of range for `i8`",
    );
}

#[test]
fn hinted_literal_overflow_u8_rejected() {
    // Session 099: a bare literal hinted to u8 by a let-binding
    // annotation gets range-checked against u8's [0, 255].
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: u8 = 1000;
            a as i64
        }
        "#,
        "literal `1000` is out of range for `u8`",
    );
}

#[test]
fn hinted_literal_overflow_i8_negated_rejected() {
    // `-200` doesn't fit i8's [-128, 127]; the negated-range
    // check rejects.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a: i8 = -200;
            a as i64
        }
        "#,
        "literal `-200` is out of range for `i8`",
    );
}

#[test]
fn hinted_literal_in_range_accepted() {
    // Sanity: in-range values still compile.
    check_ok(
        r#"
        fn main() -> i64 {
            let a: u8 = 200;
            let b: i8 = -100;
            let c: i32 = 1_000_000;
            (a as i64) + (b as i64) + (c as i64)
        }
        "#,
    );
}

#[test]
fn suffix_literal_overflow_still_rejected() {
    // Existing session 092 check stays in place: `1000u8` errors
    // even without a let annotation.
    check_has_error(
        r#"
        fn main() -> i64 {
            let a = 1000u8;
            a as i64
        }
        "#,
        "literal `1000` is out of range for `u8`",
    );
}

#[test]
fn duplicate_into_impl_rejected() {
    // Session 090: two `impl Into<AppErr> for IoErr` blocks
    // collide on target and must error at type-check. The
    // diagnostic uses the friendly struct name in the message.
    check_has_error(
        r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: 1 } }
        }
        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: 2 } }
        }
        fn main() -> i64 { 0 }
        "#,
        "duplicate `impl Into<AppErr> for IoErr`",
    );
}

#[test]
fn distinct_into_targets_accepted() {
    // Two Into impls with DIFFERENT targets remain valid — the
    // duplicate-detection only fires on identical-target collisions.
    check_ok(
        r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        struct DbErr { code: i64 }
        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: self.code + 100 } }
        }
        impl std::Into<DbErr> for IoErr {
            fn into(self: IoErr) -> DbErr { DbErr { code: self.code + 200 } }
        }
        fn main() -> i64 { 0 }
        "#,
    );
}

#[test]
fn try_without_into_impl_rejected() {
    // Session 065: `?` with mismatched err types and NO Into impl
    // is still an error. The error message names the missing impl
    // so the user knows what to write.
    check_has_error(
        r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        fn inner() -> std::Result<i64, IoErr> {
            std::Result::Err(IoErr { code: 7 })
        }
        fn outer() -> std::Result<i64, AppErr> {
            let v: i64 = inner()?;
            std::Result::Ok(v)
        }
        fn main() -> i64 { 0 }
        "#,
        "Into",
    );
}

#[test]
fn standalone_range_is_a_range_iter() {
    // Session 063: `0..10` is now a valid expression — it lowers to
    // a `std::RangeIter { cur: 0, end: 10 }` struct value that
    // implements Iterator. The standalone-error diagnostic this
    // test originally pinned is gone.
    check_ok("fn main() -> i64 { let r: std::RangeIter = 0..10; 0 }");
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
fn field_assignment_on_param_allowed() {
    // Heap-struct interior mutation. A user struct is an 8-byte
    // descriptor pointer; the caller and callee share the same
    // heap location, so `p.x = 5` mutates a value both see and
    // is not a stack-aliasing hazard. Without this, an iterator
    // `fn next(self: Counter) { self.n = self.n + 1; ... }`
    // couldn't advance its own state (session 053).
    check_ok(
        r#"
        struct P { x: i64 }
        fn touch(p: P) { p.x = 5; }
        fn main() {}
        "#,
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

// ---- ? operator ----

#[test]
fn try_typechecks_ok() {
    check_ok(r#"
        fn g() -> std::Result<i64, i64> { std::Result::Ok(1) }
        fn h() -> std::Result<i64, i64> {
            let x = g()?;
            std::Result::Ok(x)
        }
    "#);
}

#[test]
fn try_on_non_result_errors() {
    check_has_error(
        r#"
        fn h() -> std::Result<i64, i64> {
            let x = 5?;
            std::Result::Ok(x)
        }
        "#,
        "requires a `Result`",
    );
}

#[test]
fn try_in_non_result_fn_errors() {
    check_has_error(
        r#"
        fn g() -> std::Result<i64, i64> { std::Result::Ok(1) }
        fn main() -> i64 { g()? }
        "#,
        "returning a `Result`",
    );
}

#[test]
fn try_error_type_mismatch() {
    // `?` propagates a `bool` error, but the function's error type
    // is `i64`.
    check_has_error(
        r#"
        fn g() -> std::Result<i64, bool> { std::Result::Ok(1) }
        fn h() -> std::Result<i64, i64> {
            let x = g()?;
            std::Result::Ok(x)
        }
        "#,
        "propagates an error",
    );
}

// ---- dyn Trait ----

#[test]
fn dyn_trait_typechecks() {
    check_ok(r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r }
        }
        fn describe(s: dyn Shape) -> i64 { s.area() }
        fn main() -> i64 { describe(Circle { r: 1 }) }
    "#);
}

#[test]
fn dyn_non_implementing_struct_rejected() {
    // A struct that doesn't implement the trait can't coerce to it.
    check_has_error(
        r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct NotShape { x: i64 }
        fn describe(s: dyn Shape) -> i64 { 0 }
        fn main() -> i64 { describe(NotShape { x: 1 }) }
        "#,
        "argument 1",
    );
}

#[test]
fn dyn_of_non_trait_rejected() {
    // `dyn T` requires `T` to be a trait.
    check_has_error(
        r#"
        struct Foo { x: i64 }
        fn take(f: dyn Foo) -> i64 { 0 }
        fn main() -> i64 { 0 }
        "#,
        "is not a trait",
    );
}

#[test]
fn vec_of_dyn_typechecks() {
    // `Vec<dyn Shape>` is a valid element type, and a struct that
    // implements the trait coerces at the `push` argument position.
    check_ok(r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Circle { r: i64 }
        impl Shape for Circle { fn area(self: Circle) -> i64 { self.r } }
        fn main() -> i64 {
            let mut shapes: Vec<dyn Shape> = vec_new();
            shapes.push(Circle { r: 1 });
            shapes.get(0).area()
        }
    "#);
}

#[test]
fn vec_of_dyn_rejects_non_impl() {
    // Pushing a struct that doesn't implement the trait into a
    // `Vec<dyn Shape>` is rejected — no coercion is available.
    check_has_error(
        r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Circle { r: i64 }
        impl Shape for Circle { fn area(self: Circle) -> i64 { self.r } }
        struct NotShape { x: i64 }
        fn main() -> i64 {
            let mut shapes: Vec<dyn Shape> = vec_new();
            shapes.push(NotShape { x: 1 });
            0
        }
        "#,
        "argument 1",
    );
}

#[test]
fn array_type_annotation_checks() {
    check_ok(r#"
        fn sum3(a: [i64; 3]) -> i64 {
            a[0] + a[1] + a[2]
        }
        fn main() -> i64 {
            let nums: [i64; 3] = [4, 5, 6];
            sum3(nums)
        }
    "#);
}

#[test]
fn struct_and_enum_array_fields_check() {
    check_ok(r#"
        struct Grid { cells: [Vec<i64>; 2], rows: i64 }
        enum Bag { Pair([i64; 2]), Empty }
        fn main() -> i64 {
            let g: Grid = Grid { cells: [vec_new(), vec_new()], rows: 2 };
            let b: Bag = Bag::Pair([1, 2]);
            g.rows
        }
    "#);
}

#[test]
fn dyn_coercion_at_struct_field_and_enum_payload() {
    // A concrete struct coerces to `dyn Trait` at a struct-literal
    // field initializer and at an enum-variant payload position.
    check_ok(r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        struct Holder { shape: dyn Shape }
        enum Maybe { Has(dyn Shape), Empty }
        fn main() -> i64 {
            let h: Holder = Holder { shape: Circle { r: 2 } };
            let m: Maybe = Maybe::Has(Circle { r: 3 });
            0
        }
    "#);
}

#[test]
fn dyn_field_non_implementor_rejected() {
    // A struct that does not implement the trait cannot coerce at a
    // `dyn` field position.
    check_has_error(
        r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct NotShape { x: i64 }
        struct Holder { shape: dyn Shape }
        fn main() -> i64 {
            let h: Holder = Holder { shape: NotShape { x: 1 } };
            0
        }
        "#,
        "shape",
    );
}

#[test]
fn generic_impl_typechecks() {
    check_ok(r#"
        struct Box<T> { val: T }
        impl<T> Box<T> {
            fn get(self: Box<T>) -> T { self.val }
            fn replace(self: Box<T>, v: T) -> Box<T> { Box { val: v } }
        }
        fn main() -> i64 {
            let b: Box<i64> = Box { val: 5 };
            b.get()
        }
    "#);
}

#[test]
fn impl_missing_assoc_type_rejected() {
    check_has_error(
        r#"
        trait Iterator {
            type Item;
            fn next(self: dyn Iterator) -> i64;
        }
        struct C { n: i64 }
        impl Iterator for C {
            fn next(self: C) -> i64 { 0 }
        }
        fn main() -> i64 { 0 }
        "#,
        "missing associated type `Item`",
    );
}

#[test]
fn impl_unknown_assoc_type_rejected() {
    check_has_error(
        r#"
        trait Iterator {
            fn next(self: dyn Iterator) -> i64;
        }
        struct C { n: i64 }
        impl Iterator for C {
            type Other = i64;
            fn next(self: C) -> i64 { 0 }
        }
        fn main() -> i64 { 0 }
        "#,
        "trait declares no associated type `Other`",
    );
}

#[test]
fn impl_missing_supertrait_rejected() {
    check_has_error(
        r#"
        trait Animal { fn speak(self: dyn Animal) -> i64; }
        trait Dog: Animal { fn bark(self: dyn Dog) -> i64; }
        struct Lab { n: i64 }
        impl Dog for Lab {
            fn bark(self: Lab) -> i64 { self.n }
        }
        fn main() -> i64 { 0 }
        "#,
        "requires supertrait `Animal`",
    );
}

#[test]
fn unresolved_supertrait_rejected() {
    check_has_error(
        r#"
        trait Dog: Unknown { fn bark(self: dyn Dog) -> i64; }
        fn main() -> i64 { 0 }
        "#,
        "unresolved trait `Unknown`",
    );
}

#[test]
fn assoc_type_method_rejected_through_dyn() {
    // A method whose return type is `Self::Item` cannot be called
    // through a `dyn Trait` — there is no impl-side binding to
    // resolve the projection against. The checker emits a precise
    // diagnostic at the call site instead of silently producing
    // a `Ty::Error` that confuses downstream errors.
    check_has_error(
        r#"
        trait Iterator {
            type Item;
            fn next(self: dyn Iterator) -> Self::Item;
        }
        fn drive(it: dyn Iterator) -> i64 { it.next() }
        fn main() -> i64 { 0 }
        "#,
        "cannot be projected through `dyn",
    );
}

#[test]
fn assoc_type_projection_without_bound_rejects_method_call() {
    // `T` has no trait bound, so `x.next()` finds no method on a
    // bare type-parameter. The diagnostic surfaces here even
    // though the function's return is declared as the projection
    // `T::Item` — that part stays an opaque `Ty::Assoc` until
    // monomorphization, which is when a concrete `T` would be
    // available to look up `Item` against.
    check_has_error(
        r#"
        trait Iterator {
            type Item;
            fn next(self: dyn Iterator) -> Self::Item;
        }
        fn bad<T>(x: T) -> T::Item { x.next() }
        fn main() -> i64 { 0 }
        "#,
        "no method `.next`",
    );
}

#[test]
fn dyn_supertrait_missing_impl_rejected() {
    // Coercing `Lab` to `dyn Dog` requires `Lab` to implement
    // every supertrait in Dog's chain. Without `impl Animal for
    // Lab`, the impl-side conformance check (session 050) fires
    // long before the dyn coercion runs, so the diagnostic
    // points at the offending `impl Dog for Lab` block.
    check_has_error(
        r#"
        trait Animal { fn speak(self: dyn Animal) -> i64; }
        trait Dog: Animal { fn bark(self: dyn Dog) -> i64; }
        struct Lab { n: i64 }
        impl Dog for Lab {
            fn bark(self: Lab) -> i64 { self.n }
        }
        fn handle(d: dyn Dog) -> i64 { d.bark() }
        fn main() -> i64 { 0 }
        "#,
        "requires supertrait `Animal`",
    );
}

#[test]
fn dyn_method_not_on_chain_rejected() {
    // `dyn Dog` exposes Dog's and Animal's methods — nothing
    // else. A method that's on neither produces the existing
    // "no method" diagnostic; the supertrait walk in
    // `dyn_method_sig` runs to exhaustion and returns None.
    check_has_error(
        r#"
        trait Animal { fn speak(self: dyn Animal) -> i64; }
        trait Dog: Animal { fn bark(self: dyn Dog) -> i64; }
        struct Lab { n: i64 }
        impl Animal for Lab { fn speak(self: Lab) -> i64 { self.n } }
        impl Dog for Lab    { fn bark(self: Lab) -> i64 { self.n + 1 } }
        fn handle(d: dyn Dog) -> i64 { d.purr() }
        fn main() -> i64 { 0 }
        "#,
        "no method `.purr`",
    );
}

#[test]
fn for_in_non_iterator_struct_rejected() {
    // A struct with no `impl Iterator` block can't be the right-
    // hand side of `for x in ...`. The lowerer leaves an
    // `Unsupported`, but the checker's `check_for` produces the
    // user-facing diagnostic earlier.
    check_has_error(
        r#"
        struct Bag { n: i64 }
        fn main() {
            let b: Bag = Bag { n: 1 };
            for x in b { let _: i64 = x; }
        }
        "#,
        "does not implement `std::Iterator`",
    );
}

#[test]
fn iterator_impl_missing_next_rejected() {
    // The existing trait-impl conformance check fires for an
    // impl missing the trait's required method. Iterator is no
    // different — `impl Iterator for Counter` without `fn next`
    // is rejected the same way as any other partial impl.
    check_has_error(
        r#"
        struct Counter { n: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
        }
        fn main() {}
        "#,
        "missing method",
    );
}

#[test]
fn unresolved_path_bound_diagnostic_uses_full_path() {
    // A bound that doesn't resolve produces an "unresolved trait
    // `a::Unknown`" diagnostic — the full path appears, not just
    // the last segment.
    check_has_error(
        r#"
        fn ask<T: a::Unknown>(x: T) -> i64 { 0 }
        fn main() -> i64 { 0 }
        "#,
        "unresolved trait `a::Unknown`",
    );
}

#[test]
fn unresolved_path_supertrait_diagnostic_uses_full_path() {
    // Same shape for a supertrait list.
    check_has_error(
        r#"
        trait Sub: a::Unknown { fn n(self: dyn Sub) -> i64; }
        fn main() -> i64 { 0 }
        "#,
        "unresolved trait `a::Unknown`",
    );
}

#[test]
fn map_wrong_fn_signature_rejected() {
    // Session 061: `Map<I, F, U>` with `F: Fn1<I::Item, U>`. A
    // callback whose first arg is `str` doesn't satisfy the bound
    // `Fn1<I::Item = i64, U>`, so the propagation surfaces a
    // "field bound mismatch".
    check_has_error(
        r#"
        fn takes_str(s: str) -> i64 { s.len() }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1);
            let m = std::Map { iter: v.iter(), f: takes_str };
            0
        }
        "#,
        "bound mismatch",
    );
}

#[test]
fn map_inferred_struct_arg_mismatch_rejected() {
    // Session 061: the bound-arg propagation catches the same
    // class of mismatch as session 056's struct-lit subst
    // inference. `pred: takes_bool` (bool → bool) clashes with
    // `I::Item = i64`; the unification reports the bound
    // mismatch at the struct-lit's span.
    check_has_error(
        r#"
        fn takes_bool(b: bool) -> bool { b }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1);
            let m = std::Map { iter: v.iter(), f: takes_bool };
            0
        }
        "#,
        "bound mismatch",
    );
}

#[test]
fn closure_capture_ok_no_diagnostic() {
    // As of session 059, capturing closures are recorded by the
    // resolver (not rejected). This test pins that there's no
    // "captures `mult`" diagnostic; the rest of the capture
    // machinery (lowerer synthesis, codegen) is exercised by
    // codegen tests.
    check_ok(
        r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            let f: fn(i64) -> i64 = |x| x * mult;
            f(7)
        }
        "#,
    );
}

#[test]
fn closure_arity_mismatch_rejected() {
    // The contextual hint says one param; the closure has two.
    check_has_error(
        r#"
        fn main() -> i64 {
            let f: fn(i64) -> i64 = |x, y| x + y;
            f(7)
        }
        "#,
        "parameter",
    );
}

#[test]
fn closure_return_type_mismatch_rejected() {
    // Body returns `bool` but the contextual hint says `i64`.
    check_has_error(
        r#"
        fn main() -> i64 {
            let f: fn(i64) -> i64 = |x| true;
            0
        }
        "#,
        "closure body returns",
    );
}
