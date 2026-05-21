pub mod aot;
pub mod ast;
pub mod checker;
pub mod codegen;
pub mod hir;
pub mod lexer;
pub mod lower;
pub mod modules;
pub mod monomorphize;
pub mod parser;
pub mod resolver;
pub mod token;
pub mod ty;

pub use aot::{build_object, link, LinkError};
pub use ast::*;
pub use checker::{CheckResults, Checker, TypeError};
pub use codegen::{Codegen, CodegenError, OptLevel};
pub use hir::{HirModule, HirItem, HirFn};
pub use lexer::{LexError, Lexer};
pub use lower::Lowerer;
pub use modules::{expand_modules, Expansion, ModuleError, SourceMap};
pub use monomorphize::monomorphize_module;

/// The standard prelude — a `mod std { ... }` written in Rune,
/// embedded at compiler build time. Prepended to every program so
/// `std::` items (Option, Result, helper functions) are in scope.
pub const PRELUDE: &str = include_str!("std.rn");

/// Prepend the standard prelude to user source. The result is a
/// single source string occupying one span space — `compile`-style
/// drivers should lex/parse the returned string. Byte offsets in
/// errors that point at user code are shifted by the prelude's
/// length (a known v0.x rough edge).
pub fn with_prelude(user_src: &str) -> String {
    format!("{}\n{}", PRELUDE, user_src)
}
pub use parser::{ParseError, Parser};
pub use resolver::{ResolveError, Resolver, Resolutions, Symbol, SymbolKind};
pub use token::{Span, Token, TokenKind};
pub use ty::{FloatTy, IntTy, SymbolId, Ty};
