use std::env;
use std::fs;
use std::process::ExitCode;

use rune::{Lexer, Parser};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "tokens" => cmd_tokens(&args),
        "ast" => cmd_ast(&args),
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
    if !lex_errors.is_empty() {
        eprintln!();
        for err in &lex_errors {
            eprintln!("{}", err);
        }
        had_errors = true;
    }
    if !parse_errors.is_empty() {
        if lex_errors.is_empty() {
            eprintln!();
        }
        for err in &parse_errors {
            eprintln!("{}", err);
        }
        had_errors = true;
    }
    if had_errors { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
