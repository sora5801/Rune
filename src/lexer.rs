//! Hand-rolled lexer for Rune source code.
//!
//! Operates on UTF-8 input via the `Chars` iterator and tracks byte offsets.
//! Identifiers/keywords are ASCII-only for now; string and char literals
//! accept arbitrary UTF-8 content.
//!
//! The lexer accumulates errors and keeps going past them so a single bad
//! token doesn't prevent the rest of the file from being inspected.

use std::fmt;
use std::str::Chars;

use crate::token::{Span, Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lex error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for LexError {}

pub struct Lexer<'src> {
    source: &'src str,
    chars: Chars<'src>,
    /// Byte offset of the next char. Invariant: `chars` yields source[pos..].
    pos: usize,
    errors: Vec<LexError>,
}

impl<'src> Lexer<'src> {
    pub fn new(source: &'src str) -> Self {
        Self {
            source,
            chars: source.chars(),
            pos: 0,
            errors: Vec::new(),
        }
    }

    pub fn tokenize(mut self) -> (Vec<Token>, Vec<LexError>) {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            let start = self.pos;
            let Some(c) = self.bump() else {
                tokens.push(Token::new(TokenKind::Eof, Span::new(start, start)));
                break;
            };
            let kind = match c {
                c if is_ident_start(c) => self.ident(start),
                c if c.is_ascii_digit() => self.number(start, c),
                '"' => self.string(start),
                '\'' => self.char_literal(start),
                '+' => self.if_eq(TokenKind::Plus, TokenKind::PlusEq),
                '-' => match self.peek() {
                    Some('=') => { self.bump(); TokenKind::MinusEq }
                    Some('>') => { self.bump(); TokenKind::Arrow }
                    _ => TokenKind::Minus,
                },
                '*' => self.if_eq(TokenKind::Star, TokenKind::StarEq),
                '/' => self.if_eq(TokenKind::Slash, TokenKind::SlashEq),
                '%' => self.if_eq(TokenKind::Percent, TokenKind::PercentEq),
                '=' => match self.peek() {
                    Some('=') => { self.bump(); TokenKind::EqEq }
                    Some('>') => { self.bump(); TokenKind::FatArrow }
                    _ => TokenKind::Eq,
                },
                '!' => self.if_eq(TokenKind::Bang, TokenKind::BangEq),
                '<' => match self.peek() {
                    Some('=') => { self.bump(); TokenKind::LtEq }
                    Some('<') => {
                        self.bump();
                        // Session 114: `<<=` shift-left compound assign.
                        if self.peek() == Some('=') {
                            self.bump();
                            TokenKind::ShlEq
                        } else {
                            TokenKind::Shl
                        }
                    }
                    _ => TokenKind::Lt,
                },
                '>' => match self.peek() {
                    Some('=') => { self.bump(); TokenKind::GtEq }
                    Some('>') => {
                        self.bump();
                        // Session 114: `>>=` shift-right compound assign.
                        if self.peek() == Some('=') {
                            self.bump();
                            TokenKind::ShrEq
                        } else {
                            TokenKind::Shr
                        }
                    }
                    _ => TokenKind::Gt,
                },
                '&' => match self.peek() {
                    Some('&') => { self.bump(); TokenKind::AmpAmp }
                    // Session 115: `&=` bit-AND compound assign.
                    Some('=') => { self.bump(); TokenKind::AmpEq }
                    _ => TokenKind::Amp,
                },
                '|' => match self.peek() {
                    Some('|') => { self.bump(); TokenKind::PipePipe }
                    // Session 115: `|=` bit-OR compound assign.
                    Some('=') => { self.bump(); TokenKind::PipeEq }
                    _ => TokenKind::Pipe,
                },
                '^' => match self.peek() {
                    // Session 115: `^=` bit-XOR compound assign.
                    Some('=') => { self.bump(); TokenKind::CaretEq }
                    _ => TokenKind::Caret,
                },
                '~' => TokenKind::Tilde,
                '.' => match self.peek() {
                    Some('.') => {
                        self.bump();
                        if self.peek() == Some('=') {
                            self.bump();
                            TokenKind::DotDotEq
                        } else {
                            TokenKind::DotDot
                        }
                    }
                    _ => TokenKind::Dot,
                },
                ':' => match self.peek() {
                    Some(':') => { self.bump(); TokenKind::ColonColon }
                    _ => TokenKind::Colon,
                },
                '?' => TokenKind::Question,
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semi,
                c => {
                    self.error(start, format!("unexpected character {:?}", c));
                    continue;
                }
            };
            tokens.push(Token::new(kind, Span::new(start, self.pos)));
        }
        (tokens, self.errors)
    }

    fn peek(&self) -> Option<char> {
        self.chars.clone().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut iter = self.chars.clone();
        iter.next()?;
        iter.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn if_eq(&mut self, plain: TokenKind, with_eq: TokenKind) -> TokenKind {
        if self.peek() == Some('=') {
            self.bump();
            with_eq
        } else {
            plain
        }
    }

    fn error(&mut self, start: usize, message: impl Into<String>) {
        self.errors.push(LexError {
            message: message.into(),
            span: Span::new(start, self.pos),
        });
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' { break; }
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => self.block_comment(),
                _ => break,
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.bump(); // /
        self.bump(); // *
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                Some('/') if self.peek() == Some('*') => {
                    self.bump();
                    depth += 1;
                }
                Some('*') if self.peek() == Some('/') => {
                    self.bump();
                    depth -= 1;
                }
                Some(_) => {}
                None => {
                    self.error(start, "unterminated block comment");
                    return;
                }
            }
        }
    }

    fn ident(&mut self, start: usize) -> TokenKind {
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let text = &self.source[start..self.pos];
        TokenKind::keyword_from_str(text)
            .unwrap_or_else(|| TokenKind::Ident(text.to_string()))
    }

    fn number(&mut self, start: usize, first: char) -> TokenKind {
        if first == '0' {
            match self.peek() {
                Some('x') | Some('X') => {
                    self.bump();
                    return self.int_with_radix(start, 16, |c| c.is_ascii_hexdigit());
                }
                Some('b') | Some('B') => {
                    self.bump();
                    return self.int_with_radix(start, 2, |c| c == '0' || c == '1');
                }
                Some('o') | Some('O') => {
                    self.bump();
                    return self.int_with_radix(start, 8, |c| ('0'..='7').contains(&c));
                }
                _ => {}
            }
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
        let mut is_float = false;
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.bump();
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
        }
        let raw = &self.source[start..self.pos];
        let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
        // Session 088: scan a type suffix immediately following the
        // digits — `10i32`, `42u64`, `3.14f32`. Suffix is one of the
        // primitive numeric type names; mismatches (`10f32` /
        // `3.14i64`) error here. `is_float` from the dot/exponent
        // scan above can be lifted to true if the suffix is a float
        // suffix on what looked like an integer (`5f64`).
        let (int_suffix, float_suffix) = self.scan_numeric_suffix(start);
        if int_suffix.is_some() && is_float {
            self.error(
                start,
                "integer suffix on a float literal".to_string(),
            );
        }
        if let Some(_) = float_suffix {
            is_float = true;
        }
        if is_float {
            let v = match cleaned.parse::<f64>() {
                Ok(v) => v,
                Err(_) => {
                    self.error(start, format!("invalid float literal '{}'", raw));
                    0.0
                }
            };
            TokenKind::Float(v, float_suffix)
        } else {
            let v = match cleaned.parse::<i64>() {
                Ok(v) => v,
                Err(_) => {
                    self.error(start, format!("invalid integer literal '{}'", raw));
                    0
                }
            };
            TokenKind::Int(v, int_suffix)
        }
    }

    /// Session 088: peek for a numeric type suffix immediately after
    /// the digits. Returns `(Some(int_ty), None)` for an integer
    /// suffix (`i8`/`i16`/.../`u64`/`isize`/`usize`), `(None,
    /// Some(float_ty))` for `f32`/`f64`, or `(None, None)` if no
    /// suffix. Consumes the suffix's characters on a match;
    /// unrecognized suffix-shaped follow-ons are left in place (the
    /// number ends, an ident token follows).
    fn scan_numeric_suffix(
        &mut self,
        _start: usize,
    ) -> (Option<crate::ty::IntTy>, Option<crate::ty::FloatTy>) {
        let Some(first) = self.peek() else { return (None, None) };
        if first != 'i' && first != 'u' && first != 'f' {
            return (None, None);
        }
        // Collect the suffix into a small buffer (it's 2-5 chars).
        let mut buf = String::new();
        let mut probe = self.pos;
        while let Some(c) = self.source[probe..].chars().next() {
            if c.is_ascii_alphanumeric() {
                buf.push(c);
                probe += c.len_utf8();
            } else {
                break;
            }
        }
        let (int_ty, float_ty) = match buf.as_str() {
            "i8" => (Some(crate::ty::IntTy::I8), None),
            "i16" => (Some(crate::ty::IntTy::I16), None),
            "i32" => (Some(crate::ty::IntTy::I32), None),
            "i64" => (Some(crate::ty::IntTy::I64), None),
            "isize" => (Some(crate::ty::IntTy::ISize), None),
            "u8" => (Some(crate::ty::IntTy::U8), None),
            "u16" => (Some(crate::ty::IntTy::U16), None),
            "u32" => (Some(crate::ty::IntTy::U32), None),
            "u64" => (Some(crate::ty::IntTy::U64), None),
            "usize" => (Some(crate::ty::IntTy::USize), None),
            "f32" => (None, Some(crate::ty::FloatTy::F32)),
            "f64" => (None, Some(crate::ty::FloatTy::F64)),
            _ => return (None, None),
        };
        // Advance past the suffix.
        for _ in 0..buf.len() {
            self.bump();
        }
        (int_ty, float_ty)
    }

    fn int_with_radix(
        &mut self,
        start: usize,
        radix: u32,
        accept: impl Fn(char) -> bool,
    ) -> TokenKind {
        let digits_start = self.pos;
        while self.peek().is_some_and(|c| accept(c) || c == '_') {
            self.bump();
        }
        let raw = &self.source[digits_start..self.pos];
        let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
        if cleaned.is_empty() {
            self.error(start, "expected digits after numeric base prefix");
            return TokenKind::Int(0, None);
        }
        // Session 088: accept integer suffixes on radix-prefixed
        // literals too — `0xff_u8` etc.
        let (int_suffix, float_suffix) = self.scan_numeric_suffix(start);
        if float_suffix.is_some() {
            self.error(start, "float suffix on a radix-prefixed integer literal".to_string());
        }
        match i64::from_str_radix(&cleaned, radix) {
            Ok(v) => TokenKind::Int(v, int_suffix),
            Err(_) => {
                self.error(
                    start,
                    format!("invalid integer literal '{}'", &self.source[start..self.pos]),
                );
                TokenKind::Int(0, int_suffix)
            }
        }
    }

    fn string(&mut self, start: usize) -> TokenKind {
        let mut value = String::new();
        loop {
            match self.bump() {
                Some('"') => return TokenKind::Str(value),
                Some('\\') => match self.escape_char() {
                    Some(c) => value.push(c),
                    None => self.error(start, "invalid escape sequence in string"),
                },
                Some('\n') | None => {
                    self.error(start, "unterminated string literal");
                    return TokenKind::Str(value);
                }
                Some(c) => value.push(c),
            }
        }
    }

    fn escape_char(&mut self) -> Option<char> {
        match self.bump()? {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            '0' => Some('\0'),
            _ => None,
        }
    }

    fn char_literal(&mut self, start: usize) -> TokenKind {
        let c = match self.bump() {
            Some('\\') => match self.escape_char() {
                Some(c) => c,
                None => {
                    self.error(start, "invalid escape sequence in char literal");
                    '\0'
                }
            },
            Some('\'') => {
                self.error(start, "empty char literal");
                return TokenKind::Char('\0');
            }
            Some('\n') | None => {
                self.error(start, "unterminated char literal");
                return TokenKind::Char('\0');
            }
            Some(c) => c,
        };
        if self.peek() != Some('\'') {
            self.error(start, "expected closing ' in char literal");
            return TokenKind::Char(c);
        }
        self.bump();
        TokenKind::Char(c)
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}
