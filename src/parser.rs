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
    /// In condition / scrutinee position, struct literals (`Path { .. }`)
    /// are ambiguous with the start of the body block. Set when parsing
    /// `if`/`while`/`for`/`match` heads, restored elsewhere.
    no_struct_lit: bool,
    errors: Vec<ParseError>,
}

type ParseResult<T> = Result<T, ParseError>;

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        let src_end = tokens.last().map(|t| t.span.end).unwrap_or(0);
        Self {
            tokens,
            pos: 0,
            src_end,
            no_block_expr: false,
            no_struct_lit: false,
            errors: Vec::new(),
        }
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

    /// Consume one closing `>` of a type-argument list. A `>>` (`Shr`)
    /// token is split in place: one `>` is consumed here, the other is
    /// rewritten to a `Gt` for an enclosing list to consume — so
    /// `Vec<Vec<i64>>` and `Weak<Vec<i64>>` parse without spaces.
    fn expect_generic_close(&mut self) -> ParseResult<Span> {
        match self.peek() {
            TokenKind::Gt => Ok(self.bump().span),
            TokenKind::Shr => {
                let sp = self.peek_span();
                self.tokens[self.pos] = Token {
                    kind: TokenKind::Gt,
                    span: Span::new(sp.start + 1, sp.end),
                };
                Ok(Span::new(sp.start, sp.start + 1))
            }
            _ => {
                let span = self.peek_span();
                Err(ParseError {
                    message: format!(
                        "expected `>`, found {}",
                        describe_kind(self.peek())
                    ),
                    span,
                })
            }
        }
    }

    fn synchronize_item(&mut self) {
        while !self.is_eof() {
            match self.peek() {
                TokenKind::Fn
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Const
                | TokenKind::Pub
                | TokenKind::Impl
                | TokenKind::Trait
                | TokenKind::Mod
                | TokenKind::Use => return,
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
            TokenKind::Impl => Ok(Item::Impl(self.parse_impl(start)?)),
            TokenKind::Trait => Ok(Item::Trait(self.parse_trait(vis, start)?)),
            TokenKind::Mod => Ok(Item::Mod(self.parse_mod(vis, start)?)),
            TokenKind::Use => Ok(Item::Use(self.parse_use(vis, start)?)),
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
        let generics = self.parse_optional_generic_params()?;
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
        Ok(FnDecl { vis, name, generics, params, return_type, body, span: Span::new(start, end) })
    }

    /// Parse `<T>` / `<T, U>` / `<T: Display>` / `<T: A + B>` after an
    /// item's name. Returns an empty Vec if no generic params present.
    fn parse_optional_generic_params(&mut self) -> ParseResult<Vec<GenericParam>> {
        if !self.check(&TokenKind::Lt) {
            return Ok(Vec::new());
        }
        self.bump();
        let mut params = Vec::new();
        if !self.check(&TokenKind::Gt) {
            loop {
                let name = self.expect_ident()?;
                let mut bounds = Vec::new();
                if self.eat(&TokenKind::Colon) {
                    // One or more `+`-separated trait bounds.
                    loop {
                        bounds.push(self.expect_ident()?);
                        if !self.eat(&TokenKind::Plus) {
                            break;
                        }
                    }
                }
                params.push(GenericParam { name, bounds });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::Gt, "`>`")?;
        Ok(params)
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
        let generics = self.parse_optional_generic_params()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            fields.push(self.parse_field()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(StructDecl { vis, name, generics, fields, span: Span::new(start, rb.span.end) })
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
        let generics = self.parse_optional_generic_params()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut variants = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            variants.push(self.parse_variant()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(EnumDecl { vis, name, generics, variants, span: Span::new(start, rb.span.end) })
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

    fn parse_impl(&mut self, start: usize) -> ParseResult<ImplBlock> {
        self.expect(&TokenKind::Impl, "`impl`")?;
        // `impl<T> ...` — type parameters of a generic impl.
        let generics = self.parse_optional_generic_params()?;
        // `impl Path { ... }` is an inherent impl; `impl Path for Path
        // { ... }` is a trait impl. Parse the first path, then peek
        // for `for` to disambiguate.
        let first = self.parse_type_path()?;
        let (trait_path, type_path) = if self.eat_keyword("for") {
            (Some(first), self.parse_type_path()?)
        } else {
            (None, first)
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut assoc_types = Vec::new();
        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            // `type Item = Concrete;` — this impl's binding for an
            // associated type the trait declares.
            if self.check_contextual("type") {
                let t_start = self.peek_span().start;
                self.bump();
                let at_name = self.expect_ident()?;
                self.expect(&TokenKind::Eq, "`=`")?;
                let value = self.parse_type()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                assoc_types.push(AssocTypeBinding {
                    name: at_name,
                    value,
                    span: Span::new(t_start, semi.span.end),
                });
                continue;
            }
            // No visibility on impl methods (`pub` ignored if present today).
            let _ = self.eat(&TokenKind::Pub);
            let fn_start = self.peek_span().start;
            let mut method = self.parse_fn(Visibility::Private, fn_start)?;
            // A generic impl's `<T>` are type parameters of every
            // method — prepend them so each method resolves and
            // monomorphizes as a plain generic function.
            if !generics.is_empty() {
                let mut merged = generics.clone();
                merged.extend(method.generics);
                method.generics = merged;
            }
            methods.push(method);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(ImplBlock {
            generics,
            trait_path,
            type_path,
            assoc_types,
            methods,
            span: Span::new(start, rb.span.end),
        })
    }

    /// Parse a path that may carry generic args (`Foo<T>`) at an
    /// `impl` header position. `parse_path` alone stops at the `<`.
    fn parse_type_path(&mut self) -> ParseResult<Path> {
        let span = self.peek_span();
        match self.parse_type()? {
            Type::Path(p) => Ok(p),
            _ => Err(ParseError {
                message: "expected a type name after `impl`".to_string(),
                span,
            }),
        }
    }

    /// `for` is not a reserved keyword usable as an item connector
    /// elsewhere, so we match the existing `For` token directly.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        if kw == "for" && matches!(self.peek(), TokenKind::For) {
            self.bump();
            return true;
        }
        false
    }

    /// Whether the current token is an identifier equal to `kw` — for
    /// contextual keywords like `type` that are not reserved words.
    fn check_contextual(&self, kw: &str) -> bool {
        matches!(self.peek(), TokenKind::Ident(s) if s.as_str() == kw)
    }

    fn parse_trait(&mut self, vis: Visibility, start: usize) -> ParseResult<TraitDecl> {
        self.expect(&TokenKind::Trait, "`trait`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut assoc_types = Vec::new();
        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            let m_start = self.peek_span().start;
            // `type Item;` — an associated type the trait declares.
            if self.check_contextual("type") {
                self.bump();
                let at_name = self.expect_ident()?;
                let semi = self.expect(&TokenKind::Semi, "`;`")?;
                assoc_types.push(AssocTypeDecl {
                    name: at_name,
                    span: Span::new(m_start, semi.span.end),
                });
                continue;
            }
            // A trait method is a signature: `fn name(params) -> ret;`
            self.expect(&TokenKind::Fn, "`fn`")?;
            let m_name = self.expect_ident()?;
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
            let semi = self.expect(&TokenKind::Semi, "`;`")?;
            methods.push(TraitMethodSig {
                name: m_name,
                params,
                return_type,
                span: Span::new(m_start, semi.span.end),
            });
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(TraitDecl {
            vis,
            name,
            assoc_types,
            methods,
            span: Span::new(start, rb.span.end),
        })
    }

    fn parse_mod(&mut self, vis: Visibility, start: usize) -> ParseResult<ModDecl> {
        self.expect(&TokenKind::Mod, "`mod`")?;
        let name = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut items = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            items.push(self.parse_item()?);
        }
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        Ok(ModDecl {
            vis,
            name,
            items,
            span: Span::new(start, rb.span.end),
        })
    }

    fn parse_use(&mut self, vis: Visibility, start: usize) -> ParseResult<UseDecl> {
        self.expect(&TokenKind::Use, "`use`")?;
        // Parse the path by hand so a trailing `::*` glob terminator
        // doesn't trip `parse_path`'s `expect_ident`.
        let first = self.expect_ident()?;
        let path_start = first.span.start;
        let mut segments = vec![first];
        let mut glob = false;
        while self.eat(&TokenKind::ColonColon) {
            if self.eat(&TokenKind::Star) {
                glob = true;
                break;
            }
            segments.push(self.expect_ident()?);
        }
        let path_end = segments.last().unwrap().span.end;
        let path = Path {
            segments,
            generic_args: Vec::new(),
            span: Span::new(path_start, path_end),
        };
        // `use x as y;` — a glob can't be renamed.
        let alias = if !glob && self.eat(&TokenKind::As) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        let semi = self.expect(&TokenKind::Semi, "`;`")?;
        Ok(UseDecl {
            path,
            glob,
            alias,
            vis,
            span: Span::new(start, semi.span.end),
        })
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
        // `[T; N]` — a fixed-size array type.
        if self.check(&TokenKind::LBracket) {
            let start = self.peek_span().start;
            self.bump(); // `[`
            let elem = self.parse_type()?;
            self.expect(&TokenKind::Semi, "`;`")?;
            let len_span = self.peek_span();
            let len = match self.peek() {
                TokenKind::Int(n) if *n >= 0 => *n as usize,
                _ => {
                    return Err(ParseError {
                        message: "expected a non-negative array length".into(),
                        span: len_span,
                    });
                }
            };
            self.bump(); // the length literal
            let close = self.expect(&TokenKind::RBracket, "`]`")?;
            return Ok(Type::Array {
                elem: Box::new(elem),
                len,
                span: Span::new(start, close.span.end),
            });
        }
        // `dyn TraitName` — a trait object. v0.x: the trait path
        // carries no generic args.
        if self.eat(&TokenKind::Dyn) {
            let path = self.parse_path()?;
            return Ok(Type::Dyn(path));
        }
        // At type position, `Vec<i64>` is unambiguous — the parser
        // can greedily consume the `<...>`.
        let mut path = self.parse_path()?;
        if self.check(&TokenKind::Lt) {
            self.bump(); // consume `<`
            let mut args = Vec::new();
            if !self.check(&TokenKind::Gt) {
                loop {
                    args.push(self.parse_type()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            let gt = self.expect_generic_close()?;
            path.generic_args = args;
            path.span = Span::new(path.span.start, gt.end);
        }
        Ok(Type::Path(path))
    }

    fn parse_path(&mut self) -> ParseResult<Path> {
        // Just the `::`-separated segment list. At expression position
        // `Vec<i64>` would clash with `<` as comparison; turbofish
        // (`Vec::<i64>::new()`) isn't supported yet. Generic args on
        // a type-position path are consumed by parse_type.
        let first = self.expect_ident()?;
        let start = first.span.start;
        let mut segments = vec![first];
        while self.eat(&TokenKind::ColonColon) {
            segments.push(self.expect_ident()?);
        }
        let end = segments.last().unwrap().span.end;
        Ok(Path { segments, generic_args: Vec::new(), span: Span::new(start, end) })
    }

    // ---- patterns ----

    fn parse_pattern(&mut self) -> ParseResult<Pattern> {
        let first = self.parse_pattern_atom()?;
        if !self.check(&TokenKind::Pipe) {
            return Ok(first);
        }
        // Or-pattern: `pat | pat | pat`. Flatten nested Ors as we go.
        let mut patterns = match first {
            Pattern::Or { patterns: ps, .. } => ps,
            other => vec![other],
        };
        let start = patterns[0].span().start;
        while self.eat(&TokenKind::Pipe) {
            let next = self.parse_pattern_atom()?;
            match next {
                Pattern::Or { patterns: more, .. } => patterns.extend(more),
                other => patterns.push(other),
            }
        }
        let end = patterns.last().unwrap().span().end;
        Ok(Pattern::Or { patterns, span: Span::new(start, end) })
    }

    fn parse_pattern_atom(&mut self) -> ParseResult<Pattern> {
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
                let first = self.expect_ident()?;
                if self.check(&TokenKind::ColonColon) {
                    // Multi-segment path pattern, e.g. `Color::Red` or
                    // `Result::Ok(x)`.
                    let start = first.span.start;
                    let mut segments = vec![first];
                    while self.eat(&TokenKind::ColonColon) {
                        segments.push(self.expect_ident()?);
                    }
                    let path_end = segments.last().unwrap().span.end;
                    let path_span = Span::new(start, path_end);
                    let path = Path {
                        segments,
                        generic_args: Vec::new(),
                        span: path_span,
                    };
                    // Tuple-variant destructure: `Variant(pat, ...)`.
                    if self.eat(&TokenKind::LParen) {
                        let mut fields = Vec::new();
                        if !self.check(&TokenKind::RParen) {
                            loop {
                                fields.push(self.parse_pattern()?);
                                if !self.eat(&TokenKind::Comma) {
                                    break;
                                }
                            }
                        }
                        let rp = self.expect(&TokenKind::RParen, "`)`")?;
                        let s = Span::new(start, rp.span.end);
                        return Ok(Pattern::TupleVariant { path, fields, span: s });
                    }
                    // Named-variant destructure: `Variant { name: pat, ... }`
                    // or `Variant { name }` (shorthand binding).
                    if self.eat(&TokenKind::LBrace) {
                        let mut fields: Vec<(Ident, Pattern)> = Vec::new();
                        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
                            let name = self.expect_ident()?;
                            let pat = if self.eat(&TokenKind::Colon) {
                                self.parse_pattern()?
                            } else {
                                // Shorthand: `Variant { x }` binds `x`.
                                let s = name.span;
                                Pattern::Ident {
                                    name: name.clone(),
                                    mutable: false,
                                    span: s,
                                }
                            };
                            fields.push((name, pat));
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
                        let s = Span::new(start, rb.span.end);
                        return Ok(Pattern::NamedVariant { path, fields, span: s });
                    }
                    Ok(Pattern::Path { path, span: path_span })
                } else {
                    let s = first.span;
                    Ok(Pattern::Ident { name: first, mutable: false, span: s })
                }
            }
            TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::Char(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Minus => {
                let (lit, s) = self.parse_pattern_lit()?;
                // After a literal, check for a range pattern `..` / `..=`.
                if let Some(inclusive) = self.peek_range_op() {
                    self.bump(); // consume .. or ..=
                    let (hi, hi_span) = self.parse_pattern_lit()?;
                    return Ok(Pattern::Range {
                        lo: lit,
                        hi,
                        inclusive,
                        span: Span::new(s.start, hi_span.end),
                    });
                }
                Ok(Pattern::Literal { lit, span: s })
            }
            _ => Err(ParseError {
                message: format!("expected pattern, found {}", describe_kind(self.peek())),
                span,
            }),
        }
    }

    /// Parses a single literal usable as a pattern or range bound,
    /// allowing an optional leading `-` on numeric literals.
    /// Negation on non-numeric literals is rejected with an error.
    fn parse_pattern_lit(&mut self) -> ParseResult<(Lit, Span)> {
        let start_span = self.peek_span();
        let negate = self.eat(&TokenKind::Minus);
        let (lit, lit_span) = self.parse_literal()?;
        let lit = if negate {
            match lit {
                Lit::Int(v) => Lit::Int(-v),
                Lit::Float(v) => Lit::Float(-v),
                _ => {
                    return Err(ParseError {
                        message: "unary `-` is only valid on numeric literals in patterns"
                            .into(),
                        span: start_span,
                    });
                }
            }
        } else {
            lit
        };
        let span = if negate {
            Span::new(start_span.start, lit_span.end)
        } else {
            lit_span
        };
        Ok((lit, span))
    }

    /// Returns `Some(inclusive)` if the current token is `..` or `..=`.
    fn peek_range_op(&self) -> Option<bool> {
        match self.peek() {
            TokenKind::DotDot => Some(false),
            TokenKind::DotDotEq => Some(true),
            _ => None,
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
            | TokenKind::Trait
            | TokenKind::Mod
            | TokenKind::Use
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
                InfixKind::Range(inclusive) => {
                    let rhs = self.parse_expr_bp(rbp)?;
                    let span = Span::new(lhs.span().start, rhs.span().end);
                    lhs = Expr::Range {
                        start: Some(Box::new(lhs)),
                        end: Some(Box::new(rhs)),
                        inclusive,
                        span,
                    };
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
            let inner = self.parse_unary()?;
            // Postfix binds tighter than unary: `!f(x)` is `!(f(x))`, not
            // `(!f)(x)`; `-x[0]` is `-(x[0])`; `!a.b` is `!(a.b)`. Apply
            // postfix to the inner expression before wrapping in the
            // unary operator. The outer postfix loop in parse_expr_bp
            // sees no postfix tokens left, so this is the only site.
            let inner = self.parse_postfix_chain(inner)?;
            let end = inner.span().end;
            Ok(Expr::Unary { op, expr: Box::new(inner), span: Span::new(span.start, end) })
        } else {
            self.parse_primary()
        }
    }

    fn parse_postfix_chain(&mut self, mut lhs: Expr) -> ParseResult<Expr> {
        while self.is_postfix_op() {
            lhs = self.parse_postfix(lhs)?;
        }
        Ok(lhs)
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
            TokenKind::Ident(_) => {
                let path = self.parse_path()?;
                // Struct literal: `Path { field: expr, ... }`. Gated by
                // `no_struct_lit` so it doesn't shadow `if cond { body }`.
                if !self.no_struct_lit && matches!(self.peek(), TokenKind::LBrace) {
                    return self.parse_struct_lit(path);
                }
                Ok(Expr::Path(path))
            }
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

    fn parse_struct_lit(&mut self, path: Path) -> ParseResult<Expr> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        let saved_block = self.no_block_expr;
        let saved_struct = self.no_struct_lit;
        // Inside the braces, normal expression rules apply.
        self.no_block_expr = false;
        self.no_struct_lit = false;
        while !self.check(&TokenKind::RBrace) && !self.is_eof() {
            let name = self.expect_ident()?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let value = self.parse_expr()?;
            fields.push(FieldInit { name, value });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.no_block_expr = saved_block;
        self.no_struct_lit = saved_struct;
        let rb = self.expect(&TokenKind::RBrace, "`}`")?;
        let span = Span::new(path.span.start, rb.span.end);
        Ok(Expr::StructLit { path, fields, span })
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
        let saved_block = self.no_block_expr;
        let saved_struct = self.no_struct_lit;
        self.no_block_expr = true;
        self.no_struct_lit = true;
        let e = self.parse_expr();
        self.no_block_expr = saved_block;
        self.no_struct_lit = saved_struct;
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
    /// `..` (false) or `..=` (true).
    Range(bool),
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
        // range — left-associative, lower precedence than logical operators
        TokenKind::DotDot    => InfixOp { kind: InfixKind::Range(false),         bp: (3, 4) },
        TokenKind::DotDotEq  => InfixOp { kind: InfixKind::Range(true),          bp: (3, 4) },
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
