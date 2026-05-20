use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rune::{
    aot, Checker, Codegen, Lexer, Lowerer, OptLevel, Parser, Resolver, Resolutions, SymbolId,
    SymbolKind,
};

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
        "build" => cmd_build(&args),
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
    eprintln!("  tokens <file>           print tokens from a source file");
    eprintln!("  ast <file>              parse and print the AST");
    eprintln!("  check <file>            parse, resolve names, type-check");
    eprintln!("  run <file>              JIT-compile and execute `main() -> i64`");
    eprintln!("  build <file> [-o out] [--release]   AOT-compile to a native executable");
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
    let mut cg = match Codegen::new_jit() {
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
    if let Err(e) = cg.finalize() {
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

    let main_fn: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
    let result = main_fn();
    println!("{}", result);
    ExitCode::SUCCESS
}

fn cmd_build(args: &[String]) -> ExitCode {
    let (input_path, output_override, release) = parse_build_args(args);
    let Some(input_path) = input_path else {
        eprintln!("usage: rune build <file> [-o output] [--release]");
        return ExitCode::from(2);
    };
    let output_path = output_override
        .unwrap_or_else(|| derive_default_output_path(&input_path));

    let source = match read_source(&input_path) {
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

    let mut hir = Lowerer::new(&resolutions, &check_results).lower_module(&module);

    let module_name = Path::new(&input_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rune_module")
        .to_string();

    let opt = if release { OptLevel::Speed } else { OptLevel::None };
    let bytes = match aot::build_object(&mut hir, &module_name, opt) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("rune: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let obj_path = output_path.with_extension("o");
    if let Err(e) = fs::write(&obj_path, &bytes) {
        eprintln!("rune: writing {}: {}", obj_path.display(), e);
        return ExitCode::FAILURE;
    }

    match aot::link(&obj_path, &output_path) {
        Ok(linker) => {
            println!("rune: linked with {} -> {}", linker, output_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rune: link failed");
            eprintln!("{}", e);
            ExitCode::FAILURE
        }
    }
}

fn parse_build_args(args: &[String]) -> (Option<String>, Option<PathBuf>, bool) {
    let mut input: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut release = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--release" => {
                release = true;
                i += 1;
            }
            "-o" => {
                if let Some(next) = args.get(i + 1) {
                    output = Some(PathBuf::from(next));
                }
                i += 2;
            }
            s if !s.starts_with('-') => {
                if input.is_none() {
                    input = Some(args[i].clone());
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    (input, output, release)
}

fn derive_default_output_path(input_path: &str) -> PathBuf {
    let stem = Path::new(input_path).file_stem().unwrap_or_default();
    let mut out = PathBuf::from(stem);
    if cfg!(windows) {
        out.set_extension("exe");
    }
    out
}

fn find_main(res: &Resolutions) -> Option<SymbolId> {
    res.symbols
        .iter()
        .enumerate()
        .find(|(_, s)| s.name == "main" && matches!(s.kind, SymbolKind::Fn))
        .map(|(i, _)| SymbolId(i as u32))
}
