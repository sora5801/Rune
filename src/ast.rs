//! AST for Rune.

use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Pub,
}

// ---- Items ----

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    Impl(ImplBlock),
    Trait(TraitDecl),
    /// `mod name { items... }` — an inline module. Items inside are
    /// namespaced under `name`; addressable as `name::item` from
    /// outside (if `pub`) and unqualified from within.
    Mod(ModDecl),
    /// `use a::b::c;` — imports `c` into the current module's scope
    /// so it can be referenced unqualified.
    Use(UseDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModDecl {
    pub vis: Visibility,
    pub name: Ident,
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseDecl {
    pub path: Path,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    /// `Some(trait_path)` for `impl Trait for Type`, `None` for an
    /// inherent `impl Type`.
    pub trait_path: Option<Path>,
    pub type_path: Path,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

/// A trait declaration: a set of method *signatures* (no bodies).
/// Implementing types provide bodies via `impl Trait for Type`.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    pub vis: Visibility,
    pub name: Ident,
    pub methods: Vec<TraitMethodSig>,
    pub span: Span,
}

/// A method signature inside a trait declaration. Shares the shape
/// of `FnDecl` minus the body.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethodSig {
    pub name: Ident,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub span: Span,
}

/// A generic type parameter with optional trait bounds.
/// `<T>` has empty bounds; `<T: Display>` / `<T: A + B>` populate them.
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub vis: Visibility,
    pub name: Ident,
    /// Generic type parameters, e.g. `<T>` in `fn id<T>(x: T) -> T`
    /// or `<T: Display>` for bounded generics. Empty for non-generic.
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    pub vis: Visibility,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub vis: Visibility,
    pub name: Ident,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub vis: Visibility,
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: Ident,
    pub fields: VariantFields,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VariantFields {
    Unit,
    Tuple(Vec<Type>),
    Named(Vec<Field>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub vis: Visibility,
    pub name: Ident,
    pub ty: Type,
    pub value: Expr,
    pub span: Span,
}

// ---- Types ----

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Path(Path),
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Path(p) => p.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub segments: Vec<Ident>,
    /// Generic type arguments on the *last* segment, e.g. `Vec<i64>`.
    /// Used at type-position paths and call-position paths
    /// (turbofish-like `Vec::<i64>::new()` not supported yet — args
    /// go on the final segment only). Empty for non-generic paths.
    pub generic_args: Vec<Type>,
    pub span: Span,
}

// ---- Statements / Block ----

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(LetStmt),
    /// Expression statement. `has_semi` records whether it was terminated by `;`.
    /// A trailing expression in a block (no semicolon, last statement) has `has_semi == false`.
    Expr(Expr, bool),
    Item(Item),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    pub mutable: bool,
    pub pat: Pattern,
    pub ty: Option<Type>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

// ---- Patterns ----

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard(Span),
    Ident { name: Ident, mutable: bool, span: Span },
    Literal { lit: Lit, span: Span },
    /// Multi-segment path used as a pattern — today's only shape is
    /// `EnumName::Variant`, matched against the scrutinee's discriminant.
    Path { path: Path, span: Span },
    /// `EnumName::Variant(sub_pat, ...)` — destructures a payload-
    /// bearing tuple variant. Multi-field is supported.
    TupleVariant { path: Path, fields: Vec<Pattern>, span: Span },
    /// `EnumName::Variant { name: pat, ... }` — destructures a named-
    /// field enum variant. Fields may appear in any order; the
    /// lowerer reorders them into declaration order before emitting
    /// `HirPattern::EnumPayload`.
    NamedVariant { path: Path, fields: Vec<(Ident, Pattern)>, span: Span },
    /// `lo..hi` (exclusive) or `lo..=hi` (inclusive). Bounds are
    /// literals (integer or char) parsed as `Lit`; the checker rejects
    /// non-integer/char bounds and ranges where `lo > hi`.
    Range { lo: Lit, hi: Lit, inclusive: bool, span: Span },
    /// `pat | pat | pat` — matches if any alternative matches. Nested Or
    /// is flattened by the parser; the inner Vec never contains another Or.
    Or { patterns: Vec<Pattern>, span: Span },
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(s) => *s,
            Pattern::Ident { span, .. } => *span,
            Pattern::Literal { span, .. } => *span,
            Pattern::Path { span, .. } => *span,
            Pattern::TupleVariant { span, .. } => *span,
            Pattern::NamedVariant { span, .. } => *span,
            Pattern::Range { span, .. } => *span,
            Pattern::Or { span, .. } => *span,
        }
    }
}

// ---- Expressions ----

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit { lit: Lit, span: Span },
    Path(Path),
    Unary { op: UnOp, expr: Box<Expr>, span: Span },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Assign { lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    AssignOp { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Call { callee: Box<Expr>, args: Vec<Expr>, span: Span },
    MethodCall { receiver: Box<Expr>, method: Ident, args: Vec<Expr>, span: Span },
    Field { receiver: Box<Expr>, name: Ident, span: Span },
    Index { receiver: Box<Expr>, index: Box<Expr>, span: Span },
    Try { expr: Box<Expr>, span: Span },
    Cast { expr: Box<Expr>, ty: Type, span: Span },
    Array { elems: Vec<Expr>, span: Span },
    Block(Block),
    If { cond: Box<Expr>, then_branch: Block, else_branch: Option<Box<Expr>>, span: Span },
    While { cond: Box<Expr>, body: Block, span: Span },
    For { pat: Pattern, iter: Box<Expr>, body: Block, span: Span },
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm>, span: Span },
    /// `Path { field1: expr1, field2: expr2 }` — struct literal.
    StructLit { path: Path, fields: Vec<FieldInit>, span: Span },
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
        span: Span,
    },
    Return { value: Option<Box<Expr>>, span: Span },
    Break(Span),
    Continue(Span),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Lit { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Assign { span, .. }
            | Expr::AssignOp { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Field { span, .. }
            | Expr::Index { span, .. }
            | Expr::Try { span, .. }
            | Expr::Cast { span, .. }
            | Expr::Array { span, .. }
            | Expr::If { span, .. }
            | Expr::While { span, .. }
            | Expr::For { span, .. }
            | Expr::Match { span, .. }
            | Expr::StructLit { span, .. }
            | Expr::Range { span, .. }
            | Expr::Return { span, .. } => *span,
            Expr::Path(p) => p.span,
            Expr::Block(b) => b.span,
            Expr::Break(s) | Expr::Continue(s) => *s,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    pub name: Ident,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
}

// ---- Operators ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Ne, Lt, Gt, Le, Ge,
    And, Or,
    BitAnd, BitOr, BitXor, Shl, Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg, Not, BitNot,
}
