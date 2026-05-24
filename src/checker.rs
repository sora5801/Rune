//! Type checker.
//!
//! Single pass after resolution. Walks the AST bottom-up, assigning types
//! to expressions and checking compatibility at use sites.
//!
//! Errors flow through via `Ty::Error`, which compares compatible with
//! everything to avoid cascading messages.

use std::collections::HashMap;
use std::fmt;

use crate::ast::*;
use crate::resolver::{Resolutions, SymbolKind};
use crate::token::Span;
use crate::ty::{SymbolId, Ty, DEFAULT_FLOAT, DEFAULT_INT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "type error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for TypeError {}

pub struct CheckResults {
    pub expr_types: HashMap<Span, Ty>,
    pub fn_signatures: HashMap<Span, Ty>,
    pub local_types: HashMap<Span, Ty>,
    pub type_resolutions: HashMap<Span, Ty>,
    pub struct_layouts: HashMap<SymbolId, StructLayout>,
    /// Spans of expressions that coerce a concrete struct into a
    /// `dyn Trait` — `expr span → (struct sym, trait sym)`. The
    /// lowerer wraps each such expression in a `DynBox`.
    /// Coercion sites where a concrete struct is wrapped into a
    /// trait object. Keyed by the value's source span; the value
    /// is `(struct_sym, trait_sym, trait_args)`. The trait args
    /// carry generic-trait instantiation info (`dyn Fn1<i64, i64>`
    /// vs `dyn Fn1<str, bool>`); non-generic traits use an empty
    /// args list.
    pub dyn_coercions: HashMap<Span, (SymbolId, SymbolId, Vec<Ty>)>,
    /// `(struct sym, associated-type name) → resolved `Ty``. The
    /// resolver records the AST type each impl binds; the checker
    /// resolves those once with `current_self = Impl(struct_sym)`
    /// so projections resolve at substitution time.
    pub impl_assoc_bindings_ty: HashMap<(SymbolId, String), Ty>,
    /// Per-closure inferred parameter types (in declaration order).
    /// The lowerer uses these to build the synthesized `HirFn`'s
    /// params for each `|x| body` expression.
    pub closure_param_tys: HashMap<Span, Vec<Ty>>,
    /// Per-closure inferred return type. From the body's tail (or
    /// the contextual expected `Ty::Fn`'s ret).
    pub closure_ret_tys: HashMap<Span, Ty>,
    pub errors: Vec<TypeError>,
}

#[derive(Debug, Clone)]
pub struct StructLayout {
    pub fields: Vec<StructLayoutField>,
    /// Total size in bytes (with 8-byte-per-field padding for v0.x).
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct StructLayoutField {
    pub name: String,
    pub ty: Ty,
    pub offset: u32,
}

impl StructLayout {
    pub fn field(&self, name: &str) -> Option<&StructLayoutField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// What `Self::Item` refers to while the checker is type-checking
/// a particular item. `Impl(struct_sym)` resolves to the impl's
/// concrete binding from `impl_assoc_bindings`; `Trait(trait_sym)`
/// is abstract — `Self::Item` types as `Ty::Error` there.
#[derive(Clone, Copy)]
enum SelfContext {
    Impl(SymbolId),
    Trait(SymbolId),
}

pub struct Checker<'r> {
    res: &'r Resolutions,
    expr_types: HashMap<Span, Ty>,
    fn_signatures: HashMap<Span, Ty>,
    local_types: HashMap<Span, Ty>,
    type_resolutions: HashMap<Span, Ty>,
    struct_layouts: HashMap<SymbolId, StructLayout>,
    dyn_coercions: HashMap<Span, (SymbolId, SymbolId, Vec<Ty>)>,
    impl_assoc_bindings_ty: HashMap<(SymbolId, String), Ty>,
    closure_param_tys: HashMap<Span, Vec<Ty>>,
    closure_ret_tys: HashMap<Span, Ty>,
    errors: Vec<TypeError>,
    current_return: Ty,
    /// The trait or impl whose signatures / bodies are currently
    /// being checked — set around `register_signatures` and
    /// `check_item` for traits and impls, used to resolve
    /// `Self::Item` to a concrete type.
    current_self: Option<SelfContext>,
}

impl<'r> Checker<'r> {
    pub fn new(res: &'r Resolutions) -> Self {
        Self {
            res,
            expr_types: HashMap::new(),
            fn_signatures: HashMap::new(),
            local_types: HashMap::new(),
            type_resolutions: HashMap::new(),
            struct_layouts: HashMap::new(),
            dyn_coercions: HashMap::new(),
            impl_assoc_bindings_ty: HashMap::new(),
            closure_param_tys: HashMap::new(),
            closure_ret_tys: HashMap::new(),
            errors: Vec::new(),
            current_return: Ty::Unit,
            current_self: None,
        }
    }

    pub fn check_module(mut self, m: &Module) -> CheckResults {
        // Pass 1a: struct layouts — recurse into modules.
        self.collect_struct_layouts(&m.items);
        // Pass 1b: function signatures + const types + impl methods +
        // trait signature types — recurse into modules.
        self.register_signatures(&m.items);
        // Pass 2: bodies.
        for item in &m.items {
            self.check_item(item);
        }

        CheckResults {
            expr_types: self.expr_types,
            fn_signatures: self.fn_signatures,
            local_types: self.local_types,
            type_resolutions: self.type_resolutions,
            struct_layouts: self.struct_layouts,
            dyn_coercions: self.dyn_coercions,
            impl_assoc_bindings_ty: self.impl_assoc_bindings_ty,
            closure_param_tys: self.closure_param_tys,
            closure_ret_tys: self.closure_ret_tys,
            errors: self.errors,
        }
    }

    /// Pass 1a — collect struct layouts, recursing into modules.
    fn collect_struct_layouts(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Struct(s) => {
                    if let Some(&sym_id) = self.res.decl_to_sym.get(&s.name.span) {
                        let layout = self.build_struct_layout(s);
                        self.struct_layouts.insert(sym_id, layout);
                    }
                }
                Item::Mod(md) => self.collect_struct_layouts(&md.items),
                _ => {}
            }
        }
    }

    /// Pass 1b — register fn/const/impl/trait signatures, recursing
    /// into modules.
    fn register_signatures(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    let sig = self.fn_signature(f);
                    self.fn_signatures.insert(f.name.span, sig);
                }
                Item::Const(c) => {
                    let ty = self.resolve_type(&c.ty);
                    self.local_types.insert(c.name.span, ty);
                }
                Item::Impl(i) => {
                    // Set `current_self` so the method signatures'
                    // `Self::Item` references resolve to this impl's
                    // concrete binding — the stored `fn_signatures`
                    // are then fully substituted.
                    let prev = self.current_self.take();
                    let struct_sym = self.res.path_to_sym.get(&i.type_path.span).copied();
                    if let Some(sym) = struct_sym {
                        self.current_self = Some(SelfContext::Impl(sym));
                    }
                    // Pre-resolve every `type Item = ..;` binding to
                    // a concrete `Ty` so substitution-time projection
                    // resolution doesn't need the resolver's AST.
                    //
                    // Subtle: the impl block's own `<T>` is interned
                    // as a *different* SymbolId from the struct's
                    // `<T>` (resolver passes intern them in distinct
                    // scopes — see the `intern_generic_param`
                    // comment). The binding `type Item = T` records
                    // `Ty::TypeVar(T_impl)`, but downstream
                    // substitution uses the struct's generic params
                    // (`struct_generics[s] = [T_struct]`). Remap
                    // here so the stored binding speaks the
                    // struct's language. The impl's type-path is
                    // resolved to `Ty::Struct(s, [TypeVar(T_impl)])`;
                    // zip with `[T_struct]` to build the remap.
                    if let Some(sym) = struct_sym {
                        // Build the impl-T → struct-T remap by
                        // resolving each of the impl type-path's
                        // generic args (their AST nodes are already
                        // walked by the resolver, so `resolve_type`
                        // returns a `Ty::TypeVar(impl_T)`) and
                        // pairing with the struct's declared generic
                        // params in declaration order.
                        let mut impl_to_struct: std::collections::HashMap<SymbolId, Ty> =
                            std::collections::HashMap::new();
                        if let Some(struct_params) = self.res.struct_generics.get(&sym).cloned() {
                            for (ast_arg, &sg) in
                                i.type_path.generic_args.iter().zip(struct_params.iter())
                            {
                                if let Ty::TypeVar(tv) = self.resolve_type(ast_arg) {
                                    impl_to_struct.insert(tv, Ty::TypeVar(sg));
                                }
                            }
                        }
                        for binding in &i.assoc_types {
                            let raw_ty = self.resolve_type(&binding.value);
                            let remapped = self.apply_subst(&raw_ty, &impl_to_struct, None);
                            self.impl_assoc_bindings_ty
                                .insert((sym, binding.name.name.clone()), remapped);
                        }
                    }
                    for method in &i.methods {
                        let sig = self.fn_signature(method);
                        self.fn_signatures.insert(method.name.span, sig);
                    }
                    self.current_self = prev;
                }
                Item::Trait(t) => {
                    let prev = self.current_self.take();
                    if let Some(&trait_sym) =
                        self.res.decl_to_sym.get(&t.name.span)
                    {
                        self.current_self = Some(SelfContext::Trait(trait_sym));
                    }
                    for m in &t.methods {
                        for p in &m.params {
                            self.resolve_type(&p.ty);
                        }
                        if let Some(rt) = &m.return_type {
                            self.resolve_type(rt);
                        }
                    }
                    self.current_self = prev;
                }
                Item::Mod(md) => self.register_signatures(&md.items),
                Item::Struct(_) | Item::Enum(_) | Item::Use(_) => {}
            }
        }
    }

    fn build_struct_layout(&mut self, s: &StructDecl) -> StructLayout {
        let mut fields = Vec::with_capacity(s.fields.len());
        let mut offset: u32 = 0;
        for f in &s.fields {
            let ty = self.resolve_type(&f.ty);
            fields.push(StructLayoutField {
                name: f.name.name.clone(),
                ty,
                offset,
            });
            // v0.x simplification: every field is padded to 8 bytes. Avoids
            // dealing with field alignment until we have varied widths.
            offset += 8;
        }
        StructLayout { fields, size: offset }
    }

    /// Substitute type parameters in `ty`, resolving any associated-
    /// type projection (`Ty::Assoc`) once its base becomes a
    /// concrete struct known in `impl_assoc_bindings_ty`. Pass a
    /// `self_ty` to substitute `Ty::SelfType` — the trait-side
    /// stand-in produced by `Self::Item` in a trait method
    /// signature. Most callers pass `None`; only the trait-bound
    /// method-lookup path supplies a `Self` replacement.
    fn apply_subst(
        &self,
        ty: &Ty,
        subst: &std::collections::HashMap<SymbolId, Ty>,
        self_ty: Option<&Ty>,
    ) -> Ty {
        apply_subst_inner_with(
            ty,
            subst,
            self_ty,
            Some(&self.impl_assoc_bindings_ty),
            Some(self.res),
        )
    }

    fn fn_signature(&mut self, f: &FnDecl) -> Ty {
        let params: Vec<Ty> = f.params.iter().map(|p| self.resolve_type(&p.ty)).collect();
        let ret = f
            .return_type
            .as_ref()
            .map(|t| self.resolve_type(t))
            .unwrap_or(Ty::Unit);
        Ty::Fn { params, ret: Box::new(ret) }
    }

    fn resolve_type(&mut self, t: &Type) -> Ty {
        match t {
            Type::Path(p) => {
                // `Self::Item` — resolve from the enclosing impl /
                // trait. The resolver leaves these alone, so
                // `path_to_sym` has no entry to consult.
                if p.segments.len() == 2 && p.segments[0].name.as_str() == "Self" {
                    let name = p.segments[1].name.as_str();
                    let ty = match self.current_self {
                        Some(SelfContext::Impl(struct_sym)) => {
                            match self
                                .res
                                .impl_assoc_bindings
                                .get(&(struct_sym, name.to_string()))
                                .cloned()
                            {
                                Some(rhs_ast) => self.resolve_type(&rhs_ast),
                                None => {
                                    self.error(
                                        p.span,
                                        format!(
                                            "no associated type `{}` bound by this impl",
                                            name
                                        ),
                                    );
                                    Ty::Error
                                }
                            }
                        }
                        Some(SelfContext::Trait(trait_sym)) => {
                            let known = self
                                .res
                                .trait_assoc_types
                                .get(&trait_sym)
                                .map(|ns| ns.iter().any(|n| n == name))
                                .unwrap_or(false);
                            if !known {
                                self.error(
                                    p.span,
                                    format!(
                                        "trait declares no associated type `{}`",
                                        name
                                    ),
                                );
                                Ty::Error
                            } else {
                                // Trait side: leave the position as a
                                // projection through `Ty::SelfType`.
                                // `trait_bound_method_sig` substitutes
                                // it to the bound type at the call
                                // site, yielding `Ty::Assoc(TypeVar(T),
                                // "Item")` which resolves at
                                // monomorphization.
                                Ty::Assoc(
                                    Box::new(Ty::SelfType),
                                    name.to_string(),
                                )
                            }
                        }
                        None => {
                            self.error(
                                p.span,
                                "`Self` is only valid inside a trait or impl".to_string(),
                            );
                            Ty::Error
                        }
                    };
                    self.type_resolutions.insert(p.span, ty.clone());
                    return ty;
                }
                // `T::Item` — the resolver records the base TypeParam
                // symbol; the checker builds `Ty::Assoc(TypeVar(T),
                // name)` and the monomorphizer resolves once `T`
                // becomes concrete.
                if let Some(&base_sym) = self.res.assoc_proj_bases.get(&p.span) {
                    let name = p.segments[1].name.clone();
                    let ty = Ty::Assoc(Box::new(Ty::TypeVar(base_sym)), name);
                    self.type_resolutions.insert(p.span, ty.clone());
                    return ty;
                }
                let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
                    return Ty::Error;
                };
                let kind = self.res.symbol(sym_id).kind.clone();
                let name = self.res.symbol(sym_id).name.clone();
                // Resolve the path's generic args (e.g. `<i64>` in
                // `Vec<i64>`) so generic struct/enum types carry their
                // instantiation.
                let type_args: Vec<Ty> =
                    p.generic_args.iter().map(|t| self.resolve_type(t)).collect();
                let ty = match kind {
                    // `Weak<T>` is a builtin parametric type. The
                    // sentinel from the resolver is just a marker;
                    // we read the path's generic args to build the
                    // real Ty::Weak(inner).
                    SymbolKind::BuiltinType(Ty::Weak(_)) => {
                        if type_args.len() != 1 {
                            self.error(
                                p.span,
                                "`Weak` requires exactly one type argument".to_string(),
                            );
                            Ty::Error
                        } else {
                            Ty::Weak(Box::new(type_args[0].clone()))
                        }
                    }
                    // `Vec<T>` is a builtin parametric type — read the
                    // element type from the path's generic args.
                    SymbolKind::BuiltinType(Ty::Vec(_)) => {
                        if type_args.len() != 1 {
                            self.error(
                                p.span,
                                "`Vec` requires exactly one type argument".to_string(),
                            );
                            Ty::Error
                        } else {
                            let elem = type_args[0].clone();
                            if vec_element_supported(&elem) {
                                Ty::Vec(Box::new(elem))
                            } else {
                                self.error(
                                    p.span,
                                    format!(
                                        "`Vec<{}>` is not supported in v0.x — the \
                                         element type must fit an 8-byte slot \
                                         (integers, bool, char, structs, enums, \
                                         trait objects, or a nested Vec; not str, \
                                         floats, or arrays)",
                                        elem.display()
                                    ),
                                );
                                Ty::Error
                            }
                        }
                    }
                    SymbolKind::BuiltinType(t) => t,
                    SymbolKind::Struct => Ty::Struct(sym_id, type_args.clone()),
                    SymbolKind::Enum => Ty::Enum(sym_id, type_args.clone()),
                    SymbolKind::TypeParam => Ty::TypeVar(sym_id),
                    _ => {
                        self.error(p.span, format!("`{}` is not a type", name));
                        Ty::Error
                    }
                };
                self.type_resolutions.insert(p.span, ty.clone());
                ty
            }
            Type::Dyn(p) => {
                let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
                    return Ty::Error;
                };
                if !matches!(self.res.symbol(sym_id).kind, SymbolKind::Trait) {
                    let name = self.res.symbol(sym_id).name.clone();
                    self.error(
                        p.span,
                        format!("`dyn {}` — `{}` is not a trait", name, name),
                    );
                    return Ty::Error;
                }
                // Resolve any generic args on the trait path:
                // `dyn Producer<i64>` → `Ty::Dyn(ProducerSym, [i64])`.
                let type_args: Vec<Ty> =
                    p.generic_args.iter().map(|t| self.resolve_type(t)).collect();
                let ty = Ty::Dyn(sym_id, type_args);
                self.type_resolutions.insert(p.span, ty.clone());
                ty
            }
            Type::Array { elem, len, span } => {
                let elem_ty = self.resolve_type(elem);
                let ty = Ty::Array(Box::new(elem_ty), *len);
                self.type_resolutions.insert(*span, ty.clone());
                ty
            }
            Type::Fn { params, ret, span } => {
                let param_tys: Vec<Ty> =
                    params.iter().map(|t| self.resolve_type(t)).collect();
                let ret_ty = ret
                    .as_ref()
                    .map(|r| self.resolve_type(r))
                    .unwrap_or(Ty::Unit);
                let ty = Ty::Fn {
                    params: param_tys,
                    ret: Box::new(ret_ty),
                };
                self.type_resolutions.insert(*span, ty.clone());
                ty
            }
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.check_fn(f),
            Item::Const(c) => self.check_const(c),
            Item::Impl(i) => {
                let prev = self.current_self.take();
                if let Some(&struct_sym) =
                    self.res.path_to_sym.get(&i.type_path.span)
                {
                    self.current_self = Some(SelfContext::Impl(struct_sym));
                }
                for method in &i.methods {
                    self.check_fn(method);
                }
                self.check_trait_impl_conformance(i);
                self.current_self = prev;
            }
            Item::Mod(md) => {
                for inner in &md.items {
                    self.check_item(inner);
                }
            }
            Item::Struct(_) | Item::Enum(_) | Item::Trait(_) | Item::Use(_) => {
                // Field/variant types were resolved by the resolver;
                // trait method signature types are resolved in pass 1b;
                // `use` is fully handled by the resolver.
            }
        }
    }

    /// For an `impl Trait for Type`, verify every trait method has a
    /// matching impl and that arities line up. Signature equality is
    /// checked loosely (arity only) in v0.x — full param-by-param
    /// type conformance with `Self` substitution is a follow-up.
    fn check_trait_impl_conformance(&mut self, i: &ImplBlock) {
        let Some(trait_path) = &i.trait_path else { return };
        let Some(&trait_sym) = self.res.path_to_sym.get(&trait_path.span) else {
            return;
        };
        let Some(trait_sigs) = self.res.trait_methods.get(&trait_sym).cloned() else {
            return;
        };
        for sig in &trait_sigs {
            match i.methods.iter().find(|m| m.name.name == sig.name.name) {
                None => {
                    self.error(
                        i.span,
                        format!(
                            "impl is missing method `{}` required by the trait",
                            sig.name.name
                        ),
                    );
                }
                Some(m) => {
                    if m.params.len() != sig.params.len() {
                        self.error(
                            m.name.span,
                            format!(
                                "method `{}` has {} parameter(s) but the trait \
                                 declares {}",
                                sig.name.name,
                                m.params.len(),
                                sig.params.len()
                            ),
                        );
                    }
                }
            }
        }
        // Associated-type conformance: every assoc type the trait
        // declares must have a binding; the impl must not bind any
        // name the trait did not declare; and no duplicates.
        let declared: Vec<String> = self
            .res
            .trait_assoc_types
            .get(&trait_sym)
            .cloned()
            .unwrap_or_default();
        let Some(&struct_sym) = self.res.path_to_sym.get(&i.type_path.span) else {
            return;
        };
        // Supertrait conformance: walk the trait's supertrait closure
        // and require an `impl Super for Type` for each ancestor.
        // `visited` keeps the walk finite even with a (diagnosed)
        // supertrait cycle.
        let mut worklist: Vec<SymbolId> = self
            .res
            .trait_supertraits
            .get(&trait_sym)
            .cloned()
            .unwrap_or_default();
        let mut visited: std::collections::HashSet<SymbolId> =
            std::collections::HashSet::new();
        while let Some(anc) = worklist.pop() {
            if !visited.insert(anc) {
                continue;
            }
            let has_impl = self
                .res
                .impls_for
                .get(&struct_sym)
                .map_or(false, |s| s.contains(&anc));
            if !has_impl {
                self.error(
                    i.span,
                    format!(
                        "trait `{}` requires supertrait `{}` to be implemented for `{}`",
                        self.res.symbol(trait_sym).name,
                        self.res.symbol(anc).name,
                        self.res.symbol(struct_sym).name,
                    ),
                );
            }
            if let Some(supers) = self.res.trait_supertraits.get(&anc) {
                worklist.extend(supers);
            }
        }
        for name in &declared {
            if !self
                .res
                .impl_assoc_bindings
                .contains_key(&(struct_sym, name.clone()))
            {
                self.error(
                    i.span,
                    format!(
                        "impl is missing associated type `{}` required by the trait",
                        name
                    ),
                );
            }
        }
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for binding in &i.assoc_types {
            if !declared.iter().any(|n| n == &binding.name.name) {
                self.error(
                    binding.span,
                    format!(
                        "trait declares no associated type `{}`",
                        binding.name.name
                    ),
                );
            }
            if !seen.insert(binding.name.name.clone()) {
                self.error(
                    binding.span,
                    format!(
                        "associated type `{}` already bound in this impl",
                        binding.name.name
                    ),
                );
            }
        }
    }

    fn check_fn(&mut self, f: &FnDecl) {
        let sig = self.fn_signatures.get(&f.name.span).cloned().unwrap_or(Ty::Error);
        let (param_tys, ret_ty) = match sig {
            Ty::Fn { params, ret } => (params, *ret),
            _ => (Vec::new(), Ty::Error),
        };
        for (param, ty) in f.params.iter().zip(&param_tys) {
            self.local_types.insert(param.name.span, ty.clone());
        }
        let prev_ret = std::mem::replace(&mut self.current_return, ret_ty.clone());
        let body_ty = self.check_block(&f.body);
        if !body_ty.compatible(&ret_ty) {
            self.error(
                f.body.span,
                format!(
                    "function `{}` returns `{}` but body has type `{}`",
                    f.name.name,
                    ret_ty.display(),
                    body_ty.display()
                ),
            );
        }
        self.current_return = prev_ret;
    }

    fn check_const(&mut self, c: &ConstDecl) {
        let declared = self.local_types.get(&c.name.span).cloned().unwrap_or(Ty::Error);
        let actual = self.check_expr(&c.value);
        if !actual.compatible(&declared) {
            self.error(
                c.value.span(),
                format!(
                    "const `{}` declared as `{}` but value has type `{}`",
                    c.name.name,
                    declared.display(),
                    actual.display(),
                ),
            );
        }
    }

    fn check_block(&mut self, b: &Block) -> Ty {
        let mut last_ty = Ty::Unit;
        for stmt in &b.stmts {
            last_ty = self.check_stmt(stmt);
        }
        last_ty
    }

    fn check_stmt(&mut self, s: &Stmt) -> Ty {
        match s {
            Stmt::Let(l) => {
                self.check_let(l);
                Ty::Unit
            }
            Stmt::Expr(e, has_semi) => {
                let t = self.check_expr(e);
                if *has_semi { Ty::Unit } else { t }
            }
            Stmt::Item(item) => {
                // Bring nested item's signature into scope so it can be checked.
                if let Item::Fn(f) = item {
                    let sig = self.fn_signature(f);
                    self.fn_signatures.insert(f.name.span, sig);
                }
                if let Item::Const(c) = item {
                    let ty = self.resolve_type(&c.ty);
                    self.local_types.insert(c.name.span, ty);
                }
                self.check_item(item);
                Ty::Unit
            }
        }
    }

    fn check_let(&mut self, l: &LetStmt) {
        let declared = l.ty.as_ref().map(|t| self.resolve_type(t));
        // Pass `declared` as a hint to closure inits so an
        // unannotated `|x| body` picks up its param types from
        // the declared `fn(...) -> ...` annotation.
        let inferred = l
            .init
            .as_ref()
            .map(|e| self.check_expr_with_hint(e, declared.as_ref()));
        let final_ty = match (declared, inferred) {
            (Some(d), Some(i)) => {
                let init_span =
                    l.init.as_ref().map(|e| e.span()).unwrap_or(l.span);
                // Capturing-closure special case: when the init is
                // a `Ty::Struct(closure_sym, [])` whose struct has
                // a `call` method matching the declared `fn(...)`
                // signature, the annotation served as the
                // bidirectional-inference hint and the binding's
                // bound type is the *actual* closure struct (so
                // the call site dispatches via the struct's call
                // method rather than as a fn pointer).
                if let (Ty::Struct(s, _), Ty::Fn { .. }) = (&i, &d) {
                    if self
                        .res
                        .impl_methods
                        .get(&(*s, "call".to_string()))
                        .is_some()
                    {
                        self.bind_pattern(&l.pat, &i);
                        return;
                    }
                }
                if !self.check_assignable(init_span, &i, &d) {
                    self.error(
                        l.span,
                        format!(
                            "let binding declared `{}` but initializer has type `{}`",
                            d.display(),
                            i.display()
                        ),
                    );
                }
                d
            }
            (Some(d), None) => d,
            (None, Some(i)) => i,
            (None, None) => {
                self.error(l.span, "let binding has neither type nor initializer");
                Ty::Error
            }
        };
        self.bind_pattern(&l.pat, &final_ty);
    }

    /// Checks that a match expression's arms cover the scrutinee's
    /// domain (or have a catch-all). Also flags arms that are
    /// unreachable because an earlier arm matches everything they do.
    fn check_match_exhaustiveness(
        &mut self,
        scrutinee_ty: &Ty,
        arms: &[MatchArm],
        match_span: Span,
    ) {
        use std::collections::HashSet;
        if scrutinee_ty.is_error() {
            return; // suppress cascade
        }

        // First pass: detect coverage and unreachable arms.
        let mut catchall_seen: Option<Span> = None;
        let mut covered_bools: HashSet<bool> = HashSet::new();
        let mut covered_variants: HashSet<u32> = HashSet::new();
        let mut covered_ints: HashSet<i64> = HashSet::new();
        let mut covered_strs: HashSet<String> = HashSet::new();

        for arm in arms {
            // If a catch-all already fired, every subsequent arm is dead.
            if catchall_seen.is_some() {
                self.error(
                    arm.pat.span(),
                    "unreachable match arm — an earlier arm covers everything",
                );
                continue;
            }
            // Guarded arms don't "cover" the pattern fully (the guard
            // can fail), so they don't add to coverage sets and they
            // don't become catch-alls.
            let guarded = arm.guard.is_some();
            self.cover_pattern(
                &arm.pat,
                guarded,
                &mut catchall_seen,
                &mut covered_bools,
                &mut covered_variants,
                &mut covered_ints,
                &mut covered_strs,
            );
        }

        if catchall_seen.is_some() {
            return; // exhaustive by catch-all
        }

        // No catch-all — check domain coverage by type.
        match scrutinee_ty {
            Ty::Bool => {
                let missing: Vec<&str> = [false, true]
                    .iter()
                    .filter(|b| !covered_bools.contains(b))
                    .map(|b| if *b { "true" } else { "false" })
                    .collect();
                if !missing.is_empty() {
                    self.error(
                        match_span,
                        format!(
                            "non-exhaustive `match` on `bool`: missing arms for {}",
                            missing.join(", ")
                        ),
                    );
                }
            }
            Ty::Enum(enum_sym, _) => {
                let Some(variants) = self.res.enum_variants.get(enum_sym) else {
                    return;
                };
                let missing: Vec<String> = variants
                    .iter()
                    .filter_map(|(name, sid)| match self.res.symbol(*sid).kind {
                        SymbolKind::EnumVariant { discriminant, .. }
                            if !covered_variants.contains(&discriminant) =>
                        {
                            Some(name.clone())
                        }
                        _ => None,
                    })
                    .collect();
                if !missing.is_empty() {
                    let enum_name = self.res.symbol(*enum_sym).name.clone();
                    self.error(
                        match_span,
                        format!(
                            "non-exhaustive `match` on enum `{}`: missing arms for {}",
                            enum_name,
                            missing.join(", ")
                        ),
                    );
                }
            }
            _ => {
                // i64, str, char, float, Vec, struct, etc. — infinite or
                // unenumerable domains. Require a catch-all.
                self.error(
                    match_span,
                    format!(
                        "non-exhaustive `match` on `{}`: add a `_` arm to catch the rest",
                        scrutinee_ty.display()
                    ),
                );
            }
        }
    }

    /// Recursive helper for exhaustiveness. Walks a single arm's pattern
    /// (possibly an Or) and updates the coverage sets.
    ///
    /// Coverage rules:
    /// - Unguarded patterns check membership + insert. Duplicate insert
    ///   is an unreachable error.
    /// - Guarded patterns are skipped — they neither check nor insert.
    ///   `match s { Ok if cond => ..., Ok => ... }` is valid because
    ///   the second arm is reachable when the guard fails.
    fn cover_pattern(
        &mut self,
        pat: &Pattern,
        guarded: bool,
        catchall_seen: &mut Option<Span>,
        covered_bools: &mut std::collections::HashSet<bool>,
        covered_variants: &mut std::collections::HashSet<u32>,
        covered_ints: &mut std::collections::HashSet<i64>,
        covered_strs: &mut std::collections::HashSet<String>,
    ) {
        if guarded {
            // Guarded arms can fail at runtime; they don't contribute
            // to coverage and don't conflict with later arms.
            return;
        }
        match pat {
            Pattern::Wildcard(s) | Pattern::Ident { span: s, .. } => {
                *catchall_seen = Some(*s);
            }
            Pattern::Literal { lit, span: s } => match lit {
                Lit::Bool(b) => {
                    if !covered_bools.insert(*b) {
                        self.error(
                            *s,
                            format!("unreachable arm — `{}` was already covered", b),
                        );
                    }
                }
                Lit::Int(v) => {
                    if !covered_ints.insert(*v) {
                        self.error(
                            *s,
                            format!("unreachable arm — `{}` was already covered", v),
                        );
                    }
                }
                Lit::Str(text) => {
                    if !covered_strs.insert(text.clone()) {
                        self.error(
                            *s,
                            format!(
                                "unreachable arm — `\"{}\"` was already covered",
                                text
                            ),
                        );
                    }
                }
                _ => {}
            },
            Pattern::Path { path, span: s } => {
                if let Some(&sid) = self.res.path_to_sym.get(&path.span) {
                    if let SymbolKind::EnumVariant { discriminant, .. } =
                        self.res.symbol(sid).kind
                    {
                        if !covered_variants.insert(discriminant) {
                            self.error(
                                *s,
                                "unreachable arm — this variant was already covered",
                            );
                        }
                    }
                }
            }
            Pattern::Range { .. } => {
                // Ranges cover a subset of an infinite domain; we don't
                // track partial coverage. They neither contribute to
                // exhaustiveness nor cause duplicate-arm errors against
                // literals or other ranges. A standalone `0..=10 => ...`
                // still needs a `_` arm to be exhaustive.
            }
            Pattern::TupleVariant { path, span: s, .. }
            | Pattern::NamedVariant { path, span: s, .. } => {
                // A tuple/named-variant pattern covers exactly the
                // same discriminant as the bare `EnumName::Variant`
                // path — bindings inside don't change coverage.
                if let Some(&sid) = self.res.path_to_sym.get(&path.span) {
                    if let SymbolKind::EnumVariant { discriminant, .. } =
                        self.res.symbol(sid).kind
                    {
                        if !covered_variants.insert(discriminant) {
                            self.error(
                                *s,
                                "unreachable arm — this variant was already covered",
                            );
                        }
                    }
                }
            }
            Pattern::Or { patterns, .. } => {
                for sub in patterns {
                    self.cover_pattern(
                        sub,
                        guarded,
                        catchall_seen,
                        covered_bools,
                        covered_variants,
                        covered_ints,
                        covered_strs,
                    );
                }
            }
        }
    }

    /// Validates that a pattern is compatible with a scrutinee type.
    /// Wildcard and Ident patterns always match. Literal patterns must
    /// have the right type. Path patterns must resolve to a variant of
    /// the scrutinee's enum.
    fn check_pattern_matches(&mut self, pat: &Pattern, scrutinee_ty: &Ty) {
        match pat {
            Pattern::Wildcard(_) | Pattern::Ident { .. } => {}
            Pattern::Literal { lit, span } => {
                let pat_ty = self.lit_type(lit);
                if !scrutinee_ty.is_error() && !pat_ty.compatible(scrutinee_ty) {
                    self.error(
                        *span,
                        format!(
                            "pattern type `{}` doesn't match scrutinee type `{}`",
                            pat_ty.display(),
                            scrutinee_ty.display()
                        ),
                    );
                }
            }
            Pattern::Path { path, span } => {
                let Some(&sym_id) = self.res.path_to_sym.get(&path.span) else {
                    return; // resolver already complained
                };
                let kind = self.res.symbol(sym_id).kind.clone();
                match kind {
                    SymbolKind::EnumVariant { enum_sym, .. } => {
                        let pat_ty = Ty::Enum(enum_sym, Vec::new());
                        if !scrutinee_ty.is_error()
                            && !pat_ty.compatible(scrutinee_ty)
                        {
                            self.error(
                                *span,
                                format!(
                                    "pattern matches `{}` but scrutinee is `{}`",
                                    pat_ty.display(),
                                    scrutinee_ty.display()
                                ),
                            );
                        }
                    }
                    _ => {
                        self.error(
                            *span,
                            "pattern path must resolve to an enum variant".to_string(),
                        );
                    }
                }
            }
            Pattern::Range { lo, hi, inclusive, span } => {
                self.check_range_pattern(lo, hi, *inclusive, *span, scrutinee_ty);
            }
            Pattern::TupleVariant { path, fields, span } => {
                self.check_tuple_variant_pattern(path, fields, *span, scrutinee_ty);
            }
            Pattern::NamedVariant { path, fields, span } => {
                self.check_named_variant_pattern(path, fields, *span, scrutinee_ty);
            }
            Pattern::Or { patterns, span } => {
                // Reject Bind patterns inside an Or — alternatives would
                // create multiple distinct symbols with the same name and
                // codegen can't pick one.
                for sub in patterns {
                    if let Pattern::Ident { name, .. } = sub {
                        self.error(
                            *span,
                            format!(
                                "or-pattern can't contain a binding (`{}`); \
                                 alternatives can only be `_`, literals, or enum variants",
                                name.name
                            ),
                        );
                    }
                    self.check_pattern_matches(sub, scrutinee_ty);
                }
            }
        }
    }

    fn check_range_pattern(
        &mut self,
        lo: &Lit,
        hi: &Lit,
        inclusive: bool,
        span: Span,
        scrutinee_ty: &Ty,
    ) {
        let (lo_v, hi_v) = match (lo, hi) {
            (Lit::Int(a), Lit::Int(b)) => {
                if !scrutinee_ty.is_error() && !scrutinee_ty.is_integer() {
                    self.error(
                        span,
                        format!(
                            "range pattern with integer bounds doesn't match \
                             scrutinee type `{}`",
                            scrutinee_ty.display()
                        ),
                    );
                    return;
                }
                (*a, *b)
            }
            (Lit::Char(a), Lit::Char(b)) => {
                if !scrutinee_ty.is_error() && *scrutinee_ty != Ty::Char {
                    self.error(
                        span,
                        format!(
                            "range pattern with char bounds doesn't match \
                             scrutinee type `{}`",
                            scrutinee_ty.display()
                        ),
                    );
                    return;
                }
                (*a as i64, *b as i64)
            }
            _ => {
                self.error(
                    span,
                    "range pattern bounds must be two integers or two chars"
                        .to_string(),
                );
                return;
            }
        };
        let valid = if inclusive { lo_v <= hi_v } else { lo_v < hi_v };
        if !valid {
            self.error(
                span,
                format!(
                    "range pattern `{}{}{}` is empty (lo must be {} hi)",
                    lo_v,
                    if inclusive { "..=" } else { ".." },
                    hi_v,
                    if inclusive { "<=" } else { "<" },
                ),
            );
        }
    }

    fn bind_pattern(&mut self, p: &Pattern, ty: &Ty) {
        match p {
            Pattern::Wildcard(_) => {}
            Pattern::Ident { name, .. } => {
                self.local_types.insert(name.span, ty.clone());
            }
            Pattern::Literal { .. } => {}
            Pattern::Range { .. } => {
                // Range patterns don't bind.
            }
            Pattern::Path { .. } => {
                // Path patterns don't bind. The match/let context is
                // responsible for validating the variant against the
                // scrutinee type.
            }
            Pattern::TupleVariant { path, fields, .. } => {
                // Bind each sub-pattern to the corresponding payload
                // type. For a generic enum scrutinee like `Option<i64>`,
                // substitute the enum's generic args so the binding
                // type is concrete (i64, not TypeVar(T)).
                if let Some(&variant_sym) = self.res.path_to_sym.get(&path.span) {
                    let subst = build_enum_subst_from_scrutinee(self.res, ty);
                    let payloads: Vec<Ty> = self
                        .res
                        .enum_variant_payloads
                        .get(&variant_sym)
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|t| apply_subst(&self.resolve_type(t), &subst))
                        .collect();
                    for (sub, pty) in fields.iter().zip(payloads.iter()) {
                        self.bind_pattern(sub, pty);
                    }
                }
            }
            Pattern::NamedVariant { path, fields, .. } => {
                if let Some(&variant_sym) = self.res.path_to_sym.get(&path.span) {
                    let subst = build_enum_subst_from_scrutinee(self.res, ty);
                    let decl_names = self
                        .res
                        .enum_variant_field_names
                        .get(&variant_sym)
                        .cloned()
                        .unwrap_or_default();
                    let payloads: Vec<Ty> = self
                        .res
                        .enum_variant_payloads
                        .get(&variant_sym)
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|t| apply_subst(&self.resolve_type(t), &subst))
                        .collect();
                    for (name, sub) in fields {
                        if let Some(idx) =
                            decl_names.iter().position(|n| n == &name.name)
                        {
                            if let Some(pty) = payloads.get(idx) {
                                self.bind_pattern(sub, pty);
                            }
                        }
                    }
                }
            }
            Pattern::Or { patterns, .. } => {
                for sub in patterns {
                    self.bind_pattern(sub, ty);
                }
            }
        }
    }

    fn check_named_variant_pattern(
        &mut self,
        path: &Path,
        fields: &[(Ident, Pattern)],
        span: Span,
        scrutinee_ty: &Ty,
    ) {
        let Some(&variant_sym) = self.res.path_to_sym.get(&path.span) else {
            return;
        };
        let SymbolKind::EnumVariant { enum_sym, .. } =
            self.res.symbol(variant_sym).kind.clone()
        else {
            self.error(span, "named-variant pattern path is not an enum variant");
            return;
        };
        let enum_ty = Ty::Enum(enum_sym, Vec::new());
        if !scrutinee_ty.is_error() && !enum_ty.compatible(scrutinee_ty) {
            self.error(
                span,
                format!(
                    "pattern matches `{}` but scrutinee is `{}`",
                    enum_ty.display(),
                    scrutinee_ty.display()
                ),
            );
            return;
        }
        let Some(decl_names) = self
            .res
            .enum_variant_field_names
            .get(&variant_sym)
            .cloned()
        else {
            self.error(
                span,
                format!(
                    "variant `{}` is not a struct-style variant",
                    self.res.symbol(variant_sym).name
                ),
            );
            return;
        };
        let decl_tys: Vec<Ty> = self
            .res
            .enum_variant_payloads
            .get(&variant_sym)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|t| self.resolve_type(t))
            .collect();
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (name, sub) in fields {
            if !seen.insert(&name.name) {
                self.error(
                    name.span,
                    format!("duplicate field `{}` in pattern", name.name),
                );
                continue;
            }
            let Some(idx) = decl_names.iter().position(|n| n == &name.name) else {
                self.error(
                    name.span,
                    format!(
                        "no field `{}` on variant `{}`",
                        name.name,
                        self.res.symbol(variant_sym).name
                    ),
                );
                continue;
            };
            self.check_pattern_matches(sub, &decl_tys[idx]);
        }
        for decl in &decl_names {
            if !seen.contains(decl.as_str()) {
                self.error(
                    span,
                    format!(
                        "missing field `{}` in pattern for variant `{}`",
                        decl,
                        self.res.symbol(variant_sym).name
                    ),
                );
            }
        }
    }

    fn check_tuple_variant_pattern(
        &mut self,
        path: &Path,
        fields: &[Pattern],
        span: Span,
        scrutinee_ty: &Ty,
    ) {
        let Some(&variant_sym) = self.res.path_to_sym.get(&path.span) else {
            return;
        };
        let SymbolKind::EnumVariant { enum_sym, .. } =
            self.res.symbol(variant_sym).kind.clone()
        else {
            self.error(span, "tuple-variant pattern path is not an enum variant");
            return;
        };
        let enum_ty = Ty::Enum(enum_sym, Vec::new());
        if !scrutinee_ty.is_error() && !enum_ty.compatible(scrutinee_ty) {
            self.error(
                span,
                format!(
                    "pattern matches `{}` but scrutinee is `{}`",
                    enum_ty.display(),
                    scrutinee_ty.display()
                ),
            );
            return;
        }
        let payloads: Vec<Ty> = self
            .res
            .enum_variant_payloads
            .get(&variant_sym)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|t| self.resolve_type(t))
            .collect();
        if payloads.is_empty() {
            self.error(
                span,
                format!(
                    "variant `{}` has no payload — drop the parentheses",
                    self.res.symbol(variant_sym).name
                ),
            );
            return;
        }
        if payloads.len() != fields.len() {
            self.error(
                span,
                format!(
                    "variant `{}` takes {} payload{}, found {}",
                    self.res.symbol(variant_sym).name,
                    payloads.len(),
                    if payloads.len() == 1 { "" } else { "s" },
                    fields.len()
                ),
            );
            return;
        }
        for (sub, pty) in fields.iter().zip(payloads.iter()) {
            self.check_pattern_matches(sub, pty);
        }
    }

    fn check_expr(&mut self, e: &Expr) -> Ty {
        let span = e.span();
        let ty = self.check_expr_inner(e);
        self.expr_types.insert(span, ty.clone());
        ty
    }

    fn check_expr_inner(&mut self, e: &Expr) -> Ty {
        match e {
            Expr::Lit { lit, .. } => self.lit_type(lit),
            Expr::Path(p) => self.path_value_type(p),
            Expr::Unary { op, expr, span } => self.check_unary(*op, expr, *span),
            Expr::Binary { op, lhs, rhs, span } => self.check_binary(*op, lhs, rhs, *span),
            Expr::Assign { lhs, rhs, span } => self.check_assign(lhs, rhs, *span),
            Expr::AssignOp { op, lhs, rhs, span } => self.check_assign_op(*op, lhs, rhs, *span),
            Expr::Call { callee, args, span } => self.check_call(callee, args, *span),
            Expr::MethodCall { receiver, method, args, span } => {
                self.check_method_call(receiver, method, args, *span)
            }
            Expr::Field { receiver, name, span } => {
                self.check_field_access(receiver, name, *span)
            }
            Expr::Index { receiver, index, span } => self.check_index(receiver, index, *span),
            Expr::Try { expr, span } => self.check_try(expr, *span),
            Expr::Cast { expr, ty, span } => self.check_cast(expr, ty, *span),
            Expr::Array { elems, span } => self.check_array(elems, *span),
            Expr::Block(b) => self.check_block(b),
            Expr::If { cond, then_branch, else_branch, span } => {
                self.check_if(cond, then_branch, else_branch.as_deref(), *span)
            }
            Expr::While { cond, body, .. } => {
                let ct = self.check_expr(cond);
                if !ct.compatible(&Ty::Bool) {
                    self.error(
                        cond.span(),
                        format!("while condition must be `bool`, found `{}`", ct.display()),
                    );
                }
                self.check_block(body);
                Ty::Unit
            }
            Expr::For { pat, iter, body, .. } => self.check_for(pat, iter, body),
            Expr::Match { scrutinee, arms, span } => self.check_match(scrutinee, arms, *span),
            Expr::StructLit { path, fields, span } => {
                self.check_struct_lit(path, fields, *span)
            }
            Expr::Range { start, end, span, .. } => {
                // Range expressions are currently only legal inside a slice
                // index (`s[a..b]`). Outside that context, we still want to
                // surface a useful error rather than a stray "unsupported".
                if let Some(s) = start.as_deref() { self.check_expr(s); }
                if let Some(e) = end.as_deref() { self.check_expr(e); }
                self.error(
                    *span,
                    "range expressions are only allowed as a slice index (e.g. `s[a..b]`) — \
                     `for i in 0..n` and bare ranges aren't supported yet",
                );
                Ty::Error
            }
            Expr::Return { value, span } => self.check_return(value.as_deref(), *span),
            Expr::Break(_) | Expr::Continue(_) => Ty::Never,
            Expr::Closure { params, body, span } => {
                self.check_closure(params, body, *span, None)
            }
        }
    }

    /// Type-check `e` with a contextual expected type. For a
    /// closure literal, the hint feeds bidirectional inference of
    /// unannotated params and tightens the body's return-type
    /// check. For any other expression, falls through to the
    /// bottom-up `check_expr`. Called from `check_let`,
    /// `check_struct_lit` pass 2, and `check_call` argument
    /// positions.
    fn check_expr_with_hint(&mut self, e: &Expr, expected: Option<&Ty>) -> Ty {
        if let (Expr::Closure { params, body, span }, Some(_)) = (e, expected) {
            let ty = self.check_closure(params, body, *span, expected);
            self.expr_types.insert(*span, ty.clone());
            return ty;
        }
        self.check_expr(e)
    }

    /// Type-check a closure literal `|x, y| body`. The contextual
    /// `expected` is an optional `Ty::Fn { params, ret }` from the
    /// surrounding binding/field/argument position — used for
    /// bidirectional inference of unannotated closure params and
    /// to constrain the body's expected type. With no hint and no
    /// annotations, the params type as `Ty::Error` (with a
    /// diagnostic), since Rune has no top-down inference outside
    /// the threading hooks the checker provides.
    fn check_closure(
        &mut self,
        params: &[crate::ast::ClosureParam],
        body: &Expr,
        span: Span,
        expected: Option<&Ty>,
    ) -> Ty {
        // Pull `(expected_params, expected_ret)` from a Ty::Fn hint.
        let (exp_params, exp_ret) = match expected {
            Some(Ty::Fn { params: ep, ret: er }) => {
                (Some(ep.clone()), Some((**er).clone()))
            }
            _ => (None, None),
        };
        if let Some(ref ep) = exp_params {
            if ep.len() != params.len() {
                self.error(
                    span,
                    format!(
                        "closure expects {} parameter{} but the context wants {}",
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        ep.len()
                    ),
                );
            }
        }
        // Bind each parameter's type: annotation if present,
        // otherwise the hint's corresponding param. Missing both
        // is a hard error.
        let mut param_tys: Vec<Ty> = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let pty = if let Some(t) = &p.ty {
                self.resolve_type(t)
            } else if let Some(ep) = exp_params.as_ref().and_then(|ep| ep.get(i)) {
                ep.clone()
            } else {
                self.error(
                    p.span,
                    format!(
                        "closure parameter `{}` needs a type annotation \
                         (no contextual hint at this position)",
                        p.name.name
                    ),
                );
                Ty::Error
            };
            // Record the param's type at its declaration span so
            // the body's reads of the param resolve correctly via
            // `path_value_type → local_types`.
            self.local_types.insert(p.name.span, pty.clone());
            param_tys.push(pty);
        }
        self.closure_param_tys.insert(span, param_tys.clone());
        // Check the body. If the hint provided an expected return
        // type, use it as a structural check after the fact —
        // bottom-up typing for the body itself; the hint just
        // tightens what we'll claim the closure's overall type is.
        let body_ty = self.check_expr(body);
        // The return type comes from the body. The hint's ret is
        // only used to *check* compatibility — when the hint is a
        // bare TypeVar (e.g. Map's `U`), the body type is the
        // authoritative ground truth and feeds back through
        // `unify_typevars` at the call site to pin the outer
        // generic. When the hint is concrete and disagrees, the
        // body's type wins for downstream substitution and the
        // diagnostic surfaces.
        let ret_ty = match exp_ret {
            Some(er) => {
                if !body_ty.compatible(&er) {
                    self.error(
                        body.span(),
                        format!(
                            "closure body returns `{}` but the context wants `{}`",
                            body_ty.display(),
                            er.display()
                        ),
                    );
                }
                body_ty
            }
            None => body_ty,
        };
        self.closure_ret_tys.insert(span, ret_ty.clone());
        // Two shapes, depending on captures:
        //
        // - Non-capturing: the closure lowers to an anonymous `fn`
        //   item (session 057). Type is `Ty::Fn { ... }` so the
        //   call site uses IndirectCall.
        //
        // - Capturing: the closure lowers to a synthesized struct
        //   with one field per capture + a `call` method. Type is
        //   `Ty::Struct(closure_struct_sym, [])` so the call site
        //   uses method dispatch (`impl_methods[(s, "call")]`).
        //   Also build the synth struct's `StructLayout` from the
        //   capture sym list + each capture's already-known type.
        let captures = self
            .res
            .closure_captures
            .get(&span)
            .cloned()
            .unwrap_or_default();
        if captures.is_empty() {
            Ty::Fn {
                params: param_tys,
                ret: Box::new(ret_ty),
            }
        } else {
            let Some(&closure_struct_sym) =
                self.res.closure_struct_sym.get(&span)
            else {
                // Resolver didn't mint a struct sym — fall back to
                // fn-typed result; lowerer will error.
                return Ty::Fn {
                    params: param_tys,
                    ret: Box::new(ret_ty),
                };
            };
            let mut fields: Vec<StructLayoutField> = Vec::with_capacity(captures.len());
            for (i, capture_sym) in captures.iter().enumerate() {
                let sym = self.res.symbol(*capture_sym);
                let capture_ty = self
                    .local_types
                    .get(&sym.span)
                    .cloned()
                    .unwrap_or(Ty::Error);
                fields.push(StructLayoutField {
                    name: sym.name.clone(),
                    ty: capture_ty,
                    offset: (i * 8) as u32,
                });
            }
            let size = (captures.len() * 8) as u32;
            self.struct_layouts.insert(
                closure_struct_sym,
                StructLayout { fields, size },
            );
            // Register the `call` method's fn signature: takes
            // self (the closure struct) + the closure's params,
            // returns the body's type.
            if let Some(&call_sym) = self.res.closure_call_method_sym.get(&span) {
                let mut sig_params: Vec<Ty> = Vec::with_capacity(param_tys.len() + 1);
                sig_params.push(Ty::Struct(closure_struct_sym, Vec::new()));
                sig_params.extend(param_tys.iter().cloned());
                let sig = Ty::Fn {
                    params: sig_params,
                    ret: Box::new(ret_ty.clone()),
                };
                // The resolver minted the call method with the
                // closure's span; the lowerer reads the signature
                // back by span via `decl_to_sym` → not applicable
                // here. Index by call_sym's symbol-span so the
                // lowerer can find it.
                let call_method_span = self.res.symbol(call_sym).span;
                self.fn_signatures.insert(call_method_span, sig);
            }
            Ty::Struct(closure_struct_sym, Vec::new())
        }
    }

    fn lit_type(&self, lit: &Lit) -> Ty {
        match lit {
            Lit::Int(_) => DEFAULT_INT,
            Lit::Float(_) => DEFAULT_FLOAT,
            Lit::Str(_) => Ty::Str,
            Lit::Char(_) => Ty::Char,
            Lit::Bool(_) => Ty::Bool,
        }
    }

    fn path_value_type(&mut self, p: &Path) -> Ty {
        let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
            return Ty::Error;
        };
        let kind = self.res.symbol(sym_id).kind.clone();
        let sym_span = self.res.symbol(sym_id).span;
        let name = self.res.symbol(sym_id).name.clone();
        match kind {
            SymbolKind::Local { .. } | SymbolKind::Param | SymbolKind::Const => {
                self.local_types.get(&sym_span).cloned().unwrap_or(Ty::Error)
            }
            SymbolKind::Fn => self.fn_signatures.get(&sym_span).cloned().unwrap_or(Ty::Error),
            SymbolKind::BuiltinFn(b) => Ty::Fn {
                params: b.params.clone(),
                ret: Box::new(b.ret.clone()),
            },
            SymbolKind::PolyBuiltinFn(_) => {
                self.error(
                    p.span,
                    format!(
                        "`{}` is a polymorphic builtin and cannot be used as a value; \
                         use it in a call expression",
                        name
                    ),
                );
                Ty::Error
            }
            SymbolKind::EnumVariant { enum_sym, .. } => {
                // The value of an enum variant has the enum's type.
                // Type args left empty here; if the enum is generic
                // the use-site context (`let x: Option<i64> = None`)
                // unifies against this via `compatible`.
                Ty::Enum(enum_sym, Vec::new())
            }
            SymbolKind::BuiltinType(_)
            | SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::TypeParam
            | SymbolKind::Trait
            | SymbolKind::Module => {
                self.error(p.span, format!("`{}` is not a value", name));
                Ty::Error
            }
        }
    }

    fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Ty {
        let t = self.check_expr(expr);
        if t.is_error() {
            return Ty::Error;
        }
        match op {
            UnOp::Neg => {
                if t.is_numeric() {
                    t
                } else {
                    self.error(span, format!("cannot negate `{}`", t.display()));
                    Ty::Error
                }
            }
            UnOp::Not => {
                if matches!(t, Ty::Bool) {
                    Ty::Bool
                } else {
                    self.error(span, format!("`!` requires `bool`, found `{}`", t.display()));
                    Ty::Error
                }
            }
            UnOp::BitNot => {
                if t.is_integer() {
                    t
                } else {
                    self.error(
                        span,
                        format!("`~` requires an integer, found `{}`", t.display()),
                    );
                    Ty::Error
                }
            }
        }
    }

    fn check_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        let lt = self.check_expr(lhs);
        let rt = self.check_expr(rhs);
        if lt.is_error() || rt.is_error() {
            return Ty::Error;
        }
        if !lt.compatible(&rt) {
            self.error(
                span,
                format!(
                    "operands of `{}` have mismatched types: `{}` vs `{}`",
                    binop_symbol(op),
                    lt.display(),
                    rt.display()
                ),
            );
            return Ty::Error;
        }
        let t = if lt.is_never() { rt.clone() } else { lt.clone() };
        match op {
            BinOp::Add => {
                // `+` concatenates strings as well as adding numbers.
                if matches!(t, Ty::Str) {
                    return Ty::Str;
                }
                if !t.is_numeric() {
                    self.error(
                        span,
                        format!(
                            "operator `+` requires numeric or string operands, got `{}`",
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                t
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                if !t.is_numeric() {
                    self.error(
                        span,
                        format!(
                            "operator `{}` requires numeric operands, got `{}`",
                            binop_symbol(op),
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                t
            }
            BinOp::Eq | BinOp::Ne => Ty::Bool,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                if !t.is_numeric() && !matches!(t, Ty::Char) {
                    self.error(
                        span,
                        format!(
                            "operator `{}` requires ordered operands, got `{}`",
                            binop_symbol(op),
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                Ty::Bool
            }
            BinOp::And | BinOp::Or => {
                if !matches!(t, Ty::Bool) {
                    self.error(
                        span,
                        format!(
                            "operator `{}` requires `bool` operands, got `{}`",
                            binop_symbol(op),
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                Ty::Bool
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !t.is_integer() {
                    self.error(
                        span,
                        format!(
                            "operator `{}` requires integer operands, got `{}`",
                            binop_symbol(op),
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                t
            }
        }
    }

    fn check_assign(&mut self, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        let lt = self.check_expr(lhs);
        let rt = self.check_expr(rhs);
        self.check_assign_target(lhs, span);
        if !lt.is_error() && !rt.is_error() && !lt.compatible(&rt) {
            self.error(
                span,
                format!(
                    "cannot assign `{}` to `{}`",
                    rt.display(),
                    lt.display()
                ),
            );
        }
        Ty::Unit
    }

    fn check_assign_op(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        let lt = self.check_expr(lhs);
        let rt = self.check_expr(rhs);
        self.check_assign_target(lhs, span);
        if !lt.is_error() && !rt.is_error() {
            if !lt.compatible(&rt) {
                self.error(
                    span,
                    format!(
                        "compound assignment `{}=` has mismatched operand types: `{}` vs `{}`",
                        binop_symbol(op),
                        lt.display(),
                        rt.display()
                    ),
                );
            }
            let add_on_str = matches!(op, BinOp::Add) && matches!(lt, Ty::Str);
            let needs_numeric = matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            ) && !add_on_str;
            if needs_numeric && !lt.is_numeric() {
                self.error(
                    span,
                    format!(
                        "compound assignment `{}=` requires numeric operands, got `{}`",
                        binop_symbol(op),
                        lt.display()
                    ),
                );
            }
        }
        Ty::Unit
    }

    fn check_assign_target(&mut self, lhs: &Expr, span: Span) {
        match lhs {
            Expr::Path(p) => {
                let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
                    return;
                };
                let sym = self.res.symbol(sym_id);
                let name = sym.name.clone();
                let kind = sym.kind.clone();
                match kind {
                    SymbolKind::Local { mutable: true } => {}
                    SymbolKind::Local { mutable: false } => {
                        self.error(
                            span,
                            format!("cannot assign to immutable binding `{}`", name),
                        );
                    }
                    SymbolKind::Param => {
                        self.error(
                            span,
                            format!("cannot assign to parameter `{}`", name),
                        );
                    }
                    SymbolKind::Const => {
                        self.error(span, format!("cannot assign to const `{}`", name));
                    }
                    SymbolKind::Fn
                    | SymbolKind::BuiltinFn(_)
                    | SymbolKind::PolyBuiltinFn(_) => {
                        self.error(span, format!("cannot assign to function `{}`", name));
                    }
                    SymbolKind::BuiltinType(_)
                    | SymbolKind::Struct
                    | SymbolKind::Enum
                    | SymbolKind::TypeParam
                    | SymbolKind::Trait
                    | SymbolKind::Module => {
                        self.error(span, format!("cannot assign to `{}`", name));
                    }
                    SymbolKind::EnumVariant { .. } => {
                        self.error(
                            span,
                            format!("cannot assign to enum variant `{}`", name),
                        );
                    }
                }
            }
            Expr::Field { receiver, .. } => {
                // Field assignment is allowed iff the root receiver is
                // a mutable local. Walk through nested `a.b.c` reads.
                self.check_place_root_mutable(receiver, span);
            }
            Expr::Index { .. } => {
                // Allowed; deeper check deferred.
            }
            _ => {
                self.error(span, "invalid assignment target");
            }
        }
    }

    /// Walks a place expression (e.g. `a.b.c`) down to its root binding
    /// and ensures that binding is mutable. Reports an error otherwise.
    ///
    /// Parameters are allowed as roots: every user struct is heap-
    /// allocated (an 8-byte descriptor pointer), so `param.field = ...`
    /// mutates the heap location the caller and callee share — there
    /// is no stack-by-value aliasing risk. Without this, an iterator
    /// `fn next(self: Counter)` could not advance `self.n`, which is
    /// the canonical Rune mutation pattern (sessions 014 / 020 / 049).
    fn check_place_root_mutable(&mut self, e: &Expr, span: Span) {
        match e {
            Expr::Path(p) => {
                let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
                    return;
                };
                let sym = self.res.symbol(sym_id);
                let name = sym.name.clone();
                match sym.kind {
                    SymbolKind::Local { mutable: true } => {}
                    SymbolKind::Param => {
                        // Heap-struct interior mutation; see doc above.
                    }
                    SymbolKind::Local { mutable: false } => self.error(
                        span,
                        format!("cannot assign to field of immutable binding `{}`", name),
                    ),
                    _ => self.error(span, format!("cannot assign to field of `{}`", name)),
                }
            }
            Expr::Field { receiver, .. } => {
                self.check_place_root_mutable(receiver, span);
            }
            _ => {
                self.error(span, "field assignment target must reach a mutable binding");
            }
        }
    }

    /// Whether a value of type `actual` may be supplied where
    /// `expected` is wanted — directly compatible, or a concrete
    /// struct coercing into a `dyn Trait` it implements. A coercion is
    /// recorded at `span` so the lowerer wraps that expression in a
    /// `DynBox`.
    fn check_assignable(&mut self, span: Span, actual: &Ty, expected: &Ty) -> bool {
        if actual.compatible(expected) {
            return true;
        }
        if let (Ty::Struct(c, _), Ty::Dyn(t, t_args)) = (actual, expected) {
            if self.struct_impls_trait(*c, *t) {
                self.dyn_coercions.insert(span, (*c, *t, t_args.clone()));
                return true;
            }
        }
        false
    }

    /// True if struct `c` provides an impl method for every method
    /// the trait `t` declares.
    fn struct_impls_trait(&self, c: SymbolId, t: SymbolId) -> bool {
        let Some(methods) = self.res.trait_methods.get(&t) else {
            return false;
        };
        methods.iter().all(|m| {
            self.res
                .impl_methods
                .contains_key(&(c, m.name.name.clone()))
        })
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Ty {
        // Intercept calls to polymorphic builtins before checking the callee
        // as a value expression — they have no single signature to bind.
        if let Expr::Path(p) = callee {
            if let Some(&sym_id) = self.res.path_to_sym.get(&p.span) {
                if let SymbolKind::PolyBuiltinFn(name) = self.res.symbol(sym_id).kind.clone() {
                    return self.check_poly_builtin_call(name, args, span);
                }
                // Enum variant constructor: `Result::Ok(5)`.
                if let SymbolKind::EnumVariant { enum_sym, .. } =
                    self.res.symbol(sym_id).kind.clone()
                {
                    return self.check_enum_variant_call(sym_id, enum_sym, args, span);
                }
            }
        }
        let callee_ty = self.check_expr(callee);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.check_expr(a)).collect();
        match callee_ty {
            Ty::Fn { params, ret } => {
                if params.len() != arg_tys.len() {
                    self.error(
                        span,
                        format!(
                            "expected {} argument{}, found {}",
                            params.len(),
                            if params.len() == 1 { "" } else { "s" },
                            arg_tys.len()
                        ),
                    );
                    return *ret;
                }
                // Build a substitution from any TypeVar params to the
                // concrete arg types so the call's apparent return type
                // is the substituted one. This lets the lowerer see
                // concrete types at the call site; the monomorphizer
                // does the same inference to find the specialization.
                let mut subst: std::collections::HashMap<SymbolId, Ty> =
                    std::collections::HashMap::new();
                for (i, (param_ty, arg_ty)) in params.iter().zip(&arg_tys).enumerate() {
                    unify_typevars(param_ty, arg_ty, &mut subst);
                    if !self.check_assignable(args[i].span(), arg_ty, param_ty) {
                        self.error(
                            args[i].span(),
                            format!(
                                "argument {} has type `{}`, expected `{}`",
                                i + 1,
                                arg_ty.display(),
                                param_ty.display()
                            ),
                        );
                    }
                }
                self.apply_subst(&ret, &subst, None)
            }
            Ty::Error => Ty::Error,
            // Calling a closure value: the synth `call` method on
            // the closure's struct does the dispatch. Look it up
            // via `impl_methods[(closure_sym, "call")]` and use
            // its stored signature minus the leading `self` param.
            Ty::Struct(s, _)
                if self.res.impl_methods.get(&(s, "call".to_string())).is_some() =>
            {
                let call_sym = self.res.impl_methods[&(s, "call".to_string())];
                let call_span = self.res.symbol(call_sym).span;
                let sig = self.fn_signatures.get(&call_span).cloned();
                let Some(Ty::Fn { params: call_params, ret: call_ret }) = sig
                else {
                    self.error(span, "internal: closure has no call signature");
                    return Ty::Error;
                };
                // Skip the leading `self` param.
                let expected_params: Vec<Ty> =
                    call_params.into_iter().skip(1).collect();
                if expected_params.len() != arg_tys.len() {
                    self.error(
                        span,
                        format!(
                            "closure expects {} argument{} but got {}",
                            expected_params.len(),
                            if expected_params.len() == 1 { "" } else { "s" },
                            arg_tys.len()
                        ),
                    );
                    return *call_ret;
                }
                for (i, (a, p)) in arg_tys.iter().zip(expected_params.iter()).enumerate() {
                    if !self.check_assignable(args[i].span(), a, p) {
                        self.error(
                            args[i].span(),
                            format!(
                                "argument {} has type `{}`, expected `{}`",
                                i + 1,
                                a.display(),
                                p.display()
                            ),
                        );
                    }
                }
                *call_ret
            }
            other => {
                self.error(span, format!("cannot call value of type `{}`", other.display()));
                Ty::Error
            }
        }
    }

    fn check_enum_variant_call(
        &mut self,
        variant_sym: SymbolId,
        enum_sym: SymbolId,
        args: &[Expr],
        span: Span,
    ) -> Ty {
        let payload_tys: Vec<Ty> = self
            .res
            .enum_variant_payloads
            .get(&variant_sym)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|t| self.resolve_type(t))
            .collect();
        if payload_tys.is_empty() {
            self.error(
                span,
                format!(
                    "variant `{}` takes no payload — drop the parentheses",
                    self.res.symbol(variant_sym).name
                ),
            );
            // Still walk the args so user errors inside them surface.
            for a in args {
                self.check_expr(a);
            }
            return Ty::Enum(enum_sym, Vec::new());
        }
        if payload_tys.len() != args.len() {
            self.error(
                span,
                format!(
                    "variant `{}` takes {} payload{}, found {}",
                    self.res.symbol(variant_sym).name,
                    payload_tys.len(),
                    if payload_tys.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            );
        }
        // Walk payload positions, unifying declared types vs actual
        // arg types so the enum's generic args can be inferred from
        // the constructor (e.g., `Some(5)` → Option<i64>).
        let mut subst: std::collections::HashMap<SymbolId, Ty> =
            std::collections::HashMap::new();
        for (i, (param_ty, arg)) in payload_tys.iter().zip(args).enumerate() {
            let arg_ty = self.check_expr(arg);
            unify_typevars(param_ty, &arg_ty, &mut subst);
            if !self.check_assignable(arg.span(), &arg_ty, param_ty) {
                self.error(
                    arg.span(),
                    format!(
                        "variant payload {} has type `{}`, expected `{}`",
                        i + 1,
                        arg_ty.display(),
                        param_ty.display()
                    ),
                );
            }
        }
        let enum_args = enum_generic_args(self.res, enum_sym, &subst);
        Ty::Enum(enum_sym, enum_args)
    }

    fn check_named_variant_lit(
        &mut self,
        variant_sym: SymbolId,
        enum_sym: SymbolId,
        path: &Path,
        fields: &[FieldInit],
        span: Span,
    ) -> Ty {
        let Some(decl_names) = self
            .res
            .enum_variant_field_names
            .get(&variant_sym)
            .cloned()
        else {
            self.error(
                span,
                format!(
                    "variant `{}` is not a struct-style variant — use `{}(...)` instead",
                    self.res.symbol(variant_sym).name,
                    self.res.symbol(variant_sym).name,
                ),
            );
            return Ty::Enum(enum_sym, Vec::new());
        };
        let decl_tys: Vec<Ty> = self
            .res
            .enum_variant_payloads
            .get(&variant_sym)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|t| self.resolve_type(t))
            .collect();
        if fields.len() != decl_names.len() {
            self.error(
                span,
                format!(
                    "variant `{}` has {} field{}, found {}",
                    self.res.symbol(variant_sym).name,
                    decl_names.len(),
                    if decl_names.len() == 1 { "" } else { "s" },
                    fields.len()
                ),
            );
        }
        // Each value's type must match the declared field's type by
        // matching on name. Unknown names → error. Missing/duplicate
        // names → error.
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for fi in fields {
            let name = &fi.name.name;
            if !seen.insert(name) {
                self.error(
                    fi.name.span,
                    format!("duplicate field `{}` in variant initializer", name),
                );
                self.check_expr(&fi.value);
                continue;
            }
            let Some(idx) = decl_names.iter().position(|n| n == name) else {
                self.error(
                    fi.name.span,
                    format!(
                        "no field `{}` on variant `{}`",
                        name,
                        self.res.symbol(variant_sym).name
                    ),
                );
                self.check_expr(&fi.value);
                continue;
            };
            let expected = &decl_tys[idx];
            let actual = self.check_expr(&fi.value);
            if !actual.compatible(expected) {
                self.error(
                    fi.value.span(),
                    format!(
                        "field `{}` has type `{}`, expected `{}`",
                        name,
                        actual.display(),
                        expected.display()
                    ),
                );
            }
        }
        // Missing fields.
        for decl in &decl_names {
            if !seen.contains(decl.as_str()) {
                self.error(
                    path.span,
                    format!(
                        "missing field `{}` for variant `{}`",
                        decl,
                        self.res.symbol(variant_sym).name
                    ),
                );
            }
        }
        Ty::Enum(enum_sym, Vec::new())
    }

    /// Look up a method declared in an `impl` block on a struct type.
    /// Returns the method's externally-visible signature (without the
    /// `self` parameter).
    fn user_method_sig(&self, recv: &Ty, name: &str) -> Option<MethodSig> {
        let Ty::Struct(sym_id, _) = recv else { return None };
        let &method_sym = self.res.impl_methods.get(&(*sym_id, name.to_string()))?;
        let method_span = self.res.symbol(method_sym).span;
        let fn_ty = self.fn_signatures.get(&method_span)?;
        let Ty::Fn { params, ret } = fn_ty else { return None };
        // Drop the `self` parameter from the externally-visible sig.
        let (self_ty, rest) = params.split_first()?;
        // For a generic-impl method (`impl<T> Box<T>`), bind the
        // impl's type params from the concrete receiver: unify the
        // declared `self` type (`Box<T>`) against the actual receiver
        // (`Box<i64>`), then substitute through the rest of the sig.
        // For a non-generic method this leaves the signature as-is.
        let mut subst: std::collections::HashMap<SymbolId, Ty> =
            std::collections::HashMap::new();
        unify_typevars(self_ty, recv, &mut subst);
        Some(MethodSig {
            params: rest.iter().map(|p| self.apply_subst(p, &subst, None)).collect(),
            ret: self.apply_subst(ret, &subst, None),
        })
    }

    fn check_struct_lit(&mut self, path: &Path, fields: &[FieldInit], span: Span) -> Ty {
        let Some(&sym_id) = self.res.path_to_sym.get(&path.span) else {
            return Ty::Error;
        };
        let sym_kind = self.res.symbol(sym_id).kind.clone();
        let sym_name = self.res.symbol(sym_id).name.clone();
        // Dispatch named-field enum variant construction here too:
        // `Variant { name: val, ... }` parses as Expr::StructLit and
        // resolves to a variant symbol. Reuse the variant-call path
        // for type checking.
        if let SymbolKind::EnumVariant { enum_sym, .. } = sym_kind {
            return self.check_named_variant_lit(sym_id, enum_sym, path, fields, span);
        }
        if !matches!(sym_kind, SymbolKind::Struct) {
            self.error(
                path.span,
                format!("`{}` is not a struct", sym_name),
            );
            for f in fields {
                self.check_expr(&f.value);
            }
            return Ty::Error;
        }
        let Some(layout) = self.struct_layouts.get(&sym_id).cloned() else {
            self.error(path.span, format!("no layout for struct `{}`", sym_name));
            return Ty::Error;
        };

        // Track which fields have been provided so we can flag missing/duplicates.
        // Two passes: (1) typecheck each non-closure value AND infer
        // the struct's generic-arg substitution from the field
        // types, (2) substitute the declared field types, then
        // typecheck closure values with the substituted-field-type
        // hint and assignability-check every field.
        //
        // Closures are deferred to pass 2 because their parameter
        // types depend on contextual inference: `Map { iter:
        // v.iter(), f: |x| x * 2 }` needs `iter`'s value type to
        // pin `I = VecIter<i64>` so `f: fn(I::Item) -> U`
        // substitutes to `fn(i64) -> U` — the hint a closure needs
        // to bind `x: i64`. Pass-1-with-no-hint would error on
        // every unannotated closure.
        let mut provided = std::collections::HashSet::new();
        let mut subst: std::collections::HashMap<SymbolId, Ty> =
            std::collections::HashMap::new();
        let mut value_tys: Vec<(usize, Option<Ty>)> = Vec::with_capacity(fields.len());
        for (idx, init) in fields.iter().enumerate() {
            if matches!(init.value, Expr::Closure { .. }) {
                // Pass-1 deferral — pass 2 re-checks with the hint.
                value_tys.push((idx, None));
                if let Some(decl_field) = layout.field(&init.name.name) {
                    // We still know the declared field type at
                    // pass 1 (it may carry TypeVars). No subst
                    // contribution from a deferred closure.
                    let _ = decl_field;
                }
                if !provided.insert(init.name.name.clone()) {
                    self.error(
                        init.name.span,
                        format!("field `{}` set more than once", init.name.name),
                    );
                }
                continue;
            }
            let value_ty = self.check_expr(&init.value);
            let Some(decl_field) = layout.field(&init.name.name) else {
                self.error(
                    init.name.span,
                    format!("`{}` has no field `{}`", sym_name, init.name.name),
                );
                value_tys.push((idx, Some(value_ty)));
                continue;
            };
            unify_typevars(&decl_field.ty, &value_ty, &mut subst);
            value_tys.push((idx, Some(value_ty)));
            if !provided.insert(init.name.name.clone()) {
                self.error(
                    init.name.span,
                    format!("field `{}` set more than once", init.name.name),
                );
            }
        }
        // Pass 2: substitute the gathered subst into each declared
        // field type, then check assignability. Closure values
        // (deferred in pass 1) get type-checked here with the
        // substituted field type as the bidirectional hint so
        // unannotated closure params bind from the declared
        // `fn(...) -> ...` shape.
        for (idx, value_ty_in) in &value_tys {
            let init = &fields[*idx];
            let Some(decl_field) = layout.field(&init.name.name) else {
                continue;
            };
            let expected = self.apply_subst(&decl_field.ty, &subst, None);
            let value_ty = match value_ty_in {
                Some(t) => t.clone(),
                None => {
                    // Deferred closure — typecheck now with the
                    // substituted field type as the hint. Then
                    // contribute its inferred type back to `subst`
                    // so any generic params it pinned (e.g. `U` in
                    // Map's `f: fn(I::Item) -> U`) propagate to
                    // the struct's resulting type arg list.
                    let ty = self.check_expr_with_hint(&init.value, Some(&expected));
                    unify_typevars(&decl_field.ty, &ty, &mut subst);
                    ty
                }
            };
            if !self.check_assignable(init.value.span(), &value_ty, &expected) {
                self.error(
                    init.value.span(),
                    format!(
                        "field `{}` declared `{}` but value has type `{}`",
                        init.name.name,
                        expected.display(),
                        value_ty.display()
                    ),
                );
            }
        }
        for decl_field in &layout.fields {
            if !provided.contains(&decl_field.name) {
                self.error(
                    span,
                    format!("missing field `{}` for `{}`", decl_field.name, sym_name),
                );
            }
        }
        // Build the instantiated args list in the struct's generic-
        // param declaration order. Params we couldn't infer stay as
        // their original TypeVar — codegen / monomorphizer treats
        // those as i64 sized later.
        let args: Vec<Ty> = self
            .res
            .struct_generics
            .get(&sym_id)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|g| subst.get(g).cloned().unwrap_or_else(|| Ty::TypeVar(*g)))
            .collect();
        Ty::Struct(sym_id, args)
    }

    fn check_field_access(&mut self, receiver: &Expr, name: &Ident, span: Span) -> Ty {
        let recv_ty = self.check_expr(receiver);
        let Ty::Struct(sym_id, recv_args) = recv_ty else {
            if !recv_ty.is_error() {
                self.error(
                    span,
                    format!(
                        "cannot access field `{}` on type `{}`",
                        name.name,
                        recv_ty.display()
                    ),
                );
            }
            return Ty::Error;
        };
        let Some(layout) = self.struct_layouts.get(&sym_id) else {
            return Ty::Error;
        };
        let Some(field) = layout.field(&name.name) else {
            let struct_name = self.res.symbol(sym_id).name.clone();
            self.error(
                name.span,
                format!("`{}` has no field `{}`", struct_name, name.name),
            );
            return Ty::Error;
        };
        // Substitute TypeVar in the field type using the receiver's
        // generic args so `b.value` on `Box<i64>` returns i64 instead
        // of TypeVar(T).
        let subst = build_struct_subst(self.res, sym_id, &recv_args);
        self.apply_subst(&field.ty, &subst, None)
    }

    /// Resolve a method call where the receiver is a bounded generic
    /// parameter (`x: T` with `T: SomeTrait`). The method must be
    /// declared by one of `T`'s trait bounds. The returned signature
    /// drops the explicit `self` parameter to match `MethodSig`'s
    /// "externally visible" convention.
    fn trait_bound_method_sig(&self, recv: &Ty, name: &str) -> Option<MethodSig> {
        let Ty::TypeVar(tvar) = recv else { return None };
        let bounds = self.res.generic_bounds.get(tvar)?;
        // Walk the bound traits and their supertrait chains
        // transitively — a `<T: Sub>` value can call `Super`'s
        // methods. `visited` guards against supertrait cycles.
        let mut worklist: Vec<SymbolId> = bounds.clone();
        let mut visited: std::collections::HashSet<SymbolId> =
            std::collections::HashSet::new();
        while let Some(trait_sym) = worklist.pop() {
            if !visited.insert(trait_sym) {
                continue;
            }
            if let Some(methods) = self.res.trait_methods.get(&trait_sym) {
                if let Some(m) = methods.iter().find(|m| m.name.name == name) {
                    // Skip the leading `self` param; resolve the rest.
                    // The recorded types may carry `Ty::SelfType` from
                    // the trait-side `Self::Item` resolution; substitute
                    // it to the bound type `recv` (a `Ty::TypeVar(T)`),
                    // yielding `Ty::Assoc(TypeVar(T), name)` which
                    // monomorphization resolves once `T` is concrete.
                    let empty: std::collections::HashMap<SymbolId, Ty> =
                        std::collections::HashMap::new();
                    let mut params: Vec<Ty> = Vec::new();
                    for p in m.params.iter().skip(1) {
                        let raw = self
                            .type_resolutions
                            .get(&p.ty.span())
                            .cloned()
                            .unwrap_or(Ty::Error);
                        params.push(self.apply_subst(&raw, &empty, Some(recv)));
                    }
                    let raw_ret = m
                        .return_type
                        .as_ref()
                        .and_then(|t| self.type_resolutions.get(&t.span()).cloned())
                        .unwrap_or(Ty::Unit);
                    let ret = self.apply_subst(&raw_ret, &empty, Some(recv));
                    return Some(MethodSig { params, ret });
                }
            }
            if let Some(supers) = self.res.trait_supertraits.get(&trait_sym) {
                worklist.extend(supers);
            }
        }
        None
    }

    /// Method signature for a call on a `dyn Trait` receiver — looked
    /// up across the trait and its transitive supertraits (mirrors
    /// `trait_bound_method_sig`'s walk for static dispatch). The
    /// leading `self` param is dropped.
    /// An associated-type projection on a `dyn` receiver
    /// (`(it: dyn Iterator).next() -> Self::Item` substituting to
    /// `Ty::Assoc(Dyn, "Item")`) cannot be resolved without an
    /// upcast or a flattened-Item vtable — collapse to `Ty::Error`
    /// so it doesn't reach the monomorphizer. The collapse is
    /// recorded so the caller can produce a precise diagnostic at
    /// the call site.
    fn dyn_method_sig(
        &self,
        recv: &Ty,
        name: &str,
    ) -> Option<(MethodSig, bool)> {
        let Ty::Dyn(trait_sym, trait_args) = recv else { return None };
        let mut worklist: Vec<SymbolId> = vec![*trait_sym];
        let mut visited: std::collections::HashSet<SymbolId> =
            std::collections::HashSet::new();
        while let Some(t) = worklist.pop() {
            if !visited.insert(t) {
                continue;
            }
            if let Some(methods) = self.res.trait_methods.get(&t) {
                if let Some(m) = methods.iter().find(|m| m.name.name == name) {
                    // Build the trait-generic-arg substitution from
                    // the use-site's `Ty::Dyn(t, trait_args)`. The
                    // declared method types reference the trait's
                    // generic params (e.g. `fn make(...) -> T`);
                    // substitute them through to the use-site types
                    // (`fn make(...) -> i64` for `dyn Producer<i64>`).
                    let mut trait_subst: std::collections::HashMap<SymbolId, Ty> =
                        std::collections::HashMap::new();
                    if let Some(params_decl) = self.res.trait_generics.get(&t) {
                        for (g, a) in params_decl.iter().zip(trait_args.iter()) {
                            trait_subst.insert(*g, a.clone());
                        }
                    }
                    let mut params: Vec<Ty> = Vec::new();
                    let mut had_assoc_collapse = false;
                    for p in m.params.iter().skip(1) {
                        let raw = self
                            .type_resolutions
                            .get(&p.ty.span())
                            .cloned()
                            .unwrap_or(Ty::Error);
                        let subst_ty =
                            self.apply_subst(&raw, &trait_subst, Some(recv));
                        had_assoc_collapse |= is_dyn_assoc(&subst_ty);
                        params.push(collapse_dyn_assoc(subst_ty));
                    }
                    let raw_ret = m
                        .return_type
                        .as_ref()
                        .and_then(|t| self.type_resolutions.get(&t.span()).cloned())
                        .unwrap_or(Ty::Unit);
                    let subst_ret = self.apply_subst(&raw_ret, &trait_subst, Some(recv));
                    had_assoc_collapse |= is_dyn_assoc(&subst_ret);
                    let ret = collapse_dyn_assoc(subst_ret);
                    return Some((MethodSig { params, ret }, had_assoc_collapse));
                }
            }
            if let Some(supers) = self.res.trait_supertraits.get(&t) {
                worklist.extend(supers);
            }
        }
        None
    }

    fn check_method_call(
        &mut self,
        receiver: &Expr,
        method: &Ident,
        args: &[Expr],
        span: Span,
    ) -> Ty {
        let recv_ty = self.check_expr(receiver);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.check_expr(a)).collect();
        let sig = resolve_method(&recv_ty, &method.name)
            .or_else(|| self.builtin_vec_iter_sig(&recv_ty, &method.name))
            .or_else(|| self.user_method_sig(&recv_ty, &method.name))
            .or_else(|| self.trait_bound_method_sig(&recv_ty, &method.name))
            .or_else(|| self.dyn_method_sig(&recv_ty, &method.name).map(|(s, _)| s));
        let Some(sig) = sig else {
            if !recv_ty.is_error() {
                self.error(
                    span,
                    format!(
                        "no method `.{}` on type `{}`",
                        method.name,
                        recv_ty.display()
                    ),
                );
            }
            return Ty::Error;
        };
        // The `dyn` lookup collapses `Self::Item` to `Ty::Error` to
        // keep the IR well-formed; surface that as a real diagnostic
        // at the call site so the user sees *why* downstream code
        // looks broken. (Re-resolving is cheap; calling it twice
        // keeps the `or_else` chain unchanged.)
        if let Some((_, had_collapse)) = self.dyn_method_sig(&recv_ty, &method.name) {
            if had_collapse {
                self.error(
                    span,
                    format!(
                        "method `.{}` returns an associated type that cannot \
                         be projected through `{}`; call it on a concrete \
                         receiver instead",
                        method.name,
                        recv_ty.display()
                    ),
                );
            }
        }
        if sig.params.len() != arg_tys.len() {
            self.error(
                span,
                format!(
                    "method `.{}` expects {} argument{}, found {}",
                    method.name,
                    sig.params.len(),
                    if sig.params.len() == 1 { "" } else { "s" },
                    arg_tys.len()
                ),
            );
            return sig.ret;
        }
        for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
            // `check_assignable`, not bare `compatible`: a concrete
            // struct argument coerces to a `dyn Trait` parameter (e.g.
            // `v.push(Circle { .. })` on a `Vec<dyn Shape>`), and the
            // coercion is recorded for the lowerer.
            if !self.check_assignable(args[i].span(), a, p) {
                self.error(
                    args[i].span(),
                    format!(
                        "argument {} has type `{}`, expected `{}`",
                        i + 1,
                        a.display(),
                        p.display()
                    ),
                );
            }
        }
        sig.ret
    }

    fn check_poly_builtin_call(
        &mut self,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.check_expr(a)).collect();
        match name {
            "print" => {
                if arg_tys.len() != 1 {
                    self.error(
                        span,
                        format!(
                            "`print` expects 1 argument, found {}",
                            arg_tys.len()
                        ),
                    );
                    return Ty::Unit;
                }
                let t = &arg_tys[0];
                if !t.is_error() && !is_printable(t) {
                    self.error(
                        args[0].span(),
                        format!(
                            "`print` does not yet support values of type `{}` \
                             (currently: i64-family integers and str)",
                            t.display()
                        ),
                    );
                }
                Ty::Unit
            }
            "weak" => {
                // weak(v: T) -> Weak<T>. v0.x: only T = Vec works.
                if arg_tys.len() != 1 {
                    self.error(
                        span,
                        format!("`weak` expects 1 argument, found {}", arg_tys.len()),
                    );
                    return Ty::Error;
                }
                let t = arg_tys.into_iter().next().unwrap();
                if !matches!(t, Ty::Vec(_) | Ty::Error) {
                    self.error(
                        args[0].span(),
                        format!(
                            "`weak` only supports `Vec` in v0.x — got `{}`",
                            t.display()
                        ),
                    );
                    return Ty::Error;
                }
                Ty::Weak(Box::new(t))
            }
            "upgrade_or" => {
                // upgrade_or(w: Weak<T>, default: T) -> T. v0.x: T = Vec.
                if arg_tys.len() != 2 {
                    self.error(
                        span,
                        format!(
                            "`upgrade_or` expects 2 arguments, found {}",
                            arg_tys.len()
                        ),
                    );
                    return Ty::Error;
                }
                let weak_ty = &arg_tys[0];
                let default_ty = &arg_tys[1];
                let inner = match weak_ty {
                    Ty::Weak(t) => (**t).clone(),
                    Ty::Error => return Ty::Error,
                    _ => {
                        self.error(
                            args[0].span(),
                            format!(
                                "`upgrade_or` first arg must be `Weak<T>`, got `{}`",
                                weak_ty.display()
                            ),
                        );
                        return Ty::Error;
                    }
                };
                if !default_ty.compatible(&inner) {
                    self.error(
                        args[1].span(),
                        format!(
                            "`upgrade_or` default has type `{}`, expected `{}`",
                            default_ty.display(),
                            inner.display()
                        ),
                    );
                }
                inner
            }
            _ => {
                self.error(span, format!("unknown polymorphic builtin `{}`", name));
                Ty::Error
            }
        }
    }

    fn check_index(&mut self, receiver: &Expr, index: &Expr, span: Span) -> Ty {
        let rt = self.check_expr(receiver);
        // Range index: only `str[a..b]` for now — slicing.
        if let Expr::Range { start, end, .. } = index {
            if let Some(s) = start.as_deref() {
                let st = self.check_expr(s);
                if !st.is_integer() && !st.is_error() {
                    self.error(
                        s.span(),
                        format!("slice start must be integer, found `{}`", st.display()),
                    );
                }
            }
            if let Some(e) = end.as_deref() {
                let et = self.check_expr(e);
                if !et.is_integer() && !et.is_error() {
                    self.error(
                        e.span(),
                        format!("slice end must be integer, found `{}`", et.display()),
                    );
                }
            }
            return match rt {
                Ty::Str => Ty::Str,
                Ty::Error => Ty::Error,
                other => {
                    self.error(
                        span,
                        format!("cannot slice value of type `{}`", other.display()),
                    );
                    Ty::Error
                }
            };
        }
        // Non-range index.
        let it = self.check_expr(index);
        if !it.is_integer() && !it.is_error() {
            self.error(
                index.span(),
                format!("index must be an integer, found `{}`", it.display()),
            );
        }
        match rt {
            Ty::Array(elem, _) => *elem,
            // `str[i]` reads one byte and widens to i64.
            Ty::Str => Ty::Int(crate::ty::IntTy::I64),
            Ty::Error => Ty::Error,
            other => {
                self.error(span, format!("cannot index value of type `{}`", other.display()));
                Ty::Error
            }
        }
    }

    fn check_cast(&mut self, expr: &Expr, ty: &Type, span: Span) -> Ty {
        let from = self.check_expr(expr);
        let to = self.resolve_type(ty);
        if from.is_error() || to.is_error() {
            return to;
        }
        let allowed = (from.is_numeric() && to.is_numeric())
            || (matches!(from, Ty::Bool) && to.is_integer())
            || (matches!(from, Ty::Char) && to.is_integer())
            || (from.is_integer() && matches!(to, Ty::Char));
        if !allowed {
            self.error(
                span,
                format!("cannot cast `{}` to `{}`", from.display(), to.display()),
            );
        }
        to
    }

    fn check_array(&mut self, elems: &[Expr], span: Span) -> Ty {
        if elems.is_empty() {
            self.error(span, "cannot infer element type of empty array literal");
            return Ty::Array(Box::new(Ty::Error), 0);
        }
        let mut elem_ty = self.check_expr(&elems[0]);
        for e in &elems[1..] {
            let t = self.check_expr(e);
            if let Some(u) = t.unify(&elem_ty) {
                elem_ty = u;
            } else {
                self.error(
                    e.span(),
                    format!(
                        "array element has type `{}` but earlier elements have type `{}`",
                        t.display(),
                        elem_ty.display()
                    ),
                );
                elem_ty = Ty::Error;
            }
        }
        Ty::Array(Box::new(elem_ty), elems.len())
    }

    fn check_if(
        &mut self,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Expr>,
        span: Span,
    ) -> Ty {
        let ct = self.check_expr(cond);
        if !ct.compatible(&Ty::Bool) {
            self.error(
                cond.span(),
                format!("if condition must be `bool`, found `{}`", ct.display()),
            );
        }
        let tt = self.check_block(then_branch);
        match else_branch {
            Some(e) => {
                let et = self.check_expr(e);
                match tt.unify(&et) {
                    Some(t) => t,
                    None => {
                        self.error(
                            span,
                            format!(
                                "`if` branches have different types: `{}` vs `{}`",
                                tt.display(),
                                et.display()
                            ),
                        );
                        Ty::Error
                    }
                }
            }
            None => {
                if !tt.compatible(&Ty::Unit) {
                    self.error(
                        then_branch.span,
                        format!(
                            "`if` without `else` must yield `()`, found `{}`",
                            tt.display()
                        ),
                    );
                }
                Ty::Unit
            }
        }
    }

    fn check_for(&mut self, pat: &Pattern, iter: &Expr, body: &Block) -> Ty {
        // Range iter is a special-cased pseudo-iterator over integers.
        if let Expr::Range { start, end, .. } = iter {
            for endpoint in [start.as_deref(), end.as_deref()].into_iter().flatten() {
                let t = self.check_expr(endpoint);
                if !t.is_integer() && !t.is_error() {
                    self.error(
                        endpoint.span(),
                        format!(
                            "range endpoints must be integers, found `{}`",
                            t.display()
                        ),
                    );
                }
            }
            self.bind_pattern(pat, &Ty::Int(crate::ty::IntTy::I64));
            self.check_block(body);
            return Ty::Unit;
        }
        let it = self.check_expr(iter);
        let elem_ty = match it {
            Ty::Array(elem, _) => *elem,
            // A struct that implements the prelude's `std::Iterator`
            // is iterable — the lowerer desugars `for x in iter` to
            // a `while-true + match iter.next()` loop. The item type
            // is the impl's `type Item = ...` binding, which the
            // checker recorded in `impl_assoc_bindings_ty` during
            // pass 1 *as the impl-block's own `Ty::TypeVar(T)`* —
            // so the call-site type args (`Ty::Struct(s, recv_args)`)
            // must be substituted in before we hand the pattern
            // variable its type. Without this step, `for x in
            // v.iter()` where v is `Vec<i64>` would type `x` as
            // `Ty::TypeVar(T_VecIter)` and that TypeVar would leak
            // through every body type-check downstream.
            Ty::Struct(s, recv_args) if self.struct_implements_iterator(s) => {
                let raw = self
                    .impl_assoc_bindings_ty
                    .get(&(s, "Item".to_string()))
                    .cloned()
                    .unwrap_or(Ty::Error);
                let subst = build_struct_subst(self.res, s, &recv_args);
                self.apply_subst(&raw, &subst, None)
            }
            // A generic type parameter bounded by `Iterator` is also
            // iterable — the desugar runs in the bounded-generic
            // body, then monomorphization substitutes `T` with a
            // concrete struct that does have the impl, at which
            // point the projection resolves. The item type stays
            // abstract here as `Ty::Assoc(TypeVar(T), "Item")` and
            // gets concretized at substitution time (session 051).
            Ty::TypeVar(t) if self.type_param_has_iterator_bound(t) => {
                Ty::Assoc(Box::new(Ty::TypeVar(t)), "Item".into())
            }
            Ty::Error => Ty::Error,
            other => {
                self.error(
                    iter.span(),
                    format!(
                        "cannot iterate over `{}` — type does not implement `std::Iterator`",
                        other.display()
                    ),
                );
                Ty::Error
            }
        };
        self.bind_pattern(pat, &elem_ty);
        self.check_block(body);
        Ty::Unit
    }

    /// True iff `struct_sym` has an `impl Iterator for ...` block
    /// in `Resolutions::impls_for`, where "Iterator" is the prelude
    /// trait `std::Iterator`. A user-defined trait happening to be
    /// named `Iterator` in some other module is *not* matched here
    /// — the prelude's sym is unique by parse order (the prelude is
    /// prepended to every program so its symbols are interned first;
    /// `find_iterator_sym` returns the first match it finds).
    fn struct_implements_iterator(&self, struct_sym: SymbolId) -> bool {
        let Some(iter_sym) = self.find_iterator_sym() else { return false; };
        self.res
            .impls_for
            .get(&struct_sym)
            .map(|s| s.contains(&iter_sym))
            .unwrap_or(false)
    }

    /// True iff the generic type-parameter `tvar` has the prelude's
    /// `std::Iterator` trait in its bounds — the bounded-generic
    /// counterpart of `struct_implements_iterator`.
    fn type_param_has_iterator_bound(&self, tvar: SymbolId) -> bool {
        let Some(iter_sym) = self.find_iterator_sym() else { return false; };
        let Some(bounds) = self.res.generic_bounds.get(&tvar) else { return false; };
        // Walk bounds + their supertrait closures so `<T: Sub>` where
        // `Sub: Iterator` also counts. Mirrors the
        // `trait_bound_method_sig` walk for static dispatch.
        let mut worklist: Vec<SymbolId> = bounds.clone();
        let mut visited: std::collections::HashSet<SymbolId> =
            std::collections::HashSet::new();
        while let Some(t) = worklist.pop() {
            if !visited.insert(t) {
                continue;
            }
            if t == iter_sym {
                return true;
            }
            if let Some(supers) = self.res.trait_supertraits.get(&t) {
                worklist.extend(supers);
            }
        }
        false
    }

    /// Find the prelude's `std::Iterator` trait sym. Walks the
    /// resolver's symbol table once per call — cheap; cargo's
    /// test pool measures this as <1 us per for-loop site.
    fn find_iterator_sym(&self) -> Option<SymbolId> {
        for (idx, sym) in self.res.symbols.iter().enumerate() {
            if sym.name == "Iterator"
                && matches!(sym.kind, crate::resolver::SymbolKind::Trait)
            {
                return Some(SymbolId(idx as u32));
            }
        }
        None
    }

    /// Find the prelude's struct of a given name. Same heuristic as
    /// `find_iterator_sym` — first `Struct` whose name matches; the
    /// prelude is parsed first so its symbols are interned first.
    fn find_struct_sym(&self, name: &str) -> Option<SymbolId> {
        for (idx, sym) in self.res.symbols.iter().enumerate() {
            if sym.name == name
                && matches!(sym.kind, crate::resolver::SymbolKind::Struct)
            {
                return Some(SymbolId(idx as u32));
            }
        }
        None
    }

    /// `vec.iter()` builtin method. Constructs a `std::VecIter<elem>`
    /// from a `Vec<elem>`. The actual heap construction is emitted
    /// by the lowerer (`MethodCall` on a `Ty::Vec` with name "iter"
    /// is intercepted there).
    fn builtin_vec_iter_sig(&self, recv: &Ty, name: &str) -> Option<MethodSig> {
        if name != "iter" {
            return None;
        }
        let Ty::Vec(elem) = recv else { return None };
        let vec_iter_sym = self.find_struct_sym("VecIter")?;
        Some(MethodSig {
            params: vec![],
            ret: Ty::Struct(vec_iter_sym, vec![(**elem).clone()]),
        })
    }

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Ty {
        let st = self.check_expr(scrutinee);
        let mut result_ty: Option<Ty> = None;
        for arm in arms {
            self.check_pattern_matches(&arm.pat, &st);
            self.bind_pattern(&arm.pat, &st);
            if let Some(g) = &arm.guard {
                let gt = self.check_expr(g);
                if !gt.compatible(&Ty::Bool) {
                    self.error(
                        g.span(),
                        format!("match guard must be `bool`, found `{}`", gt.display()),
                    );
                }
            }
            let body_ty = self.check_expr(&arm.body);
            result_ty = Some(match result_ty {
                None => body_ty,
                Some(prev) => match prev.unify(&body_ty) {
                    Some(u) => u,
                    None => {
                        self.error(
                            arm.body.span(),
                            format!(
                                "match arm has type `{}` but previous arms had type `{}`",
                                body_ty.display(),
                                prev.display()
                            ),
                        );
                        Ty::Error
                    }
                },
            });
        }
        // Exhaustiveness + unreachable-arm checking. Runs after per-arm
        // type checks so we don't double-report on broken patterns.
        self.check_match_exhaustiveness(&st, arms, span);
        result_ty.unwrap_or(Ty::Unit)
    }

    /// `expr?` — `expr` must be a `Result`-shaped enum; the operator
    /// yields the `Ok` payload type, and the enclosing function must
    /// return a `Result` with a matching error type so a propagated
    /// `Err` can be returned as-is.
    fn check_try(&mut self, inner: &Expr, span: Span) -> Ty {
        let inner_ty = self.check_expr(inner);
        if inner_ty.is_error() {
            return Ty::Error;
        }
        let result_shape = match &inner_ty {
            Ty::Enum(s, args) if args.len() == 2 => {
                let is_result = self
                    .res
                    .enum_variants
                    .get(s)
                    .map(|v| v.contains_key("Ok") && v.contains_key("Err"))
                    .unwrap_or(false);
                if is_result {
                    Some((*s, args[0].clone(), args[1].clone()))
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some((rsym, ok_ty, err_ty)) = result_shape else {
            self.error(
                span,
                format!(
                    "the `?` operator requires a `Result`, but the operand \
                     has type `{}`",
                    inner_ty.display()
                ),
            );
            return Ty::Error;
        };
        match &self.current_return {
            Ty::Enum(s2, ret_args) if *s2 == rsym && ret_args.len() == 2 => {
                if !ret_args[1].compatible(&err_ty) {
                    self.error(
                        span,
                        format!(
                            "`?` propagates an error of type `{}`, but the \
                             enclosing function's `Result` error type is `{}`",
                            err_ty.display(),
                            ret_args[1].display()
                        ),
                    );
                }
            }
            other => {
                self.error(
                    span,
                    format!(
                        "the `?` operator can only be used in a function \
                         returning a `Result`, but this one returns `{}`",
                        other.display()
                    ),
                );
            }
        }
        ok_ty
    }

    fn check_return(&mut self, value: Option<&Expr>, span: Span) -> Ty {
        let ret_ty = self.current_return.clone();
        let actual = value.map(|v| self.check_expr(v)).unwrap_or(Ty::Unit);
        let value_span = value.map(|v| v.span()).unwrap_or(span);
        if !self.check_assignable(value_span, &actual, &ret_ty) {
            self.error(
                span,
                format!(
                    "return value has type `{}` but function returns `{}`",
                    actual.display(),
                    ret_ty.display()
                ),
            );
        }
        Ty::Never
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.errors.push(TypeError { message: msg.into(), span });
    }
}

/// Types that `print` (the polymorphic builtin) currently supports.
/// Recursive positional unification. Every `TypeVar(t)` on the
/// param side binds `t` to the corresponding concrete on the arg
/// side. Struct/Enum type args unify element-wise so passing
/// `Box<i64>` to `unbox<T>(b: Box<T>) -> T` infers T = i64.
fn unify_typevars(
    param: &Ty,
    arg: &Ty,
    subst: &mut std::collections::HashMap<SymbolId, Ty>,
) {
    match (param, arg) {
        (Ty::TypeVar(t), concrete) => {
            subst.insert(*t, concrete.clone());
        }
        (Ty::Struct(s1, pargs), Ty::Struct(s2, aargs))
        | (Ty::Enum(s1, pargs), Ty::Enum(s2, aargs))
            if s1 == s2 && pargs.len() == aargs.len() =>
        {
            for (p, a) in pargs.iter().zip(aargs.iter()) {
                unify_typevars(p, a, subst);
            }
        }
        (Ty::Array(p_elem, _), Ty::Array(a_elem, _)) => {
            unify_typevars(p_elem, a_elem, subst);
        }
        (Ty::Vec(p_elem), Ty::Vec(a_elem)) => {
            unify_typevars(p_elem, a_elem, subst);
        }
        (
            Ty::Fn { params: p_params, ret: p_ret },
            Ty::Fn { params: a_params, ret: a_ret },
        ) if p_params.len() == a_params.len() => {
            for (p, a) in p_params.iter().zip(a_params.iter()) {
                unify_typevars(p, a, subst);
            }
            unify_typevars(p_ret, a_ret, subst);
        }
        _ => {}
    }
}

/// Back-compat free shim — used in pattern-binding sites where
/// associated-type projection resolution is not needed.
fn apply_subst(ty: &Ty, subst: &std::collections::HashMap<SymbolId, Ty>) -> Ty {
    apply_subst_inner(ty, subst, None, None)
}

/// A projection through a `dyn Trait` (`Ty::Assoc(Ty::Dyn(_), _)`)
/// has no concrete impl binding to consult — the type system would
/// need either an upcast or a flattened vtable. Collapse to
/// `Ty::Error` so it's compatible with everything and never reaches
/// the monomorphizer with an unresolvable projection.
fn collapse_dyn_assoc(ty: Ty) -> Ty {
    if is_dyn_assoc(&ty) {
        return Ty::Error;
    }
    ty
}

/// Predicate for the same shape — used by `dyn_method_sig` to flag
/// the collapse so the caller can emit a precise diagnostic.
fn is_dyn_assoc(ty: &Ty) -> bool {
    if let Ty::Assoc(base, _) = ty {
        return matches!(**base, Ty::Dyn(_, _));
    }
    false
}

/// Substitute type parameters in `ty`. Recurses through `Array`,
/// `Vec`, `Fn`, `Struct`, `Enum`, `Weak`, and `Assoc` base types;
/// substitutes `Ty::SelfType` when `self_ty` is provided; resolves
/// `Ty::Assoc(Struct(s, _), name)` via `bindings` when available.
fn apply_subst_inner(
    ty: &Ty,
    subst: &std::collections::HashMap<SymbolId, Ty>,
    self_ty: Option<&Ty>,
    bindings: Option<&std::collections::HashMap<(SymbolId, String), Ty>>,
) -> Ty {
    apply_subst_inner_with(ty, subst, self_ty, bindings, None)
}

/// Same as `apply_subst_inner` but takes the resolver so that a
/// concrete-struct projection lookup can substitute the impl's own
/// generic params using the struct's type args. Without that step,
/// `VecIter<i64>::Item` resolves to the impl-block's `TypeVar(T)`
/// rather than the concrete `i64` — a partial resolution that leaks
/// to codegen.
fn apply_subst_inner_with(
    ty: &Ty,
    subst: &std::collections::HashMap<SymbolId, Ty>,
    self_ty: Option<&Ty>,
    bindings: Option<&std::collections::HashMap<(SymbolId, String), Ty>>,
    res: Option<&crate::resolver::Resolutions>,
) -> Ty {
    match ty {
        Ty::TypeVar(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
        Ty::SelfType => self_ty.cloned().unwrap_or_else(|| ty.clone()),
        Ty::Assoc(base, name) => {
            let new_base = apply_subst_inner_with(base, subst, self_ty, bindings, res);
            // If the projection's base is now a concrete struct and
            // an impl binding is known, resolve. Otherwise the
            // projection stays unresolved for the monomorphizer.
            if let Ty::Struct(s, args) = &new_base {
                if let Some(map) = bindings {
                    if let Some(resolved) = map.get(&(*s, name.clone())) {
                        // Build a struct-generic-arg substitution if
                        // the resolver is available — turns the
                        // impl-block's `Ty::TypeVar(T)` into the
                        // user-site arg type. Without this,
                        // `VecIter<i64>::Item` would resolve only to
                        // `Ty::TypeVar(T_VecIter)`.
                        let struct_subst = if let Some(r) = res {
                            build_struct_subst(r, *s, args)
                        } else {
                            std::collections::HashMap::new()
                        };
                        let after_struct = apply_subst_inner_with(
                            resolved,
                            &struct_subst,
                            self_ty,
                            bindings,
                            res,
                        );
                        return apply_subst_inner_with(
                            &after_struct,
                            subst,
                            self_ty,
                            bindings,
                            res,
                        );
                    }
                }
            }
            Ty::Assoc(Box::new(new_base), name.clone())
        }
        Ty::Array(elem, n) => Ty::Array(
            Box::new(apply_subst_inner_with(elem, subst, self_ty, bindings, res)),
            *n,
        ),
        Ty::Vec(elem) => Ty::Vec(Box::new(apply_subst_inner_with(
            elem, subst, self_ty, bindings, res,
        ))),
        Ty::Fn { params, ret } => Ty::Fn {
            params: params
                .iter()
                .map(|t| apply_subst_inner_with(t, subst, self_ty, bindings, res))
                .collect(),
            ret: Box::new(apply_subst_inner_with(ret, subst, self_ty, bindings, res)),
        },
        Ty::Struct(s, args) => Ty::Struct(
            *s,
            args.iter()
                .map(|t| apply_subst_inner_with(t, subst, self_ty, bindings, res))
                .collect(),
        ),
        Ty::Enum(s, args) => Ty::Enum(
            *s,
            args.iter()
                .map(|t| apply_subst_inner_with(t, subst, self_ty, bindings, res))
                .collect(),
        ),
        Ty::Weak(inner) => Ty::Weak(Box::new(apply_subst_inner_with(
            inner, subst, self_ty, bindings, res,
        ))),
        _ => ty.clone(),
    }
}

/// Build a substitution map from a struct's generic param syms to
/// the type args at a use site. Used by field-access type resolution
/// so a `Box<i64>` value's `value` field resolves to i64.
fn build_struct_subst(
    res: &crate::resolver::Resolutions,
    struct_sym: SymbolId,
    use_args: &[Ty],
) -> std::collections::HashMap<SymbolId, Ty> {
    let mut subst = std::collections::HashMap::new();
    if let Some(generics) = res.struct_generics.get(&struct_sym) {
        for (gsym, arg) in generics.iter().zip(use_args.iter()) {
            subst.insert(*gsym, arg.clone());
        }
    }
    subst
}

/// Build a subst for the scrutinee of a `match`. If the scrutinee
/// is a generic enum type (`Option<i64>`), pair the enum's declared
/// generic param symbols with the scrutinee's type args. Empty for
/// non-enum / non-generic scrutinees.
fn build_enum_subst_from_scrutinee(
    res: &crate::resolver::Resolutions,
    scrutinee_ty: &Ty,
) -> std::collections::HashMap<SymbolId, Ty> {
    let mut subst = std::collections::HashMap::new();
    if let Ty::Enum(enum_sym, args) = scrutinee_ty {
        if let Some(generics) = res.enum_generics.get(enum_sym) {
            for (gsym, arg) in generics.iter().zip(args.iter()) {
                subst.insert(*gsym, arg.clone());
            }
        }
    }
    subst
}

/// Read out a generic enum's args in declaration order from an
/// inferred subst built during variant construction. Params not
/// inferred (e.g., `None` on `Option<T>` with no payload) stay as
/// TypeVar; downstream context can refine them.
fn enum_generic_args(
    res: &crate::resolver::Resolutions,
    enum_sym: SymbolId,
    subst: &std::collections::HashMap<SymbolId, Ty>,
) -> Vec<Ty> {
    res.enum_generics
        .get(&enum_sym)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|g| subst.get(g).cloned().unwrap_or_else(|| Ty::TypeVar(*g)))
        .collect()
}

fn is_printable(t: &Ty) -> bool {
    matches!(t, Ty::Int(_) | Ty::Str)
}

/// Whether `T` is a valid `Vec<T>` element type in v0.x. Vec stores
/// elements in 8-byte slots, so the element must be i64-or-smaller
/// scalar or a pointer-shaped type (a struct, enum, nested Vec, or
/// trait object — all 8-byte pointers). `str` is a 16-byte stack
/// descriptor (can't fit, and storing its pointer would dangle);
/// floats and arrays are deferred. `TypeVar` is allowed — a generic
/// `Vec<T>` is validated once monomorphization makes `T` concrete.
fn vec_element_supported(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Int(_)
            | Ty::Bool
            | Ty::Char
            | Ty::Struct(_, _)
            | Ty::Enum(_, _)
            | Ty::Vec(_)
            | Ty::Dyn(_, _)
            | Ty::TypeVar(_)
            // An unresolved projection (`T::Item`) is opaque at
            // typecheck time; monomorphization replaces it with a
            // concrete type that the check above accepts. Allow it
            // through so generic helpers like `collect<T: Iterator>
            // -> Vec<T::Item>` typecheck.
            | Ty::Assoc(_, _)
            | Ty::Error
    )
}

/// Method signature, used by `check_method_call`.
struct MethodSig {
    params: Vec<Ty>,
    ret: Ty,
}

/// Hardcoded method table for builtin types. Future: methods on
/// user-defined types via `impl` blocks.
fn resolve_method(recv: &Ty, name: &str) -> Option<MethodSig> {
    use crate::ty::IntTy;
    match (recv, name) {
        (Ty::Str, "len") => Some(MethodSig {
            params: vec![],
            ret: Ty::Int(IntTy::I64),
        }),
        (Ty::Str, "is_empty") => Some(MethodSig {
            params: vec![],
            ret: Ty::Bool,
        }),
        (Ty::Str, "starts_with")
        | (Ty::Str, "ends_with")
        | (Ty::Str, "contains") => Some(MethodSig {
            params: vec![Ty::Str],
            ret: Ty::Bool,
        }),
        (Ty::Array(_, _), "len") => Some(MethodSig {
            params: vec![],
            ret: Ty::Int(IntTy::I64),
        }),
        // Vec<T> methods — element-typed off the receiver.
        (Ty::Vec(elem), "push") => Some(MethodSig {
            params: vec![(**elem).clone()],
            ret: Ty::Unit,
        }),
        (Ty::Vec(elem), "get") => Some(MethodSig {
            params: vec![Ty::Int(IntTy::I64)],
            ret: (**elem).clone(),
        }),
        (Ty::Vec(_), "len") => Some(MethodSig {
            params: vec![],
            ret: Ty::Int(IntTy::I64),
        }),
        _ => None,
    }
}

fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}
