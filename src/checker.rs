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

pub struct Checker<'r> {
    res: &'r Resolutions,
    expr_types: HashMap<Span, Ty>,
    fn_signatures: HashMap<Span, Ty>,
    local_types: HashMap<Span, Ty>,
    type_resolutions: HashMap<Span, Ty>,
    struct_layouts: HashMap<SymbolId, StructLayout>,
    errors: Vec<TypeError>,
    current_return: Ty,
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
            errors: Vec::new(),
            current_return: Ty::Unit,
        }
    }

    pub fn check_module(mut self, m: &Module) -> CheckResults {
        // Pass 1a: struct layouts — needed before signatures so that
        // function params/returns that mention struct types resolve.
        for item in &m.items {
            if let Item::Struct(s) = item {
                if let Some(&sym_id) = self.res.decl_to_sym.get(&s.name.span) {
                    let layout = self.build_struct_layout(s);
                    self.struct_layouts.insert(sym_id, layout);
                }
            }
        }
        // Pass 1b: function signatures + const types + impl methods.
        for item in &m.items {
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
                    for method in &i.methods {
                        let sig = self.fn_signature(method);
                        self.fn_signatures.insert(method.name.span, sig);
                    }
                }
                Item::Trait(t) => {
                    // Resolve each trait method signature's types so
                    // `type_resolutions` has entries before any fn
                    // body (which may call a bounded-generic method)
                    // is checked in pass 2.
                    for m in &t.methods {
                        for p in &m.params {
                            self.resolve_type(&p.ty);
                        }
                        if let Some(rt) = &m.return_type {
                            self.resolve_type(rt);
                        }
                    }
                }
                Item::Struct(_) | Item::Enum(_) => {}
            }
        }

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
            errors: self.errors,
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
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.check_fn(f),
            Item::Const(c) => self.check_const(c),
            Item::Impl(i) => {
                for method in &i.methods {
                    self.check_fn(method);
                }
                self.check_trait_impl_conformance(i);
            }
            Item::Struct(_) | Item::Enum(_) | Item::Trait(_) => {
                // Field/variant types were resolved by the resolver;
                // trait method signature types are resolved in pass 1b.
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
        let inferred = l.init.as_ref().map(|e| self.check_expr(e));
        let final_ty = match (declared, inferred) {
            (Some(d), Some(i)) => {
                if !i.compatible(&d) {
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
            Expr::Try { expr, span } => {
                self.check_expr(expr);
                self.error(*span, "the `?` operator is not yet type-checked");
                Ty::Error
            }
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
            | SymbolKind::Trait => {
                self.error(p.span, format!("`{}` is a type, not a value", name));
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
                    | SymbolKind::Trait => {
                        self.error(span, format!("cannot assign to type `{}`", name));
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
                    SymbolKind::Local { mutable: false } => self.error(
                        span,
                        format!("cannot assign to field of immutable binding `{}`", name),
                    ),
                    SymbolKind::Param => self.error(
                        span,
                        format!("cannot assign to field of parameter `{}`", name),
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
                    if !arg_ty.compatible(param_ty) {
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
                apply_subst(&ret, &subst)
            }
            Ty::Error => Ty::Error,
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
            if !arg_ty.compatible(param_ty) {
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
        let (_self_ty, rest) = params.split_first()?;
        Some(MethodSig {
            params: rest.to_vec(),
            ret: (**ret).clone(),
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
        // Also infer the struct's generic args from field types so the
        // resulting Ty::Struct carries them — downstream field access
        // can then resolve TypeVar to the concrete instantiation.
        let mut provided = std::collections::HashSet::new();
        let mut subst: std::collections::HashMap<SymbolId, Ty> =
            std::collections::HashMap::new();
        for init in fields {
            let value_ty = self.check_expr(&init.value);
            let Some(decl_field) = layout.field(&init.name.name) else {
                self.error(
                    init.name.span,
                    format!("`{}` has no field `{}`", sym_name, init.name.name),
                );
                continue;
            };
            unify_typevars(&decl_field.ty, &value_ty, &mut subst);
            if !value_ty.compatible(&decl_field.ty) {
                self.error(
                    init.value.span(),
                    format!(
                        "field `{}` declared `{}` but value has type `{}`",
                        init.name.name,
                        decl_field.ty.display(),
                        value_ty.display()
                    ),
                );
            }
            if !provided.insert(init.name.name.clone()) {
                self.error(
                    init.name.span,
                    format!("field `{}` set more than once", init.name.name),
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
        apply_subst(&field.ty, &subst)
    }

    /// Resolve a method call where the receiver is a bounded generic
    /// parameter (`x: T` with `T: SomeTrait`). The method must be
    /// declared by one of `T`'s trait bounds. The returned signature
    /// drops the explicit `self` parameter to match `MethodSig`'s
    /// "externally visible" convention.
    fn trait_bound_method_sig(&self, recv: &Ty, name: &str) -> Option<MethodSig> {
        let Ty::TypeVar(tvar) = recv else { return None };
        let bounds = self.res.generic_bounds.get(tvar)?;
        for &trait_sym in bounds {
            let methods = self.res.trait_methods.get(&trait_sym)?;
            if let Some(m) = methods.iter().find(|m| m.name.name == name) {
                // Skip the leading `self` param; resolve the rest.
                let mut params: Vec<Ty> = Vec::new();
                for p in m.params.iter().skip(1) {
                    params.push(
                        self.type_resolutions
                            .get(&p.ty.span())
                            .cloned()
                            .unwrap_or(Ty::Error),
                    );
                }
                let ret = m
                    .return_type
                    .as_ref()
                    .and_then(|t| self.type_resolutions.get(&t.span()).cloned())
                    .unwrap_or(Ty::Unit);
                return Some(MethodSig { params, ret });
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
            .or_else(|| self.user_method_sig(&recv_ty, &method.name))
            .or_else(|| self.trait_bound_method_sig(&recv_ty, &method.name));
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
            if !a.compatible(p) {
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
                if !matches!(t, Ty::Vec | Ty::Error) {
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
            Ty::Error => Ty::Error,
            other => {
                self.error(
                    iter.span(),
                    format!("cannot iterate over `{}`", other.display()),
                );
                Ty::Error
            }
        };
        self.bind_pattern(pat, &elem_ty);
        self.check_block(body);
        Ty::Unit
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

    fn check_return(&mut self, value: Option<&Expr>, span: Span) -> Ty {
        let ret_ty = self.current_return.clone();
        let actual = value.map(|v| self.check_expr(v)).unwrap_or(Ty::Unit);
        if !actual.compatible(&ret_ty) {
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
        _ => {}
    }
}

fn apply_subst(ty: &Ty, subst: &std::collections::HashMap<SymbolId, Ty>) -> Ty {
    match ty {
        Ty::TypeVar(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Array(elem, n) => Ty::Array(Box::new(apply_subst(elem, subst)), *n),
        Ty::Fn { params, ret } => Ty::Fn {
            params: params.iter().map(|t| apply_subst(t, subst)).collect(),
            ret: Box::new(apply_subst(ret, subst)),
        },
        Ty::Struct(s, args) => Ty::Struct(
            *s,
            args.iter().map(|t| apply_subst(t, subst)).collect(),
        ),
        Ty::Enum(s, args) => Ty::Enum(
            *s,
            args.iter().map(|t| apply_subst(t, subst)).collect(),
        ),
        Ty::Weak(inner) => Ty::Weak(Box::new(apply_subst(inner, subst))),
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
        // Vec methods. v0.x: element type is implicitly i64.
        (Ty::Vec, "push") => Some(MethodSig {
            params: vec![Ty::Int(IntTy::I64)],
            ret: Ty::Unit,
        }),
        (Ty::Vec, "get") => Some(MethodSig {
            params: vec![Ty::Int(IntTy::I64)],
            ret: Ty::Int(IntTy::I64),
        }),
        (Ty::Vec, "len") => Some(MethodSig {
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
