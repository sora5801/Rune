use std::env;
use std::fs;
use std::process::ExitCode;

use rune::{Checker, Codegen, Lexer, Lowerer, Parser, Resolver, Resolutions, SymbolId, SymbolKind};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "tokens" => cmd_tokens(&args),
        "ast" => cmd_ast(&args),
        "check" => cmd_check(&args),
        "run" => cmd_run(&args),
        "--help" | "-h" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("rune: unknown command '{}'", other);
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!("usage: rune <command> [args]");
    eprintln!();
    eprintln!("commands:");
    eprintln!("  tokens <file>    print tokens from a source file");
    eprintln!("  ast <file>       parse and print the AST");
    eprintln!("  check <file>     parse, resolve names, type-check");
    eprintln!("  run <file>       JIT-compile and execute `main() -> i64`");
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    fs::read_to_string(path).map_err(|e| {
        eprintln!("rune: error reading {}: {}", path, e);
        ExitCode::FAILURE
    })
}

fn cmd_tokens(args: &[String]) -> ExitCode {
    let Some(path) = args.get(1) else {
        eprintln!("usage: rune tokens <file>");
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let (tokens, errors) = Lexer::new(&source).tokenize();
    for tok in &tokens {
        println!("{:>5}..{:<5}  {}", tok.span.start, tok.span.end, tok.kind);
    }
    if !errors.is_empty() {
        eprintln!();
        for err in &errors {
            eprintln!("{}", err);
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn cmd_ast(args: &[String]) -> ExitCode {
    let Some(path) = args.get(1) else {
        eprintln!("usage: rune ast <file>");
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let (tokens, lex_errors) = Lexer::new(&source).tokenize();
    let (module, parse_errors) = Parser::new(tokens).parse_module();
    println!("{:#?}", module);
    let mut had_errors = false;
    for err in &lex_errors {
        eprintln!("{}", err);
        had_errors = true;
    }
    for err in &parse_errors {
        eprintln!("{}", err);
        had_errors = true;
    }
    if had_errors { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn cmd_check(args: &[String]) -> ExitCode {
    let Some(path) = args.get(1) else {
        eprintln!("usage: rune check <file>");
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let (tokens, lex_errors) = Lexer::new(&source).tokenize();
    let (module, parse_errors) = Parser::new(tokens).parse_module();
    let (resolutions, resolve_errors) = Resolver::new().resolve_module(&module);
    let check_results = Checker::new(&resolutions).check_module(&module);

    let mut had_errors = false;
    for err in &lex_errors { eprintln!("{}", err); had_errors = true; }
    for err in &parse_errors { eprintln!("{}", err); had_errors = true; }
    for err in &resolve_errors { eprintln!("{}", err); had_errors = true; }
    for err in &check_results.errors { eprintln!("{}", err); had_errors = true; }
    if !had_errors {
        println!("ok");
    }
    if had_errors { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn cmd_run(args: &[String]) -> ExitCode {
    let Some(path) = args.get(1) else {
        eprintln!("usage: rune run <file>");
        return ExitCode::from(2);
    };
    let source = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let (tokens, lex_errors) = Lexer::new(&source).tokenize();
    let (module, parse_errors) = Parser::new(tokens).parse_module();
    let (resolutions, resolve_errors) = Resolver::new().resolve_module(&module);
    let check_results = Checker::new(&resolutions).check_module(&module);
    let mut had_errors = false;
    for err in &lex_errors { eprintln!("{}", err); had_errors = true; }
    for err in &parse_errors { eprintln!("{}", err); had_errors = true; }
    for err in &resolve_errors { eprintln!("{}", err); had_errors = true; }
    for err in &check_results.errors { eprintln!("{}", err); had_errors = true; }
    if had_errors {
        return ExitCode::FAILURE;
    }

    let hir = Lowerer::new(&resolutions, &check_results).lower_module(&module);
    let mut cg = match Codegen::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rune: {}", e);
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = cg.compile_module(&hir) {
        eprintln!("rune: {}", e);
        return ExitCode::FAILURE;
    }

    let Some(main_sym) = find_main(&resolutions) else {
        eprintln!("rune: no `main` function found");
        return ExitCode::FAILURE;
    };

    let Some(ptr) = cg.get_function_ptr(main_sym) else {
        eprintln!("rune: `main` was not compiled");
        return ExitCode::FAILURE;
    };

    // `main` must be `fn() -> i64` per session-004 contract.
    let main_fn: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = main_fn();
    println!("{}", result);
    ExitCode::SUCCESS
}

fn find_main(res: &Resolutions) -> Option<SymbolId> {
    res.symbols
        .iter()
        .enumerate()
        .find(|(_, s)| s.name == "main" && matches!(s.kind, SymbolKind::Fn))
        .map(|(i, _)| SymbolId(i as u32))
}
