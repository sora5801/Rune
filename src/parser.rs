//! Recursive-descent + Pratt parser for Rune.
//!
//! Entry point: [`Parser::new(tokens).parse_module()`].
//!
//! Expressions use precedence climbing (a simplified Pratt scheme) via
//! `parse_expr_bp`. Items, statements, types, and patterns use straight
//! recursive descent.
//!
//! Error policy: errors are accumulated. On a per-item parse failure the
//! parser synchronizes to the next item-starting keyword and continues.

use std::fmt;

use crate::ast::*;
use crate::token::{Span, Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for ParseError {}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    src_end: usize,
    /// In condition position (if/while/for/match scrutinee), block expressions
    /// are not allowed as primaries — they're the loop/branch body.
    no_block_expr: bool,
    errors: Vec<ParseError>,
}

type ParseResult<T> = Result<T, ParseError>;

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        let src_end = tokens.last().map(|t| t.span.end).unwrap_or(0);
        Self { tokens, pos: 0, src_end, no_block_expr: false, errors: Vec::new() }
    }

    pub fn parse_module(mut self) -> (Module, Vec<ParseError>) {
        let mut items = Vec::new();
        while !self.is_eof() {
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(e) => {
                    self.errors.push(e);
                    self.synchronize_item();
                }
            }
        }
        let span = Span::new(0, self.src_end);
        (Module { items, span }, self.errors)
    }

    // ---- token helpers ----

    fn peek(&self) -> &TokenKind {
        let idx = self.pos.min(self.tokens.len().saturating_sub(1));
        &self.tokens[idx].kind
    }

    fn peek_span(&self) -> Span {
        let idx = self.pos.min(self.tokens.len().saturating_sub(1));
        self.tokens[idx].span
    }

    fn bump(&mut self) -> Token {
        let idx = self.pos.min(self.tokens.len().saturating_sub(1));
        let tok = self.tokens[idx].clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn is_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(self.peek()) == std::mem::discriminant(kind)
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> ParseResult<Token> {
        if self.check(kind) {
            Ok(self.bump())
        } else {
            let span = self.peek_span();
            Err(ParseError {
                message: format!("expected {}, found {}", what, describe_kind(self.peek())),
                span,
            })
        }
    }

    fn expect_ident(&mut self) -> ParseResult<Ident> {
        let span = self.peek_span();
        if let TokenKind::Ident(name) = self.peek() {
            let name = name.clone();
            self.bump();
            Ok(Ident { name, span })
        } else {
            Err(ParseError {
                message: format!("expected identifier, found {}", describe_kind(self.peek())),
                span,
            })
        }
    }

    fn synchronize_item(&mut self) {
        while !self.is_eof() {
            match self.peek() {
                TokenKind::Fn
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Const
                | TokenKind::Pub => return,
                _ => {
                    self.bump();
                }
            }
        }
    }

    // ---- items ----

    fn parse_item(&mut self) -> ParseResult<Item> {
        let start = self.peek_span().start;
        let vis = if self.eat(&TokenKind::Pub) {
            Visibility::Pub
        } else {
            Visibility::Private
        };
        match self.peek() {
            TokenKind::Fn => Ok(Item::Fn(self.parse_fn(vis, start)?)),
            TokenKind::Struct => Ok(Item::Struct(self.parse_struct(vis, start)?)),
            TokenKind::Enum => Ok(Item::Enum(self.parse_enum(vis, start)?)),
            TokenKind::Const => Ok(Item::Const(self.parse_const(vis, start)?)),
            _ => {
                let span = self.peek_span();
                Err(ParseError {
                    message: format!(
                        "expected item (fn, struct, enum, const), found {}",
                        describe_kind(self.peek())
                    ),
                    span,
                })
            }
        }
    }

    fn parse_fn(&mut self, vis: Visibility, start: usize) -> ParseResult<FnDecl> {
        self.expect(&TokenKind::Fn, "`fn`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        while !self.check(&TokenKind::RParen) && !self.is_eof() {
            params.push(self.parse_param()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        let return_type = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(FnDecl { vis, name, params, return_type, body, span: Span::new(start, end) })
    }

    fn parse_param(&mut self) -> ParseResult<Param> {
        let name = self.expect_ident()?;
        let start = name.span.start;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        let end = ty.span().end;
        Ok(Param { name, ty, span: Span::new(start, end) })
    }

    fn parse_struct(&mut self, vis: Visibility, start: usize) -> ParseResult<StructDecl> {
        self.expect(&TokenKind::Struct, "`struct`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            fields.push(self.parse_field()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(StructDecl { vis, name, fields, span: Span::new(start, rb.span.end) })
    }

    fn parse_field(&mut self) -> ParseResult<Field> {
        let start = self.peek_span().start;
        let vis = if self.eat(&TokenKind::Pub) { Visibility::Pub } else { Visibility::Private };
        let name = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        let end = ty.span().end;
        Ok(Field { vis, name, ty, span: Span::new(start, end) })
    }

    fn parse_enum(&mut self, vis: Visibility, start: usize) -> ParseResult<EnumDecl> {
        self.expect(&TokenKind::Enum, "`enum`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut variants = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            variants.push(self.parse_variant()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(EnumDecl { vis, name, variants, span: Span::new(start, rb.span.end) })
    }

    fn parse_variant(&mut self) -> ParseResult<Variant> {
        let name = self.expect_ident()?;
        let start = name.span.start;
        let (fields, end) = if self.eat(&TokenKind::LParen) {
            let mut types = Vec::new();
            while !self.check(&TokenKind::RParen) && !self.is_eof() {
                types.push(self.parse_type()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            let rp = self.expect(&TokenKind::RParen, "`)`")?;
            (VariantFields::Tuple(types), rp.span.end)
        } else if self.eat(&TokenKind::LBrace) {
            let mut fields = Vec::new();
            while !self.check(&TokenKind::RBrace) && !self.is_eof() {
                fields.push(self.parse_field()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            let rb = self.expect(&TokenKind::RBrace, "`}`")?;
            (VariantFields::Named(fields), rb.span.end)
        } else {
            (VariantFields::Unit, name.span.end)
        };
        Ok(Variant { name, fields, span: Span::new(start, end) })
    }

    fn parse_const(&mut self, vis: Visibility, start: usize) -> ParseResult<ConstDecl> {
        self.expect(&TokenKind::Const, "`const`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Ok(ConstDecl { vis, name, ty, value, span: Span::new(start, semi.span.end) })
    }

    // ---- types & paths ----

    fn parse_type(&mut self) -> ParseResult<Type> {
        Ok(Type::Path(self.parse_path()?))
    }

    fn parse_path(&mut self) -> ParseResult<Path> {
        let first = self.expect_ident()?;
        let start = first.span.start;
        let mut segments = vec![first];
        while self.eat(&TokenKind::ColonColon) {
            segments.push(self.expect_ident()?);
        }
        let end = segments.last().unwrap().span.end;
        Ok(Path { segments, span: Span::new(start, end) })
    }

    // ---- patterns ----

    fn parse_pattern(&mut self) -> ParseResult<Pattern> {
        let span = self.peek_span();
        match self.peek() {
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                Ok(Pattern::Wildcard(span))
            }
            TokenKind::Mut => {
                self.bump();
                let id = self.expect_ident()?;
                let full = Span::new(span.start, id.span.end);
                Ok(Pattern::Ident { name: id, mutable: true, span: full })
            }
            TokenKind::Ident(_) => {
                let id = self.expect_ident()?;
                let s = id.span;
                Ok(Pattern::Ident { name: id, mutable: false, span: s })
            }
            TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::Char(_)
            | TokenKind::True
            | TokenKind::False => {
                let (lit, s) = self.parse_literal()?;
                Ok(Pattern::Literal { lit, span: s })
            }
            _ => Err(ParseError {
                message: format!("expected pattern, found {}", describe_kind(self.peek())),
                span,
            }),
        }
    }

    // ---- statements / blocks ----

    fn parse_block(&mut self) -> ParseResult<Block> {
        let lb = self.expect(&TokenKind::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            stmts.push(self.parse_stmt()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Block { stmts, span: Span::new(lb.span.start, rb.span.end) })
    }

    fn parse_stmt(&mut self) -> ParseResult<Stmt> {
        match self.peek() {
            TokenKind::Let => Ok(Stmt::Let(self.parse_let()?)),
            TokenKind::Fn
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Const
            | TokenKind::Pub => Ok(Stmt::Item(self.parse_item()?)),
            _ => {
                let expr = self.parse_expr()?;
                let has_semi = self.eat(&TokenKind::Semi);
                if !has_semi && !self.check(&TokenKind::RBrace) && !expr_can_omit_semi(&expr) {
                    let span = self.peek_span();
                    return Err(ParseError {
                        message: format!(
                            "expected `;` or `}}` after expression, found {}",
                            describe_kind(self.peek())
                        ),
                        span,
                    });
                }
                Ok(Stmt::Expr(expr, has_semi))
            }
        }
    }

    fn parse_let(&mut self) -> ParseResult<LetStmt> {
        let kw = self.expect(&TokenKind::Let, "`let`")?;
        let mutable = self.eat(&TokenKind::Mut);
        let pat = self.parse_pattern()?;
        let ty = if self.eat(&TokenKind::Colon) { Some(self.parse_type()?) } else { None };
        let init = if self.eat(&TokenKind::Eq) { Some(self.parse_expr()?) } else { None };
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Ok(LetStmt { mutable, pat, ty, init, span: Span::new(kw.span.start, semi.span.end) })
    }

    // ---- expressions ----

    pub fn parse_expr(&mut self) -> ParseResult<Expr> {
        self.parse_expr_bp(0)
    }

    fn parse_expr_bp(&mut self, min_bp: u8) -> ParseResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            if self.is_postfix_op() {
                lhs = self.parse_postfix(lhs)?;
                continue;
            }
            let op_info = match infix_binding_power(self.peek()) {
                Some(info) => info,
                None => break,
            };
            let (lbp, rbp) = op_info.bp;
            if lbp < min_bp {
                break;
            }
            self.bump();
            match op_info.kind {
                InfixKind::BinOp(op) => {
                    let rhs = self.parse_expr_bp(rbp)?;
                    let span = Span::new(lhs.span().start, rhs.span().end);
                    lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
                }
                InfixKind::Assign => {
                    let rhs = self.parse_expr_bp(rbp)?;
                    let span = Span::new(lhs.span().start, rhs.span().end);
                    lhs = Expr::Assign { lhs: Box::new(lhs), rhs: Box::new(rhs), span };
                }
                InfixKind::AssignOp(op) => {
                    let rhs = self.parse_expr_bp(rbp)?;
                    let span = Span::new(lhs.span().start, rhs.span().end);
                    lhs = Expr::AssignOp { op, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
                }
                InfixKind::Cast => {
                    let ty = self.parse_type()?;
                    let end = ty.span().end;
                    let span = Span::new(lhs.span().start, end);
                    lhs = Expr::Cast { expr: Box::new(lhs), ty, span };
                }
            }
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> ParseResult<Expr> {
        let span = self.peek_span();
        let op = match self.peek() {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Bang => Some(UnOp::Not),
            TokenKind::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let expr = self.parse_unary()?;
            let end = expr.span().end;
            Ok(Expr::Unary { op, expr: Box::new(expr), span: Span::new(span.start, end) })
        } else {
            self.parse_primary()
        }
    }

    fn is_postfix_op(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::Dot
                | TokenKind::Question
        )
    }

    fn parse_postfix(&mut self, lhs: Expr) -> ParseResult<Expr> {
        match self.peek() {
            TokenKind::LParen => {
                self.bump();
                let saved = self.no_block_expr;
                self.no_block_expr = false;
                let mut args = Vec::new();
                while !self.check(&TokenKind::RParen) && !self.is_eof() {
                    args.push(self.parse_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.no_block_expr = saved;
                let rp = self.expect(&TokenKind::RParen, "`)`")?;
                let span = Span::new(lhs.span().start, rp.span.end);
                Ok(Expr::Call { callee: Box::new(lhs), args, span })
            }
            TokenKind::LBracket => {
                self.bump();
                let saved = self.no_block_expr;
                self.no_block_expr = false;
                let index = self.parse_expr()?;
                self.no_block_expr = saved;
                let rb = self.expect(&TokenKind::RBracket, "`]`")?;
                let span = Span::new(lhs.span().start, rb.span.end);
                Ok(Expr::Index { receiver: Box::new(lhs), index: Box::new(index), span })
            }
            TokenKind::Dot => {
                self.bump();
                let name = self.expect_ident()?;
                if self.eat(&TokenKind::LParen) {
                    let saved = self.no_block_expr;
                    self.no_block_expr = false;
                    let mut args = Vec::new();
                    while !self.check(&TokenKind::RParen) && !self.is_eof() {
                        args.push(self.parse_expr()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.no_block_expr = saved;
                    let rp = self.expect(&TokenKind::RParen, "`)`")?;
                    let span = Span::new(lhs.span().start, rp.span.end);
                    Ok(Expr::MethodCall { receiver: Box::new(lhs), method: name, args, span })
                } else {
                    let span = Span::new(lhs.span().start, name.span.end);
                    Ok(Expr::Field { receiver: Box::new(lhs), name, span })
                }
            }
            TokenKind::Question => {
                let q = self.bump();
                let span = Span::new(lhs.span().start, q.span.end);
                Ok(Expr::Try { expr: Box::new(lhs), span })
            }
            _ => unreachable!("parse_postfix called when not at postfix op"),
        }
    }

    fn parse_primary(&mut self) -> ParseResult<Expr> {
        let span = self.peek_span();
        let kind = self.peek().clone();
        match kind {
            TokenKind::Int(v) => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Int(v), span })
            }
            TokenKind::Float(v) => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Float(v), span })
            }
            TokenKind::Str(s) => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Str(s), span })
            }
            TokenKind::Char(c) => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Char(c), span })
            }
            TokenKind::True => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Bool(true), span })
            }
            TokenKind::False => {
                self.bump();
                Ok(Expr::Lit { lit: Lit::Bool(false), span })
            }
            TokenKind::Ident(_) => Ok(Expr::Path(self.parse_path()?)),
            TokenKind::LParen => {
                self.bump();
                let saved = self.no_block_expr;
                self.no_block_expr = false;
                let e = self.parse_expr()?;
                self.no_block_expr = saved;
                self.expect(&TokenKind::RParen, "`)`")?;
                Ok(e)
            }
            TokenKind::LBracket => {
                self.bump();
                let saved = self.no_block_expr;
                self.no_block_expr = false;
                let mut elems = Vec::new();
                while !self.check(&TokenKind::RBracket) && !self.is_eof() {
                    elems.push(self.parse_expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.no_block_expr = saved;
                let rb = self.expect(&TokenKind::RBracket, "`]`")?;
                Ok(Expr::Array { elems, span: Span::new(span.start, rb.span.end) })
            }
            TokenKind::LBrace if !self.no_block_expr => {
                Ok(Expr::Block(self.parse_block()?))
            }
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Match => self.parse_match(),
            TokenKind::Return => {
                self.bump();
                let value = if self.check(&TokenKind::Semi)
                    || self.check(&TokenKind::RBrace)
                    || self.check(&TokenKind::Comma)
                    || self.is_eof()
                {
                    None
                } else {
                    Some(Box::new(self.parse_expr()?))
                };
                let end = value.as_ref().map(|e| e.span().end).unwrap_or(span.end);
                Ok(Expr::Return { value, span: Span::new(span.start, end) })
            }
            TokenKind::Break => {
                self.bump();
                Ok(Expr::Break(span))
            }
            TokenKind::Continue => {
                self.bump();
                Ok(Expr::Continue(span))
            }
            _ => Err(ParseError {
                message: format!("expected expression, found {}", describe_kind(self.peek())),
                span,
            }),
        }
    }

    fn parse_if(&mut self) -> ParseResult<Expr> {
        let kw = self.expect(&TokenKind::If, "`if`")?;
        let cond = self.parse_cond_expr()?;
        let then_branch = self.parse_block()?;
        let mut end = then_branch.span.end;
        let else_branch = if self.eat(&TokenKind::Else) {
            let e = if matches!(self.peek(), TokenKind::If) {
                self.parse_if()?
            } else {
                Expr::Block(self.parse_block()?)
            };
            end = e.span().end;
            Some(Box::new(e))
        } else {
            None
        };
        Ok(Expr::If {
            cond: Box::new(cond),
            then_branch,
            else_branch,
            span: Span::new(kw.span.start, end),
        })
    }

    fn parse_while(&mut self) -> ParseResult<Expr> {
        let kw = self.expect(&TokenKind::While, "`while`")?;
        let cond = self.parse_cond_expr()?;
        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(Expr::While { cond: Box::new(cond), body, span: Span::new(kw.span.start, end) })
    }

    fn parse_for(&mut self) -> ParseResult<Expr> {
        let kw = self.expect(&TokenKind::For, "`for`")?;
        let pat = self.parse_pattern()?;
        self.expect(&TokenKind::In, "`in`")?;
        let iter = self.parse_cond_expr()?;
        let body = self.parse_block()?;
        let end = body.span.end;
        Ok(Expr::For { pat, iter: Box::new(iter), body, span: Span::new(kw.span.start, end) })
    }

    fn parse_match(&mut self) -> ParseResult<Expr> {
        let kw = self.expect(&TokenKind::Match, "`match`")?;
        let scrutinee = self.parse_cond_expr()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut arms = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            arms.push(self.parse_match_arm()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span: Span::new(kw.span.start, rb.span.end),
        })
    }

    fn parse_match_arm(&mut self) -> ParseResult<MatchArm> {
        let pat = self.parse_pattern()?;
        let start = pat.span().start;
        let guard = if self.eat(&TokenKind::If) {
            Some(self.parse_cond_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::FatArrow, "`=>`")?;
        let body = self.parse_expr()?;
        let end = body.span().end;
        self.eat(&TokenKind::Comma);
        Ok(MatchArm { pat, guard, body, span: Span::new(start, end) })
    }

    fn parse_cond_expr(&mut self) -> ParseResult<Expr> {
        let saved = self.no_block_expr;
        self.no_block_expr = true;
        let e = self.parse_expr();
        self.no_block_expr = saved;
        e
    }

    fn parse_literal(&mut self) -> ParseResult<(Lit, Span)> {
        let span = self.peek_span();
        let kind = self.peek().clone();
        match kind {
            TokenKind::Int(v) => { self.bump(); Ok((Lit::Int(v), span)) }
            TokenKind::Float(v) => { self.bump(); Ok((Lit::Float(v), span)) }
            TokenKind::Str(s) => { self.bump(); Ok((Lit::Str(s), span)) }
            TokenKind::Char(c) => { self.bump(); Ok((Lit::Char(c), span)) }
            TokenKind::True => { self.bump(); Ok((Lit::Bool(true), span)) }
            TokenKind::False => { self.bump(); Ok((Lit::Bool(false), span)) }
            _ => Err(ParseError {
                message: format!("expected literal, found {}", describe_kind(self.peek())),
                span,
            }),
        }
    }
}

// ---- precedence table ----

struct InfixOp {
    kind: InfixKind,
    /// (left binding power, right binding power). Left-assoc: lbp < rbp.
    /// Right-assoc: lbp > rbp.
    bp: (u8, u8),
}

enum InfixKind {
    BinOp(BinOp),
    Assign,
    AssignOp(BinOp),
    Cast,
}

fn infix_binding_power(tok: &TokenKind) -> Option<InfixOp> {
    Some(match tok {
        // assignment — right-associative, lowest precedence
        TokenKind::Eq        => InfixOp { kind: InfixKind::Assign,             bp: (2, 1) },
        TokenKind::PlusEq    => InfixOp { kind: InfixKind::AssignOp(BinOp::Add), bp: (2, 1) },
        TokenKind::MinusEq   => InfixOp { kind: InfixKind::AssignOp(BinOp::Sub), bp: (2, 1) },
        TokenKind::StarEq    => InfixOp { kind: InfixKind::AssignOp(BinOp::Mul), bp: (2, 1) },
        TokenKind::SlashEq   => InfixOp { kind: InfixKind::AssignOp(BinOp::Div), bp: (2, 1) },
        TokenKind::PercentEq => InfixOp { kind: InfixKind::AssignOp(BinOp::Mod), bp: (2, 1) },
        // logical
        TokenKind::PipePipe  => InfixOp { kind: InfixKind::BinOp(BinOp::Or),     bp: (5, 6) },
        TokenKind::AmpAmp    => InfixOp { kind: InfixKind::BinOp(BinOp::And),    bp: (7, 8) },
        // comparison
        TokenKind::EqEq      => InfixOp { kind: InfixKind::BinOp(BinOp::Eq),     bp: (9, 10) },
        TokenKind::BangEq    => InfixOp { kind: InfixKind::BinOp(BinOp::Ne),     bp: (9, 10) },
        TokenKind::Lt        => InfixOp { kind: InfixKind::BinOp(BinOp::Lt),     bp: (9, 10) },
        TokenKind::Gt        => InfixOp { kind: InfixKind::BinOp(BinOp::Gt),     bp: (9, 10) },
        TokenKind::LtEq      => InfixOp { kind: InfixKind::BinOp(BinOp::Le),     bp: (9, 10) },
        TokenKind::GtEq      => InfixOp { kind: InfixKind::BinOp(BinOp::Ge),     bp: (9, 10) },
        // bitwise
        TokenKind::Pipe      => InfixOp { kind: InfixKind::BinOp(BinOp::BitOr),  bp: (11, 12) },
        TokenKind::Caret     => InfixOp { kind: InfixKind::BinOp(BinOp::BitXor), bp: (13, 14) },
        TokenKind::Amp       => InfixOp { kind: InfixKind::BinOp(BinOp::BitAnd), bp: (15, 16) },
        TokenKind::Shl       => InfixOp { kind: InfixKind::BinOp(BinOp::Shl),    bp: (17, 18) },
        TokenKind::Shr       => InfixOp { kind: InfixKind::BinOp(BinOp::Shr),    bp: (17, 18) },
        // arithmetic
        TokenKind::Plus      => InfixOp { kind: InfixKind::BinOp(BinOp::Add),    bp: (19, 20) },
        TokenKind::Minus     => InfixOp { kind: InfixKind::BinOp(BinOp::Sub),    bp: (19, 20) },
        TokenKind::Star      => InfixOp { kind: InfixKind::BinOp(BinOp::Mul),    bp: (21, 22) },
        TokenKind::Slash     => InfixOp { kind: InfixKind::BinOp(BinOp::Div),    bp: (21, 22) },
        TokenKind::Percent   => InfixOp { kind: InfixKind::BinOp(BinOp::Mod),    bp: (21, 22) },
        // cast
        TokenKind::As        => InfixOp { kind: InfixKind::Cast,                 bp: (23, 24) },
        _ => return None,
    })
}

fn expr_can_omit_semi(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If { .. }
            | Expr::While { .. }
            | Expr::For { .. }
            | Expr::Match { .. }
            | Expr::Block(_)
    )
}

fn describe_kind(k: &TokenKind) -> String {
    match k {
        TokenKind::Ident(s) => format!("identifier `{}`", s),
        TokenKind::Int(v) => format!("integer `{}`", v),
        TokenKind::Float(v) => format!("float `{}`", v),
        TokenKind::Str(_) => "string literal".into(),
        TokenKind::Char(_) => "char literal".into(),
        TokenKind::Eof => "end of input".into(),
        other => format!("`{:?}`", other),
    }
}
