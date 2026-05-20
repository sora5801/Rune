//! Cranelift codegen for Rune's HIR.
//!
//! Generic over `cranelift_module::Module`. Two backends instantiate it:
//! - `Codegen<JITModule>` — JIT compilation for `rune run`.
//! - `Codegen<ObjectModule>` — AOT object emission for `rune build`.
//!
//! The per-function machinery (`FnCodegen`) is shared. Backend-specific
//! finalization is split across two specialized `impl` blocks.

use std::collections::HashMap;
use std::fmt;

use cranelift::prelude::*;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule, ObjectProduct};

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

pub struct Codegen<M: Module> {
    module: M,
    sym_to_func: HashMap<SymbolId, FuncId>,
    sym_to_sig: HashMap<SymbolId, Signature>,
    /// Imported builtin and internal-runtime functions, declared lazily.
    builtin_funcs: HashMap<String, FuncId>,
    /// Monotonic counter for naming unique string-literal data symbols.
    next_str_id: u32,
}

/// Layout of a Rune string descriptor, mirrored on both sides of the
/// codegen/runtime boundary. 16 bytes, 8-byte aligned.
#[repr(C)]
struct RuneStr {
    ptr: *const u8,
    len: i64,
}

/// Host implementation of `print(i64)` for JIT mode.
extern "C" fn rune_runtime_print_i64(x: i64) {
    println!("{}", x);
}

/// Host implementation of `print_str(str)` for JIT mode.
extern "C" fn rune_runtime_print_str(s: *const RuneStr) {
    unsafe {
        let s = &*s;
        if s.len == 0 {
            println!();
            return;
        }
        // Rune source literals are UTF-8 by construction.
        let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
        let text = std::str::from_utf8_unchecked(slice);
        println!("{}", text);
    }
}

/// Host implementation of string equality for JIT mode. Returns 1 if equal,
/// 0 otherwise.
extern "C" fn rune_runtime_str_eq(a: *const RuneStr, b: *const RuneStr) -> i8 {
    unsafe {
        let a = &*a;
        let b = &*b;
        if a.len != b.len {
            return 0;
        }
        // Empty strings: lengths match, contents trivially equal. Skip
        // from_raw_parts (its safety precondition rejects null even for
        // zero-length slices).
        if a.len == 0 {
            return 1;
        }
        let aa = std::slice::from_raw_parts(a.ptr, a.len as usize);
        let bb = std::slice::from_raw_parts(b.ptr, b.len as usize);
        if aa == bb { 1 } else { 0 }
    }
}

unsafe fn rune_str_bytes<'a>(s: *const RuneStr) -> &'a [u8] {
    if s.is_null() { return &[]; }
    let s = &*s;
    if s.len <= 0 { return &[]; }
    std::slice::from_raw_parts(s.ptr, s.len as usize)
}

extern "C" fn rune_runtime_str_starts_with(s: *const RuneStr, prefix: *const RuneStr) -> i8 {
    unsafe {
        let s = rune_str_bytes(s);
        let prefix = rune_str_bytes(prefix);
        if s.starts_with(prefix) { 1 } else { 0 }
    }
}

extern "C" fn rune_runtime_str_ends_with(s: *const RuneStr, suffix: *const RuneStr) -> i8 {
    unsafe {
        let s = rune_str_bytes(s);
        let suffix = rune_str_bytes(suffix);
        if s.ends_with(suffix) { 1 } else { 0 }
    }
}

extern "C" fn rune_runtime_str_contains(s: *const RuneStr, needle: *const RuneStr) -> i8 {
    unsafe {
        let s = rune_str_bytes(s);
        let needle = rune_str_bytes(needle);
        if needle.is_empty() {
            return 1; // matches Rust's `&str::contains` convention
        }
        if needle.len() > s.len() { return 0; }
        for window in s.windows(needle.len()) {
            if window == needle { return 1; }
        }
        0
    }
}

/// Host implementation of `s[a..b]` for JIT mode. Clamps out-of-range
/// indices instead of panicking (consistent with current "no bounds
/// checks" stance). Heap-allocates; never freed.
extern "C" fn rune_runtime_str_slice(
    s: *const RuneStr,
    start: i64,
    end: i64,
) -> *mut RuneStr {
    use std::alloc::{alloc, Layout};
    unsafe {
        let s = &*s;
        let start = start.max(0).min(s.len);
        let end = end.max(start).min(s.len);
        let new_len = end - start;
        let desc = alloc(Layout::new::<RuneStr>()) as *mut RuneStr;
        if new_len == 0 {
            (*desc).ptr = std::ptr::null();
            (*desc).len = 0;
            return desc;
        }
        let bytes = alloc(Layout::from_size_align(new_len as usize, 1).unwrap());
        std::ptr::copy_nonoverlapping(
            s.ptr.add(start as usize),
            bytes,
            new_len as usize,
        );
        (*desc).ptr = bytes;
        (*desc).len = new_len;
        desc
    }
}

/// Host implementation of string concatenation for JIT mode. Allocates a
/// fresh descriptor + fresh byte buffer on the heap, never freed (leak by
/// design — Rune v0.x is process-lifetime).
extern "C" fn rune_runtime_str_concat(a: *const RuneStr, b: *const RuneStr) -> *mut RuneStr {
    use std::alloc::{alloc, Layout};
    unsafe {
        let a = &*a;
        let b = &*b;
        let total_len = a.len + b.len;
        let desc_layout = Layout::new::<RuneStr>();
        let desc = alloc(desc_layout) as *mut RuneStr;
        if total_len == 0 {
            (*desc).ptr = std::ptr::null();
            (*desc).len = 0;
            return desc;
        }
        let bytes_layout = Layout::from_size_align(total_len as usize, 1).unwrap();
        let bytes = alloc(bytes_layout);
        if a.len > 0 {
            std::ptr::copy_nonoverlapping(a.ptr, bytes, a.len as usize);
        }
        if b.len > 0 {
            std::ptr::copy_nonoverlapping(b.ptr, bytes.add(a.len as usize), b.len as usize);
        }
        (*desc).ptr = bytes as *const u8;
        (*desc).len = total_len;
        desc
    }
}

// ---- generic methods: compile any module backend ----

impl<M: Module> Codegen<M> {
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
        Ok(())
    }

    pub fn func_id(&self, sym: SymbolId) -> Option<FuncId> {
        self.sym_to_func.get(&sym).copied()
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

        let ret_ty = f.ret_ty.clone();

        let mut fc = FnCodegen {
            module: &mut self.module,
            sym_to_func: &self.sym_to_func,
            builtin_funcs: &mut self.builtin_funcs,
            next_str_id: &mut self.next_str_id,
            builder,
            var_map: HashMap::new(),
            var_counter: 0,
        };

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
}

// ---- JIT-only ----

impl Codegen<JITModule> {
    pub fn new_jit() -> Result<Self, CodegenError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder
            .set("is_pic", "false")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder
            .set("opt_level", "none")
            .map_err(|e| CodegenError(e.to_string()))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|s| CodegenError(format!("host machine ISA: {}", s)))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| CodegenError(e.to_string()))?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("rune_print_i64", rune_runtime_print_i64 as *const u8);
        builder.symbol("rune_print_str", rune_runtime_print_str as *const u8);
        builder.symbol("rune_str_eq", rune_runtime_str_eq as *const u8);
        builder.symbol("rune_str_concat", rune_runtime_str_concat as *const u8);
        builder.symbol("rune_str_slice", rune_runtime_str_slice as *const u8);
        builder.symbol("rune_str_starts_with", rune_runtime_str_starts_with as *const u8);
        builder.symbol("rune_str_ends_with", rune_runtime_str_ends_with as *const u8);
        builder.symbol("rune_str_contains", rune_runtime_str_contains as *const u8);
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            sym_to_func: HashMap::new(),
            sym_to_sig: HashMap::new(),
            builtin_funcs: HashMap::new(),
            next_str_id: 0,
        })
    }

    pub fn finalize(&mut self) -> Result<(), CodegenError> {
        self.module
            .finalize_definitions()
            .map_err(|e| CodegenError(e.to_string()))?;
        Ok(())
    }

    pub fn get_function_ptr(&self, sym: SymbolId) -> Option<*const u8> {
        let func_id = self.sym_to_func.get(&sym)?;
        Some(self.module.get_finalized_function(*func_id))
    }
}

// ---- Object-only ----

impl Codegen<ObjectModule> {
    pub fn new_object(module_name: &str, opt_level: OptLevel) -> Result<Self, CodegenError> {
        let mut flag_builder = settings::builder();
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder
            .set("is_pic", "true")
            .map_err(|e| CodegenError(e.to_string()))?;
        flag_builder
            .set("opt_level", opt_level.as_str())
            .map_err(|e| CodegenError(e.to_string()))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|s| CodegenError(format!("host machine ISA: {}", s)))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| CodegenError(e.to_string()))?;
        let builder = ObjectBuilder::new(
            isa,
            module_name,
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| CodegenError(e.to_string()))?;
        let module = ObjectModule::new(builder);
        Ok(Self {
            module,
            sym_to_func: HashMap::new(),
            sym_to_sig: HashMap::new(),
            builtin_funcs: HashMap::new(),
            next_str_id: 0,
        })
    }

    /// Emit a C-compatible `int main(void)` that calls the Rune main
    /// (passed in as `rune_main_id`) and truncates its `i64` return value
    /// to `i32` for the OS exit code.
    pub fn emit_c_main_wrapper(&mut self, rune_main_id: FuncId) -> Result<(), CodegenError> {
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I32));
        let func_id = self
            .module
            .declare_function("main", Linkage::Export, &sig)
            .map_err(|e| CodegenError(e.to_string()))?;

        let mut ctx = self.module.make_context();
        ctx.func.signature = sig;
        let mut bctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut bctx);

        let block = builder.create_block();
        builder.switch_to_block(block);
        builder.seal_block(block);

        let rune_main_ref = self.module.declare_func_in_func(rune_main_id, builder.func);
        let inst = builder.ins().call(rune_main_ref, &[]);
        let rune_result = builder.inst_results(inst)[0];
        let exit_code = builder.ins().ireduce(types::I32, rune_result);
        builder.ins().return_(&[exit_code]);
        builder.finalize();

        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, CodegenError> {
        let product: ObjectProduct = self.module.finish();
        product
            .emit()
            .map_err(|e| CodegenError(e.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    None,
    Speed,
    SpeedAndSize,
}

impl OptLevel {
    fn as_str(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
            OptLevel::SpeedAndSize => "speed_and_size",
        }
    }
}

// ---- per-function codegen (generic over Module) ----

struct FnCodegen<'a, M: Module> {
    module: &'a mut M,
    sym_to_func: &'a HashMap<SymbolId, FuncId>,
    builtin_funcs: &'a mut HashMap<String, FuncId>,
    next_str_id: &'a mut u32,
    builder: FunctionBuilder<'a>,
    var_map: HashMap<SymbolId, Variable>,
    var_counter: u32,
}

impl<'a, M: Module> FnCodegen<'a, M> {
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
                // The AssignOp expression itself is `()`-typed; the *operation*
                // is on the variable's type (which equals rhs.ty after the
                // type checker's compatibility check).
                let new_val = self.compile_binop_value(*op, cur, r, &rhs.ty)?;
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
            HirExprKind::BuiltinCall { name, args } => self.compile_builtin_call(name, args),
            HirExprKind::MethodCall { receiver, method, args } => {
                self.compile_method_call(receiver, method, args, &e.ty)
            }
            HirExprKind::Array { elems, elem_ty } => self.compile_array(elems, elem_ty),
            HirExprKind::Index { array, index, elem_ty } => {
                self.compile_index(array, index, elem_ty)
            }
            HirExprKind::StrByteIndex { str_val, index } => {
                self.compile_str_byte_index(str_val, index)
            }
            HirExprKind::StrSlice { str_val, start, end, inclusive } => {
                self.compile_str_slice(str_val, start, end, *inclusive)
            }
            HirExprKind::For { local, iter, body, elem_ty, length } => {
                self.compile_for(*local, iter, body, elem_ty, *length)
            }
            HirExprKind::ForRange { local, start, end, inclusive, body } => {
                self.compile_for_range(*local, start, end, *inclusive, body)
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
            HirLit::Str(text) => return self.compile_str_literal(text),
            HirLit::Unit => return Ok(None),
        };
        Ok(Some(v))
    }

    fn compile_str_literal(&mut self, text: &str) -> Result<Option<Value>, CodegenError> {
        // 1. Get a pointer to the bytes. Empty strings use a null pointer
        //    (Cranelift's `define_data` rejects zero-length payloads, and
        //    `memcmp(_, _, 0)` is well-defined regardless of pointer value).
        let bytes_ptr = if text.is_empty() {
            self.builder.ins().iconst(types::I64, 0)
        } else {
            let data_name = format!("rune_str_{}", *self.next_str_id);
            *self.next_str_id += 1;
            let data_id = self
                .module
                .declare_data(&data_name, Linkage::Local, false, false)
                .map_err(|e| CodegenError(e.to_string()))?;
            let mut desc = DataDescription::new();
            desc.define(text.as_bytes().to_vec().into_boxed_slice());
            self.module
                .define_data(data_id, &desc)
                .map_err(|e| CodegenError(e.to_string()))?;
            let gv = self.module.declare_data_in_func(data_id, self.builder.func);
            self.builder.ins().symbol_value(types::I64, gv)
        };

        // 2. Build a 16-byte (ptr, len) descriptor on the stack.
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        self.builder.ins().stack_store(bytes_ptr, slot, 0);
        let len_const = self
            .builder
            .ins()
            .iconst(types::I64, text.len() as i64);
        self.builder.ins().stack_store(len_const, slot, 8);

        Ok(Some(self.builder.ins().stack_addr(types::I64, slot, 0)))
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
            HirBinOp::Add if matches!(ty, Ty::Str) => {
                let func_id = self.ensure_runtime_func("str_concat")?;
                let local_func = self
                    .module
                    .declare_func_in_func(func_id, self.builder.func);
                let inst = self.builder.ins().call(local_func, &[l, r]);
                self.builder.inst_results(inst)[0]
            }
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
            HirBinOp::Eq | HirBinOp::Ne if matches!(ty, Ty::Str) => {
                let func_id = self.ensure_runtime_func("str_eq")?;
                let local_func = self
                    .module
                    .declare_func_in_func(func_id, self.builder.func);
                let inst = self.builder.ins().call(local_func, &[l, r]);
                let eq = self.builder.inst_results(inst)[0];
                if matches!(op, HirBinOp::Ne) {
                    let one = self.builder.ins().iconst(types::I8, 1);
                    self.builder.ins().bxor(eq, one)
                } else {
                    eq
                }
            }
            HirBinOp::Eq => {
                if is_float { self.builder.ins().fcmp(FloatCC::Equal, l, r) }
                else { self.builder.ins().icmp(IntCC::Equal, l, r) }
            }
            HirBinOp::Ne => {
                if is_float { self.builder.ins().fcmp(FloatCC::NotEqual, l, r) }
                else { self.builder.ins().icmp(IntCC::NotEqual, l, r) }
            }
            HirBinOp::Lt => {
                if is_float { self.builder.ins().fcmp(FloatCC::LessThan, l, r) }
                else if signed { self.builder.ins().icmp(IntCC::SignedLessThan, l, r) }
                else { self.builder.ins().icmp(IntCC::UnsignedLessThan, l, r) }
            }
            HirBinOp::Gt => {
                if is_float { self.builder.ins().fcmp(FloatCC::GreaterThan, l, r) }
                else if signed { self.builder.ins().icmp(IntCC::SignedGreaterThan, l, r) }
                else { self.builder.ins().icmp(IntCC::UnsignedGreaterThan, l, r) }
            }
            HirBinOp::Le => {
                if is_float { self.builder.ins().fcmp(FloatCC::LessThanOrEqual, l, r) }
                else if signed { self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, l, r) }
                else { self.builder.ins().icmp(IntCC::UnsignedLessThanOrEqual, l, r) }
            }
            HirBinOp::Ge => {
                if is_float { self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, l, r) }
                else if signed { self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, l, r) }
                else { self.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, l, r) }
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
        let l = self
            .compile_expr(lhs)?
            .ok_or_else(|| CodegenError("logical lhs produced no value".into()))?;
        let then_blk = self.builder.create_block();
        let else_blk = self.builder.create_block();
        let merge_blk = self.builder.create_block();
        self.builder.append_block_param(merge_blk, types::I8);

        self.builder.ins().brif(l, then_blk, &[], else_blk, &[]);

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

        self.builder.switch_to_block(merge_blk);
        self.builder.seal_block(merge_blk);
        if produces_value {
            Ok(Some(self.builder.block_params(merge_blk)[0]))
        } else {
            Ok(None)
        }
    }

    fn compile_str_byte_index(
        &mut self,
        str_val: &HirExpr,
        index: &HirExpr,
    ) -> Result<Option<Value>, CodegenError> {
        let recv = self
            .compile_expr(str_val)?
            .ok_or_else(|| CodegenError("str receiver produced no value".into()))?;
        let i = self
            .compile_expr(index)?
            .ok_or_else(|| CodegenError("str index produced no value".into()))?;
        // descriptor layout: { ptr @ 0, len @ 8 }
        let bytes_ptr = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), recv, 0);
        let byte_addr = self.builder.ins().iadd(bytes_ptr, i);
        let byte = self
            .builder
            .ins()
            .load(types::I8, MemFlags::new(), byte_addr, 0);
        // Zero-extend to i64 — bytes are unsigned 0..=255.
        let widened = self.builder.ins().uextend(types::I64, byte);
        Ok(Some(widened))
    }

    fn compile_str_slice(
        &mut self,
        str_val: &HirExpr,
        start: &HirExpr,
        end: &HirExpr,
        inclusive: bool,
    ) -> Result<Option<Value>, CodegenError> {
        let recv = self
            .compile_expr(str_val)?
            .ok_or_else(|| CodegenError("str receiver produced no value".into()))?;
        let start_v = self
            .compile_expr(start)?
            .ok_or_else(|| CodegenError("slice start produced no value".into()))?;
        let end_v = self
            .compile_expr(end)?
            .ok_or_else(|| CodegenError("slice end produced no value".into()))?;
        // `s[a..=b]` becomes `s[a..(b+1)]` so the runtime can stay
        // ignorant of inclusivity.
        let end_v = if inclusive {
            let one = self.builder.ins().iconst(types::I64, 1);
            self.builder.ins().iadd(end_v, one)
        } else {
            end_v
        };
        let func_id = self.ensure_runtime_func("str_slice")?;
        let local_func = self
            .module
            .declare_func_in_func(func_id, self.builder.func);
        let inst = self.builder.ins().call(local_func, &[recv, start_v, end_v]);
        Ok(Some(self.builder.inst_results(inst)[0]))
    }

    fn compile_method_call(
        &mut self,
        receiver: &HirExpr,
        method: &str,
        args: &[HirExpr],
        _ret_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let recv_val = self
            .compile_expr(receiver)?
            .ok_or_else(|| CodegenError("method receiver produced no value".into()))?;
        // Compile args eagerly (preserves side effects in source order).
        // Arms that don't use them still get the IR emitted.
        let mut arg_vals: Vec<Value> = Vec::with_capacity(args.len());
        for a in args {
            let v = self
                .compile_expr(a)?
                .ok_or_else(|| CodegenError("method arg produced no value".into()))?;
            arg_vals.push(v);
        }
        match (&receiver.ty, method) {
            (Ty::Str, "len") => {
                // Descriptor layout: { ptr: i64 @ 0, len: i64 @ 8 }
                let len = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::new(), recv_val, 8);
                Ok(Some(len))
            }
            (Ty::Str, "is_empty") => {
                let len = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::new(), recv_val, 8);
                let zero = self.builder.ins().iconst(types::I64, 0);
                let is_zero = self.builder.ins().icmp(IntCC::Equal, len, zero);
                Ok(Some(is_zero))
            }
            (Ty::Array(_, length), "len") => {
                // Array length is statically known. The receiver expression
                // was compiled for side effects; the result is just the
                // constant.
                let len = self.builder.ins().iconst(types::I64, *length as i64);
                Ok(Some(len))
            }
            (Ty::Str, m) if matches!(m, "starts_with" | "ends_with" | "contains") => {
                let runtime_key = match m {
                    "starts_with" => "str_starts_with",
                    "ends_with" => "str_ends_with",
                    "contains" => "str_contains",
                    _ => unreachable!(),
                };
                let func_id = self.ensure_runtime_func(runtime_key)?;
                let local_func = self
                    .module
                    .declare_func_in_func(func_id, self.builder.func);
                let inst = self.builder.ins().call(local_func, &[recv_val, arg_vals[0]]);
                Ok(Some(self.builder.inst_results(inst)[0]))
            }
            (recv_ty, _) => Err(CodegenError(format!(
                "method `.{}` on `{}` is not implemented",
                method,
                recv_ty.display()
            ))),
        }
    }

    fn compile_builtin_call(
        &mut self,
        name: &str,
        args: &[HirExpr],
    ) -> Result<Option<Value>, CodegenError> {
        let func_id = self.ensure_runtime_func(name)?;
        let local_func = self.module.declare_func_in_func(func_id, self.builder.func);
        let mut arg_vals = Vec::with_capacity(args.len());
        for a in args {
            let v = self
                .compile_expr(a)?
                .ok_or_else(|| CodegenError("builtin arg produced no value".into()))?;
            arg_vals.push(v);
        }
        let inst = self.builder.ins().call(local_func, &arg_vals);
        let results = self.builder.inst_results(inst);
        if results.is_empty() { Ok(None) } else { Ok(Some(results[0])) }
    }

    fn ensure_runtime_func(&mut self, name: &str) -> Result<FuncId, CodegenError> {
        if let Some(&id) = self.builtin_funcs.get(name) {
            return Ok(id);
        }
        let id = declare_builtin(self.module, name)?;
        self.builtin_funcs.insert(name.to_string(), id);
        Ok(id)
    }

    fn compile_array(
        &mut self,
        elems: &[HirExpr],
        elem_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let elem_cty = cranelift_type(elem_ty)?;
        let esize = elem_size(elem_ty)?;
        if elems.is_empty() {
            return Err(CodegenError("empty arrays not yet supported".into()));
        }
        let total_size = (elems.len() as u32) * esize;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            total_size,
            3,
        ));
        for (i, elem) in elems.iter().enumerate() {
            let v = self
                .compile_expr(elem)?
                .ok_or_else(|| CodegenError("array element produced no value".into()))?;
            let offset = (i as i32) * (esize as i32);
            self.builder.ins().stack_store(v, slot, offset);
        }
        let _ = elem_cty; // suppress unused
        let addr = self.builder.ins().stack_addr(types::I64, slot, 0);
        Ok(Some(addr))
    }

    fn compile_index(
        &mut self,
        array: &HirExpr,
        index: &HirExpr,
        elem_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let arr_addr = self
            .compile_expr(array)?
            .ok_or_else(|| CodegenError("array operand produced no value".into()))?;
        let idx = self
            .compile_expr(index)?
            .ok_or_else(|| CodegenError("index operand produced no value".into()))?;
        let elem_cty = cranelift_type(elem_ty)?;
        let esize = elem_size(elem_ty)? as i64;
        let esize_const = self.builder.ins().iconst(types::I64, esize);
        let offset = self.builder.ins().imul(idx, esize_const);
        let elem_addr = self.builder.ins().iadd(arr_addr, offset);
        let val = self.builder.ins().load(elem_cty, MemFlags::new(), elem_addr, 0);
        Ok(Some(val))
    }

    fn compile_for(
        &mut self,
        local: Option<SymbolId>,
        iter: &HirExpr,
        body: &HirBlock,
        elem_ty: &Ty,
        length: usize,
    ) -> Result<Option<Value>, CodegenError> {
        let arr_addr = self
            .compile_expr(iter)?
            .ok_or_else(|| CodegenError("for-loop iterator produced no value".into()))?;
        let elem_cty = cranelift_type(elem_ty)?;
        let esize = elem_size(elem_ty)? as i64;

        let counter_var = self.alloc_var();
        self.builder.declare_var(counter_var, types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.def_var(counter_var, zero);

        let x_var = local.map(|sym| {
            let v = self.alloc_var();
            self.builder.declare_var(v, elem_cty);
            self.var_map.insert(sym, v);
            v
        });

        let header = self.builder.create_block();
        let body_blk = self.builder.create_block();
        let exit = self.builder.create_block();

        self.builder.ins().jump(header, &[]);

        self.builder.switch_to_block(header);
        let counter = self.builder.use_var(counter_var);
        let n_const = self.builder.ins().iconst(types::I64, length as i64);
        let cond = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, counter, n_const);
        self.builder.ins().brif(cond, body_blk, &[], exit, &[]);

        self.builder.switch_to_block(body_blk);
        self.builder.seal_block(body_blk);

        let counter = self.builder.use_var(counter_var);
        let esize_const = self.builder.ins().iconst(types::I64, esize);
        let offset = self.builder.ins().imul(counter, esize_const);
        let elem_addr = self.builder.ins().iadd(arr_addr, offset);
        let elem = self
            .builder
            .ins()
            .load(elem_cty, MemFlags::new(), elem_addr, 0);
        if let Some(v) = x_var {
            self.builder.def_var(v, elem);
        }

        self.compile_block(body)?;

        if !self.is_filled() {
            let counter = self.builder.use_var(counter_var);
            let one = self.builder.ins().iconst(types::I64, 1);
            let next = self.builder.ins().iadd(counter, one);
            self.builder.def_var(counter_var, next);
            self.builder.ins().jump(header, &[]);
        }

        self.builder.seal_block(header);
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        Ok(None)
    }

    fn compile_for_range(
        &mut self,
        local: Option<SymbolId>,
        start: &HirExpr,
        end: &HirExpr,
        inclusive: bool,
        body: &HirBlock,
    ) -> Result<Option<Value>, CodegenError> {
        let start_v = self
            .compile_expr(start)?
            .ok_or_else(|| CodegenError("range start produced no value".into()))?;
        let end_v_raw = self
            .compile_expr(end)?
            .ok_or_else(|| CodegenError("range end produced no value".into()))?;
        // Inclusive: fold `end+1` so the loop body uses `i < end` throughout.
        let end_v = if inclusive {
            let one = self.builder.ins().iconst(types::I64, 1);
            self.builder.ins().iadd(end_v_raw, one)
        } else {
            end_v_raw
        };

        // Counter holds the current iteration value; bind it to `local`
        // so the body can read it as the loop variable.
        let counter_var = self.alloc_var();
        self.builder.declare_var(counter_var, types::I64);
        self.builder.def_var(counter_var, start_v);
        if let Some(sym) = local {
            self.var_map.insert(sym, counter_var);
        }

        let header = self.builder.create_block();
        let body_blk = self.builder.create_block();
        let exit = self.builder.create_block();

        self.builder.ins().jump(header, &[]);

        self.builder.switch_to_block(header);
        let counter = self.builder.use_var(counter_var);
        let cond = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, counter, end_v);
        self.builder.ins().brif(cond, body_blk, &[], exit, &[]);

        self.builder.switch_to_block(body_blk);
        self.builder.seal_block(body_blk);
        self.compile_block(body)?;
        if !self.is_filled() {
            let counter = self.builder.use_var(counter_var);
            let one = self.builder.ins().iconst(types::I64, 1);
            let next = self.builder.ins().iadd(counter, one);
            self.builder.def_var(counter_var, next);
            self.builder.ins().jump(header, &[]);
        }

        self.builder.seal_block(header);
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        Ok(None)
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
        // Arrays are represented as a pointer to their first element.
        Ty::Array(_, _) => types::I64,
        // Strings are represented as a pointer to a (ptr, len) descriptor.
        Ty::Str => types::I64,
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

/// Element width in bytes — used for array stride computation.
fn elem_size(ty: &Ty) -> Result<u32, CodegenError> {
    Ok(match ty {
        Ty::Bool => 1,
        Ty::Int(IntTy::I8 | IntTy::U8) => 1,
        Ty::Int(IntTy::I16 | IntTy::U16) => 2,
        Ty::Int(IntTy::I32 | IntTy::U32) | Ty::Char | Ty::Float(FloatTy::F32) => 4,
        Ty::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize)
        | Ty::Float(FloatTy::F64) => 8,
        Ty::Array(_, _) | Ty::Str => 8,
        _ => {
            return Err(CodegenError(format!(
                "cannot determine size of `{}`",
                ty.display()
            )));
        }
    })
}

fn declare_builtin<M: Module>(module: &mut M, name: &str) -> Result<FuncId, CodegenError> {
    let (runtime_name, sig) = match name {
        "print_i64" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            ("rune_print_i64", sig)
        }
        "print_str" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *const RuneStr
            ("rune_print_str", sig)
        }
        // Internal-only runtime helper: codegen calls this for `==`/`!=`
        // on `str` operands. Not surfaced through the resolver.
        "str_eq" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // a
            sig.params.push(AbiParam::new(types::I64)); // b
            sig.returns.push(AbiParam::new(types::I8));
            ("rune_str_eq", sig)
        }
        // Internal-only: codegen calls this for `+` on `str` operands.
        // Allocates a new descriptor + bytes on the heap (process-lifetime).
        "str_concat" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_str_concat", sig)
        }
        // Internal-only: codegen calls this for `s[a..b]` slicing.
        // (s, start, end) → heap-allocated substring descriptor.
        "str_slice" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // s
            sig.params.push(AbiParam::new(types::I64)); // start
            sig.params.push(AbiParam::new(types::I64)); // end (exclusive)
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_str_slice", sig)
        }
        // String predicates — all share the same ABI: (a, b) → i8 bool.
        "str_starts_with" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I8));
            ("rune_str_starts_with", sig)
        }
        "str_ends_with" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I8));
            ("rune_str_ends_with", sig)
        }
        "str_contains" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I8));
            ("rune_str_contains", sig)
        }
        _ => return Err(CodegenError(format!("unknown builtin `{}`", name))),
    };
    module
        .declare_function(runtime_name, Linkage::Import, &sig)
        .map_err(|e| CodegenError(e.to_string()))
}
