//! AST → HIR lowering.
//!
//! Walks the AST together with `Resolutions` (for symbol identity) and
//! `CheckResults` (for type tags), producing an HIR that's ready for
//! codegen.

use std::cell::Cell;

use crate::ast;
use crate::checker::CheckResults;
use crate::hir::*;
use crate::resolver::{Resolutions, SymbolKind};
use crate::ty::{IntTy, SymbolId, Ty};

pub struct Lowerer<'a> {
    res: &'a Resolutions,
    check: &'a CheckResults,
    /// Next fresh `SymbolId` for lowering-synthesized bindings (the
    /// `?` desugar's match-arm bindings). Starts past every resolver
    /// symbol so it can't collide with one.
    next_sym: Cell<u32>,
}

impl<'a> Lowerer<'a> {
    pub fn new(res: &'a Resolutions, check: &'a CheckResults) -> Self {
        Self {
            res,
            check,
            next_sym: Cell::new(res.symbols.len() as u32),
        }
    }

    /// Allocate a fresh `SymbolId` for a synthesized binding.
    fn fresh_sym(&self) -> SymbolId {
        let n = self.next_sym.get();
        self.next_sym.set(n + 1);
        SymbolId(n)
    }

    pub fn lower_module(&self, m: &ast::Module) -> HirModule {
        let mut items = Vec::new();
        self.lower_items(&m.items, &mut items);
        // Compute the ARC-field map for every struct that contains one or
        // more ARC-managed fields. A struct is considered ARC-managed
        // transitively if it contains a Vec, Str, or another ARC struct.
        let mut struct_arc_fields: std::collections::HashMap<
            crate::ty::SymbolId,
            Vec<(u32, Ty)>,
        > = std::collections::HashMap::new();
        // Two-pass to handle cross-references between structs; fixed-point
        // (small N, the user's struct count). Each iteration adds entries
        // for newly-discovered ARC structs.
        loop {
            let mut changed = false;
            for (sym, layout) in &self.check.struct_layouts {
                if struct_arc_fields.contains_key(sym) {
                    continue;
                }
                let arc_fields: Vec<(u32, Ty)> = layout
                    .fields
                    .iter()
                    .filter(|f| match &f.ty {
                        Ty::Vec(_) | Ty::Str => true,
                        Ty::Struct(inner, _) => struct_arc_fields.contains_key(inner),
                        _ => false,
                    })
                    .map(|f| (f.offset, f.ty.clone()))
                    .collect();
                if !arc_fields.is_empty() {
                    struct_arc_fields.insert(*sym, arc_fields);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let enum_has_payload = self.res.enum_has_payload.clone();
        let struct_sizes: std::collections::HashMap<
            crate::ty::SymbolId,
            u32,
        > = self
            .check
            .struct_layouts
            .iter()
            .map(|(s, l)| (*s, l.size))
            .collect();
        // Build the per-enum payload-type lists. Variant order = the
        // resolver's declaration order; that matches discriminants
        // since the resolver assigns 0, 1, ... in source order.
        let mut enum_payload_tys: std::collections::HashMap<
            crate::ty::SymbolId,
            Vec<Vec<Ty>>,
        > = std::collections::HashMap::new();
        for &enum_sym in &enum_has_payload {
            let Some(variant_map) = self.res.enum_variants.get(&enum_sym) else {
                continue;
            };
            // Reconstruct discriminant ordering. The variant_sym's
            // SymbolKind::EnumVariant carries the discriminant.
            let mut ordered: Vec<(u32, SymbolId)> = variant_map
                .values()
                .map(|sid| {
                    let SymbolKind::EnumVariant { discriminant, .. } =
                        self.res.symbol(*sid).kind
                    else {
                        unreachable!()
                    };
                    (discriminant, *sid)
                })
                .collect();
            ordered.sort_by_key(|(d, _)| *d);
            let mut per_variant: Vec<Vec<Ty>> = Vec::with_capacity(ordered.len());
            for (_, vsym) in ordered {
                let payload_ast = self
                    .res
                    .enum_variant_payloads
                    .get(&vsym)
                    .cloned()
                    .unwrap_or_default();
                let payload_tys: Vec<Ty> = payload_ast
                    .iter()
                    .map(|t| {
                        self.check
                            .type_resolutions
                            .get(&t.span())
                            .cloned()
                            .unwrap_or(Ty::Error)
                    })
                    .collect();
                per_variant.push(payload_tys);
            }
            enum_payload_tys.insert(enum_sym, per_variant);
        }
        HirModule {
            items,
            struct_arc_fields,
            struct_sizes,
            enum_has_payload,
            enum_payload_tys,
            impl_methods: self.res.impl_methods.clone(),
            // Filled by the monomorphizer once every type is concrete.
            vec_arc_elem_tys: Vec::new(),
        }
    }

    /// Flatten items into HIR functions, recursing into modules.
    /// Modules carry no runtime weight — their functions are emitted
    /// flat (the resolver already mangled their codegen names).
    fn lower_items(&self, items: &[ast::Item], out: &mut Vec<HirItem>) {
        for it in items {
            match it {
                ast::Item::Fn(f) => out.push(HirItem::Fn(self.lower_fn(f))),
                ast::Item::Impl(i) => {
                    for method in &i.methods {
                        out.push(HirItem::Fn(self.lower_fn(method)));
                    }
                }
                ast::Item::Mod(md) => self.lower_items(&md.items, out),
                // Const, Struct, Enum, Trait, Use carry no codegen.
                _ => {}
            }
        }
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
        let name = self.res.symbol(sym).name.clone();
        // Collect the function's generic-param symbols so the
        // monomorphizer can detect "is this fn generic?" and identify
        // which TypeVar(s) to substitute.
        let generics: Vec<SymbolId> = f
            .generics
            .iter()
            .filter_map(|g| self.res.decl_to_sym.get(&g.name.span).copied())
            .collect();
        HirFn { sym, name, generics, params, ret_ty, body }
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
            ast::Pattern::Literal { .. }
            | ast::Pattern::Path { .. }
            | ast::Pattern::Range { .. }
            | ast::Pattern::TupleVariant { .. }
            | ast::Pattern::NamedVariant { .. }
            | ast::Pattern::Or { .. } => {
                // `let` doesn't currently use any of these patterns;
                // resolver / checker either accept them as no-ops or
                // reject them. No binding here.
                (None, l.mutable)
            }
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
            ast::Expr::Assign { lhs, rhs, .. } => {
                // Field assignment: receiver.field = rhs
                if let ast::Expr::Field { receiver, name, .. } = lhs.as_ref() {
                    let recv = self.lower_expr(receiver);
                    let (offset, field_ty) = match &recv.ty {
                        Ty::Struct(sym_id, args) => match self
                            .check
                            .struct_layouts
                            .get(sym_id)
                            .and_then(|l| l.field(&name.name))
                        {
                            Some(f) => {
                                let subst = build_struct_subst(self.res, *sym_id, args);
                                (f.offset, apply_subst(&f.ty, &subst))
                            }
                            None => {
                                return HirExprKind::Unsupported(format!(
                                    "no field `{}` on struct",
                                    name.name
                                ));
                            }
                        },
                        _ => {
                            return HirExprKind::Unsupported(
                                "field assignment on non-struct".into(),
                            );
                        }
                    };
                    return HirExprKind::FieldAssign {
                        receiver: Box::new(recv),
                        offset,
                        field_ty,
                        rhs: Box::new(self.lower_expr(rhs)),
                    };
                }
                // Plain local assignment.
                match self.path_symbol(lhs) {
                    Some(sym) => HirExprKind::Assign {
                        lhs: sym,
                        rhs: Box::new(self.lower_expr(rhs)),
                    },
                    None => HirExprKind::Unsupported(
                        "assignment target other than a local binding or field".into(),
                    ),
                }
            }
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
                    // Use the BuiltinFn's runtime-helper name, not the
                    // symbol's interned key — a `std::`-namespaced alias
                    // (`std::vec_new`) still calls the `vec_new` helper.
                    name: match &self.res.symbol(sym).kind {
                        SymbolKind::BuiltinFn(bf) => bf.name.to_string(),
                        _ => self.res.symbol(sym).name.clone(),
                    },
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                },
                Some(sym) if self.is_poly_builtin_fn_symbol(sym) => self.lower_poly_call(sym, args),
                Some(sym) => match self.res.symbol(sym).kind {
                    SymbolKind::EnumVariant { enum_sym, discriminant } => {
                        HirExprKind::EnumPayloadCtor {
                            enum_sym,
                            discriminant,
                            payloads: args.iter().map(|a| self.lower_expr(a)).collect(),
                        }
                    }
                    _ => HirExprKind::Unsupported(
                        "call target other than a named function".into(),
                    ),
                },
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
            ast::Expr::MethodCall { receiver, method, args, .. } => {
                let receiver_hir = self.lower_expr(receiver);
                // User-defined methods on structs (via `impl`) are lowered
                // to a regular Call with the receiver as the first
                // argument. Builtin methods go through HirExprKind::MethodCall.
                if let Ty::Struct(struct_sym, _) = &receiver_hir.ty {
                    let key = (*struct_sym, method.name.clone());
                    if let Some(&method_sym) = self.res.impl_methods.get(&key) {
                        let mut call_args = Vec::with_capacity(args.len() + 1);
                        call_args.push(receiver_hir);
                        for a in args {
                            call_args.push(self.lower_expr(a));
                        }
                        return HirExprKind::Call {
                            callee: method_sym,
                            args: call_args,
                        };
                    }
                }
                HirExprKind::MethodCall {
                    receiver: Box::new(receiver_hir),
                    method: method.name.clone(),
                    args: args.iter().map(|a| self.lower_expr(a)).collect(),
                }
            }
            ast::Expr::StructLit { path, fields, .. } => self.lower_struct_lit(path, fields),
            ast::Expr::Field { receiver, name, .. } => self.lower_field_access(receiver, name),
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
            ast::Expr::Try { expr, .. } => self.lower_try(expr),
            ast::Expr::Cast { expr, .. } => HirExprKind::Cast {
                expr: Box::new(self.lower_expr(expr)),
            },
            ast::Expr::Array { elems, .. } => {
                let lowered: Vec<HirExpr> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let elem_ty = lowered
                    .first()
                    .map(|e| e.ty.clone())
                    .unwrap_or(Ty::Error);
                HirExprKind::Array { elems: lowered, elem_ty }
            }
            ast::Expr::For { pat, iter, body, .. } => self.lower_for(pat, iter, body),
            ast::Expr::Match { scrutinee, arms, .. } => self.lower_match(scrutinee, arms),
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
            ast::Lit::Char(c) => HirLit::Char(*c),
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
            SymbolKind::EnumVariant { discriminant, .. } => {
                HirExprKind::EnumVariant { discriminant }
            }
            _ => HirExprKind::Unsupported("type name used as value".into()),
        }
    }

    fn lower_struct_lit(&self, path: &ast::Path, fields: &[ast::FieldInit]) -> HirExprKind {
        let Some(&sym_id) = self.res.path_to_sym.get(&path.span) else {
            return HirExprKind::Unsupported("unresolved struct in literal".into());
        };
        // Named-field enum variant construction: `Variant { name: val }`
        // resolves to an EnumVariant symbol. Reorder the user-provided
        // fields into declaration order and emit EnumPayloadCtor.
        if let SymbolKind::EnumVariant { enum_sym, discriminant } =
            self.res.symbol(sym_id).kind
        {
            let decl_names = self
                .res
                .enum_variant_field_names
                .get(&sym_id)
                .cloned()
                .unwrap_or_default();
            let mut payloads: Vec<HirExpr> = Vec::with_capacity(decl_names.len());
            for decl_name in &decl_names {
                if let Some(init) = fields.iter().find(|f| f.name.name == *decl_name) {
                    payloads.push(self.lower_expr(&init.value));
                } else {
                    payloads.push(HirExpr {
                        kind: HirExprKind::Unsupported(format!(
                            "missing field `{}` (caught by checker)",
                            decl_name
                        )),
                        ty: Ty::Error,
                    });
                }
            }
            return HirExprKind::EnumPayloadCtor {
                enum_sym,
                discriminant,
                payloads,
            };
        }
        let Some(layout) = self.check.struct_layouts.get(&sym_id) else {
            return HirExprKind::Unsupported("struct without a layout".into());
        };
        // Reorder user-provided fields into declaration order. Any missing
        // fields are already flagged by the checker; we lower an Unsupported
        // sentinel for them so codegen catches the discrepancy.
        let mut lowered_fields: Vec<(u32, HirExpr)> = Vec::with_capacity(layout.fields.len());
        for decl_field in &layout.fields {
            let Some(init) = fields.iter().find(|f| f.name.name == decl_field.name) else {
                lowered_fields.push((
                    decl_field.offset,
                    HirExpr {
                        kind: HirExprKind::Unsupported(format!(
                            "missing field `{}` (caught by checker)",
                            decl_field.name
                        )),
                        ty: decl_field.ty.clone(),
                    },
                ));
                continue;
            };
            lowered_fields.push((decl_field.offset, self.lower_expr(&init.value)));
        }
        HirExprKind::StructLit {
            sym: sym_id,
            fields: lowered_fields,
            size: layout.size,
        }
    }

    fn lower_field_access(&self, receiver: &ast::Expr, name: &ast::Ident) -> HirExprKind {
        let recv_hir = self.lower_expr(receiver);
        let Ty::Struct(sym_id, args) = &recv_hir.ty else {
            return HirExprKind::Unsupported("field access on non-struct".into());
        };
        let Some(layout) = self.check.struct_layouts.get(sym_id) else {
            return HirExprKind::Unsupported("struct without a layout".into());
        };
        let Some(field) = layout.field(&name.name) else {
            return HirExprKind::Unsupported(format!("no field `{}`", name.name));
        };
        // Substitute the field's declared type using the receiver's
        // generic args so the FieldAccess HIR carries the concrete
        // type at this use site.
        let subst = build_struct_subst(self.res, *sym_id, args);
        HirExprKind::FieldAccess {
            receiver: Box::new(recv_hir),
            offset: field.offset,
            field_ty: apply_subst(&field.ty, &subst),
        }
    }

    /// Desugar `expr?` into a match:
    ///   `match expr { Result::Ok(v) => v,`
    ///   `             Result::Err(e) => return Result::Err(e) }`
    /// The checker has already verified `expr` is a `Result` and the
    /// enclosing function returns a `Result` with a matching error.
    fn lower_try(&self, inner: &ast::Expr) -> HirExprKind {
        let scrutinee = self.lower_expr(inner);
        let (rsym, ok_ty, err_ty) = match &scrutinee.ty {
            Ty::Enum(s, args) if args.len() == 2 => {
                (*s, args[0].clone(), args[1].clone())
            }
            _ => return HirExprKind::Unsupported("`?` on a non-Result".into()),
        };
        // Read the Ok / Err discriminants off the enum rather than
        // assuming declaration order.
        let disc = |name: &str| -> Option<u32> {
            self.res.enum_variants.get(&rsym)?.get(name).and_then(|&vs| {
                match self.res.symbol(vs).kind {
                    SymbolKind::EnumVariant { discriminant, .. } => {
                        Some(discriminant)
                    }
                    _ => None,
                }
            })
        };
        let (Some(ok_disc), Some(err_disc)) = (disc("Ok"), disc("Err"))
        else {
            return HirExprKind::Unsupported(
                "`?` target is not Result-shaped".into(),
            );
        };
        let ok_bind = self.fresh_sym();
        let err_bind = self.fresh_sym();
        // `Ok(v) => v`
        let ok_arm = HirMatchArm {
            patterns: vec![HirPattern::EnumPayload {
                discriminant: ok_disc,
                bindings: vec![(ok_ty.clone(), Some(ok_bind))],
            }],
            guard: None,
            body: HirExpr {
                kind: HirExprKind::Local(ok_bind),
                ty: ok_ty.clone(),
            },
        };
        // `Err(e) => return Err(e)`
        let err_value = HirExpr {
            kind: HirExprKind::EnumPayloadCtor {
                enum_sym: rsym,
                discriminant: err_disc,
                payloads: vec![HirExpr {
                    kind: HirExprKind::Local(err_bind),
                    ty: err_ty.clone(),
                }],
            },
            ty: scrutinee.ty.clone(),
        };
        let err_arm = HirMatchArm {
            patterns: vec![HirPattern::EnumPayload {
                discriminant: err_disc,
                bindings: vec![(err_ty.clone(), Some(err_bind))],
            }],
            guard: None,
            body: HirExpr {
                kind: HirExprKind::Return(Some(Box::new(err_value))),
                ty: Ty::Never,
            },
        };
        HirExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![ok_arm, err_arm],
        }
    }

    fn lower_match(
        &self,
        scrutinee: &ast::Expr,
        arms: &[ast::MatchArm],
    ) -> HirExprKind {
        let scrutinee_h = self.lower_expr(scrutinee);
        // For generic enum scrutinees, build a per-enum subst so
        // payload bindings can resolve TypeVar to the concrete arg.
        let scrutinee_subst: std::collections::HashMap<SymbolId, Ty> =
            if let Ty::Enum(enum_sym, args) = &scrutinee_h.ty {
                self.res
                    .enum_generics
                    .get(enum_sym)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .zip(args.iter())
                    .map(|(g, t)| (*g, t.clone()))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };
        let mut hir_arms: Vec<HirMatchArm> = Vec::with_capacity(arms.len());
        for arm in arms {
            let mut patterns: Vec<HirPattern> = Vec::new();
            if let Err(msg) =
                self.collect_arm_patterns(&arm.pat, &mut patterns, &scrutinee_subst)
            {
                return HirExprKind::Unsupported(msg);
            }
            let guard = arm.guard.as_ref().map(|g| self.lower_expr(g));
            let body = self.lower_expr(&arm.body);
            hir_arms.push(HirMatchArm { patterns, guard, body });
        }
        HirExprKind::Match {
            scrutinee: Box::new(scrutinee_h),
            arms: hir_arms,
        }
    }

    fn collect_arm_patterns(
        &self,
        pat: &ast::Pattern,
        out: &mut Vec<HirPattern>,
        subst: &std::collections::HashMap<SymbolId, Ty>,
    ) -> Result<(), String> {
        match pat {
            ast::Pattern::Wildcard(_) => out.push(HirPattern::Wildcard),
            ast::Pattern::Ident { name, .. } => {
                let Some(&sid) = self.res.decl_to_sym.get(&name.span) else {
                    return Err("match ident pattern lost its binding".into());
                };
                out.push(HirPattern::Bind(sid));
            }
            ast::Pattern::Literal { lit, .. } => match lit {
                ast::Lit::Int(v) => out.push(HirPattern::IntLit(*v)),
                ast::Lit::Bool(b) => out.push(HirPattern::BoolLit(*b)),
                ast::Lit::Str(s) => out.push(HirPattern::StrLit(s.clone())),
                // Char patterns reuse IntLit — the scrutinee's
                // cranelift_type is I32 for `char`, so iconst+icmp
                // with the codepoint as i64 narrows correctly.
                ast::Lit::Char(c) => out.push(HirPattern::IntLit(*c as i64)),
                _ => return Err("match on this literal kind".into()),
            },
            ast::Pattern::Path { path, .. } => {
                let Some(&sid) = self.res.path_to_sym.get(&path.span) else {
                    return Err("match path pattern didn't resolve".into());
                };
                match self.res.symbol(sid).kind {
                    SymbolKind::EnumVariant { discriminant, .. } => {
                        out.push(HirPattern::EnumVariant { discriminant });
                    }
                    _ => return Err("match path didn't resolve to an enum variant".into()),
                }
            }
            ast::Pattern::Range { lo, hi, inclusive, .. } => {
                let lo_v = lit_to_int_bound(lo)
                    .ok_or_else(|| "range pattern bound must be int or char".to_string())?;
                let hi_v = lit_to_int_bound(hi)
                    .ok_or_else(|| "range pattern bound must be int or char".to_string())?;
                out.push(HirPattern::IntRange {
                    lo: lo_v,
                    hi: hi_v,
                    inclusive: *inclusive,
                });
            }
            ast::Pattern::NamedVariant { path, fields, .. } => {
                let Some(&sid) = self.res.path_to_sym.get(&path.span) else {
                    return Err("named-variant pattern didn't resolve".into());
                };
                let SymbolKind::EnumVariant { discriminant, .. } =
                    self.res.symbol(sid).kind
                else {
                    return Err(
                        "named-variant pattern path is not an enum variant".into(),
                    );
                };
                let decl_names = self
                    .res
                    .enum_variant_field_names
                    .get(&sid)
                    .cloned()
                    .unwrap_or_default();
                let payload_asts = self
                    .res
                    .enum_variant_payloads
                    .get(&sid)
                    .cloned()
                    .unwrap_or_default();
                let mut bindings: Vec<(Ty, Option<SymbolId>)> =
                    Vec::with_capacity(decl_names.len());
                for (i, decl_name) in decl_names.iter().enumerate() {
                    let raw_ty = self
                        .check
                        .type_resolutions
                        .get(&payload_asts[i].span())
                        .cloned()
                        .unwrap_or(Ty::Error);
                    let payload_ty = apply_subst(&raw_ty, subst);
                    let binding = match fields.iter().find(|(n, _)| &n.name == decl_name) {
                        Some((_, ast::Pattern::Wildcard(_))) => None,
                        Some((_, ast::Pattern::Ident { name, .. })) => {
                            self.res.decl_to_sym.get(&name.span).copied()
                        }
                        Some(_) => {
                            return Err(
                                "named-variant payload must be an identifier or `_`"
                                    .into(),
                            );
                        }
                        None => None,
                    };
                    bindings.push((payload_ty, binding));
                }
                out.push(HirPattern::EnumPayload { discriminant, bindings });
            }
            ast::Pattern::TupleVariant { path, fields, .. } => {
                let Some(&sid) = self.res.path_to_sym.get(&path.span) else {
                    return Err("tuple-variant pattern didn't resolve".into());
                };
                let SymbolKind::EnumVariant { discriminant, .. } =
                    self.res.symbol(sid).kind
                else {
                    return Err(
                        "tuple-variant pattern path is not an enum variant".into(),
                    );
                };
                let payload_asts = self
                    .res
                    .enum_variant_payloads
                    .get(&sid)
                    .cloned()
                    .unwrap_or_default();
                if fields.len() != payload_asts.len() {
                    return Err(format!(
                        "variant takes {} payload{}, found {}",
                        payload_asts.len(),
                        if payload_asts.len() == 1 { "" } else { "s" },
                        fields.len()
                    ));
                }
                let mut bindings: Vec<(Ty, Option<SymbolId>)> =
                    Vec::with_capacity(fields.len());
                for (field, payload_ast) in fields.iter().zip(&payload_asts) {
                    let raw_ty = self
                        .check
                        .type_resolutions
                        .get(&payload_ast.span())
                        .cloned()
                        .unwrap_or(Ty::Error);
                    // Substitute the scrutinee enum's generic args
                    // so a `Some(x)` arm on `Option<i64>` binds x:i64.
                    let payload_ty = apply_subst(&raw_ty, subst);
                    let binding = match field {
                        ast::Pattern::Wildcard(_) => None,
                        ast::Pattern::Ident { name, .. } => {
                            self.res.decl_to_sym.get(&name.span).copied()
                        }
                        _ => {
                            return Err(
                                "tuple-variant payload must be an identifier or `_`"
                                    .into(),
                            );
                        }
                    };
                    bindings.push((payload_ty, binding));
                }
                out.push(HirPattern::EnumPayload { discriminant, bindings });
            }
            ast::Pattern::Or { patterns, .. } => {
                for sub in patterns {
                    self.collect_arm_patterns(sub, out, subst)?;
                }
            }
        }
        Ok(())
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
            ast::Pattern::Literal { .. }
            | ast::Pattern::Path { .. }
            | ast::Pattern::Range { .. }
            | ast::Pattern::TupleVariant { .. }
            | ast::Pattern::NamedVariant { .. }
            | ast::Pattern::Or { .. } => {
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
            ("weak", Some(Ty::Vec(_))) => "weak_downgrade_vec",
            ("upgrade_or", Some(Ty::Weak(inner))) if matches!(**inner, Ty::Vec(_)) => {
                "weak_upgrade_or_vec"
            }
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

fn build_struct_subst(
    res: &Resolutions,
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

fn lit_to_int_bound(lit: &ast::Lit) -> Option<i64> {
    match lit {
        ast::Lit::Int(v) => Some(*v),
        ast::Lit::Char(c) => Some(*c as i64),
        _ => None,
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
