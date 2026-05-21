//! File-based module expansion.
//!
//! `mod name;` (no body) loads `name.rn` and splices its items in as
//! though the user had written `mod name { ... }`. Expansion is a
//! token-stream transformation between lexing and parsing, so the
//! parser, resolver, and the rest of the pipeline never see a
//! file-backed module — only inline ones. `mod name;` is rewritten
//! at the token level: `mod name ;` → `mod name { <name.rn tokens> }`.
//!
//! Each loaded file is lexed into a fresh, disjoint slice of the
//! global byte-offset space — its token spans are shifted by a base
//! offset — so spans stay globally unique. The resolver and checker
//! key several maps on `Span`, and two files lexed independently
//! would otherwise collide at the same low offsets. A `SourceMap`
//! records which file owns which range so an error offset can be
//! traced back to a file.
//!
//! v0.x keeps module files flat: `mod foo;` always loads `foo.rn`
//! from the main file's directory, regardless of nesting depth.

use crate::lexer::{LexError, Lexer};
use crate::token::{Span, Token, TokenKind};

/// A `mod name;` whose file is missing, or that forms an import
/// cycle. A separate error category from lex/parse/resolve/type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "module error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for ModuleError {}

/// Records which source file occupies which slice of the global
/// byte-offset space, so an error's offset can be attributed.
pub struct SourceMap {
    files: Vec<SourceFile>,
}

struct SourceFile {
    label: String,
    start: usize,
    end: usize,
}

impl SourceMap {
    /// File label + file-local offset for a global byte offset.
    pub fn locate(&self, offset: usize) -> Option<(&str, usize)> {
        self.files
            .iter()
            .find(|f| offset >= f.start && offset < f.end)
            .map(|f| (f.label.as_str(), offset - f.start))
    }

    /// True once a module file was loaded beyond the main source.
    pub fn is_multi_file(&self) -> bool {
        self.files.len() > 1
    }

    /// One `  label: start..end` line per mapped file.
    pub fn summary(&self) -> String {
        self.files
            .iter()
            .map(|f| format!("  {}: {}..{}", f.label, f.start, f.end))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A token stream ready for the parser, plus everything gathered
/// during expansion.
pub struct Expansion {
    pub tokens: Vec<Token>,
    pub lex_errors: Vec<LexError>,
    pub module_errors: Vec<ModuleError>,
    pub source_map: SourceMap,
}

/// Lex `main_source` and expand every `mod name;` by loading and
/// splicing `name.rn` via `loader` (a module name → source-text
/// lookup). The returned token stream contains only inline modules.
pub fn expand_modules(
    main_source: &str,
    main_label: &str,
    loader: &dyn Fn(&str) -> Option<String>,
) -> Expansion {
    let (main_tokens, main_lex_errors) = Lexer::new(main_source).tokenize();
    let mut exp = Expander {
        loader,
        // Loaded files start past the main source, with a 1-byte gap
        // so no two files share a boundary offset.
        next_base: main_source.len() + 1,
        files: vec![SourceFile {
            label: main_label.to_string(),
            start: 0,
            end: main_source.len(),
        }],
        lex_errors: main_lex_errors,
        module_errors: Vec::new(),
        loading: Vec::new(),
    };
    let tokens = exp.expand_stream(main_tokens);
    Expansion {
        tokens,
        lex_errors: exp.lex_errors,
        module_errors: exp.module_errors,
        source_map: SourceMap { files: exp.files },
    }
}

struct Expander<'a> {
    loader: &'a dyn Fn(&str) -> Option<String>,
    next_base: usize,
    files: Vec<SourceFile>,
    lex_errors: Vec<LexError>,
    module_errors: Vec<ModuleError>,
    /// Module names currently on the load stack — for cycle detection.
    loading: Vec<String>,
}

impl Expander<'_> {
    /// Rewrite a token stream, replacing each `mod name;` with
    /// `mod name { <loaded tokens> }`. Linear scan, so a `mod foo;`
    /// nested inside an inline `mod a { ... }` is expanded too.
    fn expand_stream(&mut self, tokens: Vec<Token>) -> Vec<Token> {
        let mut out = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            if let Some((name, name_span)) = file_mod_at(&tokens, i) {
                let semi_span = tokens[i + 2].span;
                out.push(tokens[i].clone()); // `mod`
                out.push(tokens[i + 1].clone()); // ident
                out.push(Token::new(TokenKind::LBrace, semi_span));
                out.extend(self.load(&name, name_span));
                out.push(Token::new(TokenKind::RBrace, semi_span));
                i += 3;
            } else {
                out.push(tokens[i].clone());
                i += 1;
            }
        }
        out
    }

    /// Load `name.rn`, lex it into a fresh offset range, recursively
    /// expand it, and return its body tokens (trailing `Eof` dropped).
    fn load(&mut self, name: &str, decl_span: Span) -> Vec<Token> {
        if self.loading.iter().any(|n| n == name) {
            self.module_errors.push(ModuleError {
                message: format!(
                    "circular module dependency — `{}` is already being loaded",
                    name
                ),
                span: decl_span,
            });
            return Vec::new();
        }
        let Some(source) = (self.loader)(name) else {
            self.module_errors.push(ModuleError {
                message: format!("cannot find module file `{}.rn`", name),
                span: decl_span,
            });
            return Vec::new();
        };
        let base = self.next_base;
        self.next_base += source.len() + 1;
        self.files.push(SourceFile {
            label: format!("{}.rn", name),
            start: base,
            end: base + source.len(),
        });
        let (mut tokens, mut errors) = Lexer::new(&source).tokenize();
        for t in &mut tokens {
            t.span = shift(t.span, base);
        }
        for e in &mut errors {
            e.span = shift(e.span, base);
        }
        self.lex_errors.append(&mut errors);
        // Drop the trailing Eof — these tokens splice mid-stream.
        if matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Eof)) {
            tokens.pop();
        }
        self.loading.push(name.to_string());
        let expanded = self.expand_stream(tokens);
        self.loading.pop();
        expanded
    }
}

/// `Some((name, ident_span))` when tokens `[i, i+1, i+2]` are
/// `mod IDENT ;` — a file-backed module declaration.
fn file_mod_at(tokens: &[Token], i: usize) -> Option<(String, Span)> {
    if i + 2 >= tokens.len() {
        return None;
    }
    if tokens[i].kind != TokenKind::Mod {
        return None;
    }
    let TokenKind::Ident(name) = &tokens[i + 1].kind else {
        return None;
    };
    if tokens[i + 2].kind != TokenKind::Semi {
        return None;
    }
    Some((name.clone(), tokens[i + 1].span))
}

fn shift(span: Span, base: usize) -> Span {
    Span::new(span.start + base, span.end + base)
}
