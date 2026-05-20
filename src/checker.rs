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
                let ty = match kind {
                    SymbolKind::BuiltinType(t) => t,
                    SymbolKind::Struct => Ty::Struct(sym_id),
                    SymbolKind::Enum => Ty::Enum(sym_id),
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
            }
            Item::Struct(_) | Item::Enum(_) => {
                // Field/variant types were resolved by the resolver and
                // signatures aren't needed yet — nothing to check here.
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

    fn bind_pattern(&mut self, p: &Pattern, ty: &Ty) {
        match p {
            Pattern::Wildcard(_) => {}
            Pattern::Ident { name, .. } => {
                self.local_types.insert(name.span, ty.clone());
            }
            Pattern::Literal { .. } => {}
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
                Ty::Enum(enum_sym)
            }
            SymbolKind::BuiltinType(_) | SymbolKind::Struct | SymbolKind::Enum => {
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
                    | SymbolKind::Enum => {
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
                for (i, (param_ty, arg_ty)) in params.iter().zip(&arg_tys).enumerate() {
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
                *ret
            }
            Ty::Error => Ty::Error,
            other => {
                self.error(span, format!("cannot call value of type `{}`", other.display()));
                Ty::Error
            }
        }
    }

    /// Look up a method declared in an `impl` block on a struct type.
    /// Returns the method's externally-visible signature (without the
    /// `self` parameter).
    fn user_method_sig(&self, recv: &Ty, name: &str) -> Option<MethodSig> {
        let Ty::Struct(sym_id) = recv else { return None };
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
        let mut provided = std::collections::HashSet::new();
        for init in fields {
            let value_ty = self.check_expr(&init.value);
            let Some(decl_field) = layout.field(&init.name.name) else {
                self.error(
                    init.name.span,
                    format!("`{}` has no field `{}`", sym_name, init.name.name),
                );
                continue;
            };
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
        Ty::Struct(sym_id)
    }

    fn check_field_access(&mut self, receiver: &Expr, name: &Ident, span: Span) -> Ty {
        let recv_ty = self.check_expr(receiver);
        let Ty::Struct(sym_id) = recv_ty else {
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
        field.ty.clone()
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
            .or_else(|| self.user_method_sig(&recv_ty, &method.name));
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
        let _ = span;
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
