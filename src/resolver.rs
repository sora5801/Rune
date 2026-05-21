//! Name resolution pass.
//!
//! Two passes over the module:
//! 1. Declare top-level items (so order-independent forward references work).
//! 2. Resolve identifiers within item bodies against the scope chain.
//!
//! Outputs a [`Resolutions`] table:
//! - `path_to_sym` — each path expression's span → the symbol it refers to.
//! - `decl_to_sym` — each declaration ident's span → the symbol it declares.
//!
//! Built-in type names (`i64`, `bool`, ...) are pre-populated as symbols
//! with `SymbolKind::BuiltinType(Ty)`; the type checker reads the embedded
//! `Ty` when it needs to materialize a type from a path.

use std::collections::HashMap;
use std::fmt;

use crate::ast::*;
use crate::token::Span;
use crate::ty::{FloatTy, IntTy, SymbolId, Ty};

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    /// Span of the declaration (function name, struct name, pattern ident, ...).
    pub span: Span,
    pub kind: SymbolKind,
}

#[derive(Debug, Clone)]
pub enum SymbolKind {
    BuiltinType(Ty),
    /// Host-provided builtin function with a fixed signature.
    BuiltinFn(BuiltinFn),
    /// Polymorphic builtin — the type checker accepts a set of argument
    /// types and the lowerer dispatches to a concrete `BuiltinCall` based
    /// on what was passed. Used for `print`, which accepts both `i64` and
    /// `str`.
    PolyBuiltinFn(&'static str),
    Fn,
    Local { mutable: bool },
    Param,
    Struct,
    Enum,
    /// A variant of an enum. Carries its parent enum's symbol and its
    /// numeric discriminant. Unit variants have no payload; tuple
    /// variants carry one or more value types.
    EnumVariant { enum_sym: SymbolId, discriminant: u32 },
    Const,
    /// A generic type parameter declared on an item (`<T>` in
    /// `fn id<T>(x: T) -> T`). The body refers to it via this symbol.
    /// Codegen rejects functions whose body still mentions any
    /// `TypeParam`; that's what makes "generics step 1" parser-only.
    TypeParam,
    /// A trait declaration. Carries no codegen weight — traits are a
    /// compile-time-only construct; method dispatch is resolved
    /// statically via monomorphization.
    Trait,
}

#[derive(Debug, Clone)]
pub struct BuiltinFn {
    pub name: &'static str,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

pub struct Resolutions {
    pub symbols: Vec<Symbol>,
    pub path_to_sym: HashMap<Span, SymbolId>,
    pub decl_to_sym: HashMap<Span, SymbolId>,
    /// User-defined inherent methods: `(struct_sym, method_name) → method's Fn symbol`.
    /// Populated from `impl` blocks. Builtin methods (`str.len()`, etc.) are
    /// resolved via the checker's hardcoded table instead.
    pub impl_methods: HashMap<(SymbolId, String), SymbolId>,
    /// For each enum symbol, a map from variant name to the variant's
    /// symbol. Variants aren't in the global scope — they're addressed
    /// as `EnumName::VariantName`.
    pub enum_variants: HashMap<SymbolId, HashMap<String, SymbolId>>,
    /// Payload types per variant (variant_sym → AST Types). Empty for
    /// unit variants. For tuple variants the payloads appear in
    /// declaration order; for named-field variants the types appear
    /// in declaration order too, with names tracked separately in
    /// `enum_variant_field_names`.
    pub enum_variant_payloads: HashMap<SymbolId, Vec<crate::ast::Type>>,
    /// Field names per **named** variant, in declaration order.
    /// Tuple and unit variants don't appear in this map. Used to
    /// validate `Variant { name: val }` construction and
    /// destructure patterns.
    pub enum_variant_field_names: HashMap<SymbolId, Vec<String>>,
    /// Generic type-parameter symbols per generic struct, in
    /// declaration order. Lets users of `Ty::Struct(sym, args)`
    /// build a substitution mapping for the struct's fields.
    pub struct_generics: HashMap<SymbolId, Vec<SymbolId>>,
    /// Same for enums.
    pub enum_generics: HashMap<SymbolId, Vec<SymbolId>>,
    /// Declared method signatures per trait — keyed by trait sym.
    /// The checker uses these for impl conformance + bounded-generic
    /// method-call resolution.
    pub trait_methods: HashMap<SymbolId, Vec<crate::ast::TraitMethodSig>>,
    /// Generic-param symbol → trait-bound symbols. `<T: Display>`
    /// records `T_sym → [Display_sym]`.
    pub generic_bounds: HashMap<SymbolId, Vec<SymbolId>>,
    /// Enums that have at least one payload-bearing variant. These use
    /// a heap-allocated `{ tag, payload, rc }` descriptor at runtime
    /// instead of the plain i64 discriminant used by tag-only enums.
    pub enum_has_payload: std::collections::HashSet<SymbolId>,
}

impl Resolutions {
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "resolve error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for ResolveError {}

pub struct Resolver {
    symbols: Vec<Symbol>,
    scopes: Vec<HashMap<String, SymbolId>>,
    path_to_sym: HashMap<Span, SymbolId>,
    decl_to_sym: HashMap<Span, SymbolId>,
    impl_methods: HashMap<(SymbolId, String), SymbolId>,
    enum_variants: HashMap<SymbolId, HashMap<String, SymbolId>>,
    enum_variant_payloads: HashMap<SymbolId, Vec<crate::ast::Type>>,
    enum_variant_field_names: HashMap<SymbolId, Vec<String>>,
    enum_has_payload: std::collections::HashSet<SymbolId>,
    struct_generics: HashMap<SymbolId, Vec<SymbolId>>,
    enum_generics: HashMap<SymbolId, Vec<SymbolId>>,
    trait_methods: HashMap<SymbolId, Vec<crate::ast::TraitMethodSig>>,
    generic_bounds: HashMap<SymbolId, Vec<SymbolId>>,
    errors: Vec<ResolveError>,
}

impl Default for Resolver {
    fn default() -> Self { Self::new() }
}

impl Resolver {
    pub fn new() -> Self {
        let mut r = Self {
            symbols: Vec::new(),
            scopes: vec![HashMap::new()],
            path_to_sym: HashMap::new(),
            decl_to_sym: HashMap::new(),
            impl_methods: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            enum_variant_field_names: HashMap::new(),
            enum_has_payload: std::collections::HashSet::new(),
            struct_generics: HashMap::new(),
            enum_generics: HashMap::new(),
            trait_methods: HashMap::new(),
            generic_bounds: HashMap::new(),
            errors: Vec::new(),
        };
        r.insert_builtins();
        r
    }

    pub fn resolve_module(mut self, m: &Module) -> (Resolutions, Vec<ResolveError>) {
        // Pass 1: declare non-impl items so impl blocks can resolve their
        // target type.
        for item in &m.items {
            self.declare_item(item);
        }
        // Pass 1.5: declare impl methods.
        for item in &m.items {
            if let Item::Impl(i) = item {
                self.declare_impl(i);
            }
        }
        // Pass 2: resolve all bodies.
        for item in &m.items {
            self.resolve_item(item);
        }
        (
            Resolutions {
                symbols: self.symbols,
                path_to_sym: self.path_to_sym,
                decl_to_sym: self.decl_to_sym,
                impl_methods: self.impl_methods,
                enum_variants: self.enum_variants,
                enum_variant_payloads: self.enum_variant_payloads,
                enum_variant_field_names: self.enum_variant_field_names,
                enum_has_payload: self.enum_has_payload,
                struct_generics: self.struct_generics,
                enum_generics: self.enum_generics,
                trait_methods: self.trait_methods,
                generic_bounds: self.generic_bounds,
            },
            self.errors,
        )
    }

    fn declare_impl(&mut self, i: &ImplBlock) {
        self.resolve_path(&i.type_path);
        let Some(&struct_sym) = self.path_to_sym.get(&i.type_path.span) else {
            return;
        };
        if !matches!(self.symbols[struct_sym.0 as usize].kind, SymbolKind::Struct) {
            let name = self.symbols[struct_sym.0 as usize].name.clone();
            self.error(
                format!(
                    "`{}` is not a struct; `impl` can only be applied to structs (for now)",
                    name
                ),
                i.type_path.span,
            );
            return;
        }
        // For a trait impl, resolve the trait path so the checker can
        // validate the impl against the trait's declared signatures.
        if let Some(trait_path) = &i.trait_path {
            self.resolve_path(trait_path);
            if let Some(&tsym) = self.path_to_sym.get(&trait_path.span) {
                if !matches!(self.symbols[tsym.0 as usize].kind, SymbolKind::Trait) {
                    self.error(
                        format!(
                            "`{}` is not a trait",
                            self.symbols[tsym.0 as usize].name
                        ),
                        trait_path.span,
                    );
                }
            }
        }
        let struct_name = self.symbols[struct_sym.0 as usize].name.clone();
        for method in &i.methods {
            let mangled = format!("{}__{}", struct_name, method.name.name);
            let id = SymbolId(self.symbols.len() as u32);
            self.symbols.push(Symbol {
                name: mangled,
                span: method.name.span,
                kind: SymbolKind::Fn,
            });
            self.decl_to_sym.insert(method.name.span, id);
            let key = (struct_sym, method.name.name.clone());
            if self.impl_methods.contains_key(&key) {
                self.error(
                    format!(
                        "method `{}` already defined on `{}`",
                        method.name.name, struct_name
                    ),
                    method.name.span,
                );
            }
            self.impl_methods.insert(key, id);
        }
    }

    fn insert_builtins(&mut self) {
        let zero = Span::new(0, 0);
        // Builtin sentinel for `Weak<T>` — the checker special-cases
        // this name and reads the path's generic args to build
        // `Ty::Weak(args[0])`. v0.x: only Weak<Vec> has runtime
        // support; the checker rejects other inner types.
        self.intern(
            "Weak".to_string(),
            zero,
            SymbolKind::BuiltinType(Ty::Weak(Box::new(Ty::Error))),
        );
        let builtins: &[(&str, Ty)] = &[
            ("bool", Ty::Bool),
            ("char", Ty::Char),
            ("str", Ty::Str),
            ("i8", Ty::Int(IntTy::I8)),
            ("i16", Ty::Int(IntTy::I16)),
            ("i32", Ty::Int(IntTy::I32)),
            ("i64", Ty::Int(IntTy::I64)),
            ("isize", Ty::Int(IntTy::ISize)),
            ("u8", Ty::Int(IntTy::U8)),
            ("u16", Ty::Int(IntTy::U16)),
            ("u32", Ty::Int(IntTy::U32)),
            ("u64", Ty::Int(IntTy::U64)),
            ("usize", Ty::Int(IntTy::USize)),
            ("f32", Ty::Float(FloatTy::F32)),
            ("f64", Ty::Float(FloatTy::F64)),
            ("Vec", Ty::Vec),
        ];
        for (name, ty) in builtins {
            self.intern(name.to_string(), zero, SymbolKind::BuiltinType(ty.clone()));
        }
        // `print` dispatches by argument type at lowering time.
        self.intern(
            "print".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("print"),
        );
        // Weak<T> primitives. Polymorphic over T; the lowerer
        // dispatches to the per-type runtime helper. v0.x supports
        // only Vec as the inner type.
        self.intern(
            "weak".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("weak"),
        );
        self.intern(
            "upgrade_or".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("upgrade_or"),
        );
        // Explicit single-type variants stay available for users who want
        // them, and are the targets of `print`'s dispatch.
        let print_str = BuiltinFn {
            name: "print_str",
            params: vec![Ty::Str],
            ret: Ty::Unit,
        };
        self.intern(
            print_str.name.to_string(),
            zero,
            SymbolKind::BuiltinFn(print_str),
        );
        let print_i64 = BuiltinFn {
            name: "print_i64",
            params: vec![Ty::Int(IntTy::I64)],
            ret: Ty::Unit,
        };
        self.intern(
            print_i64.name.to_string(),
            zero,
            SymbolKind::BuiltinFn(print_i64),
        );
        let vec_new = BuiltinFn {
            name: "vec_new",
            params: vec![],
            ret: Ty::Vec,
        };
        self.intern(
            vec_new.name.to_string(),
            zero,
            SymbolKind::BuiltinFn(vec_new),
        );
    }

    /// Insert a symbol into the current scope. Shadowing is allowed —
    /// existing entries with the same name in the same scope are overwritten
    /// in the lookup map (the old `Symbol` remains in `symbols` for span-keyed
    /// queries).
    fn intern(&mut self, name: String, span: Span, kind: SymbolKind) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol { name: name.clone(), span, kind });
        self.scopes.last_mut().unwrap().insert(name, id);
        id
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<SymbolId> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        None
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.errors.push(ResolveError { message: msg.into(), span });
    }

    // ---- pass 1: declare top-level items ----

    fn declare_item(&mut self, item: &Item) {
        let (name, kind) = match item {
            Item::Fn(f) => (&f.name, SymbolKind::Fn),
            Item::Struct(s) => (&s.name, SymbolKind::Struct),
            Item::Enum(e) => (&e.name, SymbolKind::Enum),
            Item::Const(c) => (&c.name, SymbolKind::Const),
            Item::Trait(t) => (&t.name, SymbolKind::Trait),
            // Impl blocks contribute methods, not a top-level name.
            // Methods are declared by a separate pass after all structs
            // are known. See `declare_impl`.
            Item::Impl(_) => return,
        };
        let id = self.intern(name.name.clone(), name.span, kind);
        self.decl_to_sym.insert(name.span, id);

        // For enums, also register each variant by name (off-scope —
        // only addressable as EnumName::VariantName).
        if let Item::Enum(e) = item {
            let mut variants: HashMap<String, SymbolId> = HashMap::new();
            let mut any_payload = false;
            for (discriminant, v) in e.variants.iter().enumerate() {
                // Variant symbols sit outside any lexical scope. They get
                // a fresh entry in `symbols` for span-keyed queries; lookups
                // go through `enum_variants` instead of `scopes`.
                let variant_id = SymbolId(self.symbols.len() as u32);
                self.symbols.push(Symbol {
                    name: v.name.name.clone(),
                    span: v.name.span,
                    kind: SymbolKind::EnumVariant {
                        enum_sym: id,
                        discriminant: discriminant as u32,
                    },
                });
                self.decl_to_sym.insert(v.name.span, variant_id);
                variants.insert(v.name.name.clone(), variant_id);
                // Capture payload types per variant for the checker /
                // lowerer / codegen to look up.
                let payload_tys: Vec<crate::ast::Type> = match &v.fields {
                    crate::ast::VariantFields::Unit => Vec::new(),
                    crate::ast::VariantFields::Tuple(tys) => {
                        any_payload = any_payload || !tys.is_empty();
                        tys.clone()
                    }
                    crate::ast::VariantFields::Named(fields) => {
                        any_payload = any_payload || !fields.is_empty();
                        // Track names in declaration order so the
                        // checker / lowerer can reorder `Variant
                        // { name: val, ... }` into positional form.
                        let names: Vec<String> = fields
                            .iter()
                            .map(|f| f.name.name.clone())
                            .collect();
                        self.enum_variant_field_names
                            .insert(variant_id, names);
                        fields.iter().map(|f| f.ty.clone()).collect()
                    }
                };
                self.enum_variant_payloads.insert(variant_id, payload_tys);
            }
            self.enum_variants.insert(id, variants);
            if any_payload {
                self.enum_has_payload.insert(id);
            }
        }

        // For traits, stash the method signatures so the checker can
        // validate impls and resolve bounded-generic method calls.
        if let Item::Trait(t) = item {
            self.trait_methods.insert(id, t.methods.clone());
        }
    }

    // ---- pass 2: resolve bodies ----

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.resolve_fn(f),
            Item::Struct(s) => self.resolve_struct(s),
            Item::Enum(e) => self.resolve_enum(e),
            Item::Const(c) => self.resolve_const(c),
            Item::Impl(i) => {
                for method in &i.methods {
                    self.resolve_fn(method);
                }
            }
            Item::Trait(t) => {
                // Resolve the parameter / return types of each trait
                // method signature so `Self` and any referenced types
                // bind. The bodies don't exist (signatures only).
                for m in &t.methods {
                    self.enter_scope();
                    for p in &m.params {
                        self.resolve_type(&p.ty);
                    }
                    if let Some(rt) = &m.return_type {
                        self.resolve_type(rt);
                    }
                    self.exit_scope();
                }
            }
        }
    }

    fn resolve_fn(&mut self, f: &FnDecl) {
        self.enter_scope();
        for g in &f.generics {
            let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            // Resolve each `T: Bound` to the bound trait's symbol.
            let mut bound_syms: Vec<SymbolId> = Vec::new();
            for b in &g.bounds {
                if let Some(bsym) = self.lookup(&b.name) {
                    if matches!(self.symbols[bsym.0 as usize].kind, SymbolKind::Trait) {
                        bound_syms.push(bsym);
                    } else {
                        self.error(
                            format!("`{}` is not a trait", b.name),
                            b.span,
                        );
                    }
                } else {
                    self.error(format!("unresolved trait `{}`", b.name), b.span);
                }
            }
            if !bound_syms.is_empty() {
                self.generic_bounds.insert(id, bound_syms);
            }
        }
        for p in &f.params {
            self.resolve_type(&p.ty);
            let id = self.intern(p.name.name.clone(), p.name.span, SymbolKind::Param);
            self.decl_to_sym.insert(p.name.span, id);
        }
        if let Some(rt) = &f.return_type {
            self.resolve_type(rt);
        }
        self.resolve_block(&f.body);
        self.exit_scope();
    }

    fn resolve_struct(&mut self, s: &StructDecl) {
        self.enter_scope();
        let mut gen_syms: Vec<SymbolId> = Vec::with_capacity(s.generics.len());
        for g in &s.generics {
            let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            gen_syms.push(id);
        }
        if !gen_syms.is_empty() {
            if let Some(&struct_sym) = self.decl_to_sym.get(&s.name.span) {
                self.struct_generics.insert(struct_sym, gen_syms);
            }
        }
        for f in &s.fields {
            self.resolve_type(&f.ty);
        }
        self.exit_scope();
    }

    fn resolve_enum(&mut self, e: &EnumDecl) {
        self.enter_scope();
        let mut gen_syms: Vec<SymbolId> = Vec::with_capacity(e.generics.len());
        for g in &e.generics {
            let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            gen_syms.push(id);
        }
        if !gen_syms.is_empty() {
            if let Some(&enum_sym) = self.decl_to_sym.get(&e.name.span) {
                self.enum_generics.insert(enum_sym, gen_syms);
            }
        }
        for v in &e.variants {
            match &v.fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(types) => {
                    for t in types {
                        self.resolve_type(t);
                    }
                }
                VariantFields::Named(fields) => {
                    for f in fields {
                        self.resolve_type(&f.ty);
                    }
                }
            }
        }
        self.exit_scope();
    }

    fn resolve_const(&mut self, c: &ConstDecl) {
        self.resolve_type(&c.ty);
        self.resolve_expr(&c.value);
    }

    fn resolve_block(&mut self, b: &Block) {
        self.enter_scope();
        for stmt in &b.stmts {
            self.resolve_stmt(stmt);
        }
        self.exit_scope();
    }

    fn resolve_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l) => {
                if let Some(ty) = &l.ty {
                    self.resolve_type(ty);
                }
                if let Some(init) = &l.init {
                    self.resolve_expr(init);
                }
                self.declare_pattern(&l.pat, l.mutable);
            }
            Stmt::Expr(e, _) => self.resolve_expr(e),
            Stmt::Item(item) => {
                self.declare_item(item);
                self.resolve_item(item);
            }
        }
    }

    fn declare_pattern(&mut self, p: &Pattern, mutable_let: bool) {
        match p {
            Pattern::Wildcard(_) => {}
            Pattern::Ident { name, mutable: pat_mut, .. } => {
                let mutable = mutable_let || *pat_mut;
                let id = self.intern(
                    name.name.clone(),
                    name.span,
                    SymbolKind::Local { mutable },
                );
                self.decl_to_sym.insert(name.span, id);
            }
            Pattern::Literal { .. } => {}
            Pattern::Range { .. } => {
                // Bounds are literals — no names to resolve, no bindings.
            }
            Pattern::Path { path, .. } => {
                // No binding — just resolve the path so the checker /
                // lowerer can look up what variant it refers to.
                self.resolve_path(path);
            }
            Pattern::TupleVariant { path, fields, .. } => {
                // Resolve the variant path; declare any inner bindings.
                // The checker validates that each sub-pattern position
                // matches the variant's payload type.
                self.resolve_path(path);
                for sub in fields {
                    self.declare_pattern(sub, mutable_let);
                }
            }
            Pattern::NamedVariant { path, fields, .. } => {
                self.resolve_path(path);
                for (_, sub) in fields {
                    self.declare_pattern(sub, mutable_let);
                }
            }
            Pattern::Or { patterns, .. } => {
                for sub in patterns {
                    self.declare_pattern(sub, mutable_let);
                }
            }
        }
    }

    fn resolve_type(&mut self, t: &Type) {
        match t {
            Type::Path(p) => self.resolve_path(p),
        }
    }

    fn resolve_path(&mut self, p: &Path) {
        // Recurse into generic args so any nested paths resolve.
        for arg in &p.generic_args {
            self.resolve_type(arg);
        }
        let first = &p.segments[0];
        let Some(first_id) = self.lookup(&first.name) else {
            self.error(format!("unresolved name `{}`", first.name), p.span);
            return;
        };
        if p.segments.len() == 1 {
            self.path_to_sym.insert(p.span, first_id);
            return;
        }
        // Two-segment path: `Enum::Variant` is the only shape we resolve.
        if p.segments.len() == 2
            && matches!(self.symbols[first_id.0 as usize].kind, SymbolKind::Enum)
        {
            let variant_name = &p.segments[1].name;
            if let Some(map) = self.enum_variants.get(&first_id) {
                if let Some(&variant_id) = map.get(variant_name) {
                    self.path_to_sym.insert(p.span, variant_id);
                    return;
                }
            }
            self.error(
                format!(
                    "no variant `{}` on enum `{}`",
                    variant_name, first.name
                ),
                p.span,
            );
            return;
        }
        // Longer paths (`a::b::c`, namespacing) not supported yet.
        self.error(
            format!("path `{}` has more segments than the resolver handles", first.name),
            p.span,
        );
    }

    fn resolve_expr(&mut self, e: &Expr) {
        match e {
            Expr::Lit { .. } => {}
            Expr::Path(p) => self.resolve_path(p),
            Expr::Unary { expr, .. } => self.resolve_expr(expr),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Assign { lhs, rhs, .. }
            | Expr::AssignOp { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr(callee);
                for a in args {
                    self.resolve_expr(a);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.resolve_expr(receiver);
                for a in args {
                    self.resolve_expr(a);
                }
            }
            Expr::Field { receiver, .. } => self.resolve_expr(receiver),
            Expr::Index { receiver, index, .. } => {
                self.resolve_expr(receiver);
                self.resolve_expr(index);
            }
            Expr::Try { expr, .. } => self.resolve_expr(expr),
            Expr::Cast { expr, ty, .. } => {
                self.resolve_expr(expr);
                self.resolve_type(ty);
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            Expr::Block(b) => self.resolve_block(b),
            Expr::If { cond, then_branch, else_branch, .. } => {
                self.resolve_expr(cond);
                self.resolve_block(then_branch);
                if let Some(e) = else_branch {
                    self.resolve_expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.resolve_expr(cond);
                self.resolve_block(body);
            }
            Expr::For { pat, iter, body, .. } => {
                self.resolve_expr(iter);
                self.enter_scope();
                self.declare_pattern(pat, false);
                for stmt in &body.stmts {
                    self.resolve_stmt(stmt);
                }
                self.exit_scope();
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.resolve_expr(scrutinee);
                for arm in arms {
                    self.enter_scope();
                    self.declare_pattern(&arm.pat, false);
                    if let Some(g) = &arm.guard {
                        self.resolve_expr(g);
                    }
                    self.resolve_expr(&arm.body);
                    self.exit_scope();
                }
            }
            Expr::Range { start, end, .. } => {
                if let Some(s) = start.as_deref() {
                    self.resolve_expr(s);
                }
                if let Some(e) = end.as_deref() {
                    self.resolve_expr(e);
                }
            }
            Expr::StructLit { path, fields, .. } => {
                self.resolve_path(path);
                for f in fields {
                    self.resolve_expr(&f.value);
                }
            }
            Expr::Return { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(v);
                }
            }
            Expr::Break(_) | Expr::Continue(_) => {}
        }
    }
}
