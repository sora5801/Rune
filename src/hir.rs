//! High-level intermediate representation.
//!
//! AST-shaped: same tree, but `Path` expressions resolved to `SymbolId`,
//! every node tagged with its `Ty`, and variants that don't have codegen
//! support yet are funneled into [`HirExprKind::Unsupported`] so the
//! lowerer can keep going.

use crate::ty::{FloatTy, IntTy, SymbolId, Ty};

#[derive(Debug, Clone)]
pub struct HirModule {
    pub items: Vec<HirItem>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Fn(HirFn),
}

#[derive(Debug, Clone)]
pub struct HirFn {
    pub sym: SymbolId,
    pub name: String,
    pub params: Vec<HirParam>,
    pub ret_ty: Ty,
    pub body: HirBlock,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub sym: SymbolId,
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Let(HirLet),
    /// `bool` is `has_semi`. A trailing `false`-semi statement provides
    /// the block's value.
    Expr(HirExpr, bool),
}

#[derive(Debug, Clone)]
pub struct HirLet {
    /// `None` for `let _ = ...` (wildcard).
    pub sym: Option<SymbolId>,
    pub mutable: bool,
    pub ty: Ty,
    pub init: Option<HirExpr>,
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    Lit(HirLit),
    /// Reference to a local, parameter, or const binding.
    Local(SymbolId),
    /// Reference to a function as a first-class value (not yet codegen-able).
    Fn(SymbolId),
    Unary { op: HirUnOp, expr: Box<HirExpr> },
    Binary { op: HirBinOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> },
    /// Short-circuit logical operators — kept separate from `Binary`
    /// because codegen needs branch-based evaluation.
    Logical { op: LogicalOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> },
    /// Assignment to a local binding. LHS is restricted to a SymbolId since
    /// arbitrary place expressions (index/field) aren't codegen-supported yet.
    Assign { lhs: SymbolId, rhs: Box<HirExpr> },
    AssignOp { lhs: SymbolId, op: HirBinOp, rhs: Box<HirExpr> },
    /// Direct call to a named function.
    Call { callee: SymbolId, args: Vec<HirExpr> },
    Block(HirBlock),
    If {
        cond: Box<HirExpr>,
        then_b: HirBlock,
        /// Either `None`, or `Some(Block(_))` / `Some(If { .. })`.
        else_b: Option<Box<HirExpr>>,
    },
    While { cond: Box<HirExpr>, body: HirBlock },
    Return(Option<Box<HirExpr>>),
    /// Stub for features not yet handled in codegen. Lowering succeeds
    /// to allow inspection, codegen fails with the embedded message.
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub enum HirLit {
    Int(i64, IntTy),
    Float(f64, FloatTy),
    Bool(bool),
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Ne, Lt, Gt, Le, Ge,
    BitAnd, BitOr, BitXor, Shl, Shr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp { And, Or }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnOp { Neg, Not, BitNot }
