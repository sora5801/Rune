use rune::*;

fn parse(src: &str) -> (Module, Vec<LexError>, Vec<ParseError>) {
    let (tokens, lex_errors) = Lexer::new(src).tokenize();
    let (module, parse_errors) = Parser::new(tokens).parse_module();
    (module, lex_errors, parse_errors)
}

fn parse_ok(src: &str) -> Module {
    let (m, le, pe) = parse(src);
    assert!(le.is_empty(), "unexpected lex errors: {:?}", le);
    assert!(pe.is_empty(), "unexpected parse errors: {:?}", pe);
    m
}

fn parse_expr_in_block(src: &str) -> Expr {
    let wrapped = format!("fn _t() {{ {} }}", src);
    let m = parse_ok(&wrapped);
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    let stmt = f.body.stmts.into_iter().next().unwrap();
    match stmt {
        Stmt::Expr(e, _) => e,
        other => panic!("expected expression statement, got {:?}", other),
    }
}

// ---- module shape ----

#[test]
fn empty_module() {
    let m = parse_ok("");
    assert_eq!(m.items.len(), 0);
}

#[test]
fn fn_no_args_no_return() {
    let m = parse_ok("fn main() { }");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert_eq!(f.name.name, "main");
    assert!(f.params.is_empty());
    assert!(f.return_type.is_none());
    assert_eq!(f.vis, Visibility::Private);
}

#[test]
fn fn_with_args_and_return() {
    let m = parse_ok("fn add(a: i64, b: i64) -> i64 { a + b }");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name.name, "a");
    assert_eq!(f.params[1].name.name, "b");
    assert!(f.return_type.is_some());
}

#[test]
fn pub_fn() {
    let m = parse_ok("pub fn hi() { }");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert_eq!(f.vis, Visibility::Pub);
}

#[test]
fn struct_decl() {
    let m = parse_ok("struct Point { x: i64, y: i64 }");
    let Item::Struct(s) = &m.items[0] else { panic!() };
    assert_eq!(s.name.name, "Point");
    assert_eq!(s.fields.len(), 2);
    assert_eq!(s.fields[0].name.name, "x");
}

#[test]
fn enum_decl_mixed() {
    let m = parse_ok("enum E { A, B(i64), C { x: i64 } }");
    let Item::Enum(e) = &m.items[0] else { panic!() };
    assert_eq!(e.variants.len(), 3);
    assert!(matches!(e.variants[0].fields, VariantFields::Unit));
    assert!(matches!(e.variants[1].fields, VariantFields::Tuple(_)));
    assert!(matches!(e.variants[2].fields, VariantFields::Named(_)));
}

#[test]
fn const_decl() {
    let m = parse_ok("const PI: f64 = 3.14;");
    let Item::Const(c) = &m.items[0] else { panic!() };
    assert_eq!(c.name.name, "PI");
}

// ---- statements ----

#[test]
fn let_no_type_no_init() {
    parse_ok("fn t() { let x; }");
}

#[test]
fn let_with_type() {
    parse_ok("fn t() { let x: i64; }");
}

#[test]
fn let_mut_with_init() {
    let m = parse_ok("fn t() { let mut x = 0; }");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    let Stmt::Let(l) = &f.body.stmts[0] else { panic!() };
    assert!(l.mutable);
    assert!(l.init.is_some());
}

// ---- expression precedence ----

#[test]
fn arithmetic_precedence() {
    // 1 + 2 * 3  parses as  1 + (2 * 3)
    let e = parse_expr_in_block("1 + 2 * 3");
    let Expr::Binary { op: BinOp::Add, lhs, rhs, .. } = e else { panic!() };
    assert!(matches!(*lhs, Expr::Lit { lit: Lit::Int(1), .. }));
    assert!(matches!(*rhs, Expr::Binary { op: BinOp::Mul, .. }));
}

#[test]
fn left_associativity_minus() {
    // 1 - 2 - 3  parses as  (1 - 2) - 3
    let e = parse_expr_in_block("1 - 2 - 3");
    let Expr::Binary { op: BinOp::Sub, lhs, rhs, .. } = e else { panic!() };
    assert!(matches!(*lhs, Expr::Binary { op: BinOp::Sub, .. }));
    assert!(matches!(*rhs, Expr::Lit { lit: Lit::Int(3), .. }));
}

#[test]
fn right_associativity_assign() {
    // a = b = c  parses as  a = (b = c)
    let e = parse_expr_in_block("a = b = c");
    let Expr::Assign { lhs, rhs, .. } = e else { panic!() };
    assert!(matches!(*lhs, Expr::Path(_)));
    assert!(matches!(*rhs, Expr::Assign { .. }));
}

#[test]
fn comparison_below_arithmetic() {
    // 1 + 2 < 3 * 4  parses as  (1 + 2) < (3 * 4)
    let e = parse_expr_in_block("1 + 2 < 3 * 4");
    let Expr::Binary { op: BinOp::Lt, lhs, rhs, .. } = e else { panic!() };
    assert!(matches!(*lhs, Expr::Binary { op: BinOp::Add, .. }));
    assert!(matches!(*rhs, Expr::Binary { op: BinOp::Mul, .. }));
}

#[test]
fn logical_below_comparison() {
    // a < b && c > d  parses as  (a < b) && (c > d)
    let e = parse_expr_in_block("a < b && c > d");
    let Expr::Binary { op: BinOp::And, lhs, rhs, .. } = e else { panic!() };
    assert!(matches!(*lhs, Expr::Binary { op: BinOp::Lt, .. }));
    assert!(matches!(*rhs, Expr::Binary { op: BinOp::Gt, .. }));
}

#[test]
fn unary_minus_and_not() {
    let e = parse_expr_in_block("-x");
    assert!(matches!(e, Expr::Unary { op: UnOp::Neg, .. }));
    let e = parse_expr_in_block("!flag");
    assert!(matches!(e, Expr::Unary { op: UnOp::Not, .. }));
}

#[test]
fn postfix_binds_tighter_than_unary() {
    // !f(x) is !(f(x)), not (!f)(x).
    let e = parse_expr_in_block("!f(x)");
    let Expr::Unary { op: UnOp::Not, expr, .. } = e else { panic!() };
    assert!(matches!(*expr, Expr::Call { .. }));

    // -x[0] is -(x[0]), not (-x)[0].
    let e = parse_expr_in_block("-x[0]");
    let Expr::Unary { op: UnOp::Neg, expr, .. } = e else { panic!() };
    assert!(matches!(*expr, Expr::Index { .. }));

    // !a.b is !(a.b).
    let e = parse_expr_in_block("!a.b");
    let Expr::Unary { op: UnOp::Not, expr, .. } = e else { panic!() };
    assert!(matches!(*expr, Expr::Field { .. }));
}

#[test]
fn cast_expression() {
    let e = parse_expr_in_block("x as i64");
    assert!(matches!(e, Expr::Cast { .. }));
}

#[test]
fn compound_assign() {
    let e = parse_expr_in_block("x += 1");
    let Expr::AssignOp { op, .. } = e else { panic!() };
    assert_eq!(op, BinOp::Add);
}

// ---- postfix ----

#[test]
fn function_call() {
    let e = parse_expr_in_block("foo(1, 2, 3)");
    let Expr::Call { args, .. } = e else { panic!() };
    assert_eq!(args.len(), 3);
}

#[test]
fn method_call() {
    let e = parse_expr_in_block("x.method(1)");
    assert!(matches!(e, Expr::MethodCall { .. }));
}

#[test]
fn field_access() {
    let e = parse_expr_in_block("point.x");
    assert!(matches!(e, Expr::Field { .. }));
}

#[test]
fn index_access() {
    let e = parse_expr_in_block("arr[0]");
    assert!(matches!(e, Expr::Index { .. }));
}

#[test]
fn exclusive_range() {
    let e = parse_expr_in_block("1..10");
    let Expr::Range { inclusive: false, start: Some(_), end: Some(_), .. } = e else {
        panic!("expected exclusive range");
    };
}

#[test]
fn inclusive_range() {
    let e = parse_expr_in_block("1..=10");
    let Expr::Range { inclusive: true, start: Some(_), end: Some(_), .. } = e else {
        panic!("expected inclusive range");
    };
}

#[test]
fn slice_index() {
    let e = parse_expr_in_block("s[0..5]");
    let Expr::Index { index, .. } = e else { panic!() };
    assert!(matches!(*index, Expr::Range { .. }));
}

#[test]
fn try_operator() {
    let e = parse_expr_in_block("foo()?");
    assert!(matches!(e, Expr::Try { .. }));
}

#[test]
fn chained_postfix() {
    let e = parse_expr_in_block("a.b().c[0]");
    // outermost should be Index
    assert!(matches!(e, Expr::Index { .. }));
}

// ---- primary ----

#[test]
fn array_literal() {
    let e = parse_expr_in_block("[1, 2, 3, 4]");
    let Expr::Array { elems, .. } = e else { panic!() };
    assert_eq!(elems.len(), 4);
}

#[test]
fn block_expression_value() {
    let e = parse_expr_in_block("{ let x = 5; x }");
    let Expr::Block(b) = e else { panic!() };
    assert_eq!(b.stmts.len(), 2);
    let Stmt::Expr(_, has_semi) = &b.stmts[1] else { panic!() };
    assert!(!has_semi, "trailing expression should have no semicolon");
}

#[test]
fn path_with_segments() {
    let e = parse_expr_in_block("std::io::println");
    let Expr::Path(p) = e else { panic!() };
    assert_eq!(p.segments.len(), 3);
}

// ---- control flow ----

#[test]
fn if_else() {
    let e = parse_expr_in_block("if x { 1 } else { 2 }");
    let Expr::If { else_branch: Some(_), .. } = e else { panic!() };
}

#[test]
fn if_else_if_else_chain() {
    let e = parse_expr_in_block("if a { 1 } else if b { 2 } else { 3 }");
    let Expr::If { else_branch: Some(else1), .. } = e else { panic!() };
    assert!(matches!(*else1, Expr::If { .. }));
}

#[test]
fn while_loop() {
    let e = parse_expr_in_block("while x > 0 { x = x - 1 }");
    assert!(matches!(e, Expr::While { .. }));
}

#[test]
fn for_loop() {
    let e = parse_expr_in_block("for p in primes { }");
    let Expr::For { pat, .. } = e else { panic!() };
    assert!(matches!(pat, Pattern::Ident { .. }));
}

#[test]
fn match_expression() {
    let e = parse_expr_in_block("match x { 1 => a, 2 => b, _ => c, }");
    let Expr::Match { arms, .. } = e else { panic!() };
    assert_eq!(arms.len(), 3);
    assert!(matches!(arms[2].pat, Pattern::Wildcard(_)));
}

#[test]
fn return_with_and_without_value() {
    let e = parse_expr_in_block("return 42");
    assert!(matches!(e, Expr::Return { value: Some(_), .. }));
    let e = parse_expr_in_block("return");
    assert!(matches!(e, Expr::Return { value: None, .. }));
}

#[test]
fn break_continue() {
    parse_ok("fn t() { while true { break; } }");
    parse_ok("fn t() { while true { continue; } }");
}

// ---- end-to-end ----

#[test]
fn hello_rn_parses_cleanly() {
    let src = r#"
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
        }
    "#;
    let (m, le, pe) = parse(src);
    assert!(le.is_empty(), "lex errors: {:?}", le);
    assert!(pe.is_empty(), "parse errors: {:?}", pe);
    assert_eq!(m.items.len(), 2);
}

// ---- error recovery ----

#[test]
fn recovers_at_next_item() {
    // First fn body is broken; second fn should still parse
    let (m, _le, pe) = parse("fn bad() { let ; } fn good() { 42 }");
    assert!(!pe.is_empty(), "expected at least one parse error");
    // We should have at least the second item
    assert!(m.items.iter().any(|i| matches!(i, Item::Fn(f) if f.name.name == "good")));
}

#[test]
fn missing_semicolon_is_error() {
    let (_, _le, pe) = parse("fn t() { let x = 1 let y = 2; }");
    assert!(!pe.is_empty());
}

// ---- generics: step 1 (parser only) ----

#[test]
fn parses_generic_fn_decl() {
    let m = parse_ok("fn id<T>(x: T) -> T { x }");
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(f.generics.len(), 1);
    assert_eq!(f.generics[0].name.name, "T");
}

#[test]
fn parses_multi_generic_fn() {
    let m = parse_ok("fn pair<T, U>(a: T, b: U) -> T { a }");
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(f.generics.len(), 2);
}

#[test]
fn parses_generic_struct() {
    let m = parse_ok("struct Box<T> { value: T }");
    let Item::Struct(s) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(s.generics.len(), 1);
}

#[test]
fn parses_generic_enum() {
    let m = parse_ok("enum Option<T> { Some(T), None }");
    let Item::Enum(e) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(e.generics.len(), 1);
}

#[test]
fn parses_generic_type_arg_in_type_position() {
    let m = parse_ok("fn first(v: Vec<i64>) -> i64 { 0 }");
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    let Type::Path(p) = &f.params[0].ty else { panic!() };
    assert_eq!(p.generic_args.len(), 1);
}

#[test]
fn comparison_lt_still_parses() {
    // `x < 2` must still parse as comparison, not a generic-args attempt.
    parse_ok("fn t() -> bool { let x = 1; x < 2 }");
}

// ---- traits ----

#[test]
fn parses_trait_decl() {
    let m = parse_ok("trait Display { fn fmt(self: Point) -> str; }");
    let Item::Trait(t) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(t.name.name, "Display");
    assert_eq!(t.methods.len(), 1);
    assert_eq!(t.methods[0].name.name, "fmt");
}

#[test]
fn parses_trait_impl() {
    let m = parse_ok(
        "struct Point { x: i64 } impl Display for Point { fn fmt(self: Point) -> str { \"p\" } }",
    );
    let Item::Impl(i) = m.items.into_iter().nth(1).unwrap() else { panic!() };
    assert!(i.trait_path.is_some());
}

#[test]
fn parses_bounded_generic() {
    let m = parse_ok("fn show<T: Display>(x: T) -> str { \"\" }");
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(f.generics[0].bounds.len(), 1);
    assert_eq!(f.generics[0].bounds[0].name, "Display");
}

#[test]
fn parses_multi_bound_generic() {
    let m = parse_ok("fn show<T: A + B>(x: T) -> i64 { 0 }");
    let Item::Fn(f) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(f.generics[0].bounds.len(), 2);
}

// ---- modules ----

#[test]
fn parses_module() {
    let m = parse_ok("mod math { fn square(x: i64) -> i64 { x * x } }");
    let Item::Mod(md) = m.items.into_iter().next().unwrap() else { panic!() };
    assert_eq!(md.name.name, "math");
    assert_eq!(md.items.len(), 1);
}

#[test]
fn parses_nested_module() {
    let m = parse_ok("mod a { mod b { fn f() {} } }");
    let Item::Mod(md) = m.items.into_iter().next().unwrap() else { panic!() };
    assert!(matches!(md.items[0], Item::Mod(_)));
}

#[test]
fn parses_use() {
    let m = parse_ok("mod m { fn f() {} } use m::f;");
    assert!(matches!(m.items[1], Item::Use(_)));
}
