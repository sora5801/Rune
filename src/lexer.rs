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
                    Some('<') => { self.bump(); TokenKind::Shl }
                    _ => TokenKind::Lt,
                },
                '>' => match self.peek() {
                    Some('=') => { self.bump(); TokenKind::GtEq }
                    Some('>') => { self.bump(); TokenKind::Shr }
                    _ => TokenKind::Gt,
                },
                '&' => match self.peek() {
                    Some('&') => { self.bump(); TokenKind::AmpAmp }
                    _ => TokenKind::Amp,
                },
                '|' => match self.peek() {
                    Some('|') => { self.bump(); TokenKind::PipePipe }
                    _ => TokenKind::Pipe,
                },
                '^' => TokenKind::Caret,
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
        if is_float {
            match cleaned.parse::<f64>() {
                Ok(v) => TokenKind::Float(v),
                Err(_) => {
                    self.error(start, format!("invalid float literal '{}'", raw));
                    TokenKind::Float(0.0)
                }
            }
        } else {
            match cleaned.parse::<i64>() {
                Ok(v) => TokenKind::Int(v),
                Err(_) => {
                    self.error(start, format!("invalid integer literal '{}'", raw));
                    TokenKind::Int(0)
                }
            }
        }
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
            return TokenKind::Int(0);
        }
        match i64::from_str_radix(&cleaned, radix) {
            Ok(v) => TokenKind::Int(v),
            Err(_) => {
                self.error(
                    start,
                    format!("invalid integer literal '{}'", &self.source[start..self.pos]),
                );
                TokenKind::Int(0)
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
