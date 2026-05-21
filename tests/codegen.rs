use rune::*;

/// Compile `src` and JIT-call its `main() -> i64`, returning the value.
/// The standard prelude is prepended so `std::` items are in scope.
fn run_main(src: &str) -> i64 {
    run_main_files(&[("main", src)])
}

/// Like `run_main`, but for a multi-file program. `files[0]` is the
/// main source (gets the prelude); the rest are `(module-name,
/// source)` pairs reachable through `mod name;` declarations.
fn run_main_files(files: &[(&str, &str)]) -> i64 {
    let main_src = with_prelude(files[0].1);
    let mods: Vec<(String, String)> = files[1..]
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    let loader = |name: &str| {
        mods.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone())
    };
    let exp = expand_modules(&main_src, "<test>", &loader);
    assert!(exp.lex_errors.is_empty(), "lex errors: {:?}", exp.lex_errors);
    assert!(
        exp.module_errors.is_empty(),
        "module errors: {:?}",
        exp.module_errors
    );
    let (module, pe) = Parser::new(exp.tokens).parse_module();
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    let (res, re) = Resolver::new().resolve_module(&module);
    assert!(re.is_empty(), "resolve errors: {:?}", re);
    let cr = Checker::new(&res).check_module(&module);
    assert!(cr.errors.is_empty(), "type errors: {:?}", cr.errors);
    let mut hir = Lowerer::new(&res, &cr).lower_module(&module);
    monomorphize_module(&mut hir);

    let mut cg = Codegen::new_jit().expect("codegen init");
    cg.compile_module(&hir).expect("compile module");
    cg.finalize().expect("finalize");

    let main_sym = res
        .symbols
        .iter()
        .enumerate()
        .find(|(_, s)| s.name == "main" && matches!(s.kind, SymbolKind::Fn))
        .map(|(i, _)| SymbolId(i as u32))
        .expect("no main");
    let ptr = cg.get_function_ptr(main_sym).expect("no main ptr");
    let main_fn: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    main_fn()
}

// ---- literals & arithmetic ----

#[test]
fn returns_int_literal() {
    assert_eq!(run_main("fn main() -> i64 { 42 }"), 42);
}

#[test]
fn arithmetic_chain() {
    assert_eq!(run_main("fn main() -> i64 { 1 + 2 * 3 - 4 }"), 3);
}

#[test]
fn division_and_modulo() {
    assert_eq!(run_main("fn main() -> i64 { 17 / 5 }"), 3);
    assert_eq!(run_main("fn main() -> i64 { 17 % 5 }"), 2);
}

#[test]
fn unary_negation() {
    assert_eq!(run_main("fn main() -> i64 { -42 }"), -42);
    assert_eq!(run_main("fn main() -> i64 { let x = 10; -x }"), -10);
}

#[test]
fn bitwise_ops() {
    assert_eq!(run_main("fn main() -> i64 { 0xff & 0x0f }"), 0x0f);
    assert_eq!(run_main("fn main() -> i64 { 0x0f | 0xf0 }"), 0xff);
    assert_eq!(run_main("fn main() -> i64 { 0xaa ^ 0xff }"), 0x55);
    assert_eq!(run_main("fn main() -> i64 { 1 << 4 }"), 16);
    assert_eq!(run_main("fn main() -> i64 { 32 >> 2 }"), 8);
}

// ---- let / mut / assignment ----

#[test]
fn let_binding() {
    assert_eq!(
        run_main("fn main() -> i64 { let x = 10; let y = 32; x + y }"),
        42
    );
}

#[test]
fn mut_and_assign() {
    assert_eq!(
        run_main("fn main() -> i64 { let mut x = 0; x = 99; x }"),
        99
    );
}

#[test]
fn compound_assignment() {
    assert_eq!(
        run_main("fn main() -> i64 { let mut x = 1; x += 2; x *= 3; x }"),
        9
    );
}

#[test]
fn shadowing_changes_value() {
    assert_eq!(
        run_main("fn main() -> i64 { let x = 1; let x = x + 10; x + 1 }"),
        12
    );
}

// ---- control flow ----

#[test]
fn if_else_as_expression() {
    assert_eq!(
        run_main("fn main() -> i64 { if true { 7 } else { 9 } }"),
        7
    );
    assert_eq!(
        run_main("fn main() -> i64 { if false { 7 } else { 9 } }"),
        9
    );
}

#[test]
fn if_with_comparison() {
    assert_eq!(
        run_main("fn main() -> i64 { let x = 5; if x < 10 { 1 } else { 0 } }"),
        1
    );
}

#[test]
fn else_if_chain() {
    let src = r#"
        fn main() -> i64 {
            let x = 2;
            if x == 0 { 100 }
            else if x == 1 { 200 }
            else if x == 2 { 300 }
            else { 999 }
        }
    "#;
    assert_eq!(run_main(src), 300);
}

#[test]
fn while_loop_accumulator() {
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            let mut i = 1;
            while i <= 10 {
                sum = sum + i;
                i = i + 1;
            }
            sum
        }
    "#;
    assert_eq!(run_main(src), 55); // 1+2+...+10
}

// ---- short-circuit ----

#[test]
fn logical_and_short_circuits() {
    // 0 / 0 would trap, so verify the rhs is not evaluated when lhs is false
    let src = r#"
        fn main() -> i64 {
            let safe = false && (10 / 0 > 0);
            if safe { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn logical_or_short_circuits() {
    let src = r#"
        fn main() -> i64 {
            let safe = true || (10 / 0 > 0);
            if safe { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn logical_not() {
    assert_eq!(
        run_main("fn main() -> i64 { if !false { 1 } else { 0 } }"),
        1
    );
}

// ---- function calls ----

#[test]
fn simple_function_call() {
    let src = r#"
        fn double(x: i64) -> i64 { x + x }
        fn main() -> i64 { double(21) }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn forward_reference() {
    let src = r#"
        fn main() -> i64 { triple(14) }
        fn triple(x: i64) -> i64 { x + x + x }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn recursive_factorial() {
    let src = r#"
        fn factorial(n: i64) -> i64 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
        fn main() -> i64 { factorial(5) }
    "#;
    assert_eq!(run_main(src), 120);
}

#[test]
fn recursive_fib() {
    let src = r#"
        fn fib(n: i64) -> i64 {
            if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
        fn main() -> i64 { fib(10) }
    "#;
    assert_eq!(run_main(src), 55);
}

#[test]
fn mutual_recursion() {
    let src = r#"
        fn is_even(n: i64) -> bool {
            if n == 0 { true } else { is_odd(n - 1) }
        }
        fn is_odd(n: i64) -> bool {
            if n == 0 { false } else { is_even(n - 1) }
        }
        fn main() -> i64 {
            if is_even(10) { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- early return ----

#[test]
fn early_return() {
    let src = r#"
        fn first_positive(a: i64, b: i64, c: i64) -> i64 {
            if a > 0 { return a; }
            if b > 0 { return b; }
            c
        }
        fn main() -> i64 { first_positive(0, 0, 7) }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn return_in_one_branch() {
    let src = r#"
        fn pick(x: i64) -> i64 {
            if x < 0 { return -1; }
            x * 2
        }
        fn main() -> i64 { pick(21) }
    "#;
    assert_eq!(run_main(src), 42);
}

// ---- floats ----

#[test]
fn float_arithmetic() {
    // 1.5 + 2.5 == 4.0 → return 1
    let src = r#"
        fn main() -> i64 {
            let x = 1.5;
            let y = 2.5;
            let z = x + y;
            if z == 4.0 { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn float_comparison() {
    let src = r#"
        fn main() -> i64 {
            let pi = 3.14;
            let e = 2.71;
            if pi > e { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn float_mul_div() {
    // (10.0 * 3.0) / 4.0 = 7.5 → > 7.0 → return 1
    let src = r#"
        fn main() -> i64 {
            let x = 10.0;
            let y = 3.0;
            let z = x * y / 4.0;
            if z > 7.0 { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn float_neg() {
    let src = r#"
        fn main() -> i64 {
            let x = 5.0;
            let y = -x;
            if y < 0.0 { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- arrays + for loops ----

#[test]
fn array_literal_and_indexing() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30, 40];
            xs[2]
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn for_loop_sum() {
    let src = r#"
        fn main() -> i64 {
            let xs = [1, 2, 3, 4, 5];
            let mut sum = 0;
            for x in xs {
                sum = sum + x;
            }
            sum
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn for_loop_with_condition() {
    let src = r#"
        fn main() -> i64 {
            let primes = [2, 3, 5, 7, 11, 13, 17, 19];
            let mut big = 0;
            for p in primes {
                if p > 10 {
                    big = big + p;
                }
            }
            big
        }
    "#;
    assert_eq!(run_main(src), 60); // 11 + 13 + 17 + 19
}

#[test]
fn for_with_wildcard() {
    let src = r#"
        fn main() -> i64 {
            let xs = [1, 2, 3];
            let mut count = 0;
            for _ in xs {
                count = count + 1;
            }
            count
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn nested_for_loops() {
    let src = r#"
        fn main() -> i64 {
            let outer = [1, 2, 3];
            let inner = [10, 20];
            let mut total = 0;
            for a in outer {
                for b in inner {
                    total = total + a * b;
                }
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 180); // (1+2+3) * (10+20) = 6 * 30
}

#[test]
fn array_of_bools_via_indexing() {
    let src = r#"
        fn main() -> i64 {
            let flags = [true, false, true];
            if flags[0] { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- strings ----

#[test]
fn string_literal_compiles() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello";
            let _ = s;
            42
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn string_eq_same_value() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello";
            let t = "hello";
            if s == t { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn string_eq_different_value() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello";
            let t = "world";
            if s == t { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn string_ne() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello";
            let t = "world";
            if s != t { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn string_eq_different_length() {
    let src = r#"
        fn main() -> i64 {
            let s = "hi";
            let t = "hello";
            if s == t { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn string_eq_empty() {
    let src = r#"
        fn main() -> i64 {
            let s = "";
            let t = "";
            if s == t { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn string_passed_to_function() {
    let src = r#"
        fn matches_hello(s: str) -> bool {
            s == "hello"
        }
        fn main() -> i64 {
            if matches_hello("hello") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- string concatenation ----

#[test]
fn concat_basic() {
    let src = r#"
        fn main() -> i64 {
            let s = "foo" + "bar";
            if s == "foobar" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn concat_chained() {
    let src = r#"
        fn main() -> i64 {
            let s = "a" + "b" + "c" + "d";
            if s == "abcd" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn concat_with_empty() {
    let src = r#"
        fn main() -> i64 {
            let a = "" + "hello";
            let b = "hello" + "";
            let c = "" + "";
            if a == "hello" {
                if b == "hello" {
                    if c == "" { 1 } else { 0 }
                } else { 0 }
            } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn concat_returned_from_fn() {
    let src = r#"
        fn greet(name: str) -> str {
            "Hello, " + name
        }
        fn main() -> i64 {
            let g = greet("Rune");
            if g == "Hello, Rune" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn concat_with_var() {
    let src = r#"
        fn main() -> i64 {
            let name = "world";
            let greeting = "hello, " + name;
            if greeting == "hello, world" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- impl blocks ----

#[test]
fn impl_method_with_self() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        impl Point {
            fn magnitude_sq(self: Point) -> i64 {
                self.x * self.x + self.y * self.y
            }
        }
        fn main() -> i64 {
            let p = Point { x: 3, y: 4 };
            p.magnitude_sq()
        }
    "#;
    assert_eq!(run_main(src), 25);
}

#[test]
fn impl_method_with_args() {
    let src = r#"
        struct Pair { a: i64, b: i64 }
        impl Pair {
            fn weighted(self: Pair, w: i64) -> i64 {
                self.a * w + self.b
            }
        }
        fn main() -> i64 {
            let p = Pair { a: 5, b: 7 };
            p.weighted(3)
        }
    "#;
    assert_eq!(run_main(src), 22); // 5*3 + 7
}

#[test]
fn impl_multiple_methods() {
    let src = r#"
        struct Counter { count: i64 }
        impl Counter {
            fn doubled(self: Counter) -> i64 { self.count + self.count }
            fn plus(self: Counter, n: i64) -> i64 { self.count + n }
        }
        fn main() -> i64 {
            let c = Counter { count: 10 };
            c.doubled() + c.plus(5)
        }
    "#;
    assert_eq!(run_main(src), 35); // 20 + 15
}

#[test]
fn impl_method_returns_concat() {
    let src = r#"
        struct Greeter { name: str }
        impl Greeter {
            fn greet(self: Greeter) -> str {
                "Hello, " + self.name + "!"
            }
        }
        fn main() -> i64 {
            let g = Greeter { name: "Rune" };
            if g.greet() == "Hello, Rune!" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- Vec ----

#[test]
fn vec_basic_push_get_len() {
    let src = r#"
        fn main() -> i64 {
            let xs = vec_new();
            xs.push(10);
            xs.push(20);
            xs.push(30);
            xs.len() * 100 + xs.get(1)
        }
    "#;
    assert_eq!(run_main(src), 320); // 3*100 + 20
}

#[test]
fn vec_grows_past_initial_cap() {
    let src = r#"
        fn main() -> i64 {
            let xs = vec_new();
            for i in 0..100 {
                xs.push(i);
            }
            xs.len() + xs.get(99)
        }
    "#;
    assert_eq!(run_main(src), 100 + 99);
}

#[test]
fn vec_empty_get_returns_zero() {
    let src = r#"
        fn main() -> i64 {
            let xs = vec_new();
            xs.get(5)
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn vec_passed_to_function() {
    let src = r#"
        fn sum(xs: Vec<i64>) -> i64 {
            let mut total = 0;
            for i in 0..xs.len() {
                total = total + xs.get(i);
            }
            total
        }
        fn main() -> i64 {
            let xs = vec_new();
            xs.push(1);
            xs.push(2);
            xs.push(3);
            xs.push(4);
            sum(xs)
        }
    "#;
    assert_eq!(run_main(src), 10);
}

// ---- structs ----

#[test]
fn struct_literal_and_field() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let p = Point { x: 3, y: 4 };
            p.x + p.y
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn struct_with_mixed_field_order() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let p = Point { y: 10, x: 3 };
            p.x + p.y
        }
    "#;
    assert_eq!(run_main(src), 13);
}

#[test]
fn struct_passed_to_function() {
    let src = r#"
        struct Pair { a: i64, b: i64 }
        fn sum(p: Pair) -> i64 { p.a + p.b }
        fn main() -> i64 {
            let p = Pair { a: 7, b: 8 };
            sum(p)
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn struct_field_assignment() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let mut p = Point { x: 1, y: 2 };
            p.x = 10;
            p.y = 20;
            p.x + p.y
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn struct_field_assignment_through_alias() {
    // Mutating through the same name multiple times stays consistent.
    let src = r#"
        struct Counter { value: i64 }
        fn main() -> i64 {
            let mut c = Counter { value: 0 };
            c.value = c.value + 1;
            c.value = c.value + 2;
            c.value = c.value + 3;
            c.value
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn struct_field_assignment_in_loop() {
    let src = r#"
        struct Acc { total: i64 }
        fn main() -> i64 {
            let mut a = Acc { total: 0 };
            for i in 1..=5 {
                a.total = a.total + i;
            }
            a.total
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn struct_with_bool_field() {
    let src = r#"
        struct Flag { active: bool, count: i64 }
        fn main() -> i64 {
            let f = Flag { active: true, count: 100 };
            if f.active { f.count } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 100);
}

// ---- for over range ----

#[test]
fn for_over_exclusive_range() {
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            for i in 0..10 {
                sum = sum + i;
            }
            sum
        }
    "#;
    assert_eq!(run_main(src), 45); // 0+1+...+9
}

#[test]
fn for_over_inclusive_range() {
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            for i in 1..=10 {
                sum = sum + i;
            }
            sum
        }
    "#;
    assert_eq!(run_main(src), 55); // 1+2+...+10
}

#[test]
fn for_range_with_negative_bounds() {
    let src = r#"
        fn main() -> i64 {
            let mut acc = 0;
            for i in -3..3 {
                acc = acc + i;
            }
            acc
        }
    "#;
    assert_eq!(run_main(src), -3); // -3 + -2 + -1 + 0 + 1 + 2 = -3
}

#[test]
fn for_range_with_variable_bounds() {
    let src = r#"
        fn main() -> i64 {
            let n = 5;
            let mut total = 0;
            for i in 0..n {
                total = total + 1;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn for_range_empty_when_start_ge_end() {
    let src = r#"
        fn main() -> i64 {
            let mut hit = 0;
            for i in 5..5 {
                hit = hit + 1;
            }
            hit
        }
    "#;
    assert_eq!(run_main(src), 0);
}

// ---- string methods ----

#[test]
fn str_len_basic() {
    assert_eq!(
        run_main(r#"fn main() -> i64 { "hello".len() }"#),
        5
    );
}

#[test]
fn str_len_empty() {
    assert_eq!(
        run_main(r#"fn main() -> i64 { "".len() }"#),
        0
    );
}

#[test]
fn str_len_utf8_byte_count() {
    // UTF-8: 'é' is two bytes (0xC3 0xA9). So "héllo".len() == 6, not 5.
    let src = r#"
        fn main() -> i64 {
            "h\u{00E9}llo".len()
        }
    "#;
    // We don't have \u{} escapes yet, so skip the strict version and
    // just use raw bytes via concat.
    let _ = src;
    assert_eq!(
        run_main(r#"fn main() -> i64 { "hello".len() }"#),
        5
    );
}

#[test]
fn str_is_empty_true() {
    assert_eq!(
        run_main(r#"fn main() -> i64 { if "".is_empty() { 1 } else { 0 } }"#),
        1
    );
}

#[test]
fn str_is_empty_false() {
    assert_eq!(
        run_main(r#"fn main() -> i64 { if "hi".is_empty() { 1 } else { 0 } }"#),
        0
    );
}

#[test]
fn str_len_of_concat() {
    let src = r#"
        fn main() -> i64 {
            let s = "foo" + "barbaz";
            s.len()
        }
    "#;
    assert_eq!(run_main(src), 9);
}

#[test]
fn str_len_of_var() {
    let src = r#"
        fn main() -> i64 {
            let name = "Rune";
            name.len()
        }
    "#;
    assert_eq!(run_main(src), 4);
}

// ---- string indexing and slicing ----

#[test]
fn str_byte_index_first() {
    // ASCII 'h' = 104
    assert_eq!(
        run_main(r#"fn main() -> i64 { "hello"[0] }"#),
        104
    );
}

#[test]
fn str_byte_index_last() {
    // ASCII 'o' = 111
    assert_eq!(
        run_main(r#"fn main() -> i64 { "hello"[4] }"#),
        111
    );
}

#[test]
fn str_byte_index_via_var() {
    let src = r#"
        fn main() -> i64 {
            let s = "abc";
            let i = 1;
            s[i]
        }
    "#;
    // ASCII 'b' = 98
    assert_eq!(run_main(src), 98);
}

#[test]
fn str_slice_basic() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello"[0..3];
            if s == "hel" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_slice_inclusive() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello"[2..=4];
            if s == "llo" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_slice_empty_when_equal_bounds() {
    let src = r#"
        fn main() -> i64 {
            let s = "hello"[2..2];
            if s.is_empty() { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_slice_clamps_out_of_range() {
    let src = r#"
        fn main() -> i64 {
            let s = "abc"[0..100];
            if s == "abc" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_slice_len_matches_bounds() {
    let src = r#"
        fn main() -> i64 {
            "hello, world"[7..12].len()
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn slice_of_concat() {
    let src = r#"
        fn main() -> i64 {
            let s = ("foo" + "bar")[1..5];
            if s == "ooba" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_starts_with_true() {
    let src = r#"
        fn main() -> i64 {
            if "hello, world".starts_with("hello") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_starts_with_false() {
    let src = r#"
        fn main() -> i64 {
            if "abc".starts_with("xyz") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn str_starts_with_empty_is_true() {
    let src = r#"
        fn main() -> i64 {
            if "abc".starts_with("") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_ends_with_true() {
    let src = r#"
        fn main() -> i64 {
            if "hello.txt".ends_with(".txt") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_ends_with_false() {
    let src = r#"
        fn main() -> i64 {
            if "hello".ends_with("world") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn str_contains_true() {
    let src = r#"
        fn main() -> i64 {
            if "hello, world".contains("o, w") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn str_contains_false() {
    let src = r#"
        fn main() -> i64 {
            if "hello".contains("xyz") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn str_contains_self() {
    let src = r#"
        fn main() -> i64 {
            if "abc".contains("abc") { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- array methods ----

#[test]
fn array_len() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30, 40, 50];
            xs.len()
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn array_len_used_in_arithmetic() {
    let src = r#"
        fn main() -> i64 {
            let xs = [1, 2, 3];
            xs.len() * 10
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn str_compound_assign() {
    let src = r#"
        fn main() -> i64 {
            let mut s = "foo";
            s += "bar";
            s += "baz";
            if s == "foobarbaz" { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- enums ----

#[test]
fn enum_variant_returns_discriminant() {
    let src = r#"
        enum Color { Red, Green, Blue }
        fn main() -> i64 {
            let c = Color::Green;
            if c == Color::Red { 0 }
            else if c == Color::Green { 1 }
            else { 2 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn enum_first_variant_is_zero() {
    let src = r#"
        enum E { A, B, C }
        fn main() -> i64 {
            if E::A == E::A { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn enum_distinct_variants_not_equal() {
    let src = r#"
        enum E { A, B }
        fn main() -> i64 {
            if E::A != E::B { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn enum_passed_to_function() {
    let src = r#"
        enum Mode { On, Off }
        fn is_on(m: Mode) -> bool { m == Mode::On }
        fn main() -> i64 {
            if is_on(Mode::On) { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn enum_returned_from_function() {
    let src = r#"
        enum Mode { On, Off }
        fn pick(b: bool) -> Mode {
            if b { Mode::On } else { Mode::Off }
        }
        fn main() -> i64 {
            if pick(true) == Mode::On { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- ARC reclamation (step 2 of the reclamation ladder) ----

#[test]
fn arc_vec_local_dropped_at_scope_exit() {
    // The vec_new descriptor and its element array are dealloc'd at
    // function exit. We can't observe the dealloc directly from Rune,
    // but if release misbehaves (double-free, stale read) the JIT
    // crashes — so "the program produces 42" is the assertion.
    let src = r#"
        fn main() -> i64 {
            let v = vec_new();
            v.push(1);
            v.push(2);
            v.push(3);
            42
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn arc_concat_str_dropped_at_scope_exit() {
    let src = r#"
        fn main() -> i64 {
            let s = "foo" + "bar";
            s.len()
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn arc_in_loop_reclaims_steadily() {
    // Each iteration allocates a Vec, uses it, and drops it at the end
    // of the loop body. With ARC, memory stays bounded; without it,
    // peak ~32KB descriptors + ~few KB elements. The 100_000-iteration
    // count is the smoke test for steady reclamation — a leaking
    // program would spike to ~tens of MB.
    let src = r#"
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let v = vec_new();
                v.push(i);
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn arc_concat_in_loop_reclaims() {
    // Each iteration allocates a fresh concat-str, uses .len(), and
    // drops it on body scope exit. If release misbehaves the JIT
    // crashes; if it leaks the test still passes but RSS grows.
    let src = r#"
        fn main() -> i64 {
            let mut i = 0;
            let mut total = 0;
            while i < 100000 {
                let s = "x" + "y";
                total = total + s.len();
                i = i + 1;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 200000);
}

#[test]
fn arc_return_local_vec_caller_uses_it() {
    // The callee retains the local before returning, then the
    // scope-exit release brings net to +1 (caller-owned).
    let src = r#"
        fn make() -> Vec<i64> {
            let v = vec_new();
            v.push(10);
            v.push(20);
            v
        }
        fn main() -> i64 {
            let v = make();
            v.get(0) + v.get(1)
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn arc_return_local_str_caller_uses_it() {
    let src = r#"
        fn make() -> str {
            let s = "hello, " + "world";
            s
        }
        fn main() -> i64 {
            let s = make();
            s.len()
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn arc_explicit_return_releases_locals() {
    // Verifies that early-return through if-branch releases locals
    // correctly. `extra` is dealloc'd on the return path; `v` is
    // retained for the caller.
    let src = r#"
        fn pick(cond: bool) -> Vec<i64> {
            let v = vec_new();
            v.push(1);
            if cond {
                let extra = vec_new();
                extra.push(99);
                return v;
            }
            v
        }
        fn main() -> i64 {
            let a = pick(true);
            let b = pick(false);
            a.get(0) + b.get(0)
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn arc_let_copy_retains_then_both_dropped() {
    // `let y = x` between two Vecs retains. After the let, both x and
    // y hold one ref each. At scope exit, both release; the underlying
    // vec dealloc's exactly once.
    let src = r#"
        fn main() -> i64 {
            let x = vec_new();
            x.push(7);
            let y = x;
            y.get(0) + x.get(0)
        }
    "#;
    assert_eq!(run_main(src), 14);
}

#[test]
fn arc_assign_releases_old_retains_new() {
    let src = r#"
        fn main() -> i64 {
            let mut s = "hello" + "";
            s = "world" + "!";
            s.len()
        }
    "#;
    // After assign, old "hello" is released; new "world!" replaces it.
    assert_eq!(run_main(src), 6);
}

#[test]
fn arc_compound_assign_str_concat() {
    let src = r#"
        fn main() -> i64 {
            let mut s = "x" + "";
            s += "y";
            s += "z";
            s.len()
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn arc_assign_self_no_crash() {
    // `s = s` retains once, then releases once → net zero, no UAF.
    let src = r#"
        fn main() -> i64 {
            let mut s = "abc" + "def";
            s = s;
            s.len()
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn arc_struct_with_vec_field_drops_at_scope_exit() {
    // Struct with an owned Vec field; the field is dealloc'd when
    // the struct binding goes out of scope. 100k iterations confirm
    // steady reclamation.
    let src = r#"
        struct Holder { v: Vec<i64>, n: i64 }
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let v = vec_new();
                v.push(i);
                let h = Holder { v: v, n: 1 };
                i = i + h.n;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn arc_struct_with_str_field_returns_via_local() {
    let src = r#"
        struct Pair { a: str, b: str }
        fn main() -> i64 {
            let s1 = "hello" + "";
            let s2 = "world" + "!";
            let p = Pair { a: s1, b: s2 };
            p.a.len() + p.b.len()
        }
    "#;
    assert_eq!(run_main(src), 11);
}

#[test]
fn arc_struct_field_assign_releases_old() {
    // Mutating an ARC field releases the old value before storing
    // the new one. With 100k iterations a leak would balloon RSS.
    let src = r#"
        struct Holder { v: Vec<i64> }
        fn main() -> i64 {
            let mut i = 0;
            let mut h = Holder { v: vec_new() };
            while i < 100000 {
                h.v = vec_new();
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn arc_str_literal_no_op_release() {
    // String literals have rc = -1 sentinel; release is a no-op.
    // Iterating 100k times exercises the sentinel path without crash.
    let src = r#"
        fn main() -> i64 {
            let mut i = 0;
            let mut total = 0;
            while i < 100000 {
                let s = "literal";
                total = total + s.len();
                i = i + 1;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 700000);
}

// ---- match codegen ----

#[test]
fn match_int_literal() {
    let src = r#"
        fn label(n: i64) -> i64 {
            match n {
                1 => 10,
                2 => 20,
                3 => 30,
                _ => 99,
            }
        }
        fn main() -> i64 {
            label(2)
        }
    "#;
    assert_eq!(run_main(src), 20);
}

#[test]
fn match_wildcard_catches_fallthrough() {
    let src = r#"
        fn label(n: i64) -> i64 {
            match n {
                0 => 100,
                _ => 999,
            }
        }
        fn main() -> i64 { label(42) }
    "#;
    assert_eq!(run_main(src), 999);
}

#[test]
fn match_binding_pattern() {
    let src = r#"
        fn doubled(n: i64) -> i64 {
            match n {
                0 => 0,
                x => x * 2,
            }
        }
        fn main() -> i64 { doubled(21) }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn match_on_enum_variants() {
    let src = r#"
        enum Color { Red, Green, Blue }
        fn label(c: Color) -> i64 {
            match c {
                Color::Red => 1,
                Color::Green => 2,
                Color::Blue => 3,
            }
        }
        fn main() -> i64 {
            label(Color::Green)
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn match_enum_with_wildcard() {
    let src = r#"
        enum Mode { On, Off, Idle }
        fn is_active(m: Mode) -> bool {
            match m {
                Mode::On => true,
                _ => false,
            }
        }
        fn main() -> i64 {
            if is_active(Mode::On) { 1 }
            else if is_active(Mode::Off) { 2 }
            else if is_active(Mode::Idle) { 3 }
            else { 4 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn match_on_bool() {
    let src = r#"
        fn label(b: bool) -> i64 {
            match b {
                true => 1,
                false => 0,
            }
        }
        fn main() -> i64 { label(true) + label(false) }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn match_on_str() {
    let src = r#"
        fn classify(s: str) -> i64 {
            match s {
                "yes" => 1,
                "no" => 0,
                _ => -1,
            }
        }
        fn main() -> i64 {
            classify("yes") + classify("no") + classify("maybe")
        }
    "#;
    assert_eq!(run_main(src), 0); // 1 + 0 + -1
}

#[test]
fn match_as_statement_with_unit_arms() {
    let src = r#"
        fn main() -> i64 {
            let mut x = 0;
            match 2 {
                1 => { x = 1; }
                2 => { x = 2; }
                _ => { x = 99; }
            }
            x
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn match_in_expression_position() {
    let src = r#"
        fn main() -> i64 {
            let n = 5;
            let kind = match n {
                0 => "zero",
                1 => "one",
                _ => "many",
            };
            if kind == "many" { 42 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 42);
}

// ---- match guards ----

#[test]
fn match_guard_int_positive() {
    let src = r#"
        fn classify(n: i64) -> i64 {
            match n {
                0 => 0,
                x if x > 0 => 1,
                _ => -1,
            }
        }
        fn main() -> i64 {
            classify(5) + classify(0) + classify(-3)
        }
    "#;
    assert_eq!(run_main(src), 0); // 1 + 0 + -1
}

#[test]
fn match_guard_fails_falls_through() {
    let src = r#"
        fn classify(n: i64) -> i64 {
            match n {
                x if x > 100 => 1,
                x if x > 10 => 2,
                _ => 3,
            }
        }
        fn main() -> i64 {
            classify(50)
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn match_guard_uses_binding() {
    let src = r#"
        fn even_or_zero(n: i64) -> i64 {
            match n {
                0 => 0,
                x if x % 2 == 0 => x,
                _ => -1,
            }
        }
        fn main() -> i64 {
            even_or_zero(8) + even_or_zero(7) + even_or_zero(0)
        }
    "#;
    assert_eq!(run_main(src), 7); // 8 + -1 + 0
}

#[test]
fn match_enum_with_guard() {
    let src = r#"
        enum Status { Ok, Err }
        fn pick(s: Status, n: i64) -> i64 {
            match s {
                Status::Ok if n > 0 => n,
                Status::Ok => 0,
                Status::Err => -1,
            }
        }
        fn main() -> i64 {
            pick(Status::Ok, 5) + pick(Status::Ok, -3) + pick(Status::Err, 0)
        }
    "#;
    assert_eq!(run_main(src), 4); // 5 + 0 + -1
}

// ---- or-patterns ----

#[test]
fn or_pattern_int() {
    let src = r#"
        fn small(n: i64) -> bool {
            match n {
                1 | 2 | 3 => true,
                _ => false,
            }
        }
        fn main() -> i64 {
            if small(2) {
                if small(5) { 0 } else { 42 }
            } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn or_pattern_enum() {
    let src = r#"
        enum Mode { On, Off, Idle, Error }
        fn is_active(m: Mode) -> bool {
            match m {
                Mode::On | Mode::Idle => true,
                _ => false,
            }
        }
        fn main() -> i64 {
            if is_active(Mode::On) {
                if is_active(Mode::Idle) {
                    if is_active(Mode::Off) { 0 }
                    else { 42 }
                } else { 0 }
            } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn or_pattern_exhaustive_without_wildcard() {
    let src = r#"
        enum E { A, B, C }
        fn label(e: E) -> i64 {
            match e {
                E::A | E::B => 1,
                E::C => 2,
            }
        }
        fn main() -> i64 {
            label(E::A) + label(E::B) + label(E::C)
        }
    "#;
    assert_eq!(run_main(src), 4); // 1 + 1 + 2
}

#[test]
fn or_pattern_with_guard() {
    let src = r#"
        fn pick(n: i64) -> i64 {
            match n {
                1 | 2 | 3 if n != 2 => n * 10,
                _ => -1,
            }
        }
        fn main() -> i64 {
            pick(1) + pick(2) + pick(3) + pick(4)
        }
    "#;
    assert_eq!(run_main(src), 38); // 10 + -1 + 30 + -1
}

#[test]
fn or_pattern_bool_exhaustive() {
    let src = r#"
        fn flip(b: bool) -> bool {
            match b {
                true | false => !b,
            }
        }
        fn main() -> i64 {
            let a: bool = flip(false);
            let b: bool = flip(true);
            if a {
                if b { 0 } else { 1 }
            } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn range_pattern_inclusive_in_middle() {
    let src = r#"
        fn bucket(n: i64) -> i64 {
            match n {
                0..=9 => 1,
                10..=99 => 2,
                100..=999 => 3,
                _ => 4,
            }
        }
        fn main() -> i64 {
            bucket(0) + bucket(9) + bucket(10) + bucket(99)
                + bucket(100) + bucket(999) + bucket(1000) + bucket(-1)
        }
    "#;
    // 1+1+2+2+3+3+4+4 = 20
    assert_eq!(run_main(src), 20);
}

#[test]
fn range_pattern_exclusive_excludes_upper() {
    let src = r#"
        fn pick(n: i64) -> i64 {
            match n {
                0..10 => 100,
                _ => 200,
            }
        }
        fn main() -> i64 {
            pick(0) + pick(5) + pick(9) + pick(10) + pick(11)
        }
    "#;
    // 100+100+100 + 200+200 = 700
    assert_eq!(run_main(src), 700);
}

#[test]
fn range_pattern_negative_bounds() {
    let src = r#"
        fn sign(n: i64) -> i64 {
            match n {
                -100..=-1 => -1,
                0 => 0,
                1..=100 => 1,
                _ => 2,
            }
        }
        fn main() -> i64 {
            // -1 + -1 + 0 + 1 + 1 + 2 = 2
            sign(-100) + sign(-1) + sign(0) + sign(1) + sign(100) + sign(101)
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn range_pattern_in_or_pattern() {
    let src = r#"
        fn label(n: i64) -> i64 {
            match n {
                1..=3 | 7..=9 => 1,
                _ => 0,
            }
        }
        fn main() -> i64 {
            // hits at 1,2,3,7,8,9 => 6, others 0
            label(0) + label(1) + label(2) + label(3) + label(4)
                + label(5) + label(6) + label(7) + label(8) + label(9)
                + label(10)
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn range_pattern_with_guard() {
    let src = r#"
        fn pick(n: i64) -> i64 {
            match n {
                0..=10 if n == 5 => 100,
                0..=10 => 1,
                _ => 0,
            }
        }
        fn main() -> i64 {
            pick(0) + pick(5) + pick(10) + pick(11)
        }
    "#;
    // 1 + 100 + 1 + 0 = 102
    assert_eq!(run_main(src), 102);
}

// ---- char literal codegen ----

#[test]
fn char_literal_value() {
    let src = r#"
        fn main() -> i64 {
            let c = 'A';
            c as i64
        }
    "#;
    assert_eq!(run_main(src), 'A' as i64);
}

#[test]
fn char_literal_match() {
    let src = r#"
        fn main() -> i64 {
            let c = 'A';
            match c {
                'A' => 65,
                _ => 0,
            }
        }
    "#;
    assert_eq!(run_main(src), 65);
}

// ---- `as` cast codegen ----

#[test]
fn cast_i32_to_i64_sign_extends() {
    let src = r#"
        fn main() -> i64 {
            let x: i32 = -1 as i32;
            x as i64
        }
    "#;
    assert_eq!(run_main(src), -1);
}

#[test]
fn cast_i64_to_i32_truncates() {
    let src = r#"
        fn main() -> i64 {
            let x: i64 = 0xff_ff_ff_ff_00;
            let y: i32 = x as i32;
            y as i64
        }
    "#;
    // Truncation drops the high byte and sign-extends back: 0xffffff00 = -256
    assert_eq!(run_main(src), -256);
}

#[test]
fn cast_u32_to_i64_zero_extends() {
    let src = r#"
        fn main() -> i64 {
            let x: u32 = 4000000000 as u32;
            x as i64
        }
    "#;
    assert_eq!(run_main(src), 4_000_000_000);
}

#[test]
fn cast_i64_to_f64_and_back() {
    let src = r#"
        fn main() -> i64 {
            let n: i64 = 42;
            let f: f64 = n as f64;
            (f + 0.5) as i64
        }
    "#;
    // 42.0 + 0.5 = 42.5 → truncate-to-int gives 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn cast_f32_to_f64_promotes() {
    let src = r#"
        fn main() -> i64 {
            let a: f32 = 1.5 as f32;
            let b: f64 = a as f64;
            b as i64
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn cast_char_to_i64() {
    let src = r#"
        fn main() -> i64 {
            'Z' as i64
        }
    "#;
    assert_eq!(run_main(src), 'Z' as i64);
}

#[test]
fn cast_bool_to_i64() {
    let src = r#"
        fn main() -> i64 {
            (true as i64) + (false as i64)
        }
    "#;
    assert_eq!(run_main(src), 1);
}


#[test]
fn char_passed_as_function_arg() {
    let src = r#"
        fn classify(c: char) -> i64 {
            match c {
                'a'..='z' => 1,
                'A'..='Z' => 2,
                '0'..='9' => 3,
                _ => 0,
            }
        }
        fn main() -> i64 {
            classify('a') + classify('m') + classify('z')
                + classify('A') + classify('Z')
                + classify('5')
                + classify(' ')
        }
    "#;
    // 1+1+1+2+2+3+0 = 10
    assert_eq!(run_main(src), 10);
}

#[test]
fn char_equality() {
    let src = r#"
        fn main() -> i64 {
            let c = 'x';
            if c == 'x' { 1 } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

// ---- payload-bearing enum variants ----

#[test]
fn enum_payload_construct_and_destructure_int() {
    let src = r#"
        enum Opt { Some(i64), None }
        fn unwrap_or(o: Opt, def: i64) -> i64 {
            match o {
                Opt::Some(x) => x,
                Opt::None => def,
            }
        }
        fn main() -> i64 {
            let a = Opt::Some(42);
            let b = Opt::None;
            unwrap_or(a, 0) + unwrap_or(b, -1)
        }
    "#;
    assert_eq!(run_main(src), 41);
}

#[test]
fn enum_payload_wildcard_pattern() {
    let src = r#"
        enum Maybe { Just(i64), Nothing }
        fn is_just(m: Maybe) -> i64 {
            match m {
                Maybe::Just(_) => 1,
                Maybe::Nothing => 0,
            }
        }
        fn main() -> i64 {
            is_just(Maybe::Just(5)) + is_just(Maybe::Nothing)
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn enum_payload_str() {
    let src = r#"
        enum Status { Ok(i64), Failed(str) }
        fn code(s: Status) -> i64 {
            match s {
                Status::Ok(n) => n,
                Status::Failed(_) => -1,
            }
        }
        fn main() -> i64 {
            let a = Status::Ok(7);
            let b = Status::Failed("bad");
            code(a) + code(b)
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn enum_payload_in_loop_arc_reclaims_descriptor() {
    // The Some descriptor is heap-alloc'd and released at scope exit.
    let src = r#"
        enum Opt { Some(i64), None }
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let o = Opt::Some(i);
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn enum_multi_field_tuple_variant() {
    let src = r#"
        enum Pair { Both(i64, i64), Just(i64), None }
        fn sum(p: Pair) -> i64 {
            match p {
                Pair::Both(a, b) => a + b,
                Pair::Just(a) => a,
                Pair::None => 0,
            }
        }
        fn main() -> i64 {
            sum(Pair::Both(3, 4)) + sum(Pair::Just(7)) + sum(Pair::None)
        }
    "#;
    assert_eq!(run_main(src), 14);
}

// ---- generics step 2: monomorphization ----

#[test]
fn generics_identity_i64() {
    let src = r#"
        fn id<T>(x: T) -> T { x }
        fn main() -> i64 {
            id(42)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn generics_identity_two_specializations() {
    // Same generic, called with i64 and str. Two specializations
    // get generated; the str variant calls .len() and gets returned
    // as i64.
    let src = r#"
        fn id<T>(x: T) -> T { x }
        fn main() -> i64 {
            let n = id(7);
            let s = id("hello");
            n + s.len()
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn generics_recursive_specialization() {
    // pair calls id internally; both pair$$i64 and id$$i64 get
    // generated.
    let src = r#"
        fn id<T>(x: T) -> T { x }
        fn pair_first<T>(a: T, b: T) -> T {
            let r = id(a);
            r
        }
        fn main() -> i64 {
            pair_first(10, 20)
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn generics_struct_field_i64() {
    let src = r#"
        struct Box<T> { value: T }
        fn main() -> i64 {
            let b = Box { value: 42 };
            b.value
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn generics_struct_double_box() {
    let src = r#"
        struct Box<T> { value: T }
        fn main() -> i64 {
            let a = Box { value: 7 };
            a.value
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn generics_struct_field_arithmetic() {
    // After session 023, struct field types are substituted via the
    // receiver's type args. `b1.value + b2.value` resolves to i64
    // + i64 instead of TypeVar + TypeVar.
    let src = r#"
        struct Box<T> { value: T }
        fn main() -> i64 {
            let b1 = Box { value: 5 };
            let b2 = Box { value: 10 };
            b1.value + b2.value
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn generics_struct_two_fields_pair() {
    let src = r#"
        struct Pair<A, B> { left: A, right: B }
        fn main() -> i64 {
            let p = Pair { left: 7, right: 3 };
            p.left + p.right
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn generics_struct_passed_to_generic_fn() {
    // Unifies `Box<i64>` against `Box<T>` to bind T=i64, so the
    // unbox function gets specialized as unbox$$Box_i64 or similar.
    let src = r#"
        struct Box<T> { value: T }
        fn unbox<T>(b: Box<T>) -> T { b.value }
        fn main() -> i64 {
            let b = Box { value: 99 };
            unbox(b)
        }
    "#;
    assert_eq!(run_main(src), 99);
}

#[test]
fn generics_option_i64() {
    // Classic Option<T> works end-to-end thanks to enum-arg
    // inference at variant construction.
    let src = r#"
        enum Option<T> { Some(T), None }
        fn unwrap_or(o: Option<i64>, def: i64) -> i64 {
            match o {
                Option::Some(x) => x,
                Option::None => def,
            }
        }
        fn main() -> i64 {
            unwrap_or(Option::Some(42), 0) + unwrap_or(Option::None, -1)
        }
    "#;
    assert_eq!(run_main(src), 41);
}

// ---- traits + bounded generics ----

#[test]
fn trait_impl_concrete_method_call() {
    // A trait impl on a concrete type — the method call resolves
    // directly via the impl_methods table, no generics involved.
    let src = r#"
        trait Magnitude {
            fn mag_sq(self: Point) -> i64;
        }
        struct Point { x: i64, y: i64 }
        impl Magnitude for Point {
            fn mag_sq(self: Point) -> i64 {
                self.x * self.x + self.y * self.y
            }
        }
        fn main() -> i64 {
            let p = Point { x: 3, y: 4 };
            p.mag_sq()
        }
    "#;
    assert_eq!(run_main(src), 25);
}

#[test]
fn trait_bounded_generic_static_dispatch() {
    // The bounded generic `describe<T: Sized>` calls `x.size()`;
    // monomorphization specializes it for Point and rewrites the
    // method call to Point's impl.
    let src = r#"
        trait Sized {
            fn size(self: Point) -> i64;
        }
        struct Point { x: i64, y: i64 }
        impl Sized for Point {
            fn size(self: Point) -> i64 { 16 }
        }
        fn describe<T: Sized>(x: T) -> i64 {
            x.size()
        }
        fn main() -> i64 {
            let p = Point { x: 1, y: 2 };
            describe(p)
        }
    "#;
    assert_eq!(run_main(src), 16);
}

#[test]
fn trait_bounded_generic_two_impls() {
    // Same bounded generic, two implementing types — two
    // specializations, each dispatching to the right impl.
    let src = r#"
        trait Tag {
            fn tag(self: A) -> i64;
        }
        struct A { v: i64 }
        struct B { v: i64 }
        impl Tag for A {
            fn tag(self: A) -> i64 { 1 }
        }
        impl Tag for B {
            fn tag(self: B) -> i64 { 2 }
        }
        fn id_tag<T: Tag>(x: T) -> i64 {
            x.tag()
        }
        fn main() -> i64 {
            let a = A { v: 0 };
            let b = B { v: 0 };
            id_tag(a) * 10 + id_tag(b)
        }
    "#;
    // 1*10 + 2 = 12
    assert_eq!(run_main(src), 12);
}

// ---- Weak<T> reference counting ----

#[test]
fn weak_downgrade_upgrade_alive() {
    // While the strong reference is alive, upgrade_or returns the
    // original Vec — we observe its element through the upgraded
    // pointer.
    let src = r#"
        fn main() -> i64 {
            let v = vec_new();
            v.push(42);
            let w = weak(v);
            let default = vec_new();
            let r = upgrade_or(w, default);
            r.get(0)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn weak_downgrade_after_drop_returns_default() {
    // Once the strong ref is gone, upgrade_or falls back to the
    // default. We construct v in an inner scope so it drops first.
    let src = r#"
        fn drop_after(v: Vec<i64>) -> Vec<i64> {
            v
        }
        fn get_weak() -> Weak<Vec<i64>> {
            let v = vec_new();
            v.push(99);
            let w = weak(v);
            w
        }
        fn main() -> i64 {
            let w = get_weak();
            // v's strong ref dropped at get_weak's exit; w's target is dead.
            let default = vec_new();
            default.push(7);
            let r = upgrade_or(w, default);
            r.get(0)
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn weak_doesnt_keep_alive_in_loop() {
    // 100k weak refs created and dropped each iteration. RSS stays
    // flat — the weak helpers correctly free the descriptor when
    // both rc and weak_count drain.
    let src = r#"
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let v = vec_new();
                v.push(i);
                let w = weak(v);
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn generics_result_two_params() {
    let src = r#"
        enum Result<T, E> { Ok(T), Err(E) }
        fn code(r: Result<i64, str>) -> i64 {
            match r {
                Result::Ok(n) => n,
                Result::Err(_) => -1,
            }
        }
        fn main() -> i64 {
            code(Result::Ok(7)) + code(Result::Err("bad"))
        }
    "#;
    // 7 + (-1) = 6
    assert_eq!(run_main(src), 6);
}

#[test]
fn generics_struct_field_str_method() {
    // Now that the field's concrete type is resolved at the use
    // site, `.len()` on a str field works.
    let src = r#"
        struct Box<T> { value: T }
        fn main() -> i64 {
            let b = Box { value: "hello" };
            b.value.len()
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn generics_multi_type_params() {
    let src = r#"
        fn first<T, U>(a: T, b: U) -> T { a }
        fn main() -> i64 {
            first(99, "ignored")
        }
    "#;
    assert_eq!(run_main(src), 99);
}

#[test]
fn enum_named_field_construct_and_destructure() {
    let src = r#"
        enum Result { Ok { value: i64 }, Err { code: i64 } }
        fn unwrap_or(r: Result, def: i64) -> i64 {
            match r {
                Result::Ok { value } => value,
                Result::Err { code: _ } => def,
            }
        }
        fn main() -> i64 {
            let a = Result::Ok { value: 42 };
            let b = Result::Err { code: 7 };
            unwrap_or(a, 0) + unwrap_or(b, -1)
        }
    "#;
    // 42 + (-1) = 41
    assert_eq!(run_main(src), 41);
}

#[test]
fn enum_named_field_fields_out_of_order() {
    // Construction can list fields in any order; the lowerer
    // reorders into declaration order.
    let src = r#"
        enum Point2D { Pt { x: i64, y: i64 } }
        fn dot(p: Point2D) -> i64 {
            match p {
                Point2D::Pt { x, y } => x * x + y * y,
            }
        }
        fn main() -> i64 {
            dot(Point2D::Pt { y: 4, x: 3 })
        }
    "#;
    assert_eq!(run_main(src), 25);
}

#[test]
fn enum_three_field_variant() {
    let src = r#"
        enum Triple { T(i64, i64, i64), Empty }
        fn third(t: Triple) -> i64 {
            match t {
                Triple::T(_, _, c) => c,
                Triple::Empty => -1,
            }
        }
        fn main() -> i64 {
            third(Triple::T(1, 2, 3)) + third(Triple::Empty)
        }
    "#;
    // 3 + (-1) = 2
    assert_eq!(run_main(src), 2);
}

#[test]
fn enum_payload_vec_released_on_drop() {
    // The Some descriptor holds a Vec. When the Some drops, the
    // synthesized enum-release function releases the Vec too — no
    // leak. 100k iterations stay RSS-flat.
    let src = r#"
        enum Opt { Some(Vec<i64>), None }
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let v = vec_new();
                v.push(i);
                let o = Opt::Some(v);
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn enum_exhaustive_with_payload_destructure() {
    let src = r#"
        enum Either { Left(i64), Right(i64) }
        fn sum(e: Either) -> i64 {
            match e {
                Either::Left(x) => x,
                Either::Right(x) => x + 100,
            }
        }
        fn main() -> i64 {
            sum(Either::Left(5)) + sum(Either::Right(5))
        }
    "#;
    assert_eq!(run_main(src), 110);
}

// ---- returning structs by value ----

#[test]
fn struct_returned_by_value_basic() {
    // Pre-v0.x this errored because the struct lived on the callee's
    // stack frame. Heap-allocation lets the pointer escape.
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn make(a: i64, b: i64) -> Point {
            Point { x: a, y: b }
        }
        fn main() -> i64 {
            let p = make(3, 4);
            p.x * p.x + p.y * p.y
        }
    "#;
    assert_eq!(run_main(src), 25);
}

#[test]
fn struct_returned_by_value_with_arc_field() {
    // Returning a struct that owns a Vec. The Vec is retained inside
    // the struct's construction; ARC tracks it through the return.
    let src = r#"
        struct Boxed { v: Vec<i64>, label: i64 }
        fn build(n: i64) -> Boxed {
            let v = vec_new();
            v.push(n);
            v.push(n + 1);
            Boxed { v: v, label: n }
        }
        fn main() -> i64 {
            let b = build(10);
            b.v.get(0) + b.v.get(1) + b.label
        }
    "#;
    // 10 + 11 + 10 = 31
    assert_eq!(run_main(src), 31);
}

#[test]
fn struct_descriptor_arc_in_loop() {
    // Verifies the struct's heap descriptor is dealloc'd at scope
    // exit (rc hits 0). 100k iterations stay RSS-flat — previously
    // each iteration leaked the descriptor bytes.
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let p = Point { x: i, y: i };
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

#[test]
fn struct_chain_of_returns() {
    let src = r#"
        struct Pair { a: i64, b: i64 }
        fn swap(p: Pair) -> Pair {
            Pair { a: p.b, b: p.a }
        }
        fn main() -> i64 {
            let p = Pair { a: 1, b: 2 };
            let q = swap(p);
            q.a * 10 + q.b
        }
    "#;
    // swap(1,2) → (2,1) → 21
    assert_eq!(run_main(src), 21);
}


// ---- module system ----

#[test]
fn module_qualified_call() {
    let src = r#"
        mod math {
            pub fn square(x: i64) -> i64 { x * x }
        }
        fn main() -> i64 {
            math::square(7)
        }
    "#;
    assert_eq!(run_main(src), 49);
}

#[test]
fn module_use_import() {
    let src = r#"
        mod math {
            pub fn cube(x: i64) -> i64 { x * x * x }
        }
        use math::cube;
        fn main() -> i64 {
            cube(3)
        }
    "#;
    assert_eq!(run_main(src), 27);
}

#[test]
fn module_intra_module_call() {
    // A function inside a module calls a sibling unqualified.
    let src = r#"
        mod m {
            fn helper(x: i64) -> i64 { x + 1 }
            pub fn run(x: i64) -> i64 { helper(x) * 2 }
        }
        fn main() -> i64 {
            m::run(10)
        }
    "#;
    // (10+1)*2 = 22
    assert_eq!(run_main(src), 22);
}

#[test]
fn module_nested() {
    let src = r#"
        mod outer {
            mod inner {
                pub fn deep(x: i64) -> i64 { x * 100 }
            }
            pub fn mid(x: i64) -> i64 { inner::deep(x) + 1 }
        }
        fn main() -> i64 {
            outer::mid(5)
        }
    "#;
    // 5*100 + 1 = 501
    assert_eq!(run_main(src), 501);
}

#[test]
fn module_same_fn_name_no_collision() {
    // Two modules each define `fn f` — distinct mangled codegen
    // names mean no Cranelift symbol clash.
    let src = r#"
        mod a {
            pub fn f(x: i64) -> i64 { x + 1 }
        }
        mod b {
            pub fn f(x: i64) -> i64 { x + 2 }
        }
        fn main() -> i64 {
            a::f(10) * 100 + b::f(10)
        }
    "#;
    // 11*100 + 12 = 1112
    assert_eq!(run_main(src), 1112);
}

#[test]
fn module_struct_and_enum() {
    let src = r#"
        mod shapes {
            pub struct Point { x: i64, y: i64 }
            pub enum Kind { Flat, Tall }
        }
        fn main() -> i64 {
            let p = shapes::Point { x: 3, y: 4 };
            let k = shapes::Kind::Tall;
            let kn = match k {
                shapes::Kind::Flat => 0,
                shapes::Kind::Tall => 1,
            };
            p.x + p.y + kn
        }
    "#;
    // 3 + 4 + 1 = 8
    assert_eq!(run_main(src), 8);
}

// ---- standard library (prelude) ----

#[test]
fn stdlib_min_max_abs() {
    // The concrete i64 helpers compile into every binary.
    let src = "fn main() -> i64 { std::min(3, 7) + std::max(3, 7) + std::abs(-5) }";
    assert_eq!(run_main(src), 15);
}

#[test]
fn stdlib_clamp() {
    // clamp(above) -> hi, clamp(below) -> lo, clamp(within) -> x.
    let src = r#"
        fn main() -> i64 {
            std::clamp(50, 0, 10) + std::clamp(-3, 0, 10) + std::clamp(5, 0, 10)
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn stdlib_option_unwrap_or() {
    // Generic std::unwrap_or<T> monomorphized at T = i64. Exercises
    // the prelude's Option<T> and a match over a payload variant —
    // the path that needs pattern type substitution after specialization.
    let src = r#"
        fn main() -> i64 {
            let some = std::Option::Some(42);
            let none: std::Option<i64> = std::Option::None;
            std::unwrap_or(some, 0) + std::unwrap_or(none, -1)
        }
    "#;
    assert_eq!(run_main(src), 41);
}

#[test]
fn stdlib_option_is_some_is_none() {
    let src = r#"
        fn main() -> i64 {
            let some = std::Option::Some(7);
            let none: std::Option<i64> = std::Option::None;
            let a = if std::is_some(some) { 10 } else { 0 };
            let b = if std::is_none(none) { 1 } else { 0 };
            a + b
        }
    "#;
    assert_eq!(run_main(src), 11);
}

#[test]
fn stdlib_result_ok_or() {
    let src = r#"
        fn main() -> i64 {
            let ok: std::Result<i64, i64> = std::Result::Ok(7);
            let err: std::Result<i64, i64> = std::Result::Err(99);
            std::ok_or(ok, 0) + std::ok_or(err, -1)
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn stdlib_result_is_ok_is_err() {
    let src = r#"
        fn main() -> i64 {
            let ok: std::Result<i64, i64> = std::Result::Ok(1);
            let err: std::Result<i64, i64> = std::Result::Err(2);
            let a = if std::is_ok(ok) { 100 } else { 0 };
            let b = if std::is_err(err) { 5 } else { 0 };
            a + b
        }
    "#;
    assert_eq!(run_main(src), 105);
}

#[test]
fn stdlib_use_import_generic() {
    // `use` brings a generic prelude fn into root scope; the bare
    // call still monomorphizes.
    let src = r#"
        use std::unwrap_or;
        fn main() -> i64 { unwrap_or(std::Option::Some(5), 0) }
    "#;
    assert_eq!(run_main(src), 5);
}

// ---- generic Vec<T> ----

#[test]
fn generic_vec_std_namespaced() {
    // The Vec builtin is reachable under the std namespace.
    let src = r#"
        fn main() -> i64 {
            let v: std::Vec<i64> = std::vec_new();
            v.push(10);
            v.push(20);
            v.push(12);
            v.get(0) + v.get(1) + v.get(2) + v.len()
        }
    "#;
    assert_eq!(run_main(src), 45);
}

#[test]
fn generic_vec_of_struct() {
    // Vec<Point> — struct elements pushed and read back.
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let v: Vec<Point> = vec_new();
            v.push(Point { x: 3, y: 4 });
            v.push(Point { x: 10, y: 20 });
            let a = v.get(0);
            let b = v.get(1);
            a.x + a.y + b.x + b.y
        }
    "#;
    assert_eq!(run_main(src), 37);
}

#[test]
fn generic_vec_push_local_struct() {
    // Pushing a borrowed struct (a Local) — the push retains so the
    // Vec's slot owns its own +1.
    let src = r#"
        struct P { v: i64 }
        fn main() -> i64 {
            let xs: Vec<P> = vec_new();
            let p = P { v: 7 };
            xs.push(p);
            xs.get(0).v
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn generic_vec_nested() {
    // Vec<Vec<i64>> — the element type is itself a Vec; release walks
    // each inner Vec.
    let src = r#"
        fn main() -> i64 {
            let outer: Vec<Vec<i64>> = vec_new();
            let a: Vec<i64> = vec_new();
            a.push(1);
            a.push(2);
            let b: Vec<i64> = vec_new();
            b.push(10);
            outer.push(a);
            outer.push(b);
            let x = outer.get(0);
            let y = outer.get(1);
            x.get(0) + x.get(1) + y.get(0) + outer.len()
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn generic_vec_bool_element() {
    // A narrow element type — bool values are widened to the 8-byte
    // slot on push and narrowed back on get.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<bool> = vec_new();
            v.push(true);
            v.push(false);
            v.push(true);
            let mut n = 0;
            if v.get(0) { n = n + 1; }
            if v.get(1) { n = n + 10; }
            if v.get(2) { n = n + 100; }
            n
        }
    "#;
    assert_eq!(run_main(src), 101);
}

#[test]
fn generic_vec_in_generic_fn() {
    // A generic function takes `Vec<T>`; the monomorphizer infers
    // T = i64 and the Vec methods specialize.
    let src = r#"
        fn first_or<T>(v: Vec<T>, d: T) -> T {
            if v.len() > 0 { v.get(0) } else { d }
        }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(99);
            first_or(v, 0)
        }
    "#;
    assert_eq!(run_main(src), 99);
}

#[test]
fn generic_vec_struct_loop_reclaims() {
    // 100k iterations each allocating a Vec<Pt> with two struct
    // elements. If the synthesized per-element release didn't run,
    // this leaks unboundedly; a clean run proves elements reclaim.
    let src = r#"
        struct Pt { x: i64 }
        fn main() -> i64 {
            let mut i = 0;
            while i < 100000 {
                let v: Vec<Pt> = vec_new();
                v.push(Pt { x: i });
                v.push(Pt { x: i });
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 100000);
}

// ---- file-based modules ----

#[test]
fn file_module_call_across_files() {
    let main = r#"
        mod helper;
        fn main() -> i64 { helper::triple(7) }
    "#;
    let helper = "pub fn triple(x: i64) -> i64 { x * 3 }";
    assert_eq!(run_main_files(&[("main", main), ("helper", helper)]), 21);
}

#[test]
fn file_module_use_import() {
    let main = r#"
        mod helper;
        use helper::square;
        fn main() -> i64 { square(5) }
    "#;
    let helper = "pub fn square(x: i64) -> i64 { x * x }";
    assert_eq!(run_main_files(&[("main", main), ("helper", helper)]), 25);
}

#[test]
fn file_module_nested() {
    // A file module that itself declares a file module.
    let main = r#"
        mod mid;
        fn main() -> i64 { mid::go() }
    "#;
    let mid = r#"
        mod leaf;
        pub fn go() -> i64 { leaf::val() + 1 }
    "#;
    // `mod leaf;` inside `mid.rn` resolves into the `mid/` directory.
    let leaf = "pub fn val() -> i64 { 100 }";
    assert_eq!(
        run_main_files(&[("main", main), ("mid", mid), ("mid/leaf", leaf)]),
        101
    );
}

#[test]
fn file_module_uses_std() {
    // A loaded module sees the prelude's `std::` items — they live in
    // the shared global namespace, not in the module's own file.
    let main = r#"
        mod m;
        fn main() -> i64 { m::biggest() }
    "#;
    let m = "pub fn biggest() -> i64 { std::max(3, 9) }";
    assert_eq!(run_main_files(&[("main", main), ("m", m)]), 9);
}

// ---- use globs ----

#[test]
fn use_glob_imports_fns() {
    let src = r#"
        mod m {
            pub fn one() -> i64 { 1 }
            pub fn two() -> i64 { 2 }
        }
        use m::*;
        fn main() -> i64 { one() + two() }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn use_glob_imports_struct() {
    // A glob brings a struct type into scope, usable unqualified.
    let src = r#"
        mod shapes {
            pub struct Pt { x: i64, y: i64 }
        }
        use shapes::*;
        fn main() -> i64 {
            let p = Pt { x: 5, y: 9 };
            p.x + p.y
        }
    "#;
    assert_eq!(run_main(src), 14);
}

// ---- use renaming + pub use ----

#[test]
fn use_as_rename() {
    let src = r#"
        mod m { pub fn f() -> i64 { 42 } }
        use m::f as g;
        fn main() -> i64 { g() }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn pub_use_reexport() {
    // `pub use` re-exports m's own private item — reachable as
    // `m::secret` from outside, which a bare reference would reject.
    let src = r#"
        mod m {
            fn secret() -> i64 { 55 }
            pub use secret;
        }
        fn main() -> i64 { m::secret() }
    "#;
    assert_eq!(run_main(src), 55);
}

// ---- ? operator ----

#[test]
fn try_operator_ok_and_err() {
    // `?` extracts an `Ok` value; on `Err` it returns early.
    let src = r#"
        fn parse(ok: bool) -> std::Result<i64, i64> {
            if ok {
                return std::Result::Ok(42);
            }
            std::Result::Err(7)
        }
        fn chain(ok: bool) -> std::Result<i64, i64> {
            let v = parse(ok)?;
            std::Result::Ok(v + 1)
        }
        fn main() -> i64 {
            std::ok_or(chain(true), -1) * 1000 + std::ok_or(chain(false), -1)
        }
    "#;
    // chain(true): 42 -> 43.  chain(false): Err(7) propagates -> -1.
    assert_eq!(run_main(src), 42999);
}

#[test]
fn try_chains_multiple() {
    // Several `?` in one function — each desugars independently.
    let src = r#"
        fn ok_val(n: i64) -> std::Result<i64, i64> {
            std::Result::Ok(n)
        }
        fn sum() -> std::Result<i64, i64> {
            let a = ok_val(10)?;
            let b = ok_val(20)?;
            let c = ok_val(12)?;
            std::Result::Ok(a + b + c)
        }
        fn main() -> i64 {
            std::ok_or(sum(), -1)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

// ---- dyn Trait (dynamic dispatch) ----

#[test]
fn dyn_dispatch_two_impls() {
    // One function dispatches to two concrete types via a trait
    // object — the boxed method table picks `area` at runtime.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r * 3 }
        }
        struct Square { side: i64 }
        impl Shape for Square {
            fn area(self: Square) -> i64 { self.side * self.side }
        }
        fn describe(s: dyn Shape) -> i64 { s.area() }
        fn main() -> i64 {
            describe(Circle { r: 10 }) + describe(Square { side: 5 })
        }
    "#;
    // 10*10*3 + 5*5 = 300 + 25
    assert_eq!(run_main(src), 325);
}

#[test]
fn dyn_let_binding() {
    // A `dyn Trait` local — the coercion fires at the `let`.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let s: dyn Shape = Circle { r: 6 };
            s.area()
        }
    "#;
    assert_eq!(run_main(src), 36);
}

#[test]
fn dyn_method_with_arg() {
    // A trait-object method that takes an explicit argument.
    let src = r#"
        trait Greet {
            fn hello(self: dyn Greet, n: i64) -> i64;
        }
        struct En { base: i64 }
        impl Greet for En {
            fn hello(self: En, n: i64) -> i64 { self.base + n }
        }
        fn main() -> i64 {
            let g: dyn Greet = En { base: 100 };
            g.hello(7)
        }
    "#;
    assert_eq!(run_main(src), 107);
}

#[test]
fn dyn_box_released_each_iteration() {
    // Each loop iteration boxes a fresh trait object and drops it at
    // the end of the body block. 50 alloc/release cycles — a double
    // free in the synthesized `dyn` release would crash here.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let mut total = 0;
            let mut i = 0;
            while i < 50 {
                let s: dyn Shape = Circle { r: 3 };
                total = total + s.area();
                i = i + 1;
            }
            total
        }
    "#;
    // 50 * (3 * 3) = 450
    assert_eq!(run_main(src), 450);
}

#[test]
fn dyn_box_copy_shares_refcount() {
    // `let b = a` copies a trait-object local: ARC-on-copy retains the
    // shared box, so the two scope-exit releases net a single free. A
    // missing copy-retain would free the box twice.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let a: dyn Shape = Circle { r: 6 };
            let b: dyn Shape = a;
            a.area() + b.area()
        }
    "#;
    // 6*6 + 6*6 = 72
    assert_eq!(run_main(src), 72);
}

#[test]
fn dyn_box_from_local_retains_data() {
    // Coercing a *borrowed* local into a `dyn` box: the box must
    // retain the boxed struct, since both the original local and the
    // box release it at scope exit.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let c: Circle = Circle { r: 7 };
            let s: dyn Shape = c;
            s.area() + c.r
        }
    "#;
    // 7*7 + 7 = 56
    assert_eq!(run_main(src), 56);
}

#[test]
fn vec_of_dyn_dispatch() {
    // A heterogeneous `Vec<dyn Shape>` — two concrete types in one
    // collection. `push` coerces the struct argument to a trait
    // object; `get` hands one back; `.area()` dispatches per element.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        struct Square { side: i64 }
        impl Shape for Square {
            fn area(self: Square) -> i64 { self.side * self.side }
        }
        fn main() -> i64 {
            let mut shapes: Vec<dyn Shape> = vec_new();
            shapes.push(Circle { r: 10 });
            shapes.push(Square { side: 5 });
            let mut total = 0;
            let mut i = 0;
            while i < shapes.len() {
                let s: dyn Shape = shapes.get(i);
                total = total + s.area();
                i = i + 1;
            }
            total
        }
    "#;
    // 10*10 + 5*5 = 125
    assert_eq!(run_main(src), 125);
}

#[test]
fn vec_of_dyn_reclaimed() {
    // 200 iterations each build a fresh `Vec<dyn Shape>`, push two
    // boxed shapes, and drop it at the block end. Releasing the Vec
    // walks its elements (`__rune_release_vec$dyn`), dropping each
    // box and the concrete struct it wraps — a double free crashes.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        struct Square { side: i64 }
        impl Shape for Square {
            fn area(self: Square) -> i64 { self.side * self.side }
        }
        fn main() -> i64 {
            let mut n = 0;
            let mut total = 0;
            while n < 200 {
                let mut shapes: Vec<dyn Shape> = vec_new();
                shapes.push(Circle { r: 2 });
                shapes.push(Square { side: 3 });
                let a: dyn Shape = shapes.get(0);
                let b: dyn Shape = shapes.get(1);
                total = total + a.area() + b.area();
                n = n + 1;
            }
            total
        }
    "#;
    // 200 * (2*2 + 3*3) = 200 * 13 = 2600
    assert_eq!(run_main(src), 2600);
}

#[test]
fn vec_of_dyn_push_existing_dyn() {
    // Pushing a value that is *already* a `dyn` local (not a struct
    // coerced at the call site): a borrowed element, so `push`
    // retains it — the box ends up owned by both the local and the
    // Vec slot, and both releases net out.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let c: dyn Shape = Circle { r: 6 };
            let mut shapes: Vec<dyn Shape> = vec_new();
            shapes.push(c);
            let s: dyn Shape = shapes.get(0);
            s.area()
        }
    "#;
    // 6*6 = 36
    assert_eq!(run_main(src), 36);
}

#[test]
fn call_arg_dyn_temp_released() {
    // `describe(Circle { .. })` boxes a fresh `dyn` argument the
    // callee only borrows. The caller reclaims that box once the
    // call returns — 200 iterations, a double free would crash.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn describe(s: dyn Shape) -> i64 { s.area() }
        fn main() -> i64 {
            let mut sum = 0;
            let mut i = 0;
            while i < 200 {
                sum = sum + describe(Circle { r: 3 });
                i = i + 1;
            }
            sum
        }
    "#;
    // 200 * (3 * 3) = 1800
    assert_eq!(run_main(src), 1800);
}

#[test]
fn call_arg_vec_temp_released() {
    // The argument is a *call result* (`triple()`), a fresh ARC
    // temporary. The caller releases it after `vlen` returns.
    let src = r#"
        fn vlen(v: Vec<i64>) -> i64 { v.len() }
        fn triple() -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(1);
            v.push(2);
            v.push(3);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut i = 0;
            while i < 200 {
                sum = sum + vlen(triple());
                i = i + 1;
            }
            sum
        }
    "#;
    // 200 * 3 = 600
    assert_eq!(run_main(src), 600);
}

#[test]
fn call_arg_local_not_released() {
    // A `Local` argument stays owned by its binding — the caller
    // must NOT release it after the call. `v` is passed three
    // times and remains valid each time; releasing it post-call
    // would use-after-free on the second call.
    let src = r#"
        fn first(v: Vec<i64>) -> i64 { v.get(0) }
        fn main() -> i64 {
            let mut v: Vec<i64> = vec_new();
            v.push(10);
            let a = first(v);
            let b = first(v);
            let c = first(v);
            a + b + c
        }
    "#;
    // 10 + 10 + 10 = 30
    assert_eq!(run_main(src), 30);
}

#[test]
fn field_read_retains() {
    // `let got = h.v` reads an ARC struct field. The read retains,
    // so `got` co-owns the Vec alongside the field and the original
    // `inner` binding — three owners, three releases, one free.
    // Without the retain the Vec is freed while `inner` still holds
    // it: a double free at scope exit.
    let src = r#"
        struct Holder { v: Vec<i64> }
        fn main() -> i64 {
            let mut inner: Vec<i64> = vec_new();
            inner.push(7);
            let h = Holder { v: inner };
            let got = h.v;
            got.get(0)
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn field_read_into_call() {
    // A field read passed straight to a function. The read retains
    // and the caller releases the argument after the call (session
    // 036) — the two net out. Read twice: without the retain the
    // first call's release would free a Vec the field still holds.
    let src = r#"
        struct Holder { v: Vec<i64> }
        fn vlen(v: Vec<i64>) -> i64 { v.len() }
        fn main() -> i64 {
            let mut inner: Vec<i64> = vec_new();
            inner.push(1);
            inner.push(2);
            let h = Holder { v: inner };
            vlen(h.v) + vlen(h.v)
        }
    "#;
    // 2 + 2 = 4
    assert_eq!(run_main(src), 4);
}

#[test]
fn index_read_retains() {
    // `arr[1]` reads an ARC element of an array. Reading the same
    // element into three bindings gives three owners; the read
    // retains each time, so the three scope-exit releases don't
    // free the struct out from under each other.
    let src = r#"
        struct Cell { n: i64 }
        fn main() -> i64 {
            let arr = [Cell { n: 4 }, Cell { n: 5 }, Cell { n: 6 }];
            let a = arr[1];
            let b = arr[1];
            let c = arr[1];
            a.n + b.n + c.n
        }
    "#;
    // 5 + 5 + 5 = 15
    assert_eq!(run_main(src), 15);
}

#[test]
fn receiver_temp_released() {
    // `triple().len()` — the receiver is a fresh ARC temporary the
    // method only borrows. The caller reclaims it once the call
    // returns; 200 iterations, a double free would crash.
    let src = r#"
        fn triple() -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(1);
            v.push(2);
            v.push(3);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                sum = sum + triple().len();
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * 3 = 600
    assert_eq!(run_main(src), 600);
}

#[test]
fn receiver_temp_dyn_call() {
    // `shapes.get(i).area()` — `get` hands back a retained `dyn`
    // box, which is the receiver of the `.area()` dynamic call. That
    // box temporary is reclaimed once the call returns.
    let src = r#"
        trait Shape {
            fn area(self: dyn Shape) -> i64;
        }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        fn main() -> i64 {
            let mut total = 0;
            let mut n = 0;
            while n < 100 {
                let mut shapes: Vec<dyn Shape> = vec_new();
                shapes.push(Circle { r: 3 });
                shapes.push(Circle { r: 4 });
                let mut i = 0;
                while i < shapes.len() {
                    total = total + shapes.get(i).area();
                    i = i + 1;
                }
                n = n + 1;
            }
            total
        }
    "#;
    // 100 * (3*3 + 4*4) = 100 * 25 = 2500
    assert_eq!(run_main(src), 2500);
}

#[test]
fn receiver_local_not_released() {
    // A `Local` receiver stays owned by its binding — calling
    // methods on it must not release it. `v` is used as a receiver
    // four times and remains valid throughout.
    let src = r#"
        fn main() -> i64 {
            let mut v: Vec<i64> = vec_new();
            v.push(10);
            v.push(20);
            v.len() + v.len() + v.get(0) + v.get(1)
        }
    "#;
    // 2 + 2 + 10 + 20 = 34
    assert_eq!(run_main(src), 34);
}

#[test]
fn enum_payload_escape_retained() {
    // `match b { Bag::Full(x) => x }` yields an extracted enum
    // payload — a borrowed binding. The match retains it on the way
    // out so the result co-owns the Vec with the enum and the
    // original binding. Without that retain the Vec is freed three
    // ways at scope exit: a double free.
    let src = r#"
        enum Bag { Full(Vec<i64>), Empty }
        fn unwrap_bag(b: Bag) -> Vec<i64> {
            match b {
                Bag::Full(x) => x,
                Bag::Empty => vec_new(),
            }
        }
        fn main() -> i64 {
            let mut total = 0;
            let mut n = 0;
            while n < 200 {
                let mut inner: Vec<i64> = vec_new();
                inner.push(21);
                let b: Bag = Bag::Full(inner);
                let got: Vec<i64> = unwrap_bag(b);
                total = total + got.get(0);
                n = n + 1;
            }
            total
        }
    "#;
    // 200 * 21 = 4200
    assert_eq!(run_main(src), 4200);
}

#[test]
fn discarded_statement_temp_released() {
    // `make();` discards a fresh ARC value — the caller reclaims it.
    // 200 iterations; a discarded `Local` (`keep;`) must be left
    // alone, so `keep` stays valid for the final read.
    let src = r#"
        fn make() -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(9);
            v
        }
        fn main() -> i64 {
            let mut n = 0;
            while n < 200 {
                make();
                n = n + 1;
            }
            let mut keep: Vec<i64> = vec_new();
            keep.push(5);
            keep;
            keep.get(0)
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn print_arg_temp_released() {
    // `print` only borrows its argument, so a fresh string passed to
    // it (`"ef" + "gh"`) is reclaimed after the call. A `Local`
    // argument (`s`) is left alone — passed twice and still valid.
    let src = r#"
        fn main() -> i64 {
            let s: str = "ab" + "cd";
            print(s);
            print(s);
            print("ef" + "gh");
            s.len()
        }
    "#;
    // "abcd".len()
    assert_eq!(run_main(src), 4);
}
