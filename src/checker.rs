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
use crate::ty::{Ty, DEFAULT_FLOAT, DEFAULT_INT};

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
    pub errors: Vec<TypeError>,
}

pub struct Checker<'r> {
    res: &'r Resolutions,
    expr_types: HashMap<Span, Ty>,
    fn_signatures: HashMap<Span, Ty>,
    local_types: HashMap<Span, Ty>,
    type_resolutions: HashMap<Span, Ty>,
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
            errors: Vec::new(),
            current_return: Ty::Unit,
        }
    }

    pub fn check_module(mut self, m: &Module) -> CheckResults {
        // Pass 1: function signatures — so cross-function and order-independent
        // calls work without forward declaration.
        for item in &m.items {
            if let Item::Fn(f) = item {
                let sig = self.fn_signature(f);
                self.fn_signatures.insert(f.name.span, sig);
            }
            if let Item::Const(c) = item {
                let ty = self.resolve_type(&c.ty);
                self.local_types.insert(c.name.span, ty);
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
            errors: self.errors,
        }
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
            Expr::MethodCall { receiver, args, span, .. } => {
                self.check_expr(receiver);
                for a in args {
                    self.check_expr(a);
                }
                self.error(*span, "method calls are not yet type-checked");
                Ty::Error
            }
            Expr::Field { receiver, span, .. } => {
                self.check_expr(receiver);
                self.error(*span, "field access is not yet type-checked");
                Ty::Error
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
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
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
            let needs_numeric = matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            );
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
                    SymbolKind::Fn | SymbolKind::BuiltinFn(_) => {
                        self.error(span, format!("cannot assign to function `{}`", name));
                    }
                    SymbolKind::BuiltinType(_)
                    | SymbolKind::Struct
                    | SymbolKind::Enum => {
                        self.error(span, format!("cannot assign to type `{}`", name));
                    }
                }
            }
            Expr::Index { .. } | Expr::Field { .. } => {
                // Allowed; deeper check deferred.
            }
            _ => {
                self.error(span, "invalid assignment target");
            }
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> Ty {
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

    fn check_index(&mut self, receiver: &Expr, index: &Expr, span: Span) -> Ty {
        let rt = self.check_expr(receiver);
        let it = self.check_expr(index);
        if !it.is_integer() && !it.is_error() {
            self.error(
                index.span(),
                format!("index must be an integer, found `{}`", it.display()),
            );
        }
        match rt {
            Ty::Array(elem, _) => *elem,
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
