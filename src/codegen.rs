//! Cranelift JIT codegen for Rune's HIR.
//!
//! Produces in-memory machine code via `cranelift_jit`. Functions are
//! compiled with the target's native calling convention (effectively
//! `extern "C"`), so the host can call them via `transmute`.
//!
//! Scope: i64/i32/i16/i8 (and unsigned/pointer-sized counterparts),
//! f32/f64, bool, unit. Arithmetic, comparison, bitwise, logical
//! (short-circuit), unary, if/else, while, let bindings with mutability,
//! Rune-to-Rune function calls, `return`.

use std::collections::HashMap;
use std::fmt;

use cranelift::prelude::*;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};

use crate::hir::*;
use crate::ty::{FloatTy, IntTy, SymbolId, Ty};

#[derive(Debug, Clone)]
pub struct CodegenError(pub String);

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codegen error: {}", self.0)
    }
}

impl std::error::Error for CodegenError {}

impl From<String> for CodegenError {
    fn from(s: String) -> Self { Self(s) }
}

impl From<&str> for CodegenError {
    fn from(s: &str) -> Self { Self(s.to_string()) }
}

pub struct Codegen {
    module: JITModule,
    sym_to_func: HashMap<SymbolId, FuncId>,
    sym_to_sig: HashMap<SymbolId, Signature>,
}

impl Codegen {
    pub fn new() -> Result<Self, CodegenError> {
        let mut flag_builder = settings::builder();
        flag_builder.set("use_colocated_libcalls", "false")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder.set("is_pic", "false")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder.set("opt_level", "none")
            .map_err(|e| CodegenError(e.to_string()))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|s| CodegenError(format!("host machine ISA: {}", s)))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| CodegenError(e.to_string()))?;
        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            sym_to_func: HashMap::new(),
            sym_to_sig: HashMap::new(),
        })
    }

    pub fn compile_module(&mut self, hir: &HirModule) -> Result<(), CodegenError> {
        // Pass 1: declare all functions so forward calls resolve.
        for item in &hir.items {
            let HirItem::Fn(f) = item;
            let sig = self.fn_signature(f)?;
            let func_id = self
                .module
                .declare_function(&f.name, Linkage::Export, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.sym_to_func.insert(f.sym, func_id);
            self.sym_to_sig.insert(f.sym, sig);
        }
        // Pass 2: define each body.
        for item in &hir.items {
            let HirItem::Fn(f) = item;
            self.define_fn(f)?;
        }
        self.module
            .finalize_definitions()
            .map_err(|e| CodegenError(e.to_string()))?;
        Ok(())
    }

    fn fn_signature(&self, f: &HirFn) -> Result<Signature, CodegenError> {
        let mut sig = self.module.make_signature();
        for p in &f.params {
            sig.params.push(AbiParam::new(cranelift_type(&p.ty)?));
        }
        if !matches!(f.ret_ty, Ty::Unit) {
            sig.returns.push(AbiParam::new(cranelift_type(&f.ret_ty)?));
        }
        Ok(sig)
    }

    fn define_fn(&mut self, f: &HirFn) -> Result<(), CodegenError> {
        let func_id = self.sym_to_func[&f.sym];
        let sig = self.sym_to_sig[&f.sym].clone();
        let mut ctx = self.module.make_context();
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        // Snapshot signature info we need for the body before we move builder.
        let ret_ty = f.ret_ty.clone();

        let mut fc = FnCodegen {
            module: &mut self.module,
            sym_to_func: &self.sym_to_func,
            builder,
            var_map: HashMap::new(),
            var_counter: 0,
        };

        // Declare each parameter as a Variable, seeded from the block params.
        for (i, p) in f.params.iter().enumerate() {
            let var = fc.alloc_var();
            let cty = cranelift_type(&p.ty)?;
            fc.builder.declare_var(var, cty);
            let arg = fc.builder.block_params(entry)[i];
            fc.builder.def_var(var, arg);
            fc.var_map.insert(p.sym, var);
        }

        let body_val = fc.compile_block(&f.body)?;

        if !fc.is_filled() {
            match ret_ty {
                Ty::Unit => {
                    fc.builder.ins().return_(&[]);
                }
                _ => {
                    if let Some(v) = body_val {
                        fc.builder.ins().return_(&[v]);
                    } else {
                        return Err(CodegenError(format!(
                            "function `{}` declared `{}` but body produced no value",
                            f.name,
                            ret_ty.display()
                        )));
                    }
                }
            }
        }

        fc.builder.finalize();

        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    pub fn get_function_ptr(&self, sym: SymbolId) -> Option<*const u8> {
        let func_id = self.sym_to_func.get(&sym)?;
        Some(self.module.get_finalized_function(*func_id))
    }
}

struct FnCodegen<'a> {
    module: &'a mut JITModule,
    sym_to_func: &'a HashMap<SymbolId, FuncId>,
    builder: FunctionBuilder<'a>,
    var_map: HashMap<SymbolId, Variable>,
    var_counter: u32,
}

impl<'a> FnCodegen<'a> {
    fn alloc_var(&mut self) -> Variable {
        let v = Variable::new(self.var_counter as usize);
        self.var_counter += 1;
        v
    }

    /// Whether the current Cranelift block already ends in a terminator
    /// (return/jump/brif). `FunctionBuilder::is_filled` is private, so we
    /// peek into the underlying `Function` layout directly.
    fn is_filled(&self) -> bool {
        let Some(blk) = self.builder.current_block() else { return true; };
        let Some(last) = self.builder.func.layout.last_inst(blk) else { return false; };
        self.builder.func.dfg.insts[last].opcode().is_terminator()
    }

    fn compile_block(&mut self, b: &HirBlock) -> Result<Option<Value>, CodegenError> {
        let mut last_val: Option<Value> = None;
        for s in &b.stmts {
            last_val = self.compile_stmt(s)?;
            if self.is_filled() {
                // Control flow diverged (return). No further code runs.
                break;
            }
        }
        Ok(last_val)
    }

    fn compile_stmt(&mut self, s: &HirStmt) -> Result<Option<Value>, CodegenError> {
        match s {
            HirStmt::Let(l) => {
                let cty = cranelift_type(&l.ty)?;
                let var = self.alloc_var();
                self.builder.declare_var(var, cty);
                if let Some(init) = &l.init {
                    let v = self
                        .compile_expr(init)?
                        .ok_or_else(|| CodegenError("let initializer produced no value".into()))?;
                    self.builder.def_var(var, v);
                } else {
                    let z = self.builder.ins().iconst(cty, 0);
                    self.builder.def_var(var, z);
                }
                if let Some(sym) = l.sym {
                    self.var_map.insert(sym, var);
                }
                Ok(None)
            }
            HirStmt::Expr(e, has_semi) => {
                let v = self.compile_expr(e)?;
                if *has_semi { Ok(None) } else { Ok(v) }
            }
        }
    }

    fn compile_expr(&mut self, e: &HirExpr) -> Result<Option<Value>, CodegenError> {
        match &e.kind {
            HirExprKind::Lit(lit) => self.compile_lit(lit, &e.ty),
            HirExprKind::Local(sym) => {
                let var = *self
                    .var_map
                    .get(sym)
                    .ok_or_else(|| CodegenError("unknown local".into()))?;
                Ok(Some(self.builder.use_var(var)))
            }
            HirExprKind::Fn(_) => {
                Err(CodegenError("first-class function values are not supported".into()))
            }
            HirExprKind::Unary { op, expr } => self.compile_unary(*op, expr, &e.ty),
            HirExprKind::Binary { op, lhs, rhs } => self.compile_binary(*op, lhs, rhs, &e.ty),
            HirExprKind::Logical { op, lhs, rhs } => self.compile_logical(*op, lhs, rhs),
            HirExprKind::Assign { lhs, rhs } => {
                let v = self
                    .compile_expr(rhs)?
                    .ok_or_else(|| CodegenError("assignment rhs produced no value".into()))?;
                let var = *self
                    .var_map
                    .get(lhs)
                    .ok_or_else(|| CodegenError("assignment to unknown local".into()))?;
                self.builder.def_var(var, v);
                Ok(None)
            }
            HirExprKind::AssignOp { lhs, op, rhs } => {
                let var = *self
                    .var_map
                    .get(lhs)
                    .ok_or_else(|| CodegenError("compound assign to unknown local".into()))?;
                let cur = self.builder.use_var(var);
                let r = self
                    .compile_expr(rhs)?
                    .ok_or_else(|| CodegenError("compound assign rhs produced no value".into()))?;
                let new_val = self.compile_binop_value(*op, cur, r, &e.ty)?;
                self.builder.def_var(var, new_val);
                Ok(None)
            }
            HirExprKind::Call { callee, args } => {
                let func_id = *self
                    .sym_to_func
                    .get(callee)
                    .ok_or_else(|| CodegenError("call to undeclared function".into()))?;
                let local_func = self.module.declare_func_in_func(func_id, self.builder.func);
                let mut arg_vals = Vec::with_capacity(args.len());
                for a in args {
                    let v = self
                        .compile_expr(a)?
                        .ok_or_else(|| CodegenError("call arg produced no value".into()))?;
                    arg_vals.push(v);
                }
                let inst = self.builder.ins().call(local_func, &arg_vals);
                let results = self.builder.inst_results(inst);
                if results.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(results[0]))
                }
            }
            HirExprKind::Block(b) => self.compile_block(b),
            HirExprKind::If { cond, then_b, else_b } => {
                self.compile_if(cond, then_b, else_b.as_deref(), &e.ty)
            }
            HirExprKind::While { cond, body } => self.compile_while(cond, body),
            HirExprKind::Return(value) => {
                let vals = if let Some(v) = value {
                    let val = self
                        .compile_expr(v)?
                        .ok_or_else(|| CodegenError("return value produced no value".into()))?;
                    vec![val]
                } else {
                    vec![]
                };
                self.builder.ins().return_(&vals);
                // After a return the current block has a terminator. Switch
                // to a fresh unreachable block so subsequent IR is well-formed.
                let after = self.builder.create_block();
                self.builder.switch_to_block(after);
                self.builder.seal_block(after);
                Ok(None)
            }
            HirExprKind::Unsupported(msg) => {
                Err(CodegenError(format!("unsupported in codegen: {}", msg)))
            }
        }
    }

    fn compile_lit(&mut self, lit: &HirLit, _ty: &Ty) -> Result<Option<Value>, CodegenError> {
        let v = match lit {
            HirLit::Int(v, int_ty) => {
                let cty = int_cranelift_type(*int_ty);
                self.builder.ins().iconst(cty, *v)
            }
            HirLit::Float(v, FloatTy::F32) => self.builder.ins().f32const(*v as f32),
            HirLit::Float(v, FloatTy::F64) => self.builder.ins().f64const(*v),
            HirLit::Bool(b) => self.builder.ins().iconst(types::I8, if *b { 1 } else { 0 }),
            HirLit::Unit => return Ok(None),
        };
        Ok(Some(v))
    }

    fn compile_unary(
        &mut self,
        op: HirUnOp,
        expr: &HirExpr,
        ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let v = self
            .compile_expr(expr)?
            .ok_or_else(|| CodegenError("unary operand produced no value".into()))?;
        let r = match op {
            HirUnOp::Neg => match ty {
                Ty::Int(_) => self.builder.ins().ineg(v),
                Ty::Float(_) => self.builder.ins().fneg(v),
                _ => return Err(CodegenError(format!("cannot negate `{}`", ty.display()))),
            },
            HirUnOp::Not => {
                // bool: xor with 1
                let one = self.builder.ins().iconst(types::I8, 1);
                self.builder.ins().bxor(v, one)
            }
            HirUnOp::BitNot => self.builder.ins().bnot(v),
        };
        Ok(Some(r))
    }

    fn compile_binary(
        &mut self,
        op: HirBinOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let l = self
            .compile_expr(lhs)?
            .ok_or_else(|| CodegenError("lhs produced no value".into()))?;
        let r = self
            .compile_expr(rhs)?
            .ok_or_else(|| CodegenError("rhs produced no value".into()))?;
        // For comparison the *operand* type matters; for arithmetic the
        // result type and operand type are the same.
        let operand_ty = match op {
            HirBinOp::Eq | HirBinOp::Ne | HirBinOp::Lt | HirBinOp::Gt
            | HirBinOp::Le | HirBinOp::Ge => &lhs.ty,
            _ => ty,
        };
        let v = self.compile_binop_value(op, l, r, operand_ty)?;
        Ok(Some(v))
    }

    fn compile_binop_value(
        &mut self,
        op: HirBinOp,
        l: Value,
        r: Value,
        ty: &Ty,
    ) -> Result<Value, CodegenError> {
        let signed = matches!(
            ty,
            Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize)
        );
        let is_float = matches!(ty, Ty::Float(_));
        let v = match op {
            HirBinOp::Add => {
                if is_float { self.builder.ins().fadd(l, r) } else { self.builder.ins().iadd(l, r) }
            }
            HirBinOp::Sub => {
                if is_float { self.builder.ins().fsub(l, r) } else { self.builder.ins().isub(l, r) }
            }
            HirBinOp::Mul => {
                if is_float { self.builder.ins().fmul(l, r) } else { self.builder.ins().imul(l, r) }
            }
            HirBinOp::Div => {
                if is_float {
                    self.builder.ins().fdiv(l, r)
                } else if signed {
                    self.builder.ins().sdiv(l, r)
                } else {
                    self.builder.ins().udiv(l, r)
                }
            }
            HirBinOp::Mod => {
                if is_float {
                    return Err(CodegenError("float modulo not supported".into()));
                }
                if signed { self.builder.ins().srem(l, r) } else { self.builder.ins().urem(l, r) }
            }
            HirBinOp::Eq => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::Equal, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::Equal, l, r)
                }
            }
            HirBinOp::Ne => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::NotEqual, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::NotEqual, l, r)
                }
            }
            HirBinOp::Lt => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::LessThan, l, r)
                } else if signed {
                    self.builder.ins().icmp(IntCC::SignedLessThan, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::UnsignedLessThan, l, r)
                }
            }
            HirBinOp::Gt => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::GreaterThan, l, r)
                } else if signed {
                    self.builder.ins().icmp(IntCC::SignedGreaterThan, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::UnsignedGreaterThan, l, r)
                }
            }
            HirBinOp::Le => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::LessThanOrEqual, l, r)
                } else if signed {
                    self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::UnsignedLessThanOrEqual, l, r)
                }
            }
            HirBinOp::Ge => {
                if is_float {
                    self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, l, r)
                } else if signed {
                    self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, l, r)
                }
            }
            HirBinOp::BitAnd => self.builder.ins().band(l, r),
            HirBinOp::BitOr => self.builder.ins().bor(l, r),
            HirBinOp::BitXor => self.builder.ins().bxor(l, r),
            HirBinOp::Shl => self.builder.ins().ishl(l, r),
            HirBinOp::Shr => {
                if signed { self.builder.ins().sshr(l, r) } else { self.builder.ins().ushr(l, r) }
            }
        };
        Ok(v)
    }

    fn compile_logical(
        &mut self,
        op: LogicalOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> Result<Option<Value>, CodegenError> {
        // a && b → if a { b } else { false }
        // a || b → if a { true } else { b }
        let l = self
            .compile_expr(lhs)?
            .ok_or_else(|| CodegenError("logical lhs produced no value".into()))?;
        let then_blk = self.builder.create_block();
        let else_blk = self.builder.create_block();
        let merge_blk = self.builder.create_block();
        self.builder.append_block_param(merge_blk, types::I8);

        self.builder.ins().brif(l, then_blk, &[], else_blk, &[]);

        // then
        self.builder.switch_to_block(then_blk);
        self.builder.seal_block(then_blk);
        let then_v = match op {
            LogicalOp::And => self
                .compile_expr(rhs)?
                .ok_or_else(|| CodegenError("logical rhs produced no value".into()))?,
            LogicalOp::Or => self.builder.ins().iconst(types::I8, 1),
        };
        if !self.is_filled() {
            self.builder.ins().jump(merge_blk, &[then_v]);
        }

        // else
        self.builder.switch_to_block(else_blk);
        self.builder.seal_block(else_blk);
        let else_v = match op {
            LogicalOp::And => self.builder.ins().iconst(types::I8, 0),
            LogicalOp::Or => self
                .compile_expr(rhs)?
                .ok_or_else(|| CodegenError("logical rhs produced no value".into()))?,
        };
        if !self.is_filled() {
            self.builder.ins().jump(merge_blk, &[else_v]);
        }

        // merge
        self.builder.switch_to_block(merge_blk);
        self.builder.seal_block(merge_blk);
        Ok(Some(self.builder.block_params(merge_blk)[0]))
    }

    fn compile_if(
        &mut self,
        cond: &HirExpr,
        then_b: &HirBlock,
        else_b: Option<&HirExpr>,
        ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let cond_v = self
            .compile_expr(cond)?
            .ok_or_else(|| CodegenError("if condition produced no value".into()))?;
        let then_blk = self.builder.create_block();
        let else_blk = self.builder.create_block();
        let merge_blk = self.builder.create_block();

        let produces_value = !matches!(ty, Ty::Unit | Ty::Never);
        if produces_value {
            let cty = cranelift_type(ty)?;
            self.builder.append_block_param(merge_blk, cty);
        }

        self.builder.ins().brif(cond_v, then_blk, &[], else_blk, &[]);

        // then
        self.builder.switch_to_block(then_blk);
        self.builder.seal_block(then_blk);
        let then_val = self.compile_block(then_b)?;
        if !self.is_filled() {
            if produces_value {
                let v = then_val.ok_or_else(|| {
                    CodegenError("if-then branch produced no value".into())
                })?;
                self.builder.ins().jump(merge_blk, &[v]);
            } else {
                self.builder.ins().jump(merge_blk, &[]);
            }
        }

        // else
        self.builder.switch_to_block(else_blk);
        self.builder.seal_block(else_blk);
        let else_val = if let Some(e) = else_b { self.compile_expr(e)? } else { None };
        if !self.is_filled() {
            if produces_value {
                let v = else_val.ok_or_else(|| {
                    CodegenError("if-else branch produced no value".into())
                })?;
                self.builder.ins().jump(merge_blk, &[v]);
            } else {
                self.builder.ins().jump(merge_blk, &[]);
            }
        }

        // merge
        self.builder.switch_to_block(merge_blk);
        self.builder.seal_block(merge_blk);
        if produces_value {
            Ok(Some(self.builder.block_params(merge_blk)[0]))
        } else {
            Ok(None)
        }
    }

    fn compile_while(
        &mut self,
        cond: &HirExpr,
        body: &HirBlock,
    ) -> Result<Option<Value>, CodegenError> {
        let header = self.builder.create_block();
        let body_blk = self.builder.create_block();
        let exit = self.builder.create_block();

        self.builder.ins().jump(header, &[]);

        self.builder.switch_to_block(header);
        let cond_v = self
            .compile_expr(cond)?
            .ok_or_else(|| CodegenError("while condition produced no value".into()))?;
        self.builder.ins().brif(cond_v, body_blk, &[], exit, &[]);

        self.builder.switch_to_block(body_blk);
        self.builder.seal_block(body_blk);
        self.compile_block(body)?;
        if !self.is_filled() {
            self.builder.ins().jump(header, &[]);
        }

        self.builder.seal_block(header);
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        Ok(None)
    }
}

fn cranelift_type(ty: &Ty) -> Result<Type, CodegenError> {
    Ok(match ty {
        Ty::Bool => types::I8,
        Ty::Int(it) => int_cranelift_type(*it),
        Ty::Float(FloatTy::F32) => types::F32,
        Ty::Float(FloatTy::F64) => types::F64,
        Ty::Char => types::I32,
        _ => {
            return Err(CodegenError(format!(
                "type `{}` not supported in codegen",
                ty.display()
            )));
        }
    })
}

fn int_cranelift_type(it: IntTy) -> Type {
    match it {
        IntTy::I8 | IntTy::U8 => types::I8,
        IntTy::I16 | IntTy::U16 => types::I16,
        IntTy::I32 | IntTy::U32 => types::I32,
        IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize => types::I64,
    }
}
