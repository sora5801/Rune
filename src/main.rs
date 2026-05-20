use std::env;
use std::fs;
use std::process::ExitCode;

use rune::Lexer;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_usage();
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "tokens" => cmd_tokens(&args),
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
}

fn cmd_tokens(args: &[String]) -> ExitCode {
    let Some(path) = args.get(1) else {
        eprintln!("usage: rune tokens <file>");
        return ExitCode::from(2);
    };
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rune: error reading {}: {}", path, e);
            return ExitCode::FAILURE;
        }
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
