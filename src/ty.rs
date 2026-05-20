//! Semantic types — the type-checker's view of the language.
//!
//! Distinct from `ast::Type`, which is the source-level *syntactic* shape.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8, I16, I32, I64, ISize,
    U8, U16, U32, U64, USize,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::ISize => "isize",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::USize => "usize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy { F32, F64 }

impl FloatTy {
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Bool,
    Char,
    Int(IntTy),
    Float(FloatTy),
    Str,
    Unit,
    Array(Box<Ty>, usize),
    /// Heap-allocated growable list of i64. v0.x has no generics, so this
    /// is a single concrete type rather than `Vec<T>`. Becomes `Vec<T>`
    /// once parametric polymorphism arrives.
    Vec,
    Fn { params: Vec<Ty>, ret: Box<Ty> },
    Struct(SymbolId),
    Enum(SymbolId),
    /// A generic type parameter (`T` inside `fn id<T>(x: T) -> T`).
    /// Opaque to the checker; the codegen path bails when one of these
    /// reaches it (monomorphization is step 2 of the generics roadmap).
    TypeVar(SymbolId),
    /// Diverging — `return`, `break`, `continue`.
    Never,
    /// Cascades silently; comparisons against `Error` succeed to avoid
    /// follow-on error spam.
    Error,
}

/// Pinned in session 003 — unannotated integer literals default to `i64`.
pub const DEFAULT_INT: Ty = Ty::Int(IntTy::I64);
/// Unannotated float literals default to `f64`.
pub const DEFAULT_FLOAT: Ty = Ty::Float(FloatTy::F64);

impl Ty {
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::Float(_))
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, Ty::Int(_))
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Never)
    }

    /// `self` can flow into a context expecting `other` (or vice versa).
    /// `Error` and `Never` are always compatible to suppress cascade errors.
    pub fn compatible(&self, other: &Ty) -> bool {
        self.is_error() || other.is_error() || self.is_never() || other.is_never() || self == other
    }

    /// Unify two branch types into one (used for `if`/`else`, `match` arms).
    pub fn unify(&self, other: &Ty) -> Option<Ty> {
        if self.is_error() { return Some(other.clone()); }
        if other.is_error() { return Some(self.clone()); }
        if self.is_never() { return Some(other.clone()); }
        if other.is_never() { return Some(self.clone()); }
        if self == other { Some(self.clone()) } else { None }
    }

    pub fn display(&self) -> String {
        match self {
            Ty::Bool => "bool".into(),
            Ty::Char => "char".into(),
            Ty::Int(i) => i.name().into(),
            Ty::Float(f) => f.name().into(),
            Ty::Str => "str".into(),
            Ty::Unit => "()".into(),
            Ty::Array(elem, n) => format!("[{}; {}]", elem.display(), n),
            Ty::Vec => "Vec".into(),
            Ty::Fn { params, ret } => {
                let ps: Vec<String> = params.iter().map(|t| t.display()).collect();
                format!("fn({}) -> {}", ps.join(", "), ret.display())
            }
            Ty::Struct(id) => format!("struct#{}", id.0),
            Ty::Enum(id) => format!("enum#{}", id.0),
            Ty::TypeVar(id) => format!("T#{}", id.0),
            Ty::Never => "!".into(),
            Ty::Error => "?".into(),
        }
    }
}
