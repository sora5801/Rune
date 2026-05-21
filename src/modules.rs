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
//! Module files nest by directory: `mod foo;` in the main file loads
//! `foo.rn` beside it, and `mod bar;` inside a loaded `foo.rn` loads
//! `foo/bar.rn`. Module paths are `/`-joined strings resolved by the
//! `loader` callback, which the driver backs with the filesystem.

use crate::lexer::{LexError, Lexer};
use crate::token::{Span, Token, TokenKind};

/// Cap on module nesting depth. File modules form a tree — `mod`
/// always descends into a subdirectory — so import cycles can't form;
/// this only guards against a pathological `loader`.
const MAX_MODULE_DEPTH: usize = 64;

/// A `mod name;` whose file is missing, or that nests past the depth
/// cap. A separate error category from lex/parse/resolve/type.
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
    let tokens = exp.expand_stream(main_tokens, "");
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
    /// Modules currently on the load stack; its length is the active
    /// nesting depth, capped to guard against a runaway loader.
    loading: Vec<String>,
}

impl Expander<'_> {
    /// Rewrite a token stream, replacing each `mod name;` with
    /// `mod name { <loaded tokens> }`. Linear scan, so a `mod foo;`
    /// nested inside an inline `mod a { ... }` is expanded too.
    /// `dir_prefix` is the `/`-terminated module-path prefix that the
    /// declarations in this stream resolve under (`""` for the main
    /// file, `"foo/"` inside a loaded `foo.rn`).
    fn expand_stream(&mut self, tokens: Vec<Token>, dir_prefix: &str) -> Vec<Token> {
        let mut out = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            if let Some((name, name_span)) = file_mod_at(&tokens, i) {
                let semi_span = tokens[i + 2].span;
                let module_path = format!("{}{}", dir_prefix, name);
                out.push(tokens[i].clone()); // `mod`
                out.push(tokens[i + 1].clone()); // ident
                out.push(Token::new(TokenKind::LBrace, semi_span));
                out.extend(self.load(&module_path, name_span));
                out.push(Token::new(TokenKind::RBrace, semi_span));
                i += 3;
            } else {
                out.push(tokens[i].clone());
                i += 1;
            }
        }
        out
    }

    /// Load the file at `module_path`, lex it into a fresh offset
    /// range, recursively expand it, and return its body tokens
    /// (trailing `Eof` dropped). A loaded module's own `mod`
    /// declarations resolve into a subdirectory named after it.
    fn load(&mut self, module_path: &str, decl_span: Span) -> Vec<Token> {
        if self.loading.len() >= MAX_MODULE_DEPTH {
            self.module_errors.push(ModuleError {
                message: format!(
                    "module nesting too deep (over {}) at `{}`",
                    MAX_MODULE_DEPTH, module_path
                ),
                span: decl_span,
            });
            return Vec::new();
        }
        let Some(source) = (self.loader)(module_path) else {
            self.module_errors.push(ModuleError {
                message: format!("cannot find module file `{}.rn`", module_path),
                span: decl_span,
            });
            return Vec::new();
        };
        let base = self.next_base;
        self.next_base += source.len() + 1;
        self.files.push(SourceFile {
            label: format!("{}.rn", module_path),
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
        self.loading.push(module_path.to_string());
        let child_prefix = format!("{}/", module_path);
        let expanded = self.expand_stream(tokens, &child_prefix);
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
