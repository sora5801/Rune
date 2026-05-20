//! Token definitions for Rune.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Ident(String),

    // Keywords
    Let, Mut, Fn, Return,
    If, Else, While, For, In,
    Break, Continue,
    True, False,
    Struct, Enum, Match,
    Pub, Const, As,

    // Arithmetic
    Plus, Minus, Star, Slash, Percent,

    // Assignment / compound assignment
    Eq, PlusEq, MinusEq, StarEq, SlashEq, PercentEq,

    // Comparison
    EqEq, BangEq, Lt, Gt, LtEq, GtEq,

    // Logical
    AmpAmp, PipePipe, Bang,

    // Bitwise
    Amp, Pipe, Caret, Tilde, Shl, Shr,

    // Misc operators
    Arrow,      // ->
    FatArrow,   // =>
    Dot, DotDot, DotDotEq,
    Colon, ColonColon,
    Question,

    // Delimiters
    LParen, RParen,
    LBrace, RBrace,
    LBracket, RBracket,

    // Punctuation
    Comma, Semi,

    // End of input
    Eof,
}

impl TokenKind {
    pub fn keyword_from_str(s: &str) -> Option<Self> {
        Some(match s {
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "fn" => TokenKind::Fn,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "match" => TokenKind::Match,
            "pub" => TokenKind::Pub,
            "const" => TokenKind::Const,
            "as" => TokenKind::As,
            _ => return None,
        })
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Int(v) => write!(f, "int({})", v),
            TokenKind::Float(v) => write!(f, "float({})", v),
            TokenKind::Str(v) => write!(f, "string({:?})", v),
            TokenKind::Char(v) => write!(f, "char({:?})", v),
            TokenKind::Ident(v) => write!(f, "ident({})", v),
            other => write!(f, "{:?}", other),
        }
    }
}
