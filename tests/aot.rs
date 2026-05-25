//! End-to-end AOT tests. Build a Rune program to a native executable via
//! `cranelift-object` + an external linker, run it, check the exit code.
//!
//! These tests will fail if no working C-style linker is on `PATH` —
//! `clang`, `gcc`, or `cc`. Set `RUNE_LINKER` to override.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use rune::*;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_paths() -> (PathBuf, PathBuf) {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir()
        .join(format!("rune-aot-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let obj = dir.join("test.o");
    let exe = if cfg!(windows) { dir.join("test.exe") } else { dir.join("test") };
    (obj, exe)
}

fn build_exe(src: &str, obj: &PathBuf, exe: &PathBuf) -> Result<(), String> {
    let (tokens, le) = Lexer::new(src).tokenize();
    if !le.is_empty() {
        return Err(format!("lex errors: {:?}", le));
    }
    let (module, pe) = Parser::new(tokens).parse_module();
    if !pe.is_empty() {
        return Err(format!("parse errors: {:?}", pe));
    }
    let (res, re) = Resolver::new().resolve_module(&module);
    if !re.is_empty() {
        return Err(format!("resolve errors: {:?}", re));
    }
    let cr = Checker::new(&res).check_module(&module);
    if !cr.errors.is_empty() {
        return Err(format!("type errors: {:?}", cr.errors));
    }
    let mut hir = Lowerer::new(&res, &cr).lower_module(&module);
    monomorphize_module(&mut hir);
    let bytes = aot::build_object(&mut hir, "test", OptLevel::None)
        .map_err(|e| e.to_string())?;
    std::fs::write(obj, &bytes).map_err(|e| format!("write obj: {}", e))?;
    aot::link(obj, exe).map_err(|e| e.to_string())?;
    Ok(())
}

fn build_and_run(src: &str) -> i32 {
    let (obj, exe) = temp_paths();
    if let Err(e) = build_exe(src, &obj, &exe) {
        panic!("build failed: {}", e);
    }
    let status = Command::new(&exe).status().expect("run exe");
    status.code().expect("exe did not exit normally")
}

/// Session 120: like `build_and_run` but passes extra CLI args.
fn build_and_run_with_args(src: &str, args: &[&str]) -> i32 {
    let (obj, exe) = temp_paths();
    if let Err(e) = build_exe(src, &obj, &exe) {
        panic!("build failed: {}", e);
    }
    let status = Command::new(&exe).args(args).status().expect("run exe");
    status.code().expect("exe did not exit normally")
}

#[test]
fn returns_literal() {
    assert_eq!(build_and_run("fn main() -> i64 { 42 }"), 42);
}

#[test]
fn returns_zero() {
    assert_eq!(build_and_run("fn main() -> i64 { 0 }"), 0);
}

#[test]
fn arithmetic() {
    assert_eq!(build_and_run("fn main() -> i64 { 1 + 2 * 3 }"), 7);
}

#[test]
fn control_flow() {
    let src = r#"
        fn main() -> i64 {
            let x = 10;
            if x > 5 { x * 2 } else { 0 }
        }
    "#;
    assert_eq!(build_and_run(src), 20);
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
    assert_eq!(build_and_run(src), 55);
}

#[test]
fn recursive_factorial() {
    let src = r#"
        fn factorial(n: i64) -> i64 {
            if n <= 1 { 1 } else { n * factorial(n - 1) }
        }
        fn main() -> i64 { factorial(5) }
    "#;
    assert_eq!(build_and_run(src), 120);
}

#[test]
fn recursive_fib() {
    let src = r#"
        fn fib(n: i64) -> i64 {
            if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
        fn main() -> i64 { fib(10) }
    "#;
    assert_eq!(build_and_run(src), 55);
}

#[test]
fn cross_function_calls() {
    let src = r#"
        fn add(a: i64, b: i64) -> i64 { a + b }
        fn double(x: i64) -> i64 { add(x, x) }
        fn main() -> i64 { double(21) }
    "#;
    assert_eq!(build_and_run(src), 42);
}

// ---- print builtin ----

fn build_and_capture_full(src: &str) -> (i32, String, String) {
    let (obj, exe) = temp_paths();
    build_exe(src, &obj, &exe).expect("build");
    let out = Command::new(&exe).output().expect("run exe");
    // Some panics use abort() and may not return a normal exit code on all
    // platforms; fall back to -1 so the test can still assert.
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    (code, stdout, stderr)
}

fn build_and_capture(src: &str) -> (i32, String) {
    let (obj, exe) = temp_paths();
    build_exe(src, &obj, &exe).expect("build");
    let out = Command::new(&exe).output().expect("run exe");
    let code = out.status.code().expect("exit normally");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    (code, stdout)
}

#[test]
fn print_single_value() {
    let src = r#"
        fn main() -> i64 {
            print(42);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn print_multiple_values() {
    let src = r#"
        fn main() -> i64 {
            print(1);
            print(2);
            print(3);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["1", "2", "3"]);
}

#[test]
fn print_in_loop() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30];
            for x in xs {
                print(x);
            }
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["10", "20", "30"]);
}

#[test]
fn print_computed_value() {
    let src = r#"
        fn fib(n: i64) -> i64 {
            if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
        fn main() -> i64 {
            print(fib(10));
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "55");
}

// ---- arrays + for in AOT ----

#[test]
fn aot_array_sum() {
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
    assert_eq!(build_and_run(src), 15);
}

// ---- --release flag ----

#[test]
fn release_mode_builds_and_runs() {
    // Same source as recursive_fib, but compiled with OptLevel::Speed.
    let src = r#"
        fn fib(n: i64) -> i64 {
            if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
        fn main() -> i64 { fib(10) }
    "#;
    let (obj, exe) = temp_paths();
    let (tokens, _) = Lexer::new(src).tokenize();
    let (module, _) = Parser::new(tokens).parse_module();
    let (res, _) = Resolver::new().resolve_module(&module);
    let cr = Checker::new(&res).check_module(&module);
    assert!(cr.errors.is_empty());
    let mut hir = Lowerer::new(&res, &cr).lower_module(&module);
    let bytes = aot::build_object(&mut hir, "test", OptLevel::Speed).expect("build_object");
    std::fs::write(&obj, &bytes).expect("write");
    aot::link(&obj, &exe).expect("link");
    let status = Command::new(&exe).status().expect("run");
    assert_eq!(status.code(), Some(55));
}

// ---- strings ----

#[test]
fn print_string_literal() {
    let src = r#"
        fn main() -> i64 {
            print_str("Hello, Rune!");
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "Hello, Rune!");
}

#[test]
fn print_multiple_strings() {
    let src = r#"
        fn main() -> i64 {
            print_str("first");
            print_str("second");
            print_str("third");
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["first", "second", "third"]);
}

#[test]
fn print_string_with_escapes() {
    let src = r#"
        fn main() -> i64 {
            print_str("Hello\tworld!");
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    // Normalize Windows CRLF to LF — the C runtime uses stdout in text mode.
    let stdout = stdout.replace("\r\n", "\n");
    assert_eq!(stdout, "Hello\tworld!\n");
}

#[test]
fn mixed_print_and_print_str() {
    let src = r#"
        fn main() -> i64 {
            print_str("The answer is:");
            print(42);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["The answer is:", "42"]);
}

#[test]
fn string_eq_controls_exit_code() {
    let src = r#"
        fn main() -> i64 {
            let greeting = "hello";
            if greeting == "hello" { 42 } else { 0 }
        }
    "#;
    assert_eq!(build_and_run(src), 42);
}

#[test]
fn aot_string_passed_to_function() {
    let src = r#"
        fn matches_hello(s: str) -> bool {
            s == "hello"
        }
        fn main() -> i64 {
            if matches_hello("hello") { 1 } else { 0 }
        }
    "#;
    assert_eq!(build_and_run(src), 1);
}

// ---- string concatenation ----

#[test]
fn aot_print_concat() {
    let src = r#"
        fn main() -> i64 {
            print_str("hello, " + "world");
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "hello, world");
}

#[test]
fn aot_concat_returned_from_fn() {
    let src = r#"
        fn greet(name: str) -> str {
            "Hello, " + name + "!"
        }
        fn main() -> i64 {
            print_str(greet("Rune"));
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "Hello, Rune!");
}

// ---- polymorphic print ----

#[test]
fn poly_print_int_then_str() {
    let src = r#"
        fn main() -> i64 {
            print(42);
            print("hello");
            print(7);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["42", "hello", "7"]);
}

#[test]
fn poly_print_in_loop_with_str() {
    let src = r#"
        fn main() -> i64 {
            let names = ["Alice", "Bob", "Carol"];
            for n in names {
                print(n);
            }
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["Alice", "Bob", "Carol"]);
}

#[test]
fn aot_print_str_len() {
    let src = r#"
        fn main() -> i64 {
            let s = "Hello, Rune!";
            print(s.len());
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "12");
}

#[test]
fn aot_print_slice() {
    let src = r#"
        fn main() -> i64 {
            print("Hello, world!"[7..12]);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "world");
}

#[test]
fn aot_array_len_in_loop_bound() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30];
            print(xs.len());
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "3");
}

#[test]
fn poly_print_concat() {
    let src = r#"
        fn main() -> i64 {
            let name = "Rune";
            print("Hello, " + name + "!");
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "Hello, Rune!");
}

#[test]
fn aot_concat_in_loop() {
    // Each iteration heap-allocates a new descriptor + bytes.
    let src = r#"
        fn main() -> i64 {
            let parts = ["a", "b", "c"];
            let mut acc = "";
            for p in parts {
                acc = acc + p;
            }
            print_str(acc);
            0
        }
    "#;
    let (code, stdout) = build_and_capture(src);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "abc");
}

// ---- bounds checks ----

#[test]
fn array_index_in_bounds_runs_normally() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30];
            xs[2]
        }
    "#;
    let (code, _stdout, _stderr) = build_and_capture_full(src);
    assert_eq!(code, 30);
}

#[test]
fn array_index_out_of_bounds_aborts() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30];
            xs[5]
        }
    "#;
    let (code, _stdout, stderr) = build_and_capture_full(src);
    assert_ne!(code, 0, "expected non-zero exit on bounds violation");
    assert!(
        stderr.contains("out of range"),
        "expected bounds message on stderr, got: {:?}",
        stderr
    );
}

#[test]
fn array_index_negative_aborts() {
    let src = r#"
        fn main() -> i64 {
            let xs = [10, 20, 30];
            xs[-1]
        }
    "#;
    let (code, _stdout, stderr) = build_and_capture_full(src);
    assert_ne!(code, 0);
    assert!(stderr.contains("out of range"));
}

#[test]
fn string_byte_index_in_bounds_runs() {
    let src = r#"
        fn main() -> i64 {
            let s = "abc";
            s[1]
        }
    "#;
    let (code, _stdout, _stderr) = build_and_capture_full(src);
    assert_eq!(code, 98); // 'b' = 0x62 = 98
}

#[test]
fn string_byte_index_out_of_bounds_aborts() {
    let src = r#"
        fn main() -> i64 {
            let s = "abc";
            s[10]
        }
    "#;
    let (code, _stdout, stderr) = build_and_capture_full(src);
    assert_ne!(code, 0);
    assert!(stderr.contains("out of range"));
}

// ---- match codegen ----
//
// The `rune_panic_no_match` runtime helper stays wired up as defense-
// in-depth, but the compile-time exhaustiveness check (session 015)
// now rejects non-exhaustive matches before codegen — there's no
// reliable way to construct an AOT test that hits the runtime
// backstop. The compile-time error cases are exercised in
// `tests/typecheck.rs::match_*`.

// ---- heap-ARC types end to end through the linker ----
//
// These exercise the heap allocators and synthesized release
// functions in AOT — the coverage whose absence let the runtime
// drift incomplete.

#[test]
fn aot_payload_enum() {
    let src = r#"
        enum Opt { Some(i64), None }
        fn main() -> i64 {
            let o: Opt = Opt::Some(42);
            match o {
                Opt::Some(x) => x,
                Opt::None => 0,
            }
        }
    "#;
    assert_eq!(build_and_run(src), 42);
}

#[test]
fn aot_vec_push_get() {
    let src = r#"
        fn main() -> i64 {
            let mut v: Vec<i64> = vec_new();
            v.push(10);
            v.push(20);
            v.push(30);
            v.get(0) + v.get(1) + v.get(2) + v.len()
        }
    "#;
    assert_eq!(build_and_run(src), 63);
}

#[test]
fn aot_struct_fields() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        fn main() -> i64 {
            let p: Point = Point { x: 15, y: 27 };
            p.x + p.y
        }
    "#;
    assert_eq!(build_and_run(src), 42);
}

#[test]
fn aot_dyn_dispatch() {
    let src = r#"
        trait Shape { fn area(self: dyn Shape) -> i64; }
        struct Sq { s: i64 }
        impl Shape for Sq {
            fn area(self: Sq) -> i64 { self.s * self.s }
        }
        fn describe(sh: dyn Shape) -> i64 { sh.area() }
        fn main() -> i64 {
            describe(Sq { s: 8 })
        }
    "#;
    assert_eq!(build_and_run(src), 64);
}

#[test]
fn aot_weak_upgrade_or() {
    // `upgrade_or` lowers to `rune_weak_upgrade_or_vec` — the one
    // runtime function the AOT C runtime had been missing.
    let src = r#"
        fn main() -> i64 {
            let mut v: Vec<i64> = vec_new();
            v.push(7);
            let w: Weak<Vec<i64>> = weak(v);
            let mut d: Vec<i64> = vec_new();
            d.push(99);
            let u: Vec<i64> = upgrade_or(w, d);
            u.get(0)
        }
    "#;
    assert_eq!(build_and_run(src), 7);
}

#[test]
fn aot_generic_identity() {
    // Exercises monomorphization in the AOT pipeline.
    let src = r#"
        fn id<T>(x: T) -> T { x }
        fn main() -> i64 {
            id(40) + id(2)
        }
    "#;
    assert_eq!(build_and_run(src), 42);
}

#[test]
fn aot_env_args_returns_argv() {
    // Session 120: the C main wrapper takes (argc, argv) and forwards
    // them to rune_argv_init before invoking __rune_main. The Rune
    // side reads them back via std::env::args(). Pass three extra
    // CLI args; expect argc to be 4 (program name + 3 extras).
    let src = r#"
        fn main() -> i64 {
            let args: Vec<str> = std::env::args();
            args.len()
        }
    "#;
    assert_eq!(
        build_and_run_with_args(src, &["one", "two", "three"]),
        4
    );
}

#[test]
fn aot_env_args_first_is_program_name() {
    // argv[0] is the executable path. We can't predict the exact
    // path (it's a temp file with a randomized id) but we CAN
    // confirm the first arg is non-empty. Return 1 on non-empty,
    // 0 on empty.
    let src = r#"
        fn main() -> i64 {
            let args: Vec<str> = std::env::args();
            if args.len() == 0 { 0 } else {
                let first: str = args.get(0);
                if first.is_empty() { 0 } else { 1 }
            }
        }
    "#;
    assert_eq!(build_and_run(src), 1);
}

#[test]
fn aot_env_args_content_via_starts_with() {
    // Pass an arg with a known prefix and confirm we see it back.
    let src = r#"
        fn main() -> i64 {
            let args: Vec<str> = std::env::args();
            if args.len() < 2 { return 0; }
            let arg1: str = args.get(1);
            if arg1.starts_with("rune-marker-") { 1 } else { 0 }
        }
    "#;
    assert_eq!(
        build_and_run_with_args(src, &["rune-marker-abc"]),
        1
    );
}
