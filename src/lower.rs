//! AST → HIR lowering.
//!
//! Walks the AST together with `Resolutions` (for symbol identity) and
//! `CheckResults` (for type tags), producing an HIR that's ready for
//! codegen.

use crate::ast;
use crate::checker::CheckResults;
use crate::hir::*;
use crate::resolver::{Resolutions, SymbolKind};
use crate::ty::{IntTy, Ty};

pub struct Lowerer<'a> {
    res: &'a Resolutions,
    check: &'a CheckResults,
}

impl<'a> Lowerer<'a> {
    pub fn new(res: &'a Resolutions, check: &'a CheckResults) -> Self {
        Self { res, check }
    }

    pub fn lower_module(&self, m: &ast::Module) -> HirModule {
        let mut items = Vec::new();
        for it in &m.items {
            if let ast::Item::Fn(f) = it {
                items.push(HirItem::Fn(self.lower_fn(f)));
            }
            // Const, Struct, Enum dropped — codegen doesn't handle them yet.
        }
        HirModule { items }
    }

    fn lower_fn(&self, f: &ast::FnDecl) -> HirFn {
        let sym = self.res.decl_to_sym[&f.name.span];
        let params: Vec<HirParam> = f
            .params
            .iter()
            .map(|p| {
                let sym = self.res.decl_to_sym[&p.name.span];
                let ty = self
                    .check
                    .local_types
                    .get(&p.name.span)
                    .cloned()
                    .unwrap_or(Ty::Error);
                HirParam { sym, name: p.name.name.clone(), ty }
            })
            .collect();
        let ret_ty = f
            .return_type
            .as_ref()
            .and_then(|t| self.check.type_resolutions.get(&t.span()).cloned())
            .unwrap_or(Ty::Unit);
        let body = self.lower_block(&f.body);
        HirFn { sym, name: f.name.name.clone(), params, ret_ty, body }
    }

    fn lower_block(&self, b: &ast::Block) -> HirBlock {
        let mut stmts = Vec::new();
        let mut last_ty = Ty::Unit;
        for s in &b.stmts {
            match s {
                ast::Stmt::Let(l) => {
                    stmts.push(HirStmt::Let(self.lower_let(l)));
                    last_ty = Ty::Unit;
                }
                ast::Stmt::Expr(e, has_semi) => {
                    let he = self.lower_expr(e);
                    let ty = he.ty.clone();
                    stmts.push(HirStmt::Expr(he, *has_semi));
                    last_ty = if *has_semi { Ty::Unit } else { ty };
                }
                ast::Stmt::Item(_) => {
                    // Nested items aren't lowered for codegen.
                }
            }
        }
        HirBlock { stmts, ty: last_ty }
    }

    fn lower_let(&self, l: &ast::LetStmt) -> HirLet {
        let (sym, mutable) = match &l.pat {
            ast::Pattern::Wildcard(_) => (None, l.mutable),
            ast::Pattern::Ident { name, mutable: pat_mut, .. } => {
                let s = self.res.decl_to_sym.get(&name.span).copied();
                (s, l.mutable || *pat_mut)
            }
            ast::Pattern::Literal { .. } => (None, l.mutable),
        };
        let ty = sym
            .and_then(|s| self.check.local_types.get(&self.res.symbol(s).span).cloned())
            .unwrap_or_else(|| {
                // Wildcard or no symbol — fall back to init's type or Unit.
                l.init
                    .as_ref()
                    .and_then(|e| self.check.expr_types.get(&e.span()).cloned())
                    .unwrap_or(Ty::Unit)
            });
        let init = l.init.as_ref().map(|e| self.lower_expr(e));
        HirLet { sym, mutable, ty, init }
    }

    fn lower_expr(&self, e: &ast::Expr) -> HirExpr {
        let span = e.span();
        let ty = self.check.expr_types.get(&span).cloned().unwrap_or(Ty::Error);
        let kind = self.lower_expr_kind(e);
        HirExpr { kind, ty }
    }

    fn lower_expr_kind(&self, e: &ast::Expr) -> HirExprKind {
        match e {
            ast::Expr::Lit { lit, .. } => HirExprKind::Lit(self.lower_lit(lit, e)),
            ast::Expr::Path(p) => self.lower_path(p),
            ast::Expr::Unary { op, expr, .. } => HirExprKind::Unary {
                op: lower_unop(*op),
                expr: Box::new(self.lower_expr(expr)),
            },
            ast::Expr::Binary { op, lhs, rhs, .. } => {
                match logical_op(*op) {
                    Some(lop) => HirExprKind::Logical {
                        op: lop,
                        lhs: Box::new(self.lower_expr(lhs)),
                        rhs: Box::new(self.lower_expr(rhs)),
                    },
                    None => HirExprKind::Binary {
                        op: lower_binop(*op),
                        lhs: Box::new(self.lower_expr(lhs)),
                        rhs: Box::new(self.lower_expr(rhs)),
                    },
                }
            }
            ast::Expr::Assign { lhs, rhs, .. } => match self.path_symbol(lhs) {
                Some(sym) => HirExprKind::Assign { lhs: sym, rhs: Box::new(self.lower_expr(rhs)) },
                None => HirExprKind::Unsupported("assignment target other than a local binding".into()),
            },
            ast::Expr::AssignOp { op, lhs, rhs, .. } => match self.path_symbol(lhs) {
                Some(sym) => HirExprKind::AssignOp {
                    lhs: sym,
                    op: lower_binop(*op),
                    rhs: Box::new(self.lower_expr(rhs)),
                },
                None => HirExprKind::Unsupported(
                    "compound assignment target other than a local binding".into(),
                ),
            },
            ast::Expr::Call { callee, args, .. } => match self.path_symbol(callee) {
                Some(sym) if self.is_fn_symbol(sym) => HirExprKind::Call {
                    callee: sym,
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                },
                Some(sym) if self.is_builtin_fn_symbol(sym) => HirExprKind::BuiltinCall {
                    name: self.res.symbol(sym).name.clone(),
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                },
                Some(sym) if self.is_poly_builtin_fn_symbol(sym) => self.lower_poly_call(sym, args),
                _ => HirExprKind::Unsupported("call target other than a named function".into()),
            },
            ast::Expr::Block(b) => HirExprKind::Block(self.lower_block(b)),
            ast::Expr::If { cond, then_branch, else_branch, .. } => HirExprKind::If {
                cond: Box::new(self.lower_expr(cond)),
                then_b: self.lower_block(then_branch),
                else_b: else_branch.as_ref().map(|e| Box::new(self.lower_expr(e))),
            },
            ast::Expr::While { cond, body, .. } => HirExprKind::While {
                cond: Box::new(self.lower_expr(cond)),
                body: self.lower_block(body),
            },
            ast::Expr::Return { value, .. } => {
                HirExprKind::Return(value.as_ref().map(|v| Box::new(self.lower_expr(v))))
            }
            ast::Expr::Break(_) => HirExprKind::Unsupported("break".into()),
            ast::Expr::Continue(_) => HirExprKind::Unsupported("continue".into()),
            ast::Expr::MethodCall { receiver, method, args, .. } => HirExprKind::MethodCall {
                receiver: Box::new(self.lower_expr(receiver)),
                method: method.name.clone(),
                args: args.iter().map(|a| self.lower_expr(a)).collect(),
            },
            ast::Expr::Field { .. } => HirExprKind::Unsupported("field access".into()),
            ast::Expr::Index { receiver, index, .. } => {
                let recv = self.lower_expr(receiver);
                // String indexing dispatches on whether the index is a range.
                if matches!(recv.ty, Ty::Str) {
                    if let ast::Expr::Range { start, end, inclusive, .. } = index.as_ref() {
                        // Only `a..b` / `a..=b` are parsed today; both sides
                        // are always Some.
                        let start_expr = start
                            .as_deref()
                            .map(|e| self.lower_expr(e))
                            .unwrap_or_else(|| HirExpr {
                                kind: HirExprKind::Lit(HirLit::Int(0, IntTy::I64)),
                                ty: Ty::Int(IntTy::I64),
                            });
                        let end_expr = end
                            .as_deref()
                            .map(|e| self.lower_expr(e))
                            .unwrap_or_else(|| HirExpr {
                                kind: HirExprKind::Lit(HirLit::Int(0, IntTy::I64)),
                                ty: Ty::Int(IntTy::I64),
                            });
                        return HirExprKind::StrSlice {
                            str_val: Box::new(recv),
                            start: Box::new(start_expr),
                            end: Box::new(end_expr),
                            inclusive: *inclusive,
                        };
                    }
                    return HirExprKind::StrByteIndex {
                        str_val: Box::new(recv),
                        index: Box::new(self.lower_expr(index)),
                    };
                }
                let idx = self.lower_expr(index);
                let elem_ty = match &recv.ty {
                    Ty::Array(elem, _) => (**elem).clone(),
                    _ => Ty::Error,
                };
                HirExprKind::Index {
                    array: Box::new(recv),
                    index: Box::new(idx),
                    elem_ty,
                }
            }
            ast::Expr::Try { .. } => HirExprKind::Unsupported("`?` operator".into()),
            ast::Expr::Cast { .. } => HirExprKind::Unsupported("`as` cast".into()),
            ast::Expr::Array { elems, .. } => {
                let lowered: Vec<HirExpr> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let elem_ty = lowered
                    .first()
                    .map(|e| e.ty.clone())
                    .unwrap_or(Ty::Error);
                HirExprKind::Array { elems: lowered, elem_ty }
            }
            ast::Expr::For { pat, iter, body, .. } => self.lower_for(pat, iter, body),
            ast::Expr::Match { .. } => HirExprKind::Unsupported("match expressions".into()),
            ast::Expr::Range { .. } => HirExprKind::Unsupported(
                "range expressions are only supported inside string slicing (e.g. `s[a..b]`)"
                    .into(),
            ),
        }
    }

    fn lower_lit(&self, lit: &ast::Lit, e: &ast::Expr) -> HirLit {
        let ty = self.check.expr_types.get(&e.span()).cloned().unwrap_or(Ty::Error);
        match lit {
            ast::Lit::Int(v) => {
                let int_ty = match ty {
                    Ty::Int(it) => it,
                    _ => IntTy::I64,
                };
                HirLit::Int(*v, int_ty)
            }
            ast::Lit::Float(v) => {
                let float_ty = match ty {
                    Ty::Float(ft) => ft,
                    _ => crate::ty::FloatTy::F64,
                };
                HirLit::Float(*v, float_ty)
            }
            ast::Lit::Bool(b) => HirLit::Bool(*b),
            ast::Lit::Str(s) => HirLit::Str(s.clone()),
            // Char has no codegen support yet.
            ast::Lit::Char(_) => HirLit::Unit,
        }
    }

    fn lower_path(&self, p: &ast::Path) -> HirExprKind {
        let Some(&sym_id) = self.res.path_to_sym.get(&p.span) else {
            return HirExprKind::Unsupported(format!("unresolved path"));
        };
        match self.res.symbol(sym_id).kind {
            SymbolKind::Local { .. } | SymbolKind::Param | SymbolKind::Const => {
                HirExprKind::Local(sym_id)
            }
            SymbolKind::Fn => HirExprKind::Fn(sym_id),
            _ => HirExprKind::Unsupported("type name used as value".into()),
        }
    }

    fn lower_for(
        &self,
        pat: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
    ) -> HirExprKind {
        let local = match pat {
            ast::Pattern::Wildcard(_) => None,
            ast::Pattern::Ident { name, .. } => self.res.decl_to_sym.get(&name.span).copied(),
            ast::Pattern::Literal { .. } => {
                return HirExprKind::Unsupported(
                    "for-loop pattern must be an identifier or `_`".into(),
                );
            }
        };
        // Special-case `for x in a..b` so users don't need a real
        // iterator protocol to iterate integer ranges.
        if let ast::Expr::Range { start, end, inclusive, .. } = iter {
            let start_h = start
                .as_deref()
                .map(|e| self.lower_expr(e))
                .unwrap_or_else(|| HirExpr {
                    kind: HirExprKind::Lit(HirLit::Int(0, IntTy::I64)),
                    ty: Ty::Int(IntTy::I64),
                });
            let end_h = end
                .as_deref()
                .map(|e| self.lower_expr(e))
                .unwrap_or_else(|| HirExpr {
                    kind: HirExprKind::Lit(HirLit::Int(0, IntTy::I64)),
                    ty: Ty::Int(IntTy::I64),
                });
            return HirExprKind::ForRange {
                local,
                start: Box::new(start_h),
                end: Box::new(end_h),
                inclusive: *inclusive,
                body: self.lower_block(body),
            };
        }
        let iter_hir = self.lower_expr(iter);
        let (elem_ty, length) = match &iter_hir.ty {
            Ty::Array(elem, n) => ((**elem).clone(), *n),
            _ => {
                return HirExprKind::Unsupported(
                    "for-loop iterator must be a stack-allocated array or an integer range"
                        .into(),
                );
            }
        };
        HirExprKind::For {
            local,
            iter: Box::new(iter_hir),
            body: self.lower_block(body),
            elem_ty,
            length,
        }
    }

    /// Extract a `SymbolId` if `e` is a single-segment path to a local/param/const/fn.
    fn path_symbol(&self, e: &ast::Expr) -> Option<crate::ty::SymbolId> {
        let ast::Expr::Path(p) = e else { return None };
        self.res.path_to_sym.get(&p.span).copied()
    }

    fn is_fn_symbol(&self, sym: crate::ty::SymbolId) -> bool {
        matches!(self.res.symbol(sym).kind, SymbolKind::Fn)
    }

    fn is_builtin_fn_symbol(&self, sym: crate::ty::SymbolId) -> bool {
        matches!(self.res.symbol(sym).kind, SymbolKind::BuiltinFn(_))
    }

    fn is_poly_builtin_fn_symbol(&self, sym: crate::ty::SymbolId) -> bool {
        matches!(self.res.symbol(sym).kind, SymbolKind::PolyBuiltinFn(_))
    }

    fn lower_poly_call(
        &self,
        sym: crate::ty::SymbolId,
        args: &[ast::Expr],
    ) -> HirExprKind {
        let poly_name = match &self.res.symbol(sym).kind {
            SymbolKind::PolyBuiltinFn(n) => *n,
            _ => unreachable!(),
        };
        let lowered_args: Vec<HirExpr> = args.iter().map(|a| self.lower_expr(a)).collect();
        let arg_ty = lowered_args.first().map(|a| &a.ty);
        let dispatched = match (poly_name, arg_ty) {
            ("print", Some(Ty::Int(_))) => "print_i64",
            ("print", Some(Ty::Str)) => "print_str",
            _ => {
                return HirExprKind::Unsupported(format!(
                    "no dispatch for polymorphic builtin `{}` with that argument type",
                    poly_name
                ));
            }
        };
        HirExprKind::BuiltinCall {
            name: dispatched.to_string(),
            args: lowered_args,
        }
    }
}

fn lower_binop(op: ast::BinOp) -> HirBinOp {
    match op {
        ast::BinOp::Add => HirBinOp::Add,
        ast::BinOp::Sub => HirBinOp::Sub,
        ast::BinOp::Mul => HirBinOp::Mul,
        ast::BinOp::Div => HirBinOp::Div,
        ast::BinOp::Mod => HirBinOp::Mod,
        ast::BinOp::Eq => HirBinOp::Eq,
        ast::BinOp::Ne => HirBinOp::Ne,
        ast::BinOp::Lt => HirBinOp::Lt,
        ast::BinOp::Gt => HirBinOp::Gt,
        ast::BinOp::Le => HirBinOp::Le,
        ast::BinOp::Ge => HirBinOp::Ge,
        ast::BinOp::BitAnd => HirBinOp::BitAnd,
        ast::BinOp::BitOr => HirBinOp::BitOr,
        ast::BinOp::BitXor => HirBinOp::BitXor,
        ast::BinOp::Shl => HirBinOp::Shl,
        ast::BinOp::Shr => HirBinOp::Shr,
        ast::BinOp::And | ast::BinOp::Or => {
            unreachable!("&&/|| handled via logical_op")
        }
    }
}

fn logical_op(op: ast::BinOp) -> Option<LogicalOp> {
    match op {
        ast::BinOp::And => Some(LogicalOp::And),
        ast::BinOp::Or => Some(LogicalOp::Or),
        _ => None,
    }
}

fn lower_unop(op: ast::UnOp) -> HirUnOp {
    match op {
        ast::UnOp::Neg => HirUnOp::Neg,
        ast::UnOp::Not => HirUnOp::Not,
        ast::UnOp::BitNot => HirUnOp::BitNot,
    }
}
