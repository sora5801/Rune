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
