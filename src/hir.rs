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
    /// `EnumName::Variant` — a unit-variant enum value, represented as
    /// the i64 discriminant at runtime.
    EnumVariant { discriminant: u32 },
    Unary { op: HirUnOp, expr: Box<HirExpr> },
    Binary { op: HirBinOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> },
    /// Short-circuit logical operators — kept separate from `Binary`
    /// because codegen needs branch-based evaluation.
    Logical { op: LogicalOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> },
    /// Assignment to a local binding. LHS is restricted to a SymbolId since
    /// arbitrary place expressions (index/field) aren't codegen-supported yet.
    Assign { lhs: SymbolId, rhs: Box<HirExpr> },
    AssignOp { lhs: SymbolId, op: HirBinOp, rhs: Box<HirExpr> },
    /// Direct call to a named Rune function.
    Call { callee: SymbolId, args: Vec<HirExpr> },
    /// Call to a host-provided builtin (`print`, etc.). Codegen emits a
    /// call to an imported C function (e.g. `rune_print_i64`).
    BuiltinCall { name: String, args: Vec<HirExpr> },
    /// Method call (`receiver.method(args)`). Codegen dispatches based
    /// on `receiver.ty` and `method` — most builtin methods compile to
    /// inline IR (e.g. `str.len()` is a load from the descriptor).
    MethodCall {
        receiver: Box<HirExpr>,
        method: String,
        args: Vec<HirExpr>,
    },
    /// Stack-allocated struct literal. Fields are stored at byte offsets
    /// known statically from the struct layout. Value is a pointer to
    /// the start of the stack slot.
    StructLit {
        sym: SymbolId,
        /// (byte offset, value) pairs, in declaration order.
        fields: Vec<(u32, HirExpr)>,
        size: u32,
    },
    /// `receiver.field` — loads a field at a known byte offset.
    FieldAccess {
        receiver: Box<HirExpr>,
        offset: u32,
        field_ty: Ty,
    },
    /// `receiver.field = rhs` — stores at a known byte offset.
    FieldAssign {
        receiver: Box<HirExpr>,
        offset: u32,
        field_ty: Ty,
        rhs: Box<HirExpr>,
    },
    /// Stack-allocated array literal. Value type is a pointer to the
    /// first element; element type and length are tracked statically.
    Array { elems: Vec<HirExpr>, elem_ty: Ty },
    /// `array[index]`. Loads the element at the computed offset.
    Index { array: Box<HirExpr>, index: Box<HirExpr>, elem_ty: Ty },
    /// `str[i]` — reads a single byte from the string and zero-extends to i64.
    StrByteIndex { str_val: Box<HirExpr>, index: Box<HirExpr> },
    /// `str[a..b]` or `str[a..=b]` — heap-allocates a fresh substring.
    StrSlice {
        str_val: Box<HirExpr>,
        start: Box<HirExpr>,
        end: Box<HirExpr>,
        inclusive: bool,
    },
    Block(HirBlock),
    If {
        cond: Box<HirExpr>,
        then_b: HirBlock,
        /// Either `None`, or `Some(Block(_))` / `Some(If { .. })`.
        else_b: Option<Box<HirExpr>>,
    },
    While { cond: Box<HirExpr>, body: HirBlock },
    /// `for local in iter { body }`. The iter is an array with statically
    /// known `length` and `elem_ty`. Codegen lowers to a counter-based loop.
    For {
        local: Option<SymbolId>,
        iter: Box<HirExpr>,
        body: HirBlock,
        elem_ty: Ty,
        length: usize,
    },
    /// `for local in start..end { body }`. Range-based iteration over
    /// integers. Inclusive flag controls whether `end` is included.
    ForRange {
        local: Option<SymbolId>,
        start: Box<HirExpr>,
        end: Box<HirExpr>,
        inclusive: bool,
        body: HirBlock,
    },
    /// `match scrutinee { arm1, arm2, ..., _ => default }` — sequential
    /// pattern-check chain. Any arm that ends a path without matching
    /// jumps to a fallback that calls `rune_panic_no_match` (compile-time
    /// exhaustiveness isn't checked yet).
    Match {
        scrutinee: Box<HirExpr>,
        arms: Vec<HirMatchArm>,
    },
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
    Str(String),
    Unit,
}

#[derive(Debug, Clone)]
pub struct HirMatchArm {
    /// One or more alternative patterns. With or-patterns the arm fires
    /// on the first match; without, the Vec has exactly one entry.
    pub patterns: Vec<HirPattern>,
    /// Optional guard `if cond` — checked after pattern match succeeds.
    /// Guarded arms don't count as catch-alls for exhaustiveness.
    pub guard: Option<HirExpr>,
    pub body: HirExpr,
}

#[derive(Debug, Clone)]
pub enum HirPattern {
    /// `_` — always matches, no binding.
    Wildcard,
    /// Bare identifier — always matches; binds the scrutinee value to
    /// the given symbol in the arm's body scope.
    Bind(SymbolId),
    IntLit(i64),
    BoolLit(bool),
    StrLit(String),
    EnumVariant { discriminant: u32 },
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
