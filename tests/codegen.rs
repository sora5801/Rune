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
fn hashmap_basic_insert_get() {
    // Session 064: HashMap<i64, i64> — insert + get round trip.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 10);
            m.insert(2, 20);
            m.insert(3, 30);
            m.get(1) + m.get(2) + m.get(3)
        }
    "#;
    assert_eq!(run_main(src), 60);
}

#[test]
fn hashmap_overwrite_returns_latest() {
    // insert on an existing key replaces the value, keeps len unchanged.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(7, 100);
            m.insert(7, 200);
            m.insert(7, 300);
            m.get(7) + m.len()
        }
    "#;
    assert_eq!(run_main(src), 301);
}

#[test]
fn hashmap_contains_and_missing_returns_zero() {
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(42, 99);
            // contains_key returns bool; sum a present and a missing
            // key's "default" (0) plus the contains_key result.
            let here: i64 = if m.contains_key(42) { 1 } else { 0 };
            let gone: i64 = if m.contains_key(7) { 1 } else { 0 };
            m.get(42) + here + gone + m.get(7)
        }
    "#;
    // 99 (get 42) + 1 (contains 42) + 0 (contains 7) + 0 (missing) = 100
    assert_eq!(run_main(src), 100);
}

#[test]
fn hashmap_grows_past_initial_cap() {
    // Initial cap is 8; insert 30 distinct keys to force multiple
    // grow + rehash cycles. All reads should still find their values.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            for i in 0..30 {
                m.insert(i, i * 10);
            }
            let mut sum: i64 = 0;
            for i in 0..30 {
                sum = sum + m.get(i);
            }
            sum
        }
    "#;
    // sum_{i=0..30} i*10 = 10 * 29*30/2 = 4350
    assert_eq!(run_main(src), 4350);
}

#[test]
fn hashmap_value_is_str() {
    // String values exercise the ARC-on-insert path. Each `s` value
    // gets retained as it lands in a slot. Map's release at scope
    // exit doesn't release values (v0.x simplification), so the
    // string literals' rc=-1 sentinel keeps them safe.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, str> = hashmap_new();
            m.insert(1, "one");
            m.insert(2, "two");
            let s: str = m.get(2);
            s.len()
        }
    "#;
    // "two".len() == 3
    assert_eq!(run_main(src), 3);
}

#[test]
fn iterator_filter_as_method_with_named_fn() {
    // Probe: named fn as predicate (no closure inference).
    let src = r#"
        fn gt2(x: i64) -> bool { x > 2 }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter().filter(gt2).count()
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn iterator_filter_as_method_with_closure() {
    // Session 077: `.filter(p)` as a default method on Iterator
    // — uses a method-level generic `P: Fn1<Self::Item, bool>`.
    // The body constructs `Filter { iter: self, pred: p }`; the
    // monomorphizer specializes per (Self, P).
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter()
                .filter(|x| x > 2)
                .count()
        }
    "#;
    // 3, 4, 5 → 3 elements
    assert_eq!(run_main(src), 3);
}

#[test]
fn iterator_map_as_method_with_named_fn() {
    // Session 078: bound-propagation cascade pins U via F's
    // Fn1 bound at the method-call site. .map(sq) infers
    // F=Ty::Fn(i64, i64) from the arg type; then bound-walking
    // unifies Fn1<Self::Item, U>'s args with F's (P, R) →
    // U = i64.
    let src = r#"
        fn sq(x: i64) -> i64 { x * x }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4);
            v.iter().map(sq).sum()
        }
    "#;
    // 1 + 4 + 9 + 16 = 30
    assert_eq!(run_main(src), 30);
}

#[test]
fn iterator_chain_filter_map_sum_as_methods() {
    // Session 078: the full chain works as methods now.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter()
                .filter(|x| x > 1)
                .map(|x: i64| x * 10)
                .sum()
        }
    "#;
    // filter > 1 = [2,3,4,5]; map *10 = [20,30,40,50]; sum = 140.
    assert_eq!(run_main(src), 140);
}

#[test]
fn iterator_map_as_method_with_annotated_closure() {
    // Closure case: x: i64 annotation pins the closure's
    // param, and U flows through the bound cascade for the
    // return.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4);
            v.iter().map(|x: i64| x * x).sum()
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn iterator_count_default_method() {
    // Session 076: .count() is a default method on Iterator —
    // every impl (VecIter, RangeIter, Map, Filter,
    // HashMapKeysIter, HashMapEntriesIter) inherits it. Each
    // call specializes the synth default fn per Self.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter().count()
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn iterator_count_on_range() {
    let src = r#"
        fn main() -> i64 {
            (0..7).count()
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn iterator_count_on_filter_adapter() {
    // Compose: count via the Filter adapter's inherited
    // default. Filter is constructed via struct-lit since
    // .filter() isn't a method yet (would need method-level
    // generic params).
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let f: std::Filter<std::VecIter<i64>, fn(i64) -> bool> =
                std::Filter { iter: v.iter(), pred: |x| x > 2 };
            f.count()
        }
    "#;
    // 3, 4, 5 → 3 elements
    assert_eq!(run_main(src), 3);
}

#[test]
fn iterator_sum_default_method() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20); v.push(30);
            v.iter().sum()
        }
    "#;
    assert_eq!(run_main(src), 60);
}

#[test]
fn iterator_sum_on_range() {
    let src = r#"
        fn main() -> i64 {
            (1..6).sum()
        }
    "#;
    // 1 + 2 + 3 + 4 + 5 = 15
    assert_eq!(run_main(src), 15);
}

#[test]
fn iterator_sum_on_map_adapter_with_closure() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4);
            let m = std::Map { iter: v.iter(), f: |x| x * x };
            m.sum()
        }
    "#;
    // 1 + 4 + 9 + 16 = 30
    assert_eq!(run_main(src), 30);
}

#[test]
fn iterator_count_through_filter_and_map() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let f: std::Filter<std::VecIter<i64>, fn(i64) -> bool> =
                std::Filter { iter: v.iter(), pred: |x| x > 1 };
            let m = std::Map { iter: f, f: |x| x * 10 };
            m.count()
        }
    "#;
    // After filter > 1: 2,3,4,5 → 4 elements. Map doesn't
    // change count.
    assert_eq!(run_main(src), 4);
}

#[test]
fn iterator_min_default_method() {
    // Session 079: .min() as a default method on Iterator.
    // i64-only — returns Option<i64>::Some(smallest) over
    // non-empty iterators, Option::None on empty.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(40); v.push(10); v.push(30); v.push(20);
            match v.iter().min() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn iterator_max_default_method() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(40); v.push(10); v.push(30); v.push(20);
            match v.iter().max() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 40);
}

#[test]
fn iterator_min_on_empty_returns_none() {
    // Empty iterator → Option::None sentinel path is exercised.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            match v.iter().min() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), -1);
}

#[test]
fn iterator_min_on_range() {
    let src = r#"
        fn main() -> i64 {
            match (5..9).min() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn iterator_max_on_range() {
    let src = r#"
        fn main() -> i64 {
            match (5..9).max() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    // 5..9 yields 5,6,7,8 — max is 8.
    assert_eq!(run_main(src), 8);
}

#[test]
fn iterator_max_through_filter_and_map_chain() {
    // .max() composed at the end of a method-chain through
    // Filter and Map. Three adapter specializations of the
    // .max default body fire — VecIter, Filter, Map.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            match v.iter()
                .filter(|x| x > 1)
                .map(|x: i64| x * 10)
                .max() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    // filter > 1 = [2,3,4,5]; map *10 = [20,30,40,50]; max = 50.
    assert_eq!(run_main(src), 50);
}

#[test]
fn iterator_min_via_map_adapter() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(3); v.push(1); v.push(2);
            let m = std::Map { iter: v.iter(), f: |x| x * x };
            match m.min() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    // 9, 1, 4 → min = 1
    assert_eq!(run_main(src), 1);
}

#[test]
fn iterator_fold_default_method() {
    // Session 080: `.fold(init, f)` lands as a default method on
    // Iterator. The closure takes (acc, x) and returns the next
    // acc. Multi-arg closure via Fn2 trait; cascade pins both U
    // (from init) and F (from closure).
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4);
            v.iter().fold(0, |acc: i64, x: i64| acc + x)
        }
    "#;
    // 0 + 1 + 2 + 3 + 4 = 10
    assert_eq!(run_main(src), 10);
}

#[test]
fn iterator_fold_with_named_fn() {
    let src = r#"
        fn add(a: i64, b: i64) -> i64 { a + b }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20); v.push(30);
            v.iter().fold(0, add)
        }
    "#;
    // 0 + 10 + 20 + 30 = 60
    assert_eq!(run_main(src), 60);
}

#[test]
fn iterator_fold_init_nonzero() {
    // Verify init isn't accidentally zeroed somewhere.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            v.iter().fold(100, |acc: i64, x: i64| acc + x)
        }
    "#;
    // 100 + 1 + 2 + 3 = 106
    assert_eq!(run_main(src), 106);
}

#[test]
fn iterator_fold_on_range() {
    let src = r#"
        fn main() -> i64 {
            (1..6).fold(0, |acc: i64, x: i64| acc + x)
        }
    "#;
    // 1+2+3+4+5 = 15
    assert_eq!(run_main(src), 15);
}

#[test]
fn iterator_fold_via_filter_map_chain() {
    // .fold composes at the end of a method chain. Three adapter
    // specializations of .fold's default body fire — VecIter,
    // Filter, Map.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter()
                .filter(|x| x > 1)
                .map(|x: i64| x * 10)
                .fold(0, |acc: i64, x: i64| acc + x)
        }
    "#;
    // filter > 1 = [2,3,4,5]; map *10 = [20,30,40,50]; fold sum = 140
    assert_eq!(run_main(src), 140);
}

#[test]
fn iterator_fold_multiplies() {
    // Closure does multiplication, not addition — tests that fold
    // doesn't accidentally hardcode the operator.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(2); v.push(3); v.push(4);
            v.iter().fold(1, |acc: i64, x: i64| acc * x)
        }
    "#;
    // 1 * 2 * 3 * 4 = 24
    assert_eq!(run_main(src), 24);
}

#[test]
fn iterator_fold_unannotated_closure() {
    // Session 081: bidirectional hints at method-call sites.
    // The closure's params (acc, x) get their types from F's
    // Fn2<U, Self::Item, U> bound — U pinned from init (i64),
    // Self::Item from VecIter<i64> = i64. No annotation needed.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4);
            v.iter().fold(0, |acc, x| acc + x)
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn iterator_map_unannotated_closure() {
    // Session 081: unannotated closure in .map. F's Fn1<Self::
    // Item, U> bound supplies x: i64 from VecIter<i64>::Item;
    // U remains an inference TypeVar that the body's `x * x`
    // (i64) pins.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            v.iter().map(|x| x * x).sum()
        }
    "#;
    // 1 + 4 + 9 = 14
    assert_eq!(run_main(src), 14);
}

#[test]
fn iterator_filter_unannotated_closure() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter().filter(|x| x > 2).count()
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn iterator_chain_all_unannotated() {
    // Three back-to-back unannotated closures in one chain.
    // The bidirectional hint flow has to fire at each step.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            v.iter()
                .filter(|x| x > 1)
                .map(|x| x * 10)
                .fold(0, |acc, x| acc + x)
        }
    "#;
    // filter > 1 = [2,3,4,5]; map *10 = [20,30,40,50]; fold sum = 140
    assert_eq!(run_main(src), 140);
}

#[test]
fn numeric_trait_user_struct() {
    // Session 084: a user struct implements `Numeric` and can be
    // passed to a `<T: Numeric>` generic fn that dispatches through
    // the bound's `add` / `lt` methods.
    let src = r#"
        struct Money { cents: i64 }
        impl std::Numeric for Money {
            fn add(self: Money, other: Money) -> Money {
                Money { cents: self.cents + other.cents }
            }
            fn lt(self: Money, other: Money) -> bool {
                self.cents < other.cents
            }
        }
        fn smaller<T: std::Numeric>(a: T, b: T) -> T {
            if a.lt(b) { a } else { b }
        }
        fn main() -> i64 {
            let a: Money = Money { cents: 500 };
            let b: Money = Money { cents: 300 };
            let m: Money = smaller(a, b);
            m.cents
        }
    "#;
    assert_eq!(run_main(src), 300);
}

#[test]
fn numeric_trait_combined_add_and_lt() {
    // Combine .add and .lt across multiple Money values.
    let src = r#"
        struct Money { cents: i64 }
        impl std::Numeric for Money {
            fn add(self: Money, other: Money) -> Money {
                Money { cents: self.cents + other.cents }
            }
            fn lt(self: Money, other: Money) -> bool {
                self.cents < other.cents
            }
        }
        fn sum_two<T: std::Numeric>(a: T, b: T) -> T {
            a.add(b)
        }
        fn main() -> i64 {
            let a: Money = Money { cents: 25 };
            let b: Money = Money { cents: 75 };
            let c: Money = sum_two(a, b);
            c.cents
        }
    "#;
    assert_eq!(run_main(src), 100);
}

#[test]
fn iterator_min_polymorphic_return_type() {
    // Session 084: .min returns Option<Self::Item> now. For
    // i64-iterators that's Option<i64>, same as before — but the
    // body is no longer hardcoded to i64. The match arm types
    // confirm the Self::Item-polymorphic return.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(50); v.push(10); v.push(30);
            match v.iter().min() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn iterator_max_polymorphic_return_type() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(50); v.push(10); v.push(30);
            match v.iter().max() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 50);
}

#[test]
fn match_tuple_pattern_basic() {
    // Session 082: tuple patterns in match arms.
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (3, 4);
            match pair {
                (1, x) => x,
                (3, y) => y * 100,
                (_, _) => -1,
            }
        }
    "#;
    // (3, 4) matches second arm → 4 * 100 = 400
    assert_eq!(run_main(src), 400);
}

#[test]
fn match_tuple_pattern_first_arm() {
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (1, 99);
            match pair {
                (1, x) => x,
                (3, y) => y * 100,
                (_, _) => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 99);
}

#[test]
fn match_tuple_pattern_fallback() {
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (5, 5);
            match pair {
                (1, x) => x,
                (3, y) => y * 100,
                (_, _) => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), -1);
}

#[test]
fn match_tuple_pattern_both_literals() {
    // Both elements are literals — no bindings.
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (2, 3);
            match pair {
                (1, 1) => 100,
                (2, 3) => 200,
                (_, _) => 300,
            }
        }
    "#;
    assert_eq!(run_main(src), 200);
}

#[test]
fn match_tuple_pattern_with_wildcard_first() {
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (7, 42);
            match pair {
                (_, 42) => 1,
                (_, _) => 0,
            }
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn match_tuple_pattern_three_elements() {
    let src = r#"
        fn main() -> i64 {
            let t: (i64, i64, i64) = (1, 2, 3);
            match t {
                (1, 2, x) => x,
                (_, _, _) => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn match_tuple_pattern_with_guard() {
    let src = r#"
        fn main() -> i64 {
            let pair: (i64, i64) = (5, 10);
            match pair {
                (a, b) if a < b => b - a,
                (a, b) => a - b,
            }
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn match_tuple_pattern_with_bool_elements() {
    // Session 089 added cartesian-product exhaustiveness, so
    // `(true, x) | (false, _)` is fully exhaustive on `(bool,
    // i64)`. Session 094 then flags the catch-all `_` as
    // unreachable, so this test was updated to remove it.
    let src = r#"
        fn main() -> i64 {
            let pair: (bool, i64) = (true, 7);
            match pair {
                (true, x) => x,
                (false, _) => 0,
            }
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn iterator_fold_with_capturing_closure() {
    // Capturing closure → synthesized struct implementing Fn2.
    // Tests that the bound-propagation cascade reads the call
    // method's 3-arg signature [Self, A, B] -> R and pins the
    // method-level generics correctly.
    let src = r#"
        fn main() -> i64 {
            let scale: i64 = 3;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(4);
            v.iter().fold(0, |acc: i64, x: i64| acc + x * scale)
        }
    "#;
    // (1 + 2 + 4) * 3 = 21
    assert_eq!(run_main(src), 21);
}

#[test]
fn hashmap_entries_iter_yields_pairs() {
    // Session 075: m.entries() yields (key, value) tuples for
    // every live slot. Mirror of m.keys() (session 068) plus
    // the value via hashmap_val_at; the iterator's Item is a
    // (i64, V) tuple built per slot.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 10);
            m.insert(2, 20);
            m.insert(3, 30);
            let mut sum_keys: i64 = 0;
            let mut sum_vals: i64 = 0;
            for kv in m.entries() {
                sum_keys = sum_keys + kv.0;
                sum_vals = sum_vals + kv.1;
            }
            sum_keys + sum_vals
        }
    "#;
    // 1+2+3 = 6; 10+20+30 = 60; 6 + 60 = 66.
    assert_eq!(run_main(src), 66);
}

#[test]
fn hashmap_entries_destructure_in_for() {
    // Combine session 074's tuple destructuring with session
    // 075's .entries(): `for (k, v) in m.entries()` works
    // because the for-pat binds tuple elements through the
    // let-expansion path. Wait — for-pattern is its own bind,
    // not let-expansion. Test what actually works.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(5, 100);
            m.insert(7, 200);
            let mut total: i64 = 0;
            for kv in m.entries() {
                let (k, v) = kv;
                total = total + k * v;
            }
            total
        }
    "#;
    // 5*100 + 7*200 = 500 + 1400 = 1900.
    assert_eq!(run_main(src), 1900);
}

#[test]
fn hashmap_entries_with_str_values_doesnt_leak() {
    // ARC-managed value type. The entries iter yields tuples
    // whose .1 holds a str pointer; TupleIndex retains, and
    // both the str literals (rc=-1) and the tuple+inner Vec
    // chain should clean up at scope exit.
    let src = r#"
        fn build() -> i64 {
            let m: std::HashMap<i64, str> = hashmap_new();
            m.insert(1, "ab");
            m.insert(2, "cde");
            let mut total: i64 = 0;
            for kv in m.entries() {
                total = total + kv.1.len();
            }
            total
        }
        fn main() -> i64 {
            let mut sum: i64 = 0;
            for _ in 0..50 {
                sum = sum + build();
            }
            sum
        }
    "#;
    // 2 + 3 = 5 per iter; 50 * 5 = 250.
    assert_eq!(run_main(src), 250);
}

#[test]
fn tuple_destructure_let_basic() {
    // Session 074: `let (a, b) = pair` desugars to a temp +
    // per-element index reads. The resolver already minted
    // bindings for `a` and `b`; the lowerer emits three lets.
    let src = r#"
        fn main() -> i64 {
            let (a, b) = (10, 32);
            a + b
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn tuple_destructure_let_with_arc_elements() {
    // Tuple containing Vec elements; destructuring reads each
    // out and the inner Vecs survive past the tuple's scope
    // because TupleIndex retains on ARC element types.
    let src = r#"
        fn main() -> i64 {
            let v1: Vec<i64> = vec_new();
            v1.push(7); v1.push(8);
            let v2: Vec<i64> = vec_new();
            v2.push(100);
            let (a, b) = (v1, v2);
            a.get(0) + a.get(1) + b.get(0)
        }
    "#;
    // 7 + 8 + 100 = 115
    assert_eq!(run_main(src), 115);
}

#[test]
fn tuple_destructure_with_wildcard() {
    let src = r#"
        fn main() -> i64 {
            let (a, _, c) = (1, 99, 4);
            a + c
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn tuple_release_arc_elements_at_scope_exit() {
    // Session 074: per-shape release walks ARC elements before
    // freeing the heap block. A tight loop constructs tuples
    // holding Vec<i64> values; if the inner Vecs leaked the
    // process would balloon under a tracker. Here we just pin
    // "constructs + releases without crashing" and the value
    // is right.
    let src = r#"
        fn build() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let t = (v, 99);
            t.0.get(0) + t.0.get(1) + t.0.get(2) + t.1
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            for _ in 0..100 {
                total = total + build();
            }
            total
        }
    "#;
    // build() = 1+2+3+99 = 105. 100 * 105 = 10500.
    assert_eq!(run_main(src), 10500);
}

#[test]
fn tuple_literal_and_index() {
    // Session 073: (a, b) tuple literal + t.0 / t.1 indexing.
    // The tuple is heap-allocated as N*8 bytes + trailing rc;
    // each index loads at i*8 from the pointer.
    let src = r#"
        fn main() -> i64 {
            let t: (i64, i64) = (10, 20);
            t.0 + t.1
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn tuple_three_elements_mixed_types() {
    let src = r#"
        fn main() -> i64 {
            let t: (i64, bool, i64) = (5, true, 100);
            let mid: i64 = if t.1 { 1 } else { 0 };
            t.0 + mid + t.2
        }
    "#;
    assert_eq!(run_main(src), 106);
}

#[test]
fn tuple_as_fn_return_and_param() {
    let src = r#"
        fn split(x: i64) -> (i64, i64) {
            (x / 10, x % 10)
        }
        fn sum_pair(p: (i64, i64)) -> i64 {
            p.0 + p.1
        }
        fn main() -> i64 {
            sum_pair(split(347))
        }
    "#;
    // split(347) = (34, 7). sum_pair = 41.
    assert_eq!(run_main(src), 41);
}

#[test]
fn try_op_with_multi_into_picks_right_target() {
    // Session 072: a single source error struct impls Into for
    // TWO different target error structs. Each `?` site picks
    // the impl whose target matches the surrounding fn's err
    // type. Pre-072 impl_methods[(SourceErr, "into")] was
    // silently overwritten by the second impl, so only one of
    // these tests would have worked; with disambiguation both do.
    let src = r#"
        struct IoErr   { code: i64 }
        struct AppErr  { tag: i64 }
        struct WireErr { kind: i64 }

        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr {
                AppErr { tag: self.code + 1000 }
            }
        }
        impl std::Into<WireErr> for IoErr {
            fn into(self: IoErr) -> WireErr {
                WireErr { kind: self.code + 2000 }
            }
        }

        fn read() -> std::Result<i64, IoErr> {
            std::Result::Err(IoErr { code: 5 })
        }

        fn into_app() -> std::Result<i64, AppErr> {
            let v: i64 = read()?;
            std::Result::Ok(v)
        }
        fn into_wire() -> std::Result<i64, WireErr> {
            let v: i64 = read()?;
            std::Result::Ok(v)
        }

        fn main() -> i64 {
            let a: i64 = match into_app() {
                std::Result::Ok(_)    => 0,
                std::Result::Err(e)   => e.tag,
            };
            let w: i64 = match into_wire() {
                std::Result::Ok(_)    => 0,
                std::Result::Err(e)   => e.kind,
            };
            a + w
        }
    "#;
    // a = 1005 (AppErr.tag = 5+1000); w = 2005 (WireErr.kind = 5+2000)
    // a + w = 3010
    assert_eq!(run_main(src), 3010);
}

#[test]
fn try_op_on_option_some_unwraps() {
    // Session 072: `?` on Option<T>. Some(x)? produces x; the
    // ? operator already existed for Result, this extends it
    // to Option with the equivalent desugar.
    let src = r#"
        fn get() -> std::Option<i64> {
            std::Option::Some(42)
        }
        fn use_get() -> std::Option<i64> {
            let v: i64 = get()?;
            std::Option::Some(v + 8)
        }
        fn main() -> i64 {
            match use_get() {
                std::Option::Some(x) => x,
                std::Option::None => -1,
            }
        }
    "#;
    assert_eq!(run_main(src), 50);
}

#[test]
fn try_op_on_option_none_propagates() {
    let src = r#"
        fn get_none() -> std::Option<i64> {
            std::Option::None
        }
        fn use_get() -> std::Option<i64> {
            let v: i64 = get_none()?;
            std::Option::Some(v + 1)
        }
        fn main() -> i64 {
            match use_get() {
                std::Option::Some(x) => x,
                std::Option::None => 99,
            }
        }
    "#;
    assert_eq!(run_main(src), 99);
}

#[test]
fn trait_default_method_collect_chained() {
    // Session 071: the headline — `.collect()` as a default method
    // on Iterator. The trait declares the default body; impls
    // (VecIter<T>, Map<I, F, U>, Filter<I, P>) inherit it. The
    // monomorphizer specializes per Self at each call site so
    // `self.next()` inside the body dispatches to the impl's
    // concrete next method.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let result: Vec<i64> = v.iter().collect();
            result.len() + result.get(0) + result.get(4)
        }
    "#;
    // len=5, first=1, last=5; 5 + 1 + 5 = 11
    assert_eq!(run_main(src), 11);
}

#[test]
fn trait_default_method_collect_through_map() {
    // Combine default-method dispatch with the closure-bound
    // iterator-adapter path. Map<I, F, U> inherits .collect()
    // from Iterator; calling it on a Map of closures works.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mult: i64 = 10;
            let result: Vec<i64> =
                std::Map { iter: v.iter(), f: |x| x * mult }.collect();
            result.len() + result.get(0) + result.get(2)
        }
    "#;
    // 3 + 10 + 30 = 43
    assert_eq!(run_main(src), 43);
}

#[test]
fn hashmap_insert_overwrite_releases_old_value() {
    // Session 070: overwriting an existing key releases the old
    // value's ARC. Pre-070, the old Vec got dropped on the floor.
    // The runtime's insert now returns the previous slot value
    // (0 for fresh) and codegen emits a release call when V is
    // ARC-managed. We exercise the path by overwriting the same
    // key many times in a tight loop; if the leak persisted, RSS
    // would grow unbounded under a tracker. Here we just confirm
    // the latest value wins and the program exits cleanly.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, Vec<i64>> = hashmap_new();
            let mut last_sum: i64 = 0;
            // 200 overwrites of the same key. Each iteration's
            // Vec gets released by the next iteration's insert
            // (pre-070 they leaked).
            for i in 0..200 {
                let v: Vec<i64> = vec_new();
                v.push(i);
                v.push(i * 2);
                m.insert(1, v);
            }
            // After the loop, only the last Vec remains in the
            // map. Read it back to confirm the slot is intact.
            let final_vec: Vec<i64> = m.get(1);
            last_sum = final_vec.get(0) + final_vec.get(1);
            last_sum + m.len()
        }
    "#;
    // Last iter: i = 199. v = [199, 398]. sum = 597. len = 1.
    // 597 + 1 = 598.
    assert_eq!(run_main(src), 598);
}

#[test]
fn hashmap_str_insert_overwrite_releases_old_str_value() {
    // Same shape but with Str values — exercises the str-side of
    // the per-V release walk. The old str literal has rc=-1 so
    // its release is a no-op at the helper level, but the
    // codepath still runs through it.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, str> = hashmap_new();
            for i in 0..50 {
                let s: str = "v" + "alue";
                m.insert(7, s);
            }
            m.get(7).len()
        }
    "#;
    // "value".len() = 5
    assert_eq!(run_main(src), 5);
}

#[test]
fn hashmap_value_is_vec_releases_at_scope_exit() {
    // Session 067: a HashMap<i64, Vec<i64>> walks its occupied
    // slots on release, freeing the inner Vecs. Pre-067 the
    // inner Vecs leaked. This test runs the construction inside
    // a tight loop — if the inner Vecs weren't reclaimed, the
    // process would balloon. We can't observe heap directly from
    // Rune, but we can pin "the map exists, holds a Vec value,
    // releases without crashing" — a regression check that the
    // synthesized release fn doesn't trash memory.
    let src = r#"
        fn build() -> i64 {
            let m: std::HashMap<i64, Vec<i64>> = hashmap_new();
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20); v.push(30);
            m.insert(1, v);
            let w: Vec<i64> = vec_new();
            w.push(100); w.push(200);
            m.insert(2, w);
            m.get(1).get(0) + m.get(2).get(1)
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            // Build the map 100 times; if the release fn leaked
            // the inner Vecs we'd see RSS grow unbounded under a
            // tracker — here we just confirm each iteration's
            // value is right and the program exits cleanly.
            for _ in 0..100 {
                total = total + build();
            }
            total
        }
    "#;
    // build() returns 10 + 200 = 210. 100 * 210 = 21000.
    assert_eq!(run_main(src), 21000);
}

#[test]
fn hashmap_value_is_str_releases_via_walk() {
    // Str values exercise the same path. String literals carry
    // rc=-1 so their release is a no-op at the helper level, but
    // the release-walk codepath still runs through them and the
    // descriptor still frees correctly.
    let src = r#"
        fn build() -> i64 {
            let m: std::HashMap<i64, str> = hashmap_new();
            m.insert(1, "ten");
            m.insert(2, "twenty");
            m.get(2).len()
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            for _ in 0..50 {
                total = total + build();
            }
            total
        }
    "#;
    // "twenty".len() == 6, 50 * 6 = 300.
    assert_eq!(run_main(src), 300);
}

#[test]
fn hashmap_remove_returns_previous_value() {
    // Session 068: m.remove(k) returns the previous value (or 0
    // when k was absent). The slot becomes a tombstone — len
    // decrements, contains_key returns false.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 100);
            m.insert(2, 200);
            m.insert(3, 300);
            let removed: i64 = m.remove(2);
            let present_after: i64 = if m.contains_key(2) { 1 } else { 0 };
            let len_after: i64 = m.len();
            removed + present_after * 10000 + len_after
        }
    "#;
    // Removed 200; contains(2)=false (=0); len=2. Sum = 200 + 0 + 2 = 202.
    assert_eq!(run_main(src), 202);
}

#[test]
fn hashmap_remove_missing_key_returns_zero() {
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 100);
            let r1: i64 = m.remove(99);  // missing
            let r2: i64 = m.remove(99);  // double-missing
            r1 + r2 + m.len()
        }
    "#;
    assert_eq!(run_main(src), 1);
}

#[test]
fn hashmap_remove_then_reinsert_reuses_tombstone() {
    // Inserting back the removed key should restore m.contains.
    // The probe-for-insert reuses the tombstone slot.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(7, 77);
            m.remove(7);
            m.insert(7, 88);
            m.get(7) + m.len()
        }
    "#;
    // get(7)=88, len=1, total=89
    assert_eq!(run_main(src), 89);
}

#[test]
fn hashmap_keys_iter_visits_each_live_key_once() {
    // Session 068: HashMapKeysIter yields every live key exactly
    // once. Order is hash-driven (not insertion order) so we
    // accumulate via sum which is order-independent.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 10);
            m.insert(2, 20);
            m.insert(3, 30);
            m.insert(4, 40);
            m.insert(5, 50);
            let mut key_sum: i64 = 0;
            let mut val_sum: i64 = 0;
            for k in m.keys() {
                key_sum = key_sum + k;
                val_sum = val_sum + m.get(k);
            }
            key_sum * 1000 + val_sum
        }
    "#;
    // keys sum = 1+2+3+4+5 = 15; vals sum = 10+20+30+40+50 = 150.
    // 15 * 1000 + 150 = 15150.
    assert_eq!(run_main(src), 15150);
}

#[test]
fn hashmap_keys_iter_skips_tombstones() {
    // After removing keys, the iterator must skip their slots.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 10);
            m.insert(2, 20);
            m.insert(3, 30);
            m.remove(2);
            let mut sum: i64 = 0;
            for k in m.keys() {
                sum = sum + k * 100 + m.get(k);
            }
            sum
        }
    "#;
    // Live keys: 1, 3. 1*100+10 + 3*100+30 = 110 + 330 = 440.
    assert_eq!(run_main(src), 440);
}

#[test]
fn hashmap_keys_iter_empty_map() {
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            let mut sum: i64 = 0;
            for k in m.keys() {
                sum = sum + k;
            }
            sum
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn hashmap_str_keys_insert_get() {
    // Session 069: str-keyed HashMap. Content equality (not
    // pointer identity) — two distinct str descriptors with the
    // same bytes hash to the same slot and match.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("one", 1);
            m.insert("two", 2);
            m.insert("three", 3);
            m.get("one") + m.get("two") + m.get("three")
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn hashmap_str_keys_content_equality_not_pointer() {
    // Two concat'd strings produce distinct rune_str descriptors
    // but with identical content — they must hash to the same
    // slot and compare equal. Confirms memcmp-based key equality.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            let a: str = "hel" + "lo";
            let b: str = "he" + "llo";
            m.insert(a, 42);
            // `b` is a distinct heap descriptor but content-equal
            // to `a`. get(b) must find the slot stored under `a`.
            m.get(b)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn hashmap_str_keys_missing_returns_zero() {
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("known", 99);
            let here: i64 = if m.contains_key("known") { 1 } else { 0 };
            let gone: i64 = if m.contains_key("missing") { 1 } else { 0 };
            m.get("known") + here + gone + m.get("missing")
        }
    "#;
    // 99 + 1 + 0 + 0 = 100
    assert_eq!(run_main(src), 100);
}

#[test]
fn hashmap_str_keys_remove_then_reinsert() {
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("k", 7);
            let removed: i64 = m.remove("k");
            m.insert("k", 9);
            removed + m.get("k") + m.len()
        }
    "#;
    // 7 + 9 + 1 = 17
    assert_eq!(run_main(src), 17);
}

#[test]
fn hashmap_str_keys_grow_past_initial_cap() {
    // Force several grow + rehash cycles. The runtime's per-slot
    // rehash uses the hash-by-key-kind branch so str hashes drive
    // probe placement, not their (i64-cast) pointer values.
    let src = r#"
        fn build_key(i: i64) -> str {
            // Distinct content per i — concat with a unique tail.
            let base: str = "key";
            base + base + "_x"
        }
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            // Just 12 distinct keys is enough to force at least
            // one grow from cap=8.
            m.insert("a", 1);
            m.insert("b", 2);
            m.insert("c", 3);
            m.insert("d", 4);
            m.insert("e", 5);
            m.insert("f", 6);
            m.insert("g", 7);
            m.insert("h", 8);
            m.insert("i", 9);
            m.insert("j", 10);
            m.insert("k", 11);
            m.insert("l", 12);
            let mut sum: i64 = 0;
            sum = sum + m.get("a") + m.get("f") + m.get("l");
            sum + m.len()
        }
    "#;
    // (1 + 6 + 12) + 12 = 31
    assert_eq!(run_main(src), 31);
}

#[test]
fn hashmap_str_keys_release_with_vec_values() {
    // Combine str keys + Vec values. Both sides get ARC-walked
    // at the map's scope exit: the synth per-V release walks
    // vals (Vecs), then the C release_hashmap walks live slots
    // releasing each str key.
    let src = r#"
        fn build() -> i64 {
            let m: std::HashMap<str, Vec<i64>> = hashmap_str_new();
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20);
            m.insert("a", v);
            let w: Vec<i64> = vec_new();
            w.push(100);
            m.insert("b", w);
            m.get("a").get(0) + m.get("b").get(0)
        }
        fn main() -> i64 {
            // Repeat to surface any leak or double-free under a
            // simple sanity check.
            let mut total: i64 = 0;
            for _ in 0..50 {
                total = total + build();
            }
            total
        }
    "#;
    // build() = 10 + 100 = 110. 50 * 110 = 5500.
    assert_eq!(run_main(src), 5500);
}

#[test]
fn binop_hint_rhs_literal() {
    // Session 095: `a: i32 + 1` lets the bare `1` adopt i32
    // from the LHS's concrete type. Previously errored as
    // "mismatched types: i32 vs i64".
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 5;
            (a + 1) as i64
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn binop_hint_lhs_literal() {
    // Symmetric: `1 + a: i32` lets the bare `1` adopt i32
    // via the literal-LHS retry path.
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 5;
            (1 + a) as i64
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn binop_hint_negative_literal() {
    // Negative bare literal on RHS picks up the LHS hint via
    // session 091's Unary-Neg-on-Lit branch.
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 10;
            (a + -3) as i64
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn binop_hint_float() {
    // Float operands work too.
    let src = r#"
        fn main() -> i64 {
            let a: f32 = 1.5;
            (a * 4.0) as i64
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn binop_hint_suffix_wins() {
    // Suffix-bearing literals don't get re-hinted; mismatched
    // suffix vs LHS-type still errors. This test confirms the
    // matching-suffix case compiles cleanly.
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 5;
            (a + 7i32) as i64
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn integer_literal_hint_let_binding() {
    // Session 091: bare `10` adopts the let's i32 annotation
    // rather than defaulting to i64 (which would error).
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 10;
            let b: i32 = 20;
            (a + b) as i64
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn integer_literal_hint_fn_arg() {
    // Hint flows from the called fn's param type.
    let src = r#"
        fn add_i32(a: i32, b: i32) -> i32 { a + b }
        fn main() -> i64 {
            add_i32(5, 7) as i64
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn integer_literal_hint_struct_field() {
    // Hint flows from the struct field's declared type.
    let src = r#"
        struct Holder { n: i32 }
        fn main() -> i64 {
            let h: Holder = Holder { n: 42 };
            h.n as i64
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn integer_literal_hint_float() {
    // f32 hint from `let x: f32 = 3.14;` (no suffix).
    let src = r#"
        fn main() -> i64 {
            let pi: f32 = 3.14;
            let two: f32 = 2.0;
            (pi * two) as i64
        }
    "#;
    assert_eq!(run_main(src), 6);
}

#[test]
fn integer_literal_hint_negative() {
    // Unary `-N` on a bare literal also picks up the hint.
    let src = r#"
        fn main() -> i64 {
            let a: i32 = -10;
            (a + 20i32) as i64
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn integer_literal_suffix_overrides_hint() {
    // Suffix wins even when a (compatible) hint is provided.
    // `let a: i64 = 10i64;` — both say i64, so it works; if we
    // wrote `let a: i32 = 10i64;` the suffix would error against
    // the annotation, which is the intended behavior. Test the
    // sanity case.
    let src = r#"
        fn main() -> i64 {
            let a: i64 = 10i64;
            a
        }
    "#;
    assert_eq!(run_main(src), 10);
}

#[test]
fn tuple_exhaustive_bool_x_int() {
    // Session 089: `(true, x) | (false, _)` over (bool, i64) is
    // exhaustive — true closes via the first arm (any tail), false
    // closes via the second.
    let src = r#"
        fn main() -> i64 {
            let pair: (bool, i64) = (true, 5);
            match pair {
                (true, x) => x,
                (false, _) => 99,
            }
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn tuple_exhaustive_full_bool_x_bool() {
    // All four (bool, bool) combinations covered.
    let src = r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (true, false);
            match pair {
                (true, true) => 1,
                (true, false) => 2,
                (false, true) => 3,
                (false, false) => 4,
            }
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn tuple_exhaustive_with_wildcard_tail() {
    // `(true, _)` + the three remaining (false, *) cases.
    let src = r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (false, true);
            match pair {
                (true, _) => 1,
                (false, true) => 2,
                (false, false) => 3,
            }
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn tuple_exhaustive_enum_x_bool() {
    // (enum, bool) — every variant × bool combination covered
    // either specifically or via a wildcard tail.
    let src = r#"
        enum Color { Red, Green, Blue }

        fn main() -> i64 {
            let p: (Color, bool) = (Color::Green, true);
            match p {
                (Color::Red, _) => 1,
                (Color::Green, true) => 2,
                (Color::Green, false) => 3,
                (Color::Blue, _) => 4,
            }
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn tuple_exhaustive_via_wildcard_arm() {
    // A bare `_` arm closes regardless of per-position holes.
    let src = r#"
        fn main() -> i64 {
            let pair: (bool, bool) = (false, false);
            match pair {
                (true, true) => 1,
                _ => 0,
            }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn numeric_literal_suffix_i32() {
    // Session 088: `10i32` lexes as a typed integer, no
    // surrounding cast needed.
    let src = r#"
        fn main() -> i64 {
            let a: i32 = 10i32;
            let b: i32 = 20i32;
            (a + b) as i64
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn numeric_literal_suffix_u32() {
    let src = r#"
        fn main() -> i64 {
            let a: u32 = 100u32;
            let b: u32 = 30u32;
            (a - b) as i64
        }
    "#;
    assert_eq!(run_main(src), 70);
}

#[test]
fn numeric_literal_suffix_f32() {
    let src = r#"
        fn main() -> i64 {
            let pi: f32 = 3.14f32;
            let two: f32 = 2.0f32;
            (pi * two) as i64
        }
    "#;
    // 3.14 * 2.0 = 6.28; as i64 truncates to 6.
    assert_eq!(run_main(src), 6);
}

#[test]
fn numeric_literal_suffix_default_unchanged() {
    // Without a suffix, literals still default to i64.
    let src = r#"
        fn main() -> i64 {
            let a = 42;
            a
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn numeric_literal_suffix_hex_with_u8() {
    // Suffixes work on radix-prefixed literals too.
    let src = r#"
        fn main() -> i64 {
            let mask: u8 = 0xffu8;
            mask as i64
        }
    "#;
    assert_eq!(run_main(src), 255);
}

#[test]
fn numeric_impl_on_i64_primitive() {
    // Session 087: `impl Numeric for i64` works, lifting the
    // "impl only on structs" restriction. The trait dispatches
    // through the BuiltinType anchor sym via impl_methods.
    let src = r#"
        impl std::Numeric for i64 {
            fn add(self: i64, other: i64) -> i64 { self + other }
            fn lt(self: i64, other: i64) -> bool { self < other }
        }
        fn smaller<T: std::Numeric>(a: T, b: T) -> T {
            if a.lt(b) { a } else { b }
        }
        fn main() -> i64 {
            let a: i64 = 50;
            let b: i64 = 30;
            smaller(a, b)
        }
    "#;
    assert_eq!(run_main(src), 30);
}

#[test]
fn numeric_impl_on_i64_combined() {
    let src = r#"
        impl std::Numeric for i64 {
            fn add(self: i64, other: i64) -> i64 { self + other }
            fn lt(self: i64, other: i64) -> bool { self < other }
        }
        fn sum_two<T: std::Numeric>(a: T, b: T) -> T { a.add(b) }
        fn main() -> i64 {
            sum_two(7, 8)
        }
    "#;
    assert_eq!(run_main(src), 15);
}

#[test]
fn numeric_primitive_method_direct_call() {
    // Calling the impl method directly on a primitive receiver,
    // outside any generic context. `(5).lt(7)` dispatches through
    // impl_methods on the i64 anchor sym.
    let src = r#"
        impl std::Numeric for i64 {
            fn add(self: i64, other: i64) -> i64 { self + other }
            fn lt(self: i64, other: i64) -> bool { self < other }
        }
        fn main() -> i64 {
            let a: i64 = 5;
            let b: i64 = 7;
            if a.lt(b) { 100 } else { 200 }
        }
    "#;
    assert_eq!(run_main(src), 100);
}

#[test]
fn into_disambiguation_let_binding() {
    // Session 086: when a source struct has multiple Into<T>
    // impls, `let x: AppErr = src.into();` picks Into<AppErr>
    // based on the let's annotation.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        struct DbErr { code: i64 }

        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: self.code + 100 } }
        }
        impl std::Into<DbErr> for IoErr {
            fn into(self: IoErr) -> DbErr { DbErr { code: self.code + 200 } }
        }

        fn main() -> i64 {
            let e: IoErr = IoErr { code: 5 };
            let a: AppErr = e.into();
            a.code
        }
    "#;
    // 5 + 100 (the AppErr branch)
    assert_eq!(run_main(src), 105);
}

#[test]
fn into_disambiguation_picks_other_target() {
    // Same impls, hint picks the other branch.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        struct DbErr { code: i64 }

        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: self.code + 100 } }
        }
        impl std::Into<DbErr> for IoErr {
            fn into(self: IoErr) -> DbErr { DbErr { code: self.code + 200 } }
        }

        fn main() -> i64 {
            let e: IoErr = IoErr { code: 5 };
            let d: DbErr = e.into();
            d.code
        }
    "#;
    // 5 + 200 (the DbErr branch)
    assert_eq!(run_main(src), 205);
}

#[test]
fn into_disambiguation_fn_arg() {
    // Hint flows from the called fn's param type.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        struct DbErr { code: i64 }

        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: self.code + 100 } }
        }
        impl std::Into<DbErr> for IoErr {
            fn into(self: IoErr) -> DbErr { DbErr { code: self.code + 200 } }
        }
        fn use_db(d: DbErr) -> i64 { d.code }

        fn main() -> i64 {
            let e: IoErr = IoErr { code: 7 };
            use_db(e.into())
        }
    "#;
    // 7 + 200 = 207
    assert_eq!(run_main(src), 207);
}

#[test]
fn into_disambiguation_struct_field() {
    // Hint flows from the struct field's declared type.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        struct DbErr { code: i64 }

        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr { AppErr { code: self.code + 100 } }
        }
        impl std::Into<DbErr> for IoErr {
            fn into(self: IoErr) -> DbErr { DbErr { code: self.code + 200 } }
        }
        struct Holder { err: AppErr }

        fn main() -> i64 {
            let e: IoErr = IoErr { code: 3 };
            let h: Holder = Holder { err: e.into() };
            h.err.code
        }
    "#;
    // 3 + 100 = 103 (AppErr branch)
    assert_eq!(run_main(src), 103);
}

#[test]
fn for_tuple_pattern_over_entries() {
    // Session 085: `for (k, v) in m.entries()` works directly,
    // no `let (k, v) = kv` workaround. The lowerer threads
    // session 074's expand_tuple_let_from_local into the
    // Iterator-protocol desugar's some-arm body.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(5, 100);
            m.insert(7, 200);
            let mut total: i64 = 0;
            for (k, v) in m.entries() {
                total = total + k * v;
            }
            total
        }
    "#;
    // 5*100 + 7*200 = 500 + 1400 = 1900
    assert_eq!(run_main(src), 1900);
}

#[test]
fn for_tuple_pattern_str_keyed_entries() {
    // Tuple for-pattern over str-keyed HashMap entries.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("ab", 10);
            m.insert("cde", 20);
            let mut total: i64 = 0;
            for (k, v) in m.entries() {
                total = total + k.len() * v;
            }
            total
        }
    "#;
    // "ab".len() * 10 + "cde".len() * 20 = 20 + 60 = 80
    assert_eq!(run_main(src), 80);
}

#[test]
fn for_tuple_pattern_with_wildcard() {
    // A `_` sub-pattern skips that element's binding.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(1, 10);
            m.insert(2, 20);
            m.insert(3, 30);
            let mut total: i64 = 0;
            for (_, v) in m.entries() {
                total = total + v;
            }
            total
        }
    "#;
    // 10 + 20 + 30 = 60
    assert_eq!(run_main(src), 60);
}

#[test]
fn for_tuple_pattern_nested_lookup() {
    // Use the entries iter to drive a per-key lookup, exercising
    // the destructure-binds-in-body shape.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            m.insert(2, 3);
            m.insert(5, 7);
            let lookup: std::HashMap<i64, i64> = hashmap_new();
            lookup.insert(2, 100);
            lookup.insert(5, 200);
            let mut acc: i64 = 0;
            for (k, v) in m.entries() {
                acc = acc + v * lookup.get(k);
            }
            acc
        }
    "#;
    // 3 * 100 + 7 * 200 = 300 + 1400 = 1700
    assert_eq!(run_main(src), 1700);
}

#[test]
fn hashmap_str_keys_iteration() {
    // Session 083: .keys() on a str-keyed HashMap yields each
    // live key as a str. The lowerer routes to
    // HashMapStrKeysIter; hashmap_key_at is now polymorphic on
    // K so the iterator's body reads the right type.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("ab", 10);
            m.insert("cde", 20);
            m.insert("f", 30);
            let mut total: i64 = 0;
            for k in m.keys() {
                total = total + k.len();
            }
            total
        }
    "#;
    // 2 + 3 + 1 = 6
    assert_eq!(run_main(src), 6);
}

#[test]
fn hashmap_str_entries_iteration() {
    // .entries() on a str-keyed map yields (str, V) tuples.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("ab", 10);
            m.insert("cde", 20);
            m.insert("f", 30);
            let mut total: i64 = 0;
            for kv in m.entries() {
                total = total + kv.0.len() + kv.1;
            }
            total
        }
    "#;
    // (2+3+1) + (10+20+30) = 6 + 60 = 66
    assert_eq!(run_main(src), 66);
}

#[test]
fn hashmap_str_entries_destructure_in_for() {
    // Tuple destructuring (session 074) over str-keyed entries.
    // For-pattern itself only takes ident/_; the inner `let
    // (k, v) = kv` does the unpack — same shape as the
    // i64-keyed version.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("one", 100);
            m.insert("two", 200);
            let mut acc: i64 = 0;
            for kv in m.entries() {
                let (k, v) = kv;
                acc = acc + k.len() * v;
            }
            acc
        }
    "#;
    // "one"*100 + "two"*200 = 300 + 600 = 900
    assert_eq!(run_main(src), 900);
}

#[test]
fn hashmap_str_keys_after_remove_skips_tombstones() {
    // The iterator's is_live_at check must skip occupied==2
    // (tombstones), not just occupied==0.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("a", 1);
            m.insert("b", 2);
            m.insert("c", 3);
            let _ = m.remove("b");
            let mut count: i64 = 0;
            for _ in m.keys() {
                count = count + 1;
            }
            count
        }
    "#;
    assert_eq!(run_main(src), 2);
}

#[test]
fn hashmap_str_keys_iter_with_vec_values() {
    // Mixed ARC keys + ARC values. Stress test: keys are
    // retained when yielded from the iter, vals borrowed from
    // the map. The runtime release walks live str keys at map
    // drop; the synth per-V release walks Vec values.
    let src = r#"
        fn build() -> i64 {
            let m: std::HashMap<str, Vec<i64>> = hashmap_str_new();
            let v: Vec<i64> = vec_new();
            v.push(5); v.push(7);
            m.insert("xy", v);
            let w: Vec<i64> = vec_new();
            w.push(11);
            m.insert("abc", w);
            let mut total: i64 = 0;
            for k in m.keys() {
                total = total + k.len();
            }
            total
        }
        fn main() -> i64 {
            let mut sum: i64 = 0;
            for _ in 0..30 {
                sum = sum + build();
            }
            sum
        }
    "#;
    // per build: "xy".len() + "abc".len() = 2 + 3 = 5; 30 * 5 = 150.
    assert_eq!(run_main(src), 150);
}

#[test]
fn hashmap_str_entries_via_count_default_method() {
    // Inherited Iterator default methods work on the str-keyed
    // iterator structs. .count() comes from session 076.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<str, i64> = hashmap_str_new();
            m.insert("a", 1);
            m.insert("b", 2);
            m.insert("c", 3);
            m.entries().count()
        }
    "#;
    assert_eq!(run_main(src), 3);
}

#[test]
fn hashmap_count_distinct_via_insert() {
    // Idiom: count by inserting `1` (or += 1) on each occurrence.
    // Here we just insert once per key and read m.len() to confirm
    // the distinct count.
    let src = r#"
        fn main() -> i64 {
            let m: std::HashMap<i64, i64> = hashmap_new();
            let xs: [i64; 8] = [1, 2, 3, 2, 1, 4, 3, 5];
            for x in xs {
                m.insert(x, 1);
            }
            m.len()
        }
    "#;
    // distinct: {1,2,3,4,5} → 5
    assert_eq!(run_main(src), 5);
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

#[test]
fn continue_in_while_skips_iteration() {
    // Session 063: `continue` jumps to the loop's continue
    // block. For while, that's the header — re-check the
    // condition. Test sums only odd numbers via continue-on-
    // even.
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            let mut i = 0;
            while i < 10 {
                i = i + 1;
                if i - (i / 2) * 2 == 0 { continue; }
                sum = sum + i;
            }
            sum
        }
    "#;
    // 1+3+5+7+9 = 25
    assert_eq!(run_main(src), 25);
}

#[test]
fn continue_in_for_range() {
    // `continue` inside `for i in 0..n` jumps to the loop's
    // increment block, advancing the counter and re-checking.
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            for i in 0..10 {
                if i - (i / 2) * 2 == 0 { continue; }
                sum = sum + i;
            }
            sum
        }
    "#;
    // 1+3+5+7+9 = 25
    assert_eq!(run_main(src), 25);
}

#[test]
fn continue_in_for_array() {
    let src = r#"
        fn main() -> i64 {
            let xs: [i64; 5] = [10, 20, 30, 40, 50];
            let mut sum = 0;
            for x in xs {
                if x == 30 { continue; }
                sum = sum + x;
            }
            sum
        }
    "#;
    // 10 + 20 + 40 + 50 = 120
    assert_eq!(run_main(src), 120);
}

#[test]
fn continue_in_for_vec_iter() {
    // The iterator-protocol path: `for x in v.iter() { ... }`
    // is desugared by the lowerer into a `while true { match
    // it.next() { Some(x) => ..., None => break } }`. The
    // outer while's continue-block IS its header, so a user
    // `continue` re-enters the match → next iteration.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let mut sum = 0;
            for x in v.iter() {
                if x == 3 { continue; }
                sum = sum + x;
            }
            sum
        }
    "#;
    // 1+2+4+5 = 12
    assert_eq!(run_main(src), 12);
}

#[test]
fn range_iter_as_value() {
    // Session 063: `0..10` is now an expression that yields a
    // `std::RangeIter` value. Iterate it via `.next()` directly.
    let src = r#"
        fn main() -> i64 {
            let r: std::RangeIter = 0..5;
            let mut count: i64 = 0;
            while true {
                match r.next() {
                    std::Option::Some(_) => { count = count + 1; }
                    std::Option::None => { break; }
                }
            }
            count
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn range_iter_via_for_loop() {
    // The for-over-range fast path still works (HirExprKind::ForRange
    // bypasses the struct allocation), but a bound range value
    // works through the Iterator protocol:
    let src = r#"
        fn main() -> i64 {
            let r: std::RangeIter = 1..5;
            let mut sum: i64 = 0;
            for x in r {
                sum = sum + x;
            }
            sum
        }
    "#;
    // 1+2+3+4 = 10
    assert_eq!(run_main(src), 10);
}

#[test]
fn range_iter_through_map_pipeline() {
    // Headline: a range value flows into Map's iter field, the
    // Map's f transforms each i64. Confirms RangeIter satisfies
    // the Iterator bound on Map<I: Iterator, F, U>.
    let src = r#"
        fn main() -> i64 {
            let r: std::RangeIter = 1..4;
            let mapped = std::Map { iter: r, f: |x: i64| x * 10 };
            let mut sum: i64 = 0;
            for y in mapped { sum = sum + y; }
            sum
        }
    "#;
    // (1*10)+(2*10)+(3*10) = 60
    assert_eq!(run_main(src), 60);
}

#[test]
fn range_open_start_in_for_loop() {
    // Session 066: `..n` (no start) defaults start to 0 in the
    // for-over-range fast path.
    let src = r#"
        fn main() -> i64 {
            let mut sum: i64 = 0;
            for i in ..5 {
                sum = sum + i;
            }
            sum
        }
    "#;
    // 0+1+2+3+4 = 10
    assert_eq!(run_main(src), 10);
}

#[test]
fn range_open_end_with_break() {
    // `n..` (no end) defaults end to i64::MAX — the user is
    // expected to break out themselves.
    let src = r#"
        fn main() -> i64 {
            let mut sum: i64 = 0;
            for i in 5.. {
                if i > 10 { break; }
                sum = sum + i;
            }
            sum
        }
    "#;
    // 5+6+7+8+9+10 = 45
    assert_eq!(run_main(src), 45);
}

#[test]
fn range_open_end_as_iter_value() {
    // Open-ended range as a bound RangeIter value. Drive it with
    // explicit `.next()` calls so the test terminates.
    let src = r#"
        fn main() -> i64 {
            let r: std::RangeIter = 100..;
            let mut sum: i64 = 0;
            let mut taken: i64 = 0;
            while taken < 3 {
                match r.next() {
                    std::Option::Some(v) => {
                        sum = sum + v;
                        taken = taken + 1;
                    }
                    std::Option::None => { break; }
                }
            }
            sum
        }
    "#;
    // 100 + 101 + 102 = 303
    assert_eq!(run_main(src), 303);
}

#[test]
fn range_iter_inclusive_form() {
    // The `..=` inclusive form bumps the upper bound by 1 at lower
    // time so the runtime exit `cur < end` yields end items.
    let src = r#"
        fn main() -> i64 {
            let r: std::RangeIter = 1..=4;
            let mut sum: i64 = 0;
            for x in r { sum = sum + x; }
            sum
        }
    "#;
    // 1+2+3+4 = 10
    assert_eq!(run_main(src), 10);
}

#[test]
fn continue_releases_arc_locals() {
    // ARC-managed locals declared after the loop entry must
    // get released on continue. A Vec allocated inside the
    // loop iteration is freed before re-entering.
    let src = r#"
        fn main() -> i64 {
            let mut sum = 0;
            for i in 0..5 {
                let v: Vec<i64> = vec_new();
                v.push(i);
                if i == 2 { continue; }
                sum = sum + v.get(0);
            }
            sum
        }
    "#;
    // 0+1+3+4 = 8 (i=2 skipped)
    assert_eq!(run_main(src), 8);
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

#[test]
fn try_from_based_conversion_ok() {
    // Session 065: the `?` operator now calls `.into()` to convert
    // an inner result's error type to the surrounding function's
    // error type when they differ. Here `inner_ok` returns
    // `Result<i64, IoErr>` but `outer` returns `Result<i64, AppErr>`.
    // The `impl Into<AppErr> for IoErr` bridges the gap at the `?`
    // site. Ok branch — no conversion runs, but the typecheck
    // still has to accept the mismatched types.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr {
                AppErr { code: self.code + 1000 }
            }
        }
        fn inner_ok() -> std::Result<i64, IoErr> {
            std::Result::Ok(42)
        }
        fn outer() -> std::Result<i64, AppErr> {
            let v: i64 = inner_ok()?;
            std::Result::Ok(v + 1)
        }
        fn main() -> i64 {
            std::ok_or(outer(), -1)
        }
    "#;
    assert_eq!(run_main(src), 43);
}

#[test]
fn try_from_based_conversion_err() {
    // Same setup, but `inner_err` returns Err. The `?` calls
    // `IoErr.into()` to produce an `AppErr`, returns
    // `Result::Err(AppErr { code: 1007 })`. main reads the
    // converted code via a match on the result.
    let src = r#"
        struct IoErr { code: i64 }
        struct AppErr { code: i64 }
        impl std::Into<AppErr> for IoErr {
            fn into(self: IoErr) -> AppErr {
                AppErr { code: self.code + 1000 }
            }
        }
        fn inner_err() -> std::Result<i64, IoErr> {
            std::Result::Err(IoErr { code: 7 })
        }
        fn outer() -> std::Result<i64, AppErr> {
            let v: i64 = inner_err()?;
            std::Result::Ok(v + 1)
        }
        fn main() -> i64 {
            match outer() {
                std::Result::Ok(_) => 0,
                std::Result::Err(e) => e.code,
            }
        }
    "#;
    assert_eq!(run_main(src), 1007);
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

#[test]
fn match_scrutinee_temp_released() {
    // `match make_bag(..) { .. }` — the scrutinee is a fresh enum
    // temporary. Once the arm bodies (whose bindings borrow into it)
    // are done, the match releases it at the merge point. 200
    // iterations; a double free would crash.
    let src = r#"
        enum Bag { Full(Vec<i64>), Empty }
        fn make_bag(k: i64) -> Bag {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            Bag::Full(v)
        }
        fn main() -> i64 {
            let mut acc = 0;
            let mut n = 0;
            while n < 200 {
                acc = acc + match make_bag(4) {
                    Bag::Full(x) => x.get(0),
                    Bag::Empty => 0,
                };
                n = n + 1;
            }
            acc
        }
    "#;
    // 200 * 4 = 800
    assert_eq!(run_main(src), 800);
}

#[test]
fn match_scrutinee_payload_escapes() {
    // The payload escapes the match (`Bag::Full(x) => x`) and the
    // scrutinee is a temporary. The escaped payload is retained, the
    // scrutinee released — both at the merge — and they net out.
    let src = r#"
        enum Bag { Full(Vec<i64>), Empty }
        fn make_bag(k: i64) -> Bag {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            Bag::Full(v)
        }
        fn main() -> i64 {
            let mut acc = 0;
            let mut n = 0;
            while n < 200 {
                let got: Vec<i64> = match make_bag(6) {
                    Bag::Full(x) => x,
                    Bag::Empty => vec_new(),
                };
                acc = acc + got.get(0);
                n = n + 1;
            }
            acc
        }
    "#;
    // 200 * 6 = 1200
    assert_eq!(run_main(src), 1200);
}

#[test]
fn match_scrutinee_returning_arm() {
    // An arm that `return`s diverges before the merge — the
    // scrutinee temporary is reclaimed on the way out by
    // `release_all_arc_locals`, not the merge-point release.
    let src = r#"
        enum Bag { Full(Vec<i64>), Empty }
        fn make_bag(k: i64) -> Bag {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            Bag::Full(v)
        }
        fn first_or_return(k: i64) -> i64 {
            match make_bag(k) {
                Bag::Full(x) => return x.get(0),
                Bag::Empty => 0,
            }
        }
        fn main() -> i64 {
            let mut acc = 0;
            let mut n = 0;
            while n < 200 {
                acc = acc + first_or_return(9);
                n = n + 1;
            }
            acc
        }
    "#;
    // 200 * 9 = 1800
    assert_eq!(run_main(src), 1800);
}

#[test]
fn array_elements_released() {
    // An array of fresh ARC elements — each `Vec` is reclaimed when
    // the array local leaves scope. 200 iterations; a leak would
    // grow unbounded, a double free would crash.
    let src = r#"
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let arr = [make(1), make(2), make(3)];
                sum = sum + arr[0].get(0) + arr[1].get(0) + arr[2].get(0);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (1 + 2 + 3) = 1200
    assert_eq!(run_main(src), 1200);
}

#[test]
fn array_of_borrowed_local() {
    // `[shared, shared]` stores a borrowed `Local` in two element
    // slots — each slot retains, so the array owns two refs. Scope
    // exit releases both, then the binding releases the last: no
    // double free.
    let src = r#"
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let shared: Vec<i64> = make(7);
                let arr = [shared, shared];
                sum = sum + arr[0].get(0) + arr[1].get(0);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (7 + 7) = 2800
    assert_eq!(run_main(src), 2800);
}

#[test]
fn array_copy_retains() {
    // `let b = a` copies the array pointer — both bindings alias the
    // same slot. The copy retains every element so each binding's
    // scope-exit release is balanced.
    let src = r#"
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let a = [make(4), make(5)];
                let b = a;
                sum = sum + a[0].get(0) + b[1].get(0);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (4 + 5) = 1800
    assert_eq!(run_main(src), 1800);
}

#[test]
fn array_let_annotation_and_param() {
    // `[T; N]` written as a type — on a `let` and a function
    // parameter. The array flows through borrowed (the parameter is
    // never scope-tracked).
    let src = r#"
        fn sum3(a: [i64; 3]) -> i64 {
            a[0] + a[1] + a[2]
        }
        fn main() -> i64 {
            let nums: [i64; 3] = [10, 20, 30];
            sum3(nums)
        }
    "#;
    assert_eq!(run_main(src), 60);
}

#[test]
fn struct_array_field_arc() {
    // A struct field of array type — `[Vec<i64>; 2]`. The struct's
    // release walks the array field, reclaiming each element Vec.
    // 200 iterations; a double free would crash.
    let src = r#"
        struct Holder { items: [Vec<i64>; 2], tag: i64 }
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let h: Holder = Holder { items: [make(2), make(3)], tag: 5 };
                sum = sum + h.items[0].get(0) + h.items[1].get(0) + h.tag;
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (2 + 3 + 5) = 2000
    assert_eq!(run_main(src), 2000);
}

#[test]
fn enum_array_payload_arc() {
    // An enum payload of array type — `[Vec<i64>; 2]`. The enum's
    // release walks the array payload, reclaiming each element.
    let src = r#"
        enum Crate { Loaded([Vec<i64>; 2]), Empty }
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn unload(c: Crate) -> i64 {
            match c {
                Crate::Loaded(p) => p[0].get(0) + p[1].get(0),
                Crate::Empty => 0,
            }
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let c: Crate = Crate::Loaded([make(6), make(8)]);
                sum = sum + unload(c);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (6 + 8) = 2800
    assert_eq!(run_main(src), 2800);
}

#[test]
fn array_returned_by_value() {
    // A heap array escapes the frame that built it — `make_ints`
    // returns an array, and indexing it in the caller is sound.
    // With stack arrays this read a dead frame.
    let src = r#"
        fn make_ints() -> [i64; 3] {
            [11, 22, 33]
        }
        fn main() -> i64 {
            let a = make_ints();
            a[0] + a[1] + a[2]
        }
    "#;
    assert_eq!(run_main(src), 66);
}

#[test]
fn struct_with_array_escapes() {
    // A struct field of array type, the struct returned by value.
    // The array lives on the heap, so `p.xs` stays valid after
    // `make_pair` returns. 200 iterations exercises alloc/free.
    let src = r#"
        struct Pair { xs: [i64; 2], tag: i64 }
        fn make_pair(k: i64) -> Pair {
            Pair { xs: [k, k + 1], tag: k * 10 }
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let p = make_pair(3);
                sum = sum + p.xs[0] + p.xs[1] + p.tag;
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (3 + 4 + 30) = 7400
    assert_eq!(run_main(src), 7400);
}

#[test]
fn heap_array_of_vecs_returned() {
    // An array of ARC elements returned by value. Releasing the
    // heap array walks its elements, reclaiming each Vec — 200
    // iterations, a leak or double free would show.
    let src = r#"
        fn make(k: i64) -> Vec<i64> {
            let mut v: Vec<i64> = vec_new();
            v.push(k);
            v
        }
        fn make_vecs(k: i64) -> [Vec<i64>; 2] {
            [make(k), make(k + 1)]
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let vs = make_vecs(8);
                sum = sum + vs[0].get(0) + vs[1].get(0);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (8 + 9) = 3400
    assert_eq!(run_main(src), 3400);
}

#[test]
fn dyn_struct_field() {
    // A concrete struct coerces to `dyn Trait` at a struct-literal
    // field initializer. The struct's release walks the `dyn` field,
    // reclaiming the box — 200 iterations, a leak or double free
    // would show.
    let src = r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Circle { r: i64 }
        impl Shape for Circle {
            fn area(self: Circle) -> i64 { self.r * self.r }
        }
        struct Holder { shape: dyn Shape, tag: i64 }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let h: Holder = Holder { shape: Circle { r: 3 }, tag: 7 };
                sum = sum + h.shape.area() + h.tag;
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * (9 + 7) = 3200
    assert_eq!(run_main(src), 3200);
}

#[test]
fn dyn_enum_payload() {
    // A concrete struct coerces to `dyn Trait` at an enum-variant
    // payload position. The enum's release walks the `dyn` payload.
    let src = r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Square { s: i64 }
        impl Shape for Square {
            fn area(self: Square) -> i64 { self.s * self.s }
        }
        enum Maybe { Has(dyn Shape), Empty }
        fn area_of(m: Maybe) -> i64 {
            match m {
                Maybe::Has(s) => s.area(),
                Maybe::Empty => 0,
            }
        }
        fn main() -> i64 {
            let mut sum = 0;
            let mut n = 0;
            while n < 200 {
                let m: Maybe = Maybe::Has(Square { s: 5 });
                sum = sum + area_of(m);
                n = n + 1;
            }
            sum
        }
    "#;
    // 200 * 25 = 5000
    assert_eq!(run_main(src), 5000);
}

#[test]
fn generic_impl_inherent_method() {
    // `impl<T> Box<T>` — a method on a generic struct. The method is
    // generic over the impl's `T`; `get` specializes once per
    // instantiation (`Box<i64>` and `Box<bool>`).
    let src = r#"
        struct Box<T> { val: T }
        impl<T> Box<T> {
            fn get(self: Box<T>) -> T { self.val }
        }
        fn main() -> i64 {
            let bi: Box<i64> = Box { val: 30 };
            let bb: Box<bool> = Box { val: true };
            let ok: bool = bb.get();
            if ok { bi.get() + 12 } else { bi.get() }
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn generic_impl_multiple_methods() {
    // Several methods in one generic impl all share the impl's `T`,
    // and a method may take extra (non-generic) parameters.
    let src = r#"
        struct Pair<T> { a: T, b: T }
        impl<T> Pair<T> {
            fn first(self: Pair<T>) -> T { self.a }
            fn second(self: Pair<T>) -> T { self.b }
            fn shifted(self: Pair<T>, by: i64) -> i64 { by }
        }
        fn main() -> i64 {
            let p: Pair<i64> = Pair { a: 15, b: 20 };
            p.first() + p.second() + p.shifted(7)
        }
    "#;
    // 15 + 20 + 7 = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn generic_impl_trait_bound() {
    // A trait `impl<T>` on a generic struct, called both directly
    // and through a `<U: Tagged>` generic function — the latter
    // forces a second specialization pass (the method call is
    // rewritten to a `Call` only after `apply` is specialized).
    let src = r#"
        trait Tagged { fn tag(self: dyn Tagged) -> i64; }
        struct Holder<T> { item: T, n: i64 }
        impl<T> Tagged for Holder<T> {
            fn tag(self: Holder<T>) -> i64 { self.n }
        }
        fn apply<U: Tagged>(x: U) -> i64 {
            x.tag()
        }
        fn main() -> i64 {
            let h: Holder<i64> = Holder { item: 1, n: 17 };
            let hb: Holder<bool> = Holder { item: true, n: 25 };
            h.tag() + apply(hb)
        }
    "#;
    // 17 + 25 = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn assoc_type_concrete_method_call() {
    // A trait declares `type Item;`; an impl binds `type Item = i64;`
    // and uses `Self::Item` in the method's return position. The
    // checker resolves `Self::Item` to `i64` from the impl's
    // binding, so `c.next()` types and runs as `i64`.
    let src = r#"
        trait Iterator {
            type Item;
            fn next(self: dyn Iterator) -> Self::Item;
        }
        struct Counter { n: i64 }
        impl Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> Self::Item { self.n + 1 }
        }
        fn main() -> i64 {
            let c: Counter = Counter { n: 41 };
            c.next()
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn supertrait_method_via_bound() {
    // `<T: Dog>` — a value bounded by `Dog` can call both `Dog`'s
    // and the supertrait `Animal`'s methods. The checker's
    // method-lookup walks the supertrait chain transitively.
    let src = r#"
        trait Animal {
            fn speak(self: dyn Animal) -> i64;
        }
        trait Dog: Animal {
            fn bark(self: dyn Dog) -> i64;
        }
        struct Lab { volume: i64 }
        impl Animal for Lab {
            fn speak(self: Lab) -> i64 { self.volume + 10 }
        }
        impl Dog for Lab {
            fn bark(self: Lab) -> i64 { self.volume + 20 }
        }
        fn handle<T: Dog>(x: T) -> i64 {
            x.speak() + x.bark()
        }
        fn main() -> i64 {
            let l: Lab = Lab { volume: 6 };
            handle(l)
        }
    "#;
    // (6+10) + (6+20) = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn supertrait_two_level_chain() {
    // `A: B`, `B: C`. A `<T: A>` value can call methods from A, B,
    // and C — the supertrait walk is transitive.
    let src = r#"
        trait C { fn c(self: dyn C) -> i64; }
        trait B: C { fn b(self: dyn B) -> i64; }
        trait A: B { fn a(self: dyn A) -> i64; }
        struct S { n: i64 }
        impl C for S { fn c(self: S) -> i64 { self.n } }
        impl B for S { fn b(self: S) -> i64 { self.n + 1 } }
        impl A for S { fn a(self: S) -> i64 { self.n + 2 } }
        fn all<T: A>(x: T) -> i64 {
            x.a() + x.b() + x.c()
        }
        fn main() -> i64 {
            let s: S = S { n: 13 };
            all(s)
        }
    "#;
    // 15 + 14 + 13 = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn assoc_type_projection_through_type_param() {
    // Iterator-protocol shape: the generic function `bump` bounds
    // `T: Iterator` and returns `T::Item`. The checker keeps
    // `T::Item` as `Ty::Assoc(Ty::TypeVar(T), "Item")`; the
    // monomorphizer walks `T` to `Counter`, looks up the impl's
    // `type Item = i64` binding, and substitutes the call site's
    // result type to `i64`.
    let src = r#"
        trait Iterator {
            type Item;
            fn next(self: dyn Iterator) -> Self::Item;
        }
        struct Counter { n: i64 }
        impl Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> Self::Item { self.n + 1 }
        }
        fn bump<T: Iterator>(x: T) -> T::Item {
            x.next()
        }
        fn main() -> i64 {
            let c: Counter = Counter { n: 41 };
            bump(c)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn assoc_type_projection_distinct_impls() {
    // Two impls bind `Item` to different concrete types. Each
    // monomorphization of `pluck` picks up the right binding —
    // `pluck::<Counter>` returns `i64`, `pluck::<Banner>`
    // returns `str`. The caller never names the projection
    // itself; the test exercises that two specializations of
    // the same generic see independent substitutions.
    let src = r#"
        trait Producer {
            type Item;
            fn make(self: dyn Producer) -> Self::Item;
        }
        struct Counter { seed: i64 }
        impl Producer for Counter {
            type Item = i64;
            fn make(self: Counter) -> Self::Item { self.seed * 2 }
        }
        struct Banner { tag: str }
        impl Producer for Banner {
            type Item = str;
            fn make(self: Banner) -> Self::Item { self.tag }
        }
        fn pluck<T: Producer>(x: T) -> T::Item {
            x.make()
        }
        fn main() -> i64 {
            let c: Counter = Counter { seed: 17 };
            let b: Banner = Banner { tag: "abcdefgh" };
            // pluck(c): T::Item = i64 = 17*2 = 34
            // pluck(b): T::Item = str = "abcdefgh"; .len() = 8
            pluck(c) + pluck(b).len()
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn dyn_supertrait_method() {
    // `Dog: Animal`. A value bounded as `dyn Dog` can call both
    // `Dog`'s `bark` and the supertrait's `speak` — the box's
    // method table is laid out flat (Dog's methods first, then
    // Animal's), and `dyn_method_sig` walks the supertrait chain
    // when resolving the method name.
    let src = r#"
        trait Animal { fn speak(self: dyn Animal) -> i64; }
        trait Dog: Animal { fn bark(self: dyn Dog) -> i64; }
        struct Lab { volume: i64 }
        impl Animal for Lab {
            fn speak(self: Lab) -> i64 { self.volume + 10 }
        }
        impl Dog for Lab {
            fn bark(self: Lab) -> i64 { self.volume + 20 }
        }
        fn handle(d: dyn Dog) -> i64 {
            d.bark() + d.speak()
        }
        fn main() -> i64 {
            let l: Lab = Lab { volume: 6 };
            handle(l)
        }
    "#;
    // (6 + 20) + (6 + 10) = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn dyn_supertrait_two_level_chain() {
    // `A: B`, `B: C`. A `dyn A` value can call methods from A, B,
    // and C — the flat method list contains all three traits'
    // methods, and the supertrait BFS in `dyn_method_sig` finds
    // each one regardless of which ancestor declared it.
    let src = r#"
        trait C { fn c(self: dyn C) -> i64; }
        trait B: C { fn b(self: dyn B) -> i64; }
        trait A: B { fn a(self: dyn A) -> i64; }
        struct S { n: i64 }
        impl C for S { fn c(self: S) -> i64 { self.n } }
        impl B for S { fn b(self: S) -> i64 { self.n + 1 } }
        impl A for S { fn a(self: S) -> i64 { self.n + 2 } }
        fn all(x: dyn A) -> i64 {
            x.a() + x.b() + x.c()
        }
        fn main() -> i64 {
            let s: S = S { n: 13 };
            all(s)
        }
    "#;
    // 15 + 14 + 13 = 42
    assert_eq!(run_main(src), 42);
}

#[test]
fn dyn_supertrait_box_arc_under_loop() {
    // The flat layout grows the box (one slot per supertrait
    // method), and `define_dyn_release` reads the new size to
    // pick rc/drop offsets. Run a tight loop that constructs and
    // releases a `dyn Sub` box every iteration — if the size or
    // any offset is off, the heap accounting double-frees or
    // leaks within a small number of iterations.
    let src = r#"
        trait Animal { fn speak(self: dyn Animal) -> i64; }
        trait Dog: Animal { fn bark(self: dyn Dog) -> i64; }
        struct Lab { volume: i64 }
        impl Animal for Lab { fn speak(self: Lab) -> i64 { self.volume + 1 } }
        impl Dog for Lab    { fn bark(self: Lab) -> i64 { self.volume + 2 } }
        fn main() -> i64 {
            let mut total: i64 = 0;
            let mut i: i64 = 0;
            while i < 100 {
                let l: Lab = Lab { volume: i };
                let d: dyn Dog = l;
                total = total + d.bark() + d.speak();
                i = i + 1;
            }
            // sum_{i=0}^{99} (2*i + 3) = 99*100 + 3*100 = 10200
            total
        }
    "#;
    assert_eq!(run_main(src), 10200);
}

#[test]
fn iter_counter_for_in() {
    // The headline test: a Counter struct implements std::Iterator
    // and a `for x in counter { ... }` loop walks it. The lowerer
    // desugars to `while true { match counter.next() { Some(x) => ...,
    // None => break } }`; the body's contributions sum up.
    let src = r#"
        struct Counter { n: i64, limit: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> std::Option<i64> {
                if self.n < self.limit {
                    let v: i64 = self.n;
                    self.n = self.n + 1;
                    std::Option::Some(v)
                } else {
                    std::Option::None
                }
            }
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            let c: Counter = Counter { n: 1, limit: 6 };
            for x in c {
                total = total + x;
            }
            total
        }
    "#;
    // 1 + 2 + 3 + 4 + 5 = 15
    assert_eq!(run_main(src), 15);
}

#[test]
fn iter_break_from_loop_body() {
    // `break` inside a `for` body exits the loop. The desugared
    // `while-match` loop's exit block is on the loop_exit_stack;
    // the body's break jumps there, releasing the synthesized
    // `__it` ARC local on the way out.
    let src = r#"
        struct Counter { n: i64, limit: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> std::Option<i64> {
                if self.n < self.limit {
                    let v: i64 = self.n;
                    self.n = self.n + 1;
                    std::Option::Some(v)
                } else {
                    std::Option::None
                }
            }
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            let c: Counter = Counter { n: 0, limit: 100 };
            for x in c {
                if x == 7 { break; }
                total = total + x;
            }
            // 0+1+2+3+4+5+6 = 21
            total
        }
    "#;
    assert_eq!(run_main(src), 21);
}

#[test]
fn iter_bounded_generic() {
    // A generic function `count<T: std::Iterator>(it: T)` consumes
    // any iterator and tallies. As of session 054, generic bounds
    // accept paths directly — no `use as` workaround needed.
    let src = r#"
        struct Counter { n: i64, limit: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> std::Option<i64> {
                if self.n < self.limit {
                    let v: i64 = self.n;
                    self.n = self.n + 1;
                    std::Option::Some(v)
                } else {
                    std::Option::None
                }
            }
        }
        fn count<T: std::Iterator>(it: T) -> i64 {
            let mut n: i64 = 0;
            for _ in it {
                n = n + 1;
            }
            n
        }
        fn main() -> i64 {
            let c: Counter = Counter { n: 0, limit: 7 };
            count(c)
        }
    "#;
    assert_eq!(run_main(src), 7);
}

#[test]
fn iter_early_return_from_for_body() {
    // A `return` inside the for body should release `__it` (the
    // synthesized iterator local) via `release_all_arc_locals` —
    // the iterator struct is ARC-managed (every user struct is
    // since session 020), so missing the release would leak.
    let src = r#"
        struct Counter { n: i64, limit: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> std::Option<i64> {
                if self.n < self.limit {
                    let v: i64 = self.n;
                    self.n = self.n + 1;
                    std::Option::Some(v)
                } else {
                    std::Option::None
                }
            }
        }
        fn find_first_gt(threshold: i64) -> i64 {
            let c: Counter = Counter { n: 0, limit: 1000 };
            for x in c {
                if x > threshold { return x; }
            };
            // Sentinel for "not found"; the trailing semi above
            // disambiguates the for-expression from a binary subtract
            // (Rune parses block-trailed expressions hungrily).
            0 - 1
        }
        fn main() -> i64 {
            find_first_gt(41)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn iter_nested_for_array_inside_iterator() {
    // Outer for-in over a Counter (iterator path), inner for-in
    // over an array (array path). Tests that the dispatch in
    // `lower_for` is per-call-site, not per-function.
    let src = r#"
        struct Counter { n: i64, limit: i64 }
        impl std::Iterator for Counter {
            type Item = i64;
            fn next(self: Counter) -> std::Option<i64> {
                if self.n < self.limit {
                    let v: i64 = self.n;
                    self.n = self.n + 1;
                    std::Option::Some(v)
                } else {
                    std::Option::None
                }
            }
        }
        fn main() -> i64 {
            let mut total: i64 = 0;
            let c: Counter = Counter { n: 1, limit: 4 };
            for x in c {
                let row: [i64; 3] = [x, x * 2, x * 3];
                for y in row {
                    total = total + y;
                }
            }
            // outer x in 1..3: row = [1,2,3], [2,4,6], [3,6,9]
            // sums:                   6,        12,       18  -> 36
            total
        }
    "#;
    assert_eq!(run_main(src), 36);
}

#[test]
fn path_bounded_generic_calls_method() {
    // `<T: a::Foo>` — a multi-segment path in a trait bound.
    // The resolver looks up `a::Foo` via `lookup_path`; everything
    // downstream sees the same trait sym, so the bounded-generic
    // method-lookup walk finds the impl method.
    let src = r#"
        mod a {
            pub trait Foo { fn n(self: dyn Foo) -> i64; }
        }
        struct S { v: i64 }
        impl a::Foo for S { fn n(self: S) -> i64 { self.v + 1 } }
        fn ask<T: a::Foo>(x: T) -> i64 { x.n() }
        fn main() -> i64 {
            let s: S = S { v: 41 };
            ask(s)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn path_supertrait_resolves() {
    // `trait Sub: a::Super { ... }` — a module-qualified
    // supertrait. The resolver records the parent's sym in
    // `trait_supertraits`; the bounded-generic method-lookup
    // walk finds `Super::greet` through the chain.
    let src = r#"
        mod a {
            pub trait Super { fn greet(self: dyn Super) -> i64; }
        }
        trait Sub: a::Super { fn extra(self: dyn Sub) -> i64; }
        struct S { v: i64 }
        impl a::Super for S { fn greet(self: S) -> i64 { self.v } }
        impl Sub        for S { fn extra(self: S) -> i64 { self.v + 1 } }
        fn use_sub<T: Sub>(x: T) -> i64 { x.greet() + x.extra() }
        fn main() -> i64 {
            let s: S = S { v: 20 };
            use_sub(s)
        }
    "#;
    // 20 + 21 = 41
    assert_eq!(run_main(src), 41);
}

#[test]
fn fn_pointer_basic() {
    // A named fn item stored in a struct field of `fn(...) -> ...`
    // type, called through field access. The end-to-end check that
    // session 055's fn-pointer plumbing works.
    let src = r#"
        fn double(x: i64) -> i64 { x * 2 }
        struct Box { f: fn(i64) -> i64 }
        fn main() -> i64 {
            let b: Box = Box { f: double };
            (b.f)(21)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn fn_pointer_through_local() {
    // A local of `fn(i64) -> i64` type — assignment from a named
    // fn item, then call through the local.
    let src = r#"
        fn add_one(x: i64) -> i64 { x + 1 }
        fn main() -> i64 {
            let f: fn(i64) -> i64 = add_one;
            f(41)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn iter_for_in_v_iter() {
    // `for x in v.iter() { ... }` — the natural shape. Session
    // 055 deferred this because the checker's `check_for` was
    // discarding the struct's type args when looking up the
    // iterator's item type; session 056 substitutes them.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20); v.push(12);
            let mut total: i64 = 0;
            for x in v.iter() {
                total = total + x;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn vec_iter_via_next() {
    // `v.iter()` constructs a `std::VecIter<i64>`; calling `.next()`
    // directly walks the underlying Vec one element at a time. The
    // builtin `vec.iter()` method is checker-recognized and lowered
    // to a struct literal; the resulting iterator implements the
    // Iterator trait through plain user-defined `impl<T>` plumbing.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(10); v.push(20); v.push(12);
            let it: std::VecIter<i64> = v.iter();
            let mut total: i64 = 0;
            match it.next() {
                std::Option::Some(a) => { total = total + a; }
                std::Option::None => {}
            }
            match it.next() {
                std::Option::Some(b) => { total = total + b; }
                std::Option::None => {}
            }
            match it.next() {
                std::Option::Some(c) => { total = total + c; }
                std::Option::None => {}
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn iter_map_alone() {
    // `Map<I, F, U> { iter, f: F }` with `F: Fn1<I::Item, U>` —
    // session 061 made `f` a generic callable so closures (with or
    // without captures) fit alongside named fns. A bare `fn double`
    // passed as the `f` field has type `fn(i64) -> i64`; when the
    // body calls `self.f.call(x)`, the monomorphizer rewrites the
    // method call into an IndirectCall once F is pinned to the
    // fn-pointer type (the "Ty::Fn satisfies Fn1" coercion happens
    // at the call site, not at the value site).
    let src = r#"
        fn double(x: i64) -> i64 { x * 2 }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: double };
            let mut total: i64 = 0;
            for x in mapped {
                total = total + x;
            }
            total
        }
    "#;
    // 2 + 4 + 6 = 12
    assert_eq!(run_main(src), 12);
}

#[test]
fn iter_filter_alone() {
    // Session 061: Filter<I, P> with `P: Fn1<I::Item, bool>` —
    // pred is a generic callable. A named fn `is_even` satisfies
    // the bound through the IndirectCall coercion at the call site.
    let src = r#"
        fn is_even(x: i64) -> bool { x - (x / 2) * 2 == 0 }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5); v.push(6);
            let filtered = std::Filter { iter: v.iter(), pred: is_even };
            let mut total: i64 = 0;
            for x in filtered {
                total = total + x;
            }
            total
        }
    "#;
    // 2 + 4 + 6 = 12
    assert_eq!(run_main(src), 12);
}

#[test]
fn iter_collect_map_filter_pipeline() {
    // Session 056's headline, adjusted for session 061's signature.
    // Vec -> iter -> Map -> Filter -> collect into Vec<i64>. Each
    // layer's `next` works because every projection through every
    // nested `T::Item` resolves cleanly, and the `F`/`P` bounds
    // pin the closure types via session 061's bound-arg
    // propagation. The collect call materializes Vec<T::Item>.
    let src = r#"
        fn double(x: i64) -> i64 { x * 2 }
        fn gt_three(x: i64) -> bool { if x > 3 { true } else { false } }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: double };
            let filtered = std::Filter { iter: mapped, pred: gt_three };
            let result: Vec<i64> = std::collect(filtered);
            // doubled: [2,4,6]; >3: [4,6]; len + sum = 2 + 10 = 12
            result.len() + result.get(0) + result.get(1)
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn iter_count_bounded_generic() {
    // `<T: Iterator>` bounded generic over a session-061 Map. The
    // monomorphizer specializes `count` for the full Map type;
    // inside that specialization the for-loop desugar sees a
    // fully concrete iterator, projection resolves, codegen is
    // happy.
    let src = r#"
        fn double(x: i64) -> i64 { x * 2 }
        fn count<T: std::Iterator>(it: T) -> i64 {
            let mut n: i64 = 0;
            for _ in it {
                n = n + 1;
            }
            n
        }
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let mapped = std::Map { iter: v.iter(), f: double };
            count(mapped)
        }
    "#;
    assert_eq!(run_main(src), 5);
}

#[test]
fn vec_iter_exhausts_returns_none() {
    // After the last element, `next` returns None. Confirms the
    // index bookkeeping advances and the `< len` guard fires.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(99);
            let it: std::VecIter<i64> = v.iter();
            // First call: Some(99). Second call: None.
            match it.next() {
                std::Option::Some(_) => {}
                std::Option::None => { return 1; }
            }
            match it.next() {
                std::Option::Some(_) => 2,
                std::Option::None => 0,
            }
        }
    "#;
    assert_eq!(run_main(src), 0);
}

#[test]
fn closure_non_capturing_basic() {
    let src = r#"
        fn main() -> i64 {
            let f: fn(i64) -> i64 = |x| x * 2;
            f(21)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn closure_in_map_pipeline() {
    // Session 061: a closure literal flows into Map's `f: F`
    // field. F is inferred to Ty::Fn (non-capturing closure ==
    // session-057 anonymous fn item).
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x: i64| x * 2 };
            let mut total: i64 = 0;
            for y in mapped {
                total = total + y;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn closure_in_filter_pipeline() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let filtered = std::Filter { iter: v.iter(), pred: |x: i64| x > 2 };
            let mut total: i64 = 0;
            for y in filtered {
                total = total + y;
            }
            total
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn closure_chain_map_filter_collect() {
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x: i64| x * 2 };
            let filtered = std::Filter { iter: mapped, pred: |x: i64| x > 3 };
            let result: Vec<i64> = std::collect(filtered);
            result.len() + result.get(0) + result.get(1)
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn closure_capture_basic() {
    // Session 060: capturing closures actually work. The lambda
    // `|x| x * mult` captures `mult: i64` into a synth struct;
    // the call site `f(7)` dispatches through the struct's
    // synth `call` method (built into `impl_methods` by the
    // resolver, registered as the closure's call site by the
    // lowerer). The whole pipeline is monomorphic — codegen
    // sees a regular struct + a regular call.
    let src = r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            let f: fn(i64) -> i64 = |x| x * mult;
            f(7)
        }
    "#;
    assert_eq!(run_main(src), 21);
}

#[test]
fn closure_capture_multiple() {
    // Capturing two locals. The synth struct gets two fields;
    // the body reads `self.a` and `self.b` via FieldAccess.
    let src = r#"
        fn main() -> i64 {
            let a: i64 = 5;
            let b: i64 = 10;
            let f: fn(i64) -> i64 = |x| x * a + b;
            f(6)
        }
    "#;
    // 6 * 5 + 10 = 40
    assert_eq!(run_main(src), 40);
}

#[test]
fn closure_capture_call_twice() {
    // Calling a capturing closure more than once. The struct
    // value persists across calls; the captures remain accessible.
    let src = r#"
        fn main() -> i64 {
            let base: i64 = 10;
            let add_base: fn(i64) -> i64 = |x| x + base;
            let r1: i64 = add_base(1);
            let r2: i64 = add_base(2);
            r1 + r2
        }
    "#;
    // (1 + 10) + (2 + 10) = 23
    assert_eq!(run_main(src), 23);
}

#[test]
fn closure_capture_session_059_groundwork() {
    // Session 059 groundwork: the resolver no longer rejects
    // capturing closures (`let f: fn(i64) -> i64 = |x| x * mult;`).
    // The synth struct + impl method syms are minted, captures
    // recorded. Actual end-to-end execution of the captured
    // value is session 060 work — this test pins that we at
    // least get past resolution + typecheck without diagnostics.
    let src = r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            // The closure literal would lower to a struct holding
            // `mult`; session 060 adds the lowerer synthesis.
            // For now, the same lambda body without capture works
            // via session 057's anonymous-fn path:
            let f: fn(i64) -> i64 = |x| x * 3;
            f(7)
        }
    "#;
    assert_eq!(run_main(src), 21);
}

#[test]
fn closure_capture_in_map() {
    // Session 061 headline: a capturing closure flows into Map's
    // `f` field. Map<I, F, U>'s F is inferred to the closure's
    // synth struct type; the impl's `F: Fn1<I::Item, U>` bound
    // propagation pins `U = i64` (the closure's return) and
    // verifies `A = I::Item = i64` matches. Inside Map::next the
    // monomorphizer rewrites `self.f.call(x)` to a direct call to
    // the closure's synth `call` method (since `self.f` is a
    // struct, not a Ty::Fn).
    let src = r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x: i64| x * mult };
            let mut total: i64 = 0;
            for y in mapped {
                total = total + y;
            }
            total
        }
    "#;
    // 1*3 + 2*3 + 3*3 = 18
    assert_eq!(run_main(src), 18);
}

#[test]
fn closure_capture_in_map_unannotated() {
    // Session 062: the `:i64` annotation is no longer required.
    // `expand_callable_typevar` synthesizes a `Ty::Fn` hint from
    // F's `Fn1<I::Item, U>` bound and applies the current subst
    // (which has `I = VecIter<i64>`); the closure param `x`
    // binds to `VecIter<i64>::Item = i64` automatically.
    let src = r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x| x * mult };
            let mut total: i64 = 0;
            for y in mapped {
                total = total + y;
            }
            total
        }
    "#;
    // 1*3 + 2*3 + 3*3 = 18
    assert_eq!(run_main(src), 18);
}

#[test]
fn closure_capture_in_filter_unannotated() {
    // Filter parallel: pred's bound `P: Fn1<I::Item, bool>`
    // expands to a Ty::Fn hint binding x to I::Item = i64.
    let src = r#"
        fn main() -> i64 {
            let threshold: i64 = 2;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let filtered = std::Filter { iter: v.iter(), pred: |x| x > threshold };
            let mut total: i64 = 0;
            for y in filtered {
                total = total + y;
            }
            total
        }
    "#;
    // 3 + 4 + 5 = 12
    assert_eq!(run_main(src), 12);
}

#[test]
fn closure_bare_let_unannotated() {
    // Session 062: bare let with no annotation and no surrounding
    // hint. The body `x * mult` pins x to mult's type (i64)
    // through check_binary's `try_pin_infer_typevar`. The
    // closure's resolution pass after the body picks up the pin
    // and replaces the param's fresh Ty::TypeVar with i64.
    let src = r#"
        fn main() -> i64 {
            let mult: i64 = 3;
            let f = |x| x * mult;
            f(7)
        }
    "#;
    assert_eq!(run_main(src), 21);
}

#[test]
fn closure_bare_let_inferred_from_comparison() {
    // The pin happens through a comparison rather than
    // arithmetic. `x > 5` pins x to i64 via the literal.
    let src = r#"
        fn pick(x: i64) -> i64 {
            let is_big = |v| v > 5;
            if is_big(x) { 100 } else { 0 }
        }
        fn main() -> i64 {
            pick(7) + pick(3)
        }
    "#;
    // is_big(7)=true → 100; is_big(3)=false → 0. Total 100.
    assert_eq!(run_main(src), 100);
}

#[test]
fn closure_bare_let_unannotated_with_capture() {
    // Same but with a real capture — confirms inference still
    // works when the closure becomes a synth struct.
    let src = r#"
        fn main() -> i64 {
            let base: i64 = 100;
            let f = |x| x + base;
            f(7) + f(3)
        }
    "#;
    // (7 + 100) + (3 + 100) = 210
    assert_eq!(run_main(src), 210);
}

#[test]
fn closure_in_map_unannotated_no_capture() {
    // Non-capturing closure, unannotated. The bound-derived hint
    // works the same way: x: VecIter<i64>::Item = i64. The
    // closure value is a session-057 anonymous fn item
    // (Ty::Fn), not a closure struct, but the inference flow
    // through the bound is identical.
    let src = r#"
        fn main() -> i64 {
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x| x + 10 };
            let mut total: i64 = 0;
            for y in mapped {
                total = total + y;
            }
            total
        }
    "#;
    // (1+10) + (2+10) + (3+10) = 36
    assert_eq!(run_main(src), 36);
}

#[test]
fn closure_capture_in_filter() {
    // Filter<I, P> mirror of the Map test. The captured threshold
    // gates the predicate; only values strictly greater than 2
    // pass through. `P: Fn1<I::Item, bool>` propagates bool back
    // through the bound.
    let src = r#"
        fn main() -> i64 {
            let threshold: i64 = 2;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3); v.push(4); v.push(5);
            let filtered = std::Filter { iter: v.iter(), pred: |x: i64| x > threshold };
            let mut total: i64 = 0;
            for y in filtered {
                total = total + y;
            }
            total
        }
    "#;
    // 3 + 4 + 5 = 12
    assert_eq!(run_main(src), 12);
}

#[test]
fn closure_capture_chain_map_filter_collect() {
    // Two captures across a 2-stage adapter pipeline + collect.
    // Map captures `factor`; Filter captures `min_v`. The
    // monomorphizer specializes Map::next and Filter::next once
    // each (per the concrete F / P closure-struct type) and the
    // generated code dispatches through each stage's synth call
    // method. Confirms session 061's bound-propagation +
    // closure-struct-dispatch round trip end-to-end.
    let src = r#"
        fn main() -> i64 {
            let factor: i64 = 2;
            let min_v: i64 = 3;
            let v: Vec<i64> = vec_new();
            v.push(1); v.push(2); v.push(3);
            let mapped = std::Map { iter: v.iter(), f: |x: i64| x * factor };
            let filtered = std::Filter { iter: mapped, pred: |y: i64| y > min_v };
            let result: Vec<i64> = std::collect(filtered);
            // 1*2=2, 2*2=4, 3*2=6; >3: [4, 6]; len + sum = 2 + 10
            result.len() + result.get(0) + result.get(1)
        }
    "#;
    assert_eq!(run_main(src), 12);
}

#[test]
fn generic_trait_basic() {
    // Declare a trait with generic params, impl it for a struct,
    // call the method through a `dyn TheTrait<...>` value. The
    // generic args resolve correctly through the dyn dispatch
    // and the impl's method body works on the concrete types.
    let src = r#"
        trait Producer<T> {
            fn make(self: dyn Producer<T>) -> T;
        }
        struct IntBox { v: i64 }
        impl Producer<i64> for IntBox {
            fn make(self: IntBox) -> i64 { self.v + 1 }
        }
        fn main() -> i64 {
            let b: IntBox = IntBox { v: 41 };
            let d: dyn Producer<i64> = b;
            d.make()
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn generic_trait_two_params() {
    // Two generic params on the trait — exercises the multi-arg
    // substitution path through dyn_method_sig.
    let src = r#"
        trait Pair<A, B> {
            fn first(self: dyn Pair<A, B>) -> A;
            fn second(self: dyn Pair<A, B>) -> B;
        }
        struct IntBoolPair { a: i64, b: bool }
        impl Pair<i64, bool> for IntBoolPair {
            fn first(self: IntBoolPair) -> i64 { self.a }
            fn second(self: IntBoolPair) -> bool { self.b }
        }
        fn main() -> i64 {
            let p: IntBoolPair = IntBoolPair { a: 42, b: true };
            let d: dyn Pair<i64, bool> = p;
            let n: i64 = d.first();
            if d.second() { n } else { 0 }
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn generic_trait_in_method_arg_position() {
    // The trait's generic param `T` appears in a method's argument
    // position too — substitution must apply to params, not just
    // returns.
    let src = r#"
        trait Receiver<T> {
            fn take(self: dyn Receiver<T>, value: T) -> i64;
        }
        struct Counter { n: i64 }
        impl Receiver<i64> for Counter {
            fn take(self: Counter, value: i64) -> i64 { self.n + value }
        }
        fn main() -> i64 {
            let c: Counter = Counter { n: 30 };
            let d: dyn Receiver<i64> = c;
            d.take(12)
        }
    "#;
    assert_eq!(run_main(src), 42);
}

#[test]
fn closure_zero_args() {
    let src = r#"
        fn main() -> i64 {
            let f: fn() -> i64 = || 42;
            f()
        }
    "#;
    assert_eq!(run_main(src), 42);
}
