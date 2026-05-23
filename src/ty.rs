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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Bool,
    Char,
    Int(IntTy),
    Float(FloatTy),
    Str,
    Unit,
    Array(Box<Ty>, usize),
    /// Heap-allocated growable list — `Vec<T>`, carrying its element
    /// type. The descriptor (`{ ptr, len, cap, rc, weak_count }`) and
    /// the runtime helpers are element-type-agnostic (8-byte slots);
    /// the element type drives type-checking and per-element ARC
    /// release. `vec_new()` produces a placeholder `Vec<i64>` that an
    /// annotated binding refines to the real element type.
    Vec(Box<Ty>),
    Fn { params: Vec<Ty>, ret: Box<Ty> },
    /// A struct type, with its type arguments at this use site.
    /// Empty `Vec` for non-generic structs; for generic structs the
    /// args are populated by the resolver from `Path::generic_args`.
    /// The arg list is what lets `b.value` on `Box<i64>` resolve the
    /// field to i64 even though the struct's layout has TypeVar.
    Struct(SymbolId, Vec<Ty>),
    /// An enum type, with its type arguments. Same convention as
    /// Struct: empty Vec for non-generic.
    Enum(SymbolId, Vec<Ty>),
    /// A generic type parameter (`T` inside `fn id<T>(x: T) -> T`).
    /// Opaque to the checker; the codegen path bails when one of these
    /// reaches it (monomorphization is step 2 of the generics roadmap).
    TypeVar(SymbolId),
    /// `Weak<T>` — a non-owning reference to an ARC-managed value.
    /// At runtime, the same pointer as the underlying value but
    /// retain/release use the weak helpers. v0.x: only Weak<Vec> has
    /// runtime support; other inner types are accepted by the
    /// checker but error at codegen.
    Weak(Box<Ty>),
    /// `dyn Trait` — a trait object. Carries the trait's `SymbolId`.
    /// At runtime an 8-byte pointer to a heap cell holding the method
    /// pointers followed by the concrete data pointer.
    Dyn(SymbolId),
    /// `Self` as a stand-in inside a trait method signature — the
    /// position where the implementor's type will be substituted.
    /// Appears only after `Self::Item` resolves trait-side; the
    /// checker substitutes it away at the call site. Must not reach
    /// the monomorphizer.
    SelfType,
    /// `T::Item` — the projection of an associated type through a
    /// base. The base is the type parameter (or `Ty::SelfType` in a
    /// trait sig); the string is the associated-type name. Resolves
    /// at monomorphization once the base becomes a concrete struct
    /// for which `impl_assoc_bindings` has an entry.
    Assoc(Box<Ty>, String),
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
    /// `TypeVar` is opaque — it's compatible with anything, since the
    /// monomorphizer will resolve it to a concrete type later. This is
    /// a coarse rule but suffices for v0.x without trait constraints.
    /// Struct/Enum with matching syms are compatible regardless of
    /// their type-arg lists — args may differ at variant-construction
    /// sites (where we use `[]`) vs use sites (where the path carries
    /// the args). The monomorphizer + lowerer use the args directly
    /// for codegen, not the checker's `compatible`. `Vec` follows the
    /// same rule: any `Vec` is compatible with any other `Vec`, since
    /// `vec_new()` yields a placeholder `Vec<i64>` whose real element
    /// type is pinned by the annotated binding it flows into.
    pub fn compatible(&self, other: &Ty) -> bool {
        if self.is_error() || other.is_error() || self.is_never() || other.is_never() {
            return true;
        }
        if matches!(self, Ty::TypeVar(_)) || matches!(other, Ty::TypeVar(_)) {
            return true;
        }
        // An unresolved projection (`T::Item`) is opaque at
        // typecheck time. Treat it as compatible with anything so a
        // struct literal like `Map { iter, f: double }` can flow
        // a concrete `fn(i64) -> i64` into a field declared as
        // `fn(I::Item) -> U` — monomorphization resolves the
        // projection later, and a real mismatch surfaces then.
        if matches!(self, Ty::Assoc(_, _)) || matches!(other, Ty::Assoc(_, _)) {
            return true;
        }
        match (self, other) {
            (Ty::Struct(s1, _), Ty::Struct(s2, _)) => s1 == s2,
            (Ty::Enum(s1, _), Ty::Enum(s2, _)) => s1 == s2,
            (Ty::Vec(_), Ty::Vec(_)) => true,
            (
                Ty::Fn { params: p1, ret: r1 },
                Ty::Fn { params: p2, ret: r2 },
            ) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2.iter()).all(|(a, b)| a.compatible(b))
                    && r1.compatible(r2)
            }
            _ => self == other,
        }
    }

    /// Unify two branch types into one (used for `if`/`else`, `match` arms).
    /// Generic types match by sym alone, exactly mirroring `compatible`:
    /// `Some(v): Enum(option, [i64])` and `None: Enum(option, [])` (the
    /// placeholder used for bare variant construction) must unify to
    /// `Enum(option, [i64])` so an `if c { Some(x) } else { None }`
    /// typechecks. Pick whichever side has non-empty args; if both do,
    /// they must match exactly.
    pub fn unify(&self, other: &Ty) -> Option<Ty> {
        if self.is_error() { return Some(other.clone()); }
        if other.is_error() { return Some(self.clone()); }
        if self.is_never() { return Some(other.clone()); }
        if other.is_never() { return Some(self.clone()); }
        match (self, other) {
            (Ty::Struct(s1, a1), Ty::Struct(s2, a2)) if s1 == s2 => {
                Some(Ty::Struct(*s1, pick_concrete_args(a1, a2)?))
            }
            (Ty::Enum(s1, a1), Ty::Enum(s2, a2)) if s1 == s2 => {
                Some(Ty::Enum(*s1, pick_concrete_args(a1, a2)?))
            }
            (Ty::Vec(e1), Ty::Vec(e2)) => {
                // The placeholder element type from `vec_new()` is
                // `Vec<i64>` — unify with whichever side carries the
                // user-pinned element. If both are concrete and
                // differ, the unification fails (caller errors).
                let inner = e1.unify(e2)?;
                Some(Ty::Vec(Box::new(inner)))
            }
            _ => if self == other { Some(self.clone()) } else { None },
        }
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
            Ty::Vec(elem) => format!("Vec<{}>", elem.display()),
            Ty::Fn { params, ret } => {
                let ps: Vec<String> = params.iter().map(|t| t.display()).collect();
                format!("fn({}) -> {}", ps.join(", "), ret.display())
            }
            Ty::Struct(id, args) | Ty::Enum(id, args) => {
                let prefix = if matches!(self, Ty::Struct(_, _)) {
                    "struct"
                } else {
                    "enum"
                };
                if args.is_empty() {
                    format!("{}#{}", prefix, id.0)
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|t| t.display()).collect();
                    format!("{}#{}<{}>", prefix, id.0, arg_strs.join(", "))
                }
            }
            Ty::TypeVar(id) => format!("T#{}", id.0),
            Ty::Weak(inner) => format!("Weak<{}>", inner.display()),
            Ty::Dyn(id) => format!("dyn#{}", id.0),
            Ty::SelfType => "Self".into(),
            Ty::Assoc(base, name) => format!("{}::{}", base.display(), name),
            Ty::Never => "!".into(),
            Ty::Error => "?".into(),
        }
    }
}

/// Helper for `Ty::unify` on generic types: given two args lists for
/// the *same* generic sym, return the one with concrete (non-empty)
/// args, or fail if both are concrete and disagree. The empty list
/// is the "placeholder" Rune emits at variant construction sites
/// like `None` — it always loses to a use-site list that names the
/// concrete type parameters.
fn pick_concrete_args(a: &[Ty], b: &[Ty]) -> Option<Vec<Ty>> {
    if a.is_empty() {
        return Some(b.to_vec());
    }
    if b.is_empty() {
        return Some(a.to_vec());
    }
    if a.len() != b.len() {
        return None;
    }
    let mut out: Vec<Ty> = Vec::with_capacity(a.len());
    for (x, y) in a.iter().zip(b.iter()) {
        out.push(x.unify(y)?);
    }
    Some(out)
}
