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
                    .filter(|f| field_ty_is_arc(&f.ty, &struct_arc_fields))
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
        let trait_methods: std::collections::HashMap<SymbolId, Vec<String>> = self
            .res
            .trait_methods
            .iter()
            .map(|(t, ms)| {
                (*t, ms.iter().map(|m| m.name.name.clone()).collect())
            })
            .collect();
        // Build the flat method list per trait: the trait's own
        // methods first (decl order), then each supertrait in
        // declaration order, deduped first-wins by method name.
        // The visited set against the supertrait DAG is a HashSet
        // so each trait contributes at most once; declaration-order
        // BFS makes the slot ordering deterministic, which matters
        // because the box layout and the call-site offset both read
        // this list keyed by the call-site trait sym.
        let mut trait_methods_flat: std::collections::HashMap<
            SymbolId,
            Vec<(SymbolId, String)>,
        > = std::collections::HashMap::new();
        for &trait_sym in trait_methods.keys() {
            let mut flat: Vec<(SymbolId, String)> = Vec::new();
            let mut seen_names: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut visited: std::collections::HashSet<SymbolId> =
                std::collections::HashSet::new();
            let mut queue: std::collections::VecDeque<SymbolId> =
                std::collections::VecDeque::new();
            queue.push_back(trait_sym);
            visited.insert(trait_sym);
            while let Some(t) = queue.pop_front() {
                if let Some(methods) = trait_methods.get(&t) {
                    for name in methods {
                        if seen_names.insert(name.clone()) {
                            flat.push((t, name.clone()));
                        }
                    }
                }
                if let Some(supers) = self.res.trait_supertraits.get(&t) {
                    for &s in supers {
                        if visited.insert(s) {
                            queue.push_back(s);
                        }
                    }
                }
            }
            trait_methods_flat.insert(trait_sym, flat);
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
            array_tys: Vec::new(),
            trait_methods,
            trait_methods_flat,
            impl_assoc_bindings_ty: self.check.impl_assoc_bindings_ty.clone(),
            struct_generics: self.res.struct_generics.clone(),
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
        let inner = HirExpr {
            kind: self.lower_expr_kind(e),
            ty,
        };
        // A `dyn Trait` coercion the checker recorded at this span:
        // wrap the value in a `DynBox`.
        if let Some(&(struct_sym, trait_sym)) =
            self.check.dyn_coercions.get(&span)
        {
            return HirExpr {
                kind: HirExprKind::DynBox {
                    value: Box::new(inner),
                    struct_sym,
                    trait_sym,
                },
                ty: Ty::Dyn(trait_sym),
            };
        }
        inner
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
                    // A Local of `Ty::Fn` type — calling through a
                    // function-pointer-typed binding (e.g. a struct
                    // field accessed via `let f = m.f; f(x)`). Goes
                    // through IndirectCall.
                    SymbolKind::Local { .. } | SymbolKind::Param => {
                        let callee_hir = self.lower_expr(callee);
                        if matches!(callee_hir.ty, Ty::Fn { .. }) {
                            HirExprKind::IndirectCall {
                                callee: Box::new(callee_hir),
                                args: args.iter().map(|a| self.lower_expr(a)).collect(),
                            }
                        } else {
                            HirExprKind::Unsupported(
                                "call target other than a named function".into(),
                            )
                        }
                    }
                    _ => HirExprKind::Unsupported(
                        "call target other than a named function".into(),
                    ),
                },
                // Non-path callee — typically a parenthesized field
                // access like `(self.f)(x)` where `self.f: Ty::Fn`.
                // Lower the callee expression and emit IndirectCall.
                None => {
                    let callee_hir = self.lower_expr(callee);
                    if matches!(callee_hir.ty, Ty::Fn { .. }) {
                        HirExprKind::IndirectCall {
                            callee: Box::new(callee_hir),
                            args: args.iter().map(|a| self.lower_expr(a)).collect(),
                        }
                    } else {
                        HirExprKind::Unsupported(
                            "call target other than a named function".into(),
                        )
                    }
                }
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
            ast::Expr::Break(_) => HirExprKind::Break,
            ast::Expr::Continue(_) => HirExprKind::Unsupported("continue".into()),
            ast::Expr::MethodCall { receiver, method, args, .. } => {
                let receiver_hir = self.lower_expr(receiver);
                // A method call on a `dyn Trait` receiver dispatches
                // dynamically through the boxed method table.
                if let Ty::Dyn(trait_sym) = &receiver_hir.ty {
                    return HirExprKind::DynCall {
                        receiver: Box::new(receiver_hir.clone()),
                        trait_sym: *trait_sym,
                        method: method.name.clone(),
                        args: args.iter().map(|a| self.lower_expr(a)).collect(),
                    };
                }
                // `vec.iter()` — the builtin Vec → VecIter constructor.
                // Desugar to a struct literal so the rest of the
                // pipeline (monomorphization, ARC, codegen) sees an
                // ordinary `Ty::Struct(VecIterSym, [elem])` value.
                if matches!(&receiver_hir.ty, Ty::Vec(_)) && method.name == "iter" {
                    if let Some(desugar) = self.lower_vec_iter(receiver_hir.clone()) {
                        return desugar;
                    }
                }
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
        // Array iter — the existing counted-loop path. Fast, no
        // method dispatch.
        if let Ty::Array(elem, n) = iter_hir.ty.clone() {
            let elem_ty = *elem;
            let length = n;
            return HirExprKind::For {
                local,
                iter: Box::new(iter_hir),
                body: self.lower_block(body),
                elem_ty,
                length,
            };
        }
        // Iterator protocol — desugar `for x in iter` to
        //   { let __it = iter;
        //     while true {
        //         match __it.next() { Some(__x) => { let x = __x; body },
        //                             None     => break } } }
        // The match's `Some` arm is what binds the user's pattern.
        // The struct's `next` method is resolved through the existing
        // user-method path; the monomorphizer rewrites the MethodCall
        // into a Call once the receiver type is concrete.
        match &iter_hir.ty {
            Ty::Struct(struct_sym, _) => {
                if let Some(desugar) = self.lower_for_iterator(
                    Some(*struct_sym),
                    local,
                    iter_hir.clone(),
                    body,
                ) {
                    return desugar;
                }
            }
            // Bounded-generic body: `<T: Iterator>(x: T) { for v in x { .. } }`.
            // The item type is `Ty::Assoc(TypeVar(T), "Item")`, an
            // abstract projection the monomorphizer resolves once `T`
            // is concrete (session 051). No concrete struct yet, so
            // pass `None` for the impl-binding lookup.
            Ty::TypeVar(_) => {
                if let Some(desugar) = self.lower_for_iterator(
                    None,
                    local,
                    iter_hir.clone(),
                    body,
                ) {
                    return desugar;
                }
            }
            _ => {}
        }
        HirExprKind::Unsupported(
            "for-loop iterator must be an array, integer range, or a struct implementing `std::Iterator`"
                .into(),
        )
    }

    /// Build the `while-match` desugar for `for x in iter`.
    ///
    /// `struct_sym` is `Some(s)` when the iter's type is concretely
    /// `Ty::Struct(s, _)` — we resolve `Item` from the impl binding.
    /// It is `None` for a bounded-generic body (`<T: Iterator>(x: T)`)
    /// where the item type stays abstract as
    /// `Ty::Assoc(iter_hir.ty, "Item")` until monomorphization.
    /// Returns `None` (so the caller emits `Unsupported`) if any
    /// precondition isn't met.
    fn lower_for_iterator(
        &self,
        struct_sym: Option<SymbolId>,
        local: Option<SymbolId>,
        iter_hir: HirExpr,
        body: &ast::Block,
    ) -> Option<HirExprKind> {
        // For a concrete struct, verify it implements Iterator and
        // look up the impl's `type Item = ...` binding. For a generic
        // type-param, the checker already confirmed the bound, and
        // the item type is the abstract projection.
        let item_ty = if let Some(s) = struct_sym {
            let iter_trait_sym = self.find_iterator_sym()?;
            if !self
                .res
                .impls_for
                .get(&s)
                .map(|set| set.contains(&iter_trait_sym))
                .unwrap_or(false)
            {
                return None;
            }
            // The impl binding stored for `(s, "Item")` may reference
            // the impl-block's own generic param (e.g. `type Item =
            // T` inside `impl<T> Iterator for VecIter<T>` records
            // `Ty::TypeVar(T)`). When the call-site iter has concrete
            // type args, substitute them in so the desugar's match
            // pattern is fully concrete; otherwise codegen sees a
            // TypeVar leaked into the IR. Mirrors the checker's
            // `apply_subst_inner_with` struct-arg fix.
            let raw_item = self
                .check
                .impl_assoc_bindings_ty
                .get(&(s, "Item".to_string()))
                .cloned()?;
            let struct_args: Vec<Ty> = match &iter_hir.ty {
                Ty::Struct(_, args) => args.clone(),
                _ => Vec::new(),
            };
            self.subst_struct_typevars(s, &struct_args, &raw_item)
        } else {
            // Bounded-generic body — item is `T::Item`.
            Ty::Assoc(Box::new(iter_hir.ty.clone()), "Item".into())
        };
        // Look up the prelude's std::Option enum and its variant
        // discriminants. They were interned during prelude resolution.
        let option_sym = self.find_option_sym()?;
        let some_disc = self.enum_variant_disc(option_sym, "Some")?;
        let none_disc = self.enum_variant_disc(option_sym, "None")?;
        let option_item_ty = Ty::Enum(option_sym, vec![item_ty.clone()]);
        let iter_ty = iter_hir.ty.clone();

        // __it: synthetic local that holds the iterator state across
        // the loop. Its `Local` reads in the body type as `iter_ty`.
        let it_sym = self.fresh_sym();
        let let_it = HirStmt::Let(HirLet {
            sym: Some(it_sym),
            mutable: true,
            ty: iter_ty.clone(),
            init: Some(iter_hir),
        });

        // __it.next() — resolved as a method call on the concrete
        // struct. The monomorphizer rewrites it to a direct Call.
        let it_local = HirExpr {
            kind: HirExprKind::Local(it_sym),
            ty: iter_ty.clone(),
        };
        let next_call = HirExpr {
            kind: HirExprKind::MethodCall {
                receiver: Box::new(it_local),
                method: "next".into(),
                args: Vec::new(),
            },
            ty: option_item_ty.clone(),
        };

        // Some(__x) => { (if pat is `let x`, bind x); body; () }
        let x_sym = self.fresh_sym();
        let mut some_body_stmts: Vec<HirStmt> = Vec::new();
        if let Some(user_sym) = local {
            // Alias the user's pattern symbol to the freshly bound __x.
            some_body_stmts.push(HirStmt::Let(HirLet {
                sym: Some(user_sym),
                mutable: false,
                ty: item_ty.clone(),
                init: Some(HirExpr {
                    kind: HirExprKind::Local(x_sym),
                    ty: item_ty.clone(),
                }),
            }));
        }
        let body_hir = self.lower_block(body);
        some_body_stmts.extend(body_hir.stmts);
        // Inner block returns unit — the match-arm body's tail is the
        // body's tail, but a for-loop body is unit-typed by spec.
        let some_arm_body = HirExpr {
            kind: HirExprKind::Block(HirBlock {
                stmts: some_body_stmts,
                ty: Ty::Unit,
            }),
            ty: Ty::Unit,
        };
        let some_arm = HirMatchArm {
            patterns: vec![HirPattern::EnumPayload {
                discriminant: some_disc,
                bindings: vec![(item_ty.clone(), Some(x_sym))],
            }],
            guard: None,
            body: some_arm_body,
        };
        // None => break
        let none_arm = HirMatchArm {
            patterns: vec![HirPattern::EnumPayload {
                discriminant: none_disc,
                bindings: vec![],
            }],
            guard: None,
            body: HirExpr {
                kind: HirExprKind::Break,
                ty: Ty::Never,
            },
        };
        let match_expr = HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(next_call),
                arms: vec![some_arm, none_arm],
            },
            ty: Ty::Unit,
        };

        // while true { <match> }
        let while_expr = HirExpr {
            kind: HirExprKind::While {
                cond: Box::new(HirExpr {
                    kind: HirExprKind::Lit(HirLit::Bool(true)),
                    ty: Ty::Bool,
                }),
                body: HirBlock {
                    stmts: vec![HirStmt::Expr(match_expr, true)],
                    ty: Ty::Unit,
                },
            },
            ty: Ty::Unit,
        };

        Some(HirExprKind::Block(HirBlock {
            stmts: vec![let_it, HirStmt::Expr(while_expr, true)],
            ty: Ty::Unit,
        }))
    }

    /// Walk the resolver's symbol table for the prelude's
    /// `std::Iterator` trait sym. Same heuristic as `Checker`'s
    /// `find_iterator_sym` — first `Trait` named `Iterator`.
    fn find_iterator_sym(&self) -> Option<SymbolId> {
        for (idx, sym) in self.res.symbols.iter().enumerate() {
            if sym.name == "Iterator" && matches!(sym.kind, SymbolKind::Trait) {
                return Some(SymbolId(idx as u32));
            }
        }
        None
    }

    /// Walk the resolver's symbol table for the prelude's
    /// `std::Option` enum sym. The prelude is parsed first, so the
    /// first `Enum` named `Option` is the right one.
    fn find_option_sym(&self) -> Option<SymbolId> {
        for (idx, sym) in self.res.symbols.iter().enumerate() {
            if sym.name == "Option" && matches!(sym.kind, SymbolKind::Enum) {
                return Some(SymbolId(idx as u32));
            }
        }
        None
    }

    /// Substitute a struct's impl-side generic params using the
    /// struct's use-site type args. `subst_struct_typevars(VecIterSym,
    /// [i64], TypeVar(T_VecIter))` returns `i64`. Walks `Ty::Fn`,
    /// `Ty::Struct`, etc. recursively. For `Ty::Assoc`, looks up the
    /// projection through `impl_assoc_bindings_ty` so a binding
    /// stored as `Ty::Assoc(TypeVar(I), "Item")` (e.g. Filter's
    /// `type Item = I::Item`) resolves fully when `I` becomes a
    /// concrete struct with a known Item binding.
    fn subst_struct_typevars(
        &self,
        struct_sym: SymbolId,
        struct_args: &[Ty],
        ty: &Ty,
    ) -> Ty {
        let Some(generics) = self.res.struct_generics.get(&struct_sym) else {
            return ty.clone();
        };
        let mut subst: std::collections::HashMap<SymbolId, Ty> =
            std::collections::HashMap::new();
        for (gsym, arg) in generics.iter().zip(struct_args.iter()) {
            subst.insert(*gsym, arg.clone());
        }
        self.apply_subst_ty(ty, &subst)
    }

    /// Ty substitution used by the lowerer. Substitutes TypeVars
    /// against `subst` and resolves `Ty::Assoc` projections through
    /// the checker's `impl_assoc_bindings_ty` map — same two-layer
    /// approach as the monomorphizer's runtime `subst_ty` (session
    /// 056). Recurses through every type constructor.
    fn apply_subst_ty(
        &self,
        ty: &Ty,
        subst: &std::collections::HashMap<SymbolId, Ty>,
    ) -> Ty {
        match ty {
            Ty::TypeVar(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
            Ty::Array(elem, n) => {
                Ty::Array(Box::new(self.apply_subst_ty(elem, subst)), *n)
            }
            Ty::Vec(elem) => Ty::Vec(Box::new(self.apply_subst_ty(elem, subst))),
            Ty::Fn { params, ret } => Ty::Fn {
                params: params.iter().map(|t| self.apply_subst_ty(t, subst)).collect(),
                ret: Box::new(self.apply_subst_ty(ret, subst)),
            },
            Ty::Struct(s, args) => Ty::Struct(
                *s,
                args.iter().map(|t| self.apply_subst_ty(t, subst)).collect(),
            ),
            Ty::Enum(s, args) => Ty::Enum(
                *s,
                args.iter().map(|t| self.apply_subst_ty(t, subst)).collect(),
            ),
            Ty::Weak(inner) => Ty::Weak(Box::new(self.apply_subst_ty(inner, subst))),
            // Projection: substitute the base, then if it landed on
            // a concrete struct with a known impl binding, resolve
            // through that binding using a per-struct substitution
            // (impl's T -> call-site args). Mirrors the checker's
            // `apply_subst_inner_with` and the monomorphizer's
            // `subst_ty` Assoc arm.
            Ty::Assoc(base, name) => {
                let new_base = self.apply_subst_ty(base, subst);
                if let Ty::Struct(s, args) = &new_base {
                    if let Some(raw) = self
                        .check
                        .impl_assoc_bindings_ty
                        .get(&(*s, name.clone()))
                        .cloned()
                    {
                        let mut struct_subst: std::collections::HashMap<SymbolId, Ty> =
                            std::collections::HashMap::new();
                        if let Some(gens) = self.res.struct_generics.get(s) {
                            for (gsym, arg) in gens.iter().zip(args.iter()) {
                                struct_subst.insert(*gsym, arg.clone());
                            }
                        }
                        let after_struct = self.apply_subst_ty(&raw, &struct_subst);
                        return self.apply_subst_ty(&after_struct, subst);
                    }
                }
                Ty::Assoc(Box::new(new_base), name.clone())
            }
            _ => ty.clone(),
        }
    }

    /// Walk the resolver's symbol table for a prelude struct by
    /// name. Same first-wins heuristic as `find_iterator_sym` /
    /// `find_option_sym` — prelude symbols are interned first.
    fn find_struct_sym(&self, name: &str) -> Option<SymbolId> {
        for (idx, sym) in self.res.symbols.iter().enumerate() {
            if sym.name == name && matches!(sym.kind, SymbolKind::Struct) {
                return Some(SymbolId(idx as u32));
            }
        }
        None
    }

    /// Build `std::VecIter<elem> { vec: <receiver>, idx: 0 }` from a
    /// `Vec<elem>` receiver. The checker types `vec.iter()` as the
    /// resulting struct type; the lowerer is responsible for the
    /// concrete construction so the rest of the pipeline sees an
    /// ordinary struct literal.
    fn lower_vec_iter(&self, receiver_hir: HirExpr) -> Option<HirExprKind> {
        let elem_ty = match &receiver_hir.ty {
            Ty::Vec(e) => (**e).clone(),
            _ => return None,
        };
        let vec_iter_sym = self.find_struct_sym("VecIter")?;
        let layout = self.check.struct_layouts.get(&vec_iter_sym)?;
        // Locate the two fields by name so the result tolerates a
        // future re-ordering in std.rn.
        let vec_offset = layout
            .fields
            .iter()
            .find(|f| f.name == "vec")
            .map(|f| f.offset)?;
        let idx_offset = layout
            .fields
            .iter()
            .find(|f| f.name == "idx")
            .map(|f| f.offset)?;
        let vec_ty_hir = Ty::Vec(Box::new(elem_ty));
        let idx_lit = HirExpr {
            kind: HirExprKind::Lit(HirLit::Int(0, IntTy::I64)),
            ty: Ty::Int(IntTy::I64),
        };
        let mut fields_in_decl_order: Vec<(u32, HirExpr)> = Vec::with_capacity(2);
        for decl in &layout.fields {
            match decl.name.as_str() {
                "vec" => fields_in_decl_order.push((vec_offset, HirExpr {
                    kind: receiver_hir.kind.clone(),
                    ty: vec_ty_hir.clone(),
                })),
                "idx" => fields_in_decl_order.push((idx_offset, idx_lit.clone())),
                _ => return None,
            }
        }
        Some(HirExprKind::StructLit {
            sym: vec_iter_sym,
            fields: fields_in_decl_order,
            size: layout.size,
        })
    }

    /// Discriminant of a variant on an enum, looked up by name.
    fn enum_variant_disc(
        &self,
        enum_sym: SymbolId,
        variant_name: &str,
    ) -> Option<u32> {
        let &variant_sym = self.res.enum_variants.get(&enum_sym)?.get(variant_name)?;
        match self.res.symbol(variant_sym).kind {
            SymbolKind::EnumVariant { discriminant, .. } => Some(discriminant),
            _ => None,
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

/// Whether a struct field type takes part in the per-struct ARC
/// release walk: a `Vec` / `Str`, an ARC-bearing struct, any array
/// (a heap array is a refcounted block regardless of element type),
/// or a `dyn` trait object (a refcounted box). Enums and `Weak`
/// fields are still not walked.
fn field_ty_is_arc(
    ty: &Ty,
    known: &std::collections::HashMap<SymbolId, Vec<(u32, Ty)>>,
) -> bool {
    match ty {
        Ty::Vec(_) | Ty::Str => true,
        Ty::Struct(inner, _) => known.contains_key(inner),
        Ty::Array(_, _) => true,
        Ty::Dyn(_) => true,
        _ => false,
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
