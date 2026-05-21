//! Generics step 2 — monomorphization.
//!
//! Each call to a generic function gets specialized for the concrete
//! types inferred from the value arguments. The pass works after
//! `Lowerer::lower_module` produces a HirModule and before codegen
//! sees it. The output module has all `Ty::TypeVar` resolved away and
//! one HirFn per (generic, type-args) combination, named with a
//! mangle suffix (`id$$i64`, `pair$$i64$$str`).
//!
//! Inference is positional and one-shot: walk the generic's HirParam
//! types alongside the call's argument types; whenever a `TypeVar(t)`
//! appears on the param side, bind `t` to the arg's concrete type.
//! Conflicting bindings for the same `t` produce an error. Today
//! there's no bidirectional inference or trait resolution.
//!
//! Recursive instantiation: a specialized body may itself call other
//! generic fns; those calls are queued and processed until the
//! worklist drains.
//!
//! Constraints in v0.x:
//! - Only functions are monomorphized. Generic structs/enums are out
//!   of scope this session.
//! - Type arguments at the *call site* are inferred only — no
//!   turbofish (`f::<T>()`).
//! - No higher-kinded types, traits, or generic constraints.

use std::collections::HashMap;

use crate::hir::*;
use crate::ty::{SymbolId, Ty};

pub fn monomorphize_module(module: &mut HirModule) {
    let mut state = MonoState::new(module);
    state.run();
    state.finish(module);
}

struct MonoState {
    /// Generic functions, keyed by their declared SymbolId.
    generics: HashMap<SymbolId, HirFn>,
    /// Concrete (non-generic) functions kept in the output module.
    concrete: Vec<HirFn>,
    /// Already-instantiated (generic_sym, type_args) → specialized sym.
    cache: HashMap<(SymbolId, Vec<Ty>), SymbolId>,
    /// Pending instantiation requests; processed FIFO until drained.
    worklist: Vec<(SymbolId, Vec<Ty>)>,
    /// Counter for fresh SymbolIds used by specialized functions.
    /// Starts above any sym present in the input module.
    next_sym: u32,
}

impl MonoState {
    fn new(module: &HirModule) -> Self {
        // Find the highest SymbolId mentioned anywhere in the module so
        // we can allocate fresh ones beyond it.
        let mut max_sym: u32 = 0;
        for item in &module.items {
            let HirItem::Fn(f) = item;
            max_sym = max_sym.max(f.sym.0);
            for p in &f.params {
                max_sym = max_sym.max(p.sym.0);
            }
            walk_block_collect_syms(&f.body, &mut max_sym);
        }
        Self {
            generics: HashMap::new(),
            concrete: Vec::new(),
            cache: HashMap::new(),
            worklist: Vec::new(),
            next_sym: max_sym + 1,
        }
    }

    fn run(&mut self) {
        // (Filled in `finish` — we split the input module into generics
        // vs concrete there. This shim runs the worklist after that.)
    }

    fn finish(mut self, module: &mut HirModule) {
        // Split items into generics and concrete.
        let items = std::mem::take(&mut module.items);
        for item in items {
            let HirItem::Fn(f) = item;
            if !f.generics.is_empty() {
                self.generics.insert(f.sym, f);
            } else {
                self.concrete.push(f);
            }
        }
        // Walk every concrete function's body for calls to generics.
        let concrete_clone = self.concrete.clone();
        for f in &concrete_clone {
            self.collect_requests_in_block(&f.body);
        }
        // Drain the worklist, producing specialized fns and queueing
        // any further requests their bodies surface.
        while let Some((sym, args)) = self.worklist.pop() {
            if self.cache.contains_key(&(sym, args.clone())) {
                continue;
            }
            let Some(generic) = self.generics.get(&sym).cloned() else {
                continue; // unknown — leave the original sym; codegen errors
            };
            let subst: HashMap<SymbolId, Ty> = generic
                .generics
                .iter()
                .copied()
                .zip(args.iter().cloned())
                .collect();
            let specialized_sym = SymbolId(self.next_sym);
            self.next_sym += 1;
            let specialized_name = mangle(&generic.name, &args);
            let mut specialized = subst_fn(&generic, &subst);
            specialized.sym = specialized_sym;
            specialized.name = specialized_name;
            specialized.generics = Vec::new();
            self.cache.insert((sym, args), specialized_sym);
            // Surface further requests from the specialized body.
            self.collect_requests_in_block(&specialized.body);
            self.concrete.push(specialized);
        }
        // Rewrite call sites in every concrete fn to point at the
        // specialized symbol.
        for f in &mut self.concrete {
            rewrite_calls(&mut f.body, &self.cache, &self.generics);
        }
        // Final module is just the concrete (originals + specializations).
        module.items = self
            .concrete
            .into_iter()
            .map(HirItem::Fn)
            .collect();
    }

    fn collect_requests_in_block(&mut self, b: &HirBlock) {
        for s in &b.stmts {
            match s {
                HirStmt::Let(l) => {
                    if let Some(init) = &l.init {
                        self.collect_requests_in_expr(init);
                    }
                }
                HirStmt::Expr(e, _) => self.collect_requests_in_expr(e),
            }
        }
    }

    fn collect_requests_in_expr(&mut self, e: &HirExpr) {
        match &e.kind {
            HirExprKind::Call { callee, args } => {
                if let Some(generic) = self.generics.get(callee).cloned() {
                    if let Some(type_args) = infer_type_args(&generic, args) {
                        if !self.cache.contains_key(&(*callee, type_args.clone())) {
                            self.worklist.push((*callee, type_args));
                        }
                    }
                }
                for a in args {
                    self.collect_requests_in_expr(a);
                }
            }
            // Recurse into sub-exprs.
            HirExprKind::Unary { expr, .. } => self.collect_requests_in_expr(expr),
            HirExprKind::Cast { expr } => self.collect_requests_in_expr(expr),
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.collect_requests_in_expr(lhs);
                self.collect_requests_in_expr(rhs);
            }
            HirExprKind::Logical { lhs, rhs, .. } => {
                self.collect_requests_in_expr(lhs);
                self.collect_requests_in_expr(rhs);
            }
            HirExprKind::Assign { rhs, .. } => self.collect_requests_in_expr(rhs),
            HirExprKind::AssignOp { rhs, .. } => self.collect_requests_in_expr(rhs),
            HirExprKind::BuiltinCall { args, .. } => {
                for a in args {
                    self.collect_requests_in_expr(a);
                }
            }
            HirExprKind::MethodCall { receiver, args, .. } => {
                self.collect_requests_in_expr(receiver);
                for a in args {
                    self.collect_requests_in_expr(a);
                }
            }
            HirExprKind::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.collect_requests_in_expr(v);
                }
            }
            HirExprKind::EnumPayloadCtor { payloads, .. } => {
                for p in payloads {
                    self.collect_requests_in_expr(p);
                }
            }
            HirExprKind::FieldAccess { receiver, .. } => {
                self.collect_requests_in_expr(receiver)
            }
            HirExprKind::FieldAssign { receiver, rhs, .. } => {
                self.collect_requests_in_expr(receiver);
                self.collect_requests_in_expr(rhs);
            }
            HirExprKind::Array { elems, .. } => {
                for el in elems {
                    self.collect_requests_in_expr(el);
                }
            }
            HirExprKind::Index { array, index, .. } => {
                self.collect_requests_in_expr(array);
                self.collect_requests_in_expr(index);
            }
            HirExprKind::StrByteIndex { str_val, index } => {
                self.collect_requests_in_expr(str_val);
                self.collect_requests_in_expr(index);
            }
            HirExprKind::StrSlice { str_val, start, end, .. } => {
                self.collect_requests_in_expr(str_val);
                self.collect_requests_in_expr(start);
                self.collect_requests_in_expr(end);
            }
            HirExprKind::Block(b) => self.collect_requests_in_block(b),
            HirExprKind::If { cond, then_b, else_b } => {
                self.collect_requests_in_expr(cond);
                self.collect_requests_in_block(then_b);
                if let Some(e) = else_b {
                    self.collect_requests_in_expr(e);
                }
            }
            HirExprKind::While { cond, body } => {
                self.collect_requests_in_expr(cond);
                self.collect_requests_in_block(body);
            }
            HirExprKind::For { iter, body, .. } => {
                self.collect_requests_in_expr(iter);
                self.collect_requests_in_block(body);
            }
            HirExprKind::ForRange { start, end, body, .. } => {
                self.collect_requests_in_expr(start);
                self.collect_requests_in_expr(end);
                self.collect_requests_in_block(body);
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.collect_requests_in_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.collect_requests_in_expr(g);
                    }
                    self.collect_requests_in_expr(&arm.body);
                }
            }
            HirExprKind::Return(v) => {
                if let Some(v) = v {
                    self.collect_requests_in_expr(v);
                }
            }
            _ => {}
        }
    }
}

/// Try to bind each generic param symbol to a concrete Ty by walking
/// the param/arg types in parallel. Returns None if the call doesn't
/// produce a complete binding (e.g., the user's program has a type
/// error; the checker will have already flagged it).
fn infer_type_args(generic: &HirFn, args: &[HirExpr]) -> Option<Vec<Ty>> {
    if generic.params.len() != args.len() {
        return None;
    }
    let mut subst: HashMap<SymbolId, Ty> = HashMap::new();
    for (param, arg) in generic.params.iter().zip(args) {
        if !unify(&param.ty, &arg.ty, &mut subst) {
            return None;
        }
    }
    // Return type args in declaration order. If a param wasn't bound
    // by inference, we can't proceed.
    generic
        .generics
        .iter()
        .map(|g| subst.get(g).cloned())
        .collect()
}

/// Best-effort unification: every `TypeVar(t)` on the `param` side
/// binds `t` to the matching concrete on the `arg` side. Struct/Enum
/// type args unify element-wise so passing `Box<i64>` to a function
/// expecting `Box<T>` infers T = i64.
fn unify(param: &Ty, arg: &Ty, subst: &mut HashMap<SymbolId, Ty>) -> bool {
    match (param, arg) {
        (Ty::TypeVar(t), concrete) => match subst.get(t) {
            None => {
                subst.insert(*t, concrete.clone());
                true
            }
            Some(prev) => prev == concrete,
        },
        (Ty::Struct(s1, pargs), Ty::Struct(s2, aargs))
        | (Ty::Enum(s1, pargs), Ty::Enum(s2, aargs))
            if s1 == s2 =>
        {
            // Args lengths may differ when one side is the "empty
            // args" placeholder used for variant construction; only
            // unify the positions we can.
            for (p, a) in pargs.iter().zip(aargs.iter()) {
                if !unify(p, a, subst) {
                    return false;
                }
            }
            true
        }
        (Ty::Array(p_elem, _), Ty::Array(a_elem, _)) => unify(p_elem, a_elem, subst),
        (a, b) => a == b,
    }
}

/// Build a fresh HirFn with `subst` applied to every Ty in params,
/// return type, and body. Locals keep their original symbols — the
/// monomorphizer doesn't touch SymbolIds inside the body (they're
/// scoped to the function and don't clash across instantiations).
fn subst_fn(f: &HirFn, subst: &HashMap<SymbolId, Ty>) -> HirFn {
    HirFn {
        sym: f.sym,
        name: f.name.clone(),
        generics: f.generics.clone(),
        params: f
            .params
            .iter()
            .map(|p| HirParam {
                sym: p.sym,
                name: p.name.clone(),
                ty: subst_ty(&p.ty, subst),
            })
            .collect(),
        ret_ty: subst_ty(&f.ret_ty, subst),
        body: subst_block(&f.body, subst),
    }
}

fn subst_ty(ty: &Ty, subst: &HashMap<SymbolId, Ty>) -> Ty {
    match ty {
        Ty::TypeVar(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Array(elem, n) => Ty::Array(Box::new(subst_ty(elem, subst)), *n),
        Ty::Fn { params, ret } => Ty::Fn {
            params: params.iter().map(|t| subst_ty(t, subst)).collect(),
            ret: Box::new(subst_ty(ret, subst)),
        },
        Ty::Struct(s, args) => Ty::Struct(
            *s,
            args.iter().map(|t| subst_ty(t, subst)).collect(),
        ),
        Ty::Enum(s, args) => Ty::Enum(
            *s,
            args.iter().map(|t| subst_ty(t, subst)).collect(),
        ),
        Ty::Weak(inner) => Ty::Weak(Box::new(subst_ty(inner, subst))),
        _ => ty.clone(),
    }
}

fn subst_block(b: &HirBlock, subst: &HashMap<SymbolId, Ty>) -> HirBlock {
    HirBlock {
        stmts: b.stmts.iter().map(|s| subst_stmt(s, subst)).collect(),
        ty: subst_ty(&b.ty, subst),
    }
}

fn subst_stmt(s: &HirStmt, subst: &HashMap<SymbolId, Ty>) -> HirStmt {
    match s {
        HirStmt::Let(l) => HirStmt::Let(HirLet {
            sym: l.sym,
            mutable: l.mutable,
            ty: subst_ty(&l.ty, subst),
            init: l.init.as_ref().map(|e| subst_expr(e, subst)),
        }),
        HirStmt::Expr(e, has_semi) => HirStmt::Expr(subst_expr(e, subst), *has_semi),
    }
}

fn subst_expr(e: &HirExpr, subst: &HashMap<SymbolId, Ty>) -> HirExpr {
    HirExpr {
        ty: subst_ty(&e.ty, subst),
        kind: subst_expr_kind(&e.kind, subst),
    }
}

fn subst_expr_kind(k: &HirExprKind, subst: &HashMap<SymbolId, Ty>) -> HirExprKind {
    use HirExprKind::*;
    match k {
        Lit(l) => Lit(l.clone()),
        Local(s) => Local(*s),
        Fn(s) => Fn(*s),
        EnumVariant { discriminant } => EnumVariant { discriminant: *discriminant },
        EnumPayloadCtor { enum_sym, discriminant, payloads } => EnumPayloadCtor {
            enum_sym: *enum_sym,
            discriminant: *discriminant,
            payloads: payloads.iter().map(|e| subst_expr(e, subst)).collect(),
        },
        Unary { op, expr } => Unary {
            op: *op,
            expr: Box::new(subst_expr(expr, subst)),
        },
        Cast { expr } => Cast {
            expr: Box::new(subst_expr(expr, subst)),
        },
        Binary { op, lhs, rhs } => Binary {
            op: *op,
            lhs: Box::new(subst_expr(lhs, subst)),
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        Logical { op, lhs, rhs } => Logical {
            op: *op,
            lhs: Box::new(subst_expr(lhs, subst)),
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        Assign { lhs, rhs } => Assign {
            lhs: *lhs,
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        AssignOp { lhs, op, rhs } => AssignOp {
            lhs: *lhs,
            op: *op,
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        Call { callee, args } => Call {
            callee: *callee,
            args: args.iter().map(|e| subst_expr(e, subst)).collect(),
        },
        BuiltinCall { name, args } => BuiltinCall {
            name: name.clone(),
            args: args.iter().map(|e| subst_expr(e, subst)).collect(),
        },
        MethodCall { receiver, method, args } => MethodCall {
            receiver: Box::new(subst_expr(receiver, subst)),
            method: method.clone(),
            args: args.iter().map(|e| subst_expr(e, subst)).collect(),
        },
        StructLit { sym, fields, size } => StructLit {
            sym: *sym,
            fields: fields
                .iter()
                .map(|(o, e)| (*o, subst_expr(e, subst)))
                .collect(),
            size: *size,
        },
        FieldAccess { receiver, offset, field_ty } => FieldAccess {
            receiver: Box::new(subst_expr(receiver, subst)),
            offset: *offset,
            field_ty: subst_ty(field_ty, subst),
        },
        FieldAssign { receiver, offset, field_ty, rhs } => FieldAssign {
            receiver: Box::new(subst_expr(receiver, subst)),
            offset: *offset,
            field_ty: subst_ty(field_ty, subst),
            rhs: Box::new(subst_expr(rhs, subst)),
        },
        Array { elems, elem_ty } => Array {
            elems: elems.iter().map(|e| subst_expr(e, subst)).collect(),
            elem_ty: subst_ty(elem_ty, subst),
        },
        Index { array, index, elem_ty } => Index {
            array: Box::new(subst_expr(array, subst)),
            index: Box::new(subst_expr(index, subst)),
            elem_ty: subst_ty(elem_ty, subst),
        },
        StrByteIndex { str_val, index } => StrByteIndex {
            str_val: Box::new(subst_expr(str_val, subst)),
            index: Box::new(subst_expr(index, subst)),
        },
        StrSlice { str_val, start, end, inclusive } => StrSlice {
            str_val: Box::new(subst_expr(str_val, subst)),
            start: Box::new(subst_expr(start, subst)),
            end: Box::new(subst_expr(end, subst)),
            inclusive: *inclusive,
        },
        Block(b) => Block(subst_block(b, subst)),
        If { cond, then_b, else_b } => If {
            cond: Box::new(subst_expr(cond, subst)),
            then_b: subst_block(then_b, subst),
            else_b: else_b.as_ref().map(|e| Box::new(subst_expr(e, subst))),
        },
        While { cond, body } => While {
            cond: Box::new(subst_expr(cond, subst)),
            body: subst_block(body, subst),
        },
        For { local, iter, body, elem_ty, length } => For {
            local: *local,
            iter: Box::new(subst_expr(iter, subst)),
            body: subst_block(body, subst),
            elem_ty: subst_ty(elem_ty, subst),
            length: *length,
        },
        ForRange { local, start, end, inclusive, body } => ForRange {
            local: *local,
            start: Box::new(subst_expr(start, subst)),
            end: Box::new(subst_expr(end, subst)),
            inclusive: *inclusive,
            body: subst_block(body, subst),
        },
        Match { scrutinee, arms } => Match {
            scrutinee: Box::new(subst_expr(scrutinee, subst)),
            arms: arms
                .iter()
                .map(|a| HirMatchArm {
                    patterns: a.patterns.clone(),
                    guard: a.guard.as_ref().map(|g| subst_expr(g, subst)),
                    body: subst_expr(&a.body, subst),
                })
                .collect(),
        },
        Return(v) => Return(v.as_ref().map(|e| Box::new(subst_expr(e, subst)))),
        Unsupported(m) => Unsupported(m.clone()),
    }
}

fn rewrite_calls(
    b: &mut HirBlock,
    cache: &HashMap<(SymbolId, Vec<Ty>), SymbolId>,
    generics: &HashMap<SymbolId, HirFn>,
) {
    for s in &mut b.stmts {
        match s {
            HirStmt::Let(l) => {
                if let Some(init) = &mut l.init {
                    rewrite_calls_in_expr(init, cache, generics);
                }
            }
            HirStmt::Expr(e, _) => rewrite_calls_in_expr(e, cache, generics),
        }
    }
}

fn rewrite_calls_in_expr(
    e: &mut HirExpr,
    cache: &HashMap<(SymbolId, Vec<Ty>), SymbolId>,
    generics: &HashMap<SymbolId, HirFn>,
) {
    use HirExprKind::*;
    match &mut e.kind {
        Call { callee, args } => {
            for a in args.iter_mut() {
                rewrite_calls_in_expr(a, cache, generics);
            }
            if let Some(generic) = generics.get(callee).cloned() {
                if let Some(type_args) = infer_type_args(&generic, args) {
                    if let Some(&specialized) =
                        cache.get(&(*callee, type_args.clone()))
                    {
                        *callee = specialized;
                    }
                }
            }
        }
        Unary { expr, .. } => rewrite_calls_in_expr(expr, cache, generics),
        Cast { expr } => rewrite_calls_in_expr(expr, cache, generics),
        Binary { lhs, rhs, .. } => {
            rewrite_calls_in_expr(lhs, cache, generics);
            rewrite_calls_in_expr(rhs, cache, generics);
        }
        Logical { lhs, rhs, .. } => {
            rewrite_calls_in_expr(lhs, cache, generics);
            rewrite_calls_in_expr(rhs, cache, generics);
        }
        Assign { rhs, .. } => rewrite_calls_in_expr(rhs, cache, generics),
        AssignOp { rhs, .. } => rewrite_calls_in_expr(rhs, cache, generics),
        BuiltinCall { args, .. } => {
            for a in args.iter_mut() {
                rewrite_calls_in_expr(a, cache, generics);
            }
        }
        MethodCall { receiver, args, .. } => {
            rewrite_calls_in_expr(receiver, cache, generics);
            for a in args.iter_mut() {
                rewrite_calls_in_expr(a, cache, generics);
            }
        }
        StructLit { fields, .. } => {
            for (_, v) in fields.iter_mut() {
                rewrite_calls_in_expr(v, cache, generics);
            }
        }
        EnumPayloadCtor { payloads, .. } => {
            for p in payloads.iter_mut() {
                rewrite_calls_in_expr(p, cache, generics);
            }
        }
        FieldAccess { receiver, .. } => rewrite_calls_in_expr(receiver, cache, generics),
        FieldAssign { receiver, rhs, .. } => {
            rewrite_calls_in_expr(receiver, cache, generics);
            rewrite_calls_in_expr(rhs, cache, generics);
        }
        Array { elems, .. } => {
            for el in elems.iter_mut() {
                rewrite_calls_in_expr(el, cache, generics);
            }
        }
        Index { array, index, .. } => {
            rewrite_calls_in_expr(array, cache, generics);
            rewrite_calls_in_expr(index, cache, generics);
        }
        StrByteIndex { str_val, index } => {
            rewrite_calls_in_expr(str_val, cache, generics);
            rewrite_calls_in_expr(index, cache, generics);
        }
        StrSlice { str_val, start, end, .. } => {
            rewrite_calls_in_expr(str_val, cache, generics);
            rewrite_calls_in_expr(start, cache, generics);
            rewrite_calls_in_expr(end, cache, generics);
        }
        Block(b) => rewrite_calls(b, cache, generics),
        If { cond, then_b, else_b } => {
            rewrite_calls_in_expr(cond, cache, generics);
            rewrite_calls(then_b, cache, generics);
            if let Some(e) = else_b {
                rewrite_calls_in_expr(e, cache, generics);
            }
        }
        While { cond, body } => {
            rewrite_calls_in_expr(cond, cache, generics);
            rewrite_calls(body, cache, generics);
        }
        For { iter, body, .. } => {
            rewrite_calls_in_expr(iter, cache, generics);
            rewrite_calls(body, cache, generics);
        }
        ForRange { start, end, body, .. } => {
            rewrite_calls_in_expr(start, cache, generics);
            rewrite_calls_in_expr(end, cache, generics);
            rewrite_calls(body, cache, generics);
        }
        Match { scrutinee, arms } => {
            rewrite_calls_in_expr(scrutinee, cache, generics);
            for arm in arms.iter_mut() {
                if let Some(g) = &mut arm.guard {
                    rewrite_calls_in_expr(g, cache, generics);
                }
                rewrite_calls_in_expr(&mut arm.body, cache, generics);
            }
        }
        Return(v) => {
            if let Some(v) = v {
                rewrite_calls_in_expr(v, cache, generics);
            }
        }
        _ => {}
    }
}

fn mangle(name: &str, args: &[Ty]) -> String {
    let mut out = String::from(name);
    for a in args {
        out.push_str("$$");
        out.push_str(&mangle_ty(a));
    }
    out
}

fn mangle_ty(t: &Ty) -> String {
    use crate::ty::IntTy::*;
    match t {
        Ty::Bool => "bool".into(),
        Ty::Char => "char".into(),
        Ty::Int(I8) => "i8".into(),
        Ty::Int(I16) => "i16".into(),
        Ty::Int(I32) => "i32".into(),
        Ty::Int(I64) => "i64".into(),
        Ty::Int(ISize) => "isize".into(),
        Ty::Int(U8) => "u8".into(),
        Ty::Int(U16) => "u16".into(),
        Ty::Int(U32) => "u32".into(),
        Ty::Int(U64) => "u64".into(),
        Ty::Int(USize) => "usize".into(),
        Ty::Float(crate::ty::FloatTy::F32) => "f32".into(),
        Ty::Float(crate::ty::FloatTy::F64) => "f64".into(),
        Ty::Str => "str".into(),
        Ty::Unit => "unit".into(),
        Ty::Vec => "Vec".into(),
        Ty::Array(e, n) => format!("arr{}_{}", mangle_ty(e), n),
        Ty::Struct(s, args) => {
            if args.is_empty() {
                format!("S{}", s.0)
            } else {
                let inner: Vec<String> = args.iter().map(mangle_ty).collect();
                format!("S{}_{}", s.0, inner.join("_"))
            }
        }
        Ty::Enum(s, args) => {
            if args.is_empty() {
                format!("E{}", s.0)
            } else {
                let inner: Vec<String> = args.iter().map(mangle_ty).collect();
                format!("E{}_{}", s.0, inner.join("_"))
            }
        }
        _ => "x".into(),
    }
}

fn walk_block_collect_syms(b: &HirBlock, max: &mut u32) {
    for s in &b.stmts {
        match s {
            HirStmt::Let(l) => {
                if let Some(sym) = l.sym {
                    *max = (*max).max(sym.0);
                }
                if let Some(init) = &l.init {
                    walk_expr_collect_syms(init, max);
                }
            }
            HirStmt::Expr(e, _) => walk_expr_collect_syms(e, max),
        }
    }
}

fn walk_expr_collect_syms(e: &HirExpr, max: &mut u32) {
    use HirExprKind::*;
    match &e.kind {
        Local(s) | Fn(s) | Assign { lhs: s, .. } | AssignOp { lhs: s, .. } => {
            *max = (*max).max(s.0);
        }
        Call { callee, args } => {
            *max = (*max).max(callee.0);
            for a in args {
                walk_expr_collect_syms(a, max);
            }
        }
        BuiltinCall { args, .. } => {
            for a in args {
                walk_expr_collect_syms(a, max);
            }
        }
        Block(b) => walk_block_collect_syms(b, max),
        _ => {}
    }
}
