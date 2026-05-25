use rune::{Lexer, TokenKind};

fn tokens_of(src: &str) -> Vec<TokenKind> {
    let (tokens, errors) = Lexer::new(src).tokenize();
    assert!(errors.is_empty(), "unexpected lex errors: {:?}", errors);
    tokens.into_iter().map(|t| t.kind).collect()
}

#[test]
fn empty_input() {
    assert_eq!(tokens_of(""), vec![TokenKind::Eof]);
}

#[test]
fn skips_whitespace() {
    assert_eq!(tokens_of("   \t\r\n  "), vec![TokenKind::Eof]);
}

#[test]
fn keywords_and_idents() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("let mut fn return if else while for in break continue true false struct enum match pub const as"),
        vec![
            Let, Mut, Fn, Return, If, Else, While, For, In,
            Break, Continue, True, False, Struct, Enum, Match,
            Pub, Const, As, Eof,
        ]
    );
}

#[test]
fn ident_shapes() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("x foo_bar _Baz CamelCase n1 _0"),
        vec![
            Ident("x".into()),
            Ident("foo_bar".into()),
            Ident("_Baz".into()),
            Ident("CamelCase".into()),
            Ident("n1".into()),
            Ident("_0".into()),
            Eof,
        ]
    );
}

#[test]
fn decimal_integers() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("0 1 42 1_000_000"),
        vec![Int(0, None), Int(1, None), Int(42, None), Int(1_000_000, None), Eof]
    );
}

#[test]
fn radix_integers() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("0xff 0xFF 0b1010 0o17"),
        vec![Int(0xff, None), Int(0xff, None), Int(0b1010, None), Int(0o17, None), Eof]
    );
}

#[test]
fn float_literals() {
    use TokenKind::*;
    let toks = tokens_of("3.14 1e10 2.5e-3");
    assert!(matches!(toks[0], Float(f, _) if (f - 3.14).abs() < 1e-9));
    assert!(matches!(toks[1], Float(f, _) if (f - 1e10).abs() < 1.0));
    assert!(matches!(toks[2], Float(f, _) if (f - 2.5e-3).abs() < 1e-9));
    assert_eq!(toks[3], Eof);
}

#[test]
fn dot_after_int_is_not_a_float() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("1..10"),
        vec![Int(1, None), DotDot, Int(10, None), Eof]
    );
}

#[test]
fn strings_with_escapes() {
    use TokenKind::*;
    assert_eq!(
        tokens_of(r#""hello" "a\nb" "tab\there" "quote\"end""#),
        vec![
            Str("hello".into()),
            Str("a\nb".into()),
            Str("tab\there".into()),
            Str("quote\"end".into()),
            Eof,
        ]
    );
}

#[test]
fn char_literals() {
    use TokenKind::*;
    assert_eq!(
        tokens_of(r"'a' 'Z' '0' '\n' '\\' '\''"),
        vec![
            Char('a'),
            Char('Z'),
            Char('0'),
            Char('\n'),
            Char('\\'),
            Char('\''),
            Eof,
        ]
    );
}

#[test]
fn single_char_operators() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("+ - * / % = < > ! & | ^ ~ ? ."),
        vec![Plus, Minus, Star, Slash, Percent, Eq, Lt, Gt, Bang,
             Amp, Pipe, Caret, Tilde, Question, Dot, Eof]
    );
}

#[test]
fn multi_char_operators() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("== != <= >= && || << >> -> => :: .. ..="),
        vec![EqEq, BangEq, LtEq, GtEq, AmpAmp, PipePipe,
             Shl, Shr, Arrow, FatArrow, ColonColon, DotDot, DotDotEq, Eof]
    );
}

#[test]
fn compound_assign() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("+= -= *= /= %="),
        vec![PlusEq, MinusEq, StarEq, SlashEq, PercentEq, Eof]
    );
}

#[test]
fn delimiters_and_punctuation() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("(){}[],;:"),
        vec![LParen, RParen, LBrace, RBrace, LBracket, RBracket,
             Comma, Semi, Colon, Eof]
    );
}

#[test]
fn line_comments_skipped() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("let // a comment\nx"),
        vec![Let, Ident("x".into()), Eof]
    );
}

#[test]
fn nested_block_comments() {
    use TokenKind::*;
    assert_eq!(
        tokens_of("let /* outer /* inner */ still in */ x"),
        vec![Let, Ident("x".into()), Eof]
    );
}

#[test]
fn spans_are_byte_offsets() {
    let (tokens, _) = Lexer::new("let xy").tokenize();
    assert_eq!(tokens[0].span.start, 0);
    assert_eq!(tokens[0].span.end, 3);
    assert_eq!(tokens[1].span.start, 4);
    assert_eq!(tokens[1].span.end, 6);
}

#[test]
fn unterminated_string_is_error() {
    let (_, errors) = Lexer::new(r#""hi"#).tokenize();
    assert!(!errors.is_empty());
}

#[test]
fn unterminated_block_comment_is_error() {
    let (_, errors) = Lexer::new("/* unterminated").tokenize();
    assert!(!errors.is_empty());
}

#[test]
fn unexpected_character_is_error() {
    let (_, errors) = Lexer::new("let x = #").tokenize();
    assert!(!errors.is_empty());
}

#[test]
fn fibonacci_sample_lexes_cleanly() {
    let src = r#"
        fn fib(n: i64) -> i64 {
            if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
    "#;
    let (_, errors) = Lexer::new(src).tokenize();
    assert!(errors.is_empty(), "expected clean lex, got {:?}", errors);
}
