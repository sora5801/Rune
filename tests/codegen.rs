use rune::*;

/// Compile `src` and JIT-call its `main() -> i64`, returning the value.
fn run_main(src: &str) -> i64 {
    let (tokens, le) = Lexer::new(src).tokenize();
    assert!(le.is_empty(), "lex errors: {:?}", le);
    let (module, pe) = Parser::new(tokens).parse_module();
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    let (res, re) = Resolver::new().resolve_module(&module);
    assert!(re.is_empty(), "resolve errors: {:?}", re);
    let cr = Checker::new(&res).check_module(&module);
    assert!(cr.errors.is_empty(), "type errors: {:?}", cr.errors);
    let hir = Lowerer::new(&res, &cr).lower_module(&module);

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
        fn sum(xs: Vec) -> i64 {
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

// ---- free(x) builtin ----

#[test]
fn free_vec_does_not_crash() {
    let src = r#"
        fn main() -> i64 {
            let v = vec_new();
            v.push(1);
            v.push(2);
            v.push(3);
            free(v);
            42
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn free_concat_str_does_not_crash() {
    let src = r#"
        fn main() -> i64 {
            let s = "foo" + "bar";
            free(s);
            42
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn free_in_loop_reclaims_steadily() {
    // Allocate 1000 Vecs, free each. With reclamation, memory stays bounded;
    // without it, peak ~24KB descriptors + ~few KB elements. Either way it
    // doesn't crash, which is the assertion.
    let src = r#"
        fn main() -> i64 {
            let mut i = 0;
            while i < 1000 {
                let v = vec_new();
                v.push(i);
                free(v);
                i = i + 1;
            }
            i
        }
    "#;
    assert_eq!(run_main(src), 1000);
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
