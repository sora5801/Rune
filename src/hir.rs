//! High-level intermediate representation.
//!
//! AST-shaped: same tree, but `Path` expressions resolved to `SymbolId`,
//! every node tagged with its `Ty`, and variants that don't have codegen
//! support yet are funneled into [`HirExprKind::Unsupported`] so the
//! lowerer can keep going.

use crate::ty::{FloatTy, IntTy, SymbolId, Ty};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct HirModule {
    pub items: Vec<HirItem>,
    /// Per-struct list of (offset, ty) for fields whose type is ARC-
    /// managed (Vec, Str, or another struct with ARC fields). Only
    /// structs that contain at least one ARC-managed field appear.
    /// Codegen uses this to emit per-field retain on construction
    /// and per-field release on scope drop.
    pub struct_arc_fields: HashMap<SymbolId, Vec<(u32, Ty)>>,
    /// Per-struct field-area size in bytes. The actual heap
    /// allocation is `size + 8` (rc at offset `size`). Populated for
    /// every user-defined struct, ARC-managed or not.
    pub struct_sizes: HashMap<SymbolId, u32>,
    /// Enums with at least one payload-bearing variant. Their values
    /// are heap-allocated `{ tag, payload, rc }` descriptors and
    /// participate in ARC; unit variants of the same enum allocate
    /// the same shape with payload=0.
    pub enum_has_payload: std::collections::HashSet<SymbolId>,
    /// Per-enum payload types per variant, indexed by discriminant.
    /// `Vec<Vec<Ty>>` lets a multi-field tuple variant carry several
    /// types; for v0.x single-field, each inner Vec has 0 or 1
    /// entries. Only populated for enums in `enum_has_payload`.
    pub enum_payload_tys: HashMap<SymbolId, Vec<Vec<Ty>>>,
    /// `(type_sym, method_name) → impl-method fn sym`. Covers both
    /// inherent and trait-impl methods. The monomorphizer uses this
    /// to rewrite trait method calls on a now-concrete receiver
    /// (`x.fmt()` inside a `<T: Display>` body) into direct calls.
    pub impl_methods: HashMap<(SymbolId, String), SymbolId>,
    /// Distinct ARC-managed `Vec<T>` element types used anywhere in
    /// the program (transitively closed — `Vec<Vec<S>>` contributes
    /// both `Vec<S>` and `S`). Populated by the monomorphizer after
    /// all specialization. Codegen synthesizes one per-element-type
    /// release function (`__rune_release_vec$<elem>`) for each, so a
    /// `Vec` whose elements are themselves ARC-managed reclaims them
    /// when its strong count hits zero.
    pub vec_arc_elem_tys: Vec<Ty>,
    /// Distinct fixed-size array types (`[T; N]`) used anywhere in
    /// the program. A heap array is a refcounted block, so codegen
    /// synthesizes one release function (`__rune_release_array$<ty>`)
    /// per distinct array type. Transitively closed — `[[S; 2]; 3]`
    /// contributes both itself and `[S; 2]`.
    pub array_tys: Vec<Ty>,
    /// Per-trait ordered method names. Codegen uses the order to lay
    /// out a trait object's method-pointer table and to find a
    /// method's slot for an indirect call.
    pub trait_methods: HashMap<SymbolId, Vec<String>>,
    /// Per-trait flattened method list including transitive supertrait
    /// methods. Each entry is `(owning_trait_sym, method_name)` —
    /// the owning sym names the trait whose `impl Trait for S` block
    /// holds the method body; the name keys `impl_methods`. The
    /// vec's index is the method's slot in a `dyn` box laid out for
    /// the outer key trait. Built by `lower.rs` in BFS order: the
    /// trait's own methods first, then each supertrait in declaration
    /// order, deduped first-wins by method name. A `dyn Dog` box and
    /// a `dyn Animal` box are distinct types with distinct slot
    /// orderings — both keys exist in this map.
    pub trait_methods_flat: HashMap<SymbolId, Vec<(SymbolId, String)>>,
    /// Resolved `(struct sym, associated-type name) → Ty` bindings —
    /// the checker pre-resolved every impl's `type Item = ..;` so
    /// the monomorphizer can resolve `Ty::Assoc(Struct(s,_), name)`
    /// projections at substitution time.
    pub impl_assoc_bindings_ty: HashMap<(SymbolId, String), Ty>,
}

#[derive(Debug, Clone)]
pub enum HirItem {
    Fn(HirFn),
}

#[derive(Debug, Clone)]
pub struct HirFn {
    pub sym: SymbolId,
    pub name: String,
    /// Generic type parameters declared on this function. Empty for
    /// non-generic and for specialized functions produced by
    /// monomorphization. Their referent symbols are used as keys for
    /// type substitution.
    pub generics: Vec<SymbolId>,
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
    /// Payload-bearing enum variant construction:
    /// `Some(5)` or `Pair(1, 2)`. Codegen heap-allocates a
    /// `{ tag, payload[N], rc }` descriptor sized to the enum's max
    /// variant arity.
    EnumPayloadCtor {
        enum_sym: SymbolId,
        discriminant: u32,
        payloads: Vec<HirExpr>,
    },
    Unary { op: HirUnOp, expr: Box<HirExpr> },
    /// `expr as Ty` — numeric / char / bool conversion. The `from` type
    /// is the expression's HirExpr.ty; the destination is the enclosing
    /// HirExpr.ty.
    Cast { expr: Box<HirExpr> },
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
    /// Coerce a concrete struct value into a `dyn Trait` object —
    /// heap-allocate a cell holding the trait's method pointers
    /// followed by the data pointer.
    DynBox {
        value: Box<HirExpr>,
        struct_sym: SymbolId,
        trait_sym: SymbolId,
    },
    /// A method call on a `dyn Trait` receiver — dispatched through
    /// the boxed method-pointer table (`call_indirect`).
    DynCall {
        receiver: Box<HirExpr>,
        trait_sym: SymbolId,
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
    /// Single Unicode scalar; codegen as iconst.i32 of the codepoint.
    Char(char),
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
    /// Tuple-variant destructure pattern like `Some(x)` or
    /// `Pair(a, b)`. Matches if the scrutinee's tag equals
    /// `discriminant`; binds each payload position to the
    /// corresponding entry in `bindings` (None for `_`).
    EnumPayload {
        discriminant: u32,
        bindings: Vec<(Ty, Option<SymbolId>)>,
    },
    /// `lo..hi` (exclusive) or `lo..=hi` (inclusive). For integer and
    /// char scrutinees; chars are pre-converted to their codepoint by
    /// the lowerer so codegen only sees i64 bounds.
    IntRange { lo: i64, hi: i64, inclusive: bool },
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
