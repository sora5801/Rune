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
    /// Per-struct ARC-managed fields (offset, ty). Populated by
    /// `compile_module` from the HirModule. Used by FnCodegen to emit
    /// per-field retain on struct construction / release on drop.
    struct_arc_fields: HashMap<SymbolId, Vec<(u32, Ty)>>,
    /// Per-struct field-area size. Used for `struct_new(size)` and
    /// `struct_dealloc(ptr, size)` calls.
    struct_sizes: HashMap<SymbolId, u32>,
    /// FuncId of the synthesized release function for each user
    /// struct. Populated up front so per-struct release can call
    /// other structs' release (for nested struct fields).
    struct_release_funcs: HashMap<SymbolId, FuncId>,
    /// Enums whose values are heap-allocated `{ tag, payload, rc }`
    /// descriptors (i.e., the enum has at least one payload variant).
    enum_has_payload: std::collections::HashSet<SymbolId>,
    /// Per-enum payload types per variant (indexed by discriminant).
    enum_payload_tys: HashMap<SymbolId, Vec<Vec<Ty>>>,
    /// FuncId of the synthesized release function per payload enum.
    enum_release_funcs: HashMap<SymbolId, FuncId>,
    /// FuncId of the synthesized per-element-type release function for
    /// each ARC-managed `Vec<T>` element type. Keyed by the element
    /// `Ty`; the function walks a Vec's live elements releasing each.
    vec_release_funcs: HashMap<Ty, FuncId>,
    /// Per-trait ordered method names — the trait-object method-table
    /// layout. `(type_sym, method) → impl fn sym` for building tables.
    trait_methods: HashMap<SymbolId, Vec<String>>,
    /// Per-trait *flattened* method list: the trait's own methods
    /// followed by every supertrait method in BFS order, deduped
    /// first-wins. The vec's index is the method's slot in a `dyn`
    /// box laid out for that trait. A `dyn Sub` box and a `dyn Super`
    /// box are distinct types with distinct slot orderings — both
    /// keys exist here. Each entry's first component is the
    /// *owning* trait sym (kept for documentation; `impl_methods` is
    /// keyed by `(struct, method_name)` and doesn't need it today).
    trait_methods_flat: HashMap<SymbolId, Vec<(SymbolId, String)>>,
    impl_methods: HashMap<(SymbolId, String), SymbolId>,
    /// FuncId of the synthesized per-trait `dyn` release function. The
    /// box decrements its rc and, at zero, drops the boxed data
    /// through the box's drop slot and frees the box.
    dyn_release_funcs: HashMap<SymbolId, FuncId>,
    /// FuncId of the synthesized release function per distinct array
    /// type. A heap array is a refcounted block; its release walks
    /// the ARC elements and frees the block at zero.
    array_release_funcs: HashMap<Ty, FuncId>,
}

// The Rune runtime lives in `runtime.c`, the single source of
// truth, compiled into this binary by `build.rs`. These
// declarations only let the JIT register each symbol's address —
// the JIT-compiled program is what actually calls them, and the
// AOT path links `runtime.c` directly. Pointers are declared as
// `*mut u8` since Rust never dereferences them here.
unsafe extern "C" {
    fn rune_print_i64(x: i64);
    fn rune_print_str(s: *const u8);
    fn rune_str_eq(a: *const u8, b: *const u8) -> i8;
    fn rune_str_concat(a: *const u8, b: *const u8) -> *mut u8;
    fn rune_str_slice(s: *const u8, start: i64, end: i64) -> *mut u8;
    fn rune_str_starts_with(s: *const u8, prefix: *const u8) -> i8;
    fn rune_str_ends_with(s: *const u8, suffix: *const u8) -> i8;
    fn rune_str_contains(s: *const u8, needle: *const u8) -> i8;
    fn rune_vec_new() -> *mut u8;
    fn rune_vec_push(v: *mut u8, x: i64);
    fn rune_vec_get(v: *const u8, i: i64) -> i64;
    fn rune_vec_len(v: *const u8) -> i64;
    fn rune_panic_bounds(idx: i64, len: i64);
    fn rune_panic_no_match();
    fn rune_retain_str(s: *mut u8);
    fn rune_release_str(s: *mut u8);
    fn rune_retain_vec(v: *mut u8);
    fn rune_release_vec(v: *mut u8);
    fn rune_weak_downgrade_vec(v: *mut u8) -> *mut u8;
    fn rune_weak_retain_vec(v: *mut u8);
    fn rune_weak_release_vec(v: *mut u8);
    fn rune_weak_upgrade_vec(v: *mut u8) -> *mut u8;
    fn rune_weak_upgrade_or_vec(w: *mut u8, def: *mut u8) -> *mut u8;
    fn rune_struct_new(size: i64) -> *mut u8;
    fn rune_struct_dealloc(p: *mut u8, size: i64);
}

// ---- generic methods: compile any module backend ----

impl<M: Module> Codegen<M> {
    pub fn compile_module(&mut self, hir: &HirModule) -> Result<(), CodegenError> {
        // Capture the struct-ARC-field map up front so each FnCodegen can
        // reference it by `&'a HashMap<...>`.
        self.struct_arc_fields = hir.struct_arc_fields.clone();
        self.struct_sizes = hir.struct_sizes.clone();
        self.enum_has_payload = hir.enum_has_payload.clone();
        self.enum_payload_tys = hir.enum_payload_tys.clone();
        self.trait_methods = hir.trait_methods.clone();
        self.trait_methods_flat = hir.trait_methods_flat.clone();
        self.impl_methods = hir.impl_methods.clone();
        // Pass 0: declare per-struct + per-enum release functions so
        // they can call each other (e.g. a struct with a nested
        // struct field, or an enum payload that's a struct).
        for &sym in hir.struct_sizes.keys() {
            let name = format!("__rune_release_struct${}", sym.0);
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.struct_release_funcs.insert(sym, id);
        }
        for &sym in &self.enum_has_payload.clone() {
            let name = format!("__rune_release_enum${}", sym.0);
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.enum_release_funcs.insert(sym, id);
        }
        // Pass 0 (cont.): declare a per-element-type release function
        // for each ARC-managed Vec element type. Its body walks the
        // live elements releasing each, so a `Vec<Vec<_>>` or
        // `Vec<Struct>` reclaims its contents.
        for elem in &hir.vec_arc_elem_tys {
            let name = format!("__rune_release_vec${}", mangle_ty_name(elem));
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.vec_release_funcs.insert(elem.clone(), id);
        }
        // Pass 0 (cont.): declare a per-trait `dyn` release function.
        // A trait object is a heap box `[fnptr_0..fnptr_{N-1}, data,
        // drop, rc]`; its release decrements rc and, at zero, calls
        // the drop slot (the concrete struct's release) and frees it.
        for &trait_sym in hir.trait_methods.keys() {
            let name = format!("__rune_release_dyn${}", trait_sym.0);
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.dyn_release_funcs.insert(trait_sym, id);
        }
        // Pass 0 (cont.): declare a release function per distinct
        // array type. A heap array is a refcounted block; its release
        // walks the ARC elements and frees the block at zero.
        for array_ty in &hir.array_tys {
            let name = format!("__rune_release_array${}", mangle_ty_name(array_ty));
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let id = self
                .module
                .declare_function(&name, Linkage::Local, &sig)
                .map_err(|e| CodegenError(e.to_string()))?;
            self.array_release_funcs.insert(array_ty.clone(), id);
        }
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
        // Pass 3: define synthesized struct + enum release functions.
        for (&sym, &func_id) in &self.struct_release_funcs.clone() {
            self.define_struct_release(sym, func_id)?;
        }
        for (&sym, &func_id) in &self.enum_release_funcs.clone() {
            self.define_enum_release(sym, func_id)?;
        }
        for (elem, &func_id) in &self.vec_release_funcs.clone() {
            self.define_vec_release(elem, func_id)?;
        }
        for (&sym, &func_id) in &self.dyn_release_funcs.clone() {
            self.define_dyn_release(sym, func_id)?;
        }
        for (array_ty, &func_id) in &self.array_release_funcs.clone() {
            self.define_array_release(array_ty, func_id)?;
        }
        Ok(())
    }

    /// Build a payload enum's release function. Layout depends on
    /// the enum's max variant arity: rc lives at `(8 + 8*max_arity)`.
    ///   load rc; if -1 return; rc--; if rc>0 return
    ///   load tag; per variant, release each ARC payload at its slot
    ///   call rune_struct_dealloc(p, field_size)
    fn define_enum_release(
        &mut self,
        enum_sym: SymbolId,
        func_id: FuncId,
    ) -> Result<(), CodegenError> {
        let payload_tys = self
            .enum_payload_tys
            .get(&enum_sym)
            .cloned()
            .unwrap_or_default();
        let max_arity = enum_max_arity(enum_sym, &self.enum_payload_tys);
        let field_size = 8 + 8 * max_arity as i32;
        let rc_offset = field_size;
        let mut ctx = self.module.make_context();
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let ptr = builder.block_params(entry)[0];

        let rc = builder
            .ins()
            .load(types::I64, MemFlags::new(), ptr, rc_offset);
        let sentinel = builder.ins().iconst(types::I64, -1);
        let is_sentinel = builder.ins().icmp(IntCC::Equal, rc, sentinel);
        let do_dec = builder.create_block();
        let done = builder.create_block();
        builder.ins().brif(is_sentinel, done, &[], do_dec, &[]);

        builder.switch_to_block(do_dec);
        builder.seal_block(do_dec);
        let one = builder.ins().iconst(types::I64, 1);
        let new_rc = builder.ins().isub(rc, one);
        builder
            .ins()
            .store(MemFlags::new(), new_rc, ptr, rc_offset);
        let zero = builder.ins().iconst(types::I64, 0);
        let alive = builder
            .ins()
            .icmp(IntCC::SignedGreaterThan, new_rc, zero);
        let do_free = builder.create_block();
        builder.ins().brif(alive, done, &[], do_free, &[]);

        builder.switch_to_block(do_free);
        builder.seal_block(do_free);
        let tag = builder
            .ins()
            .load(types::I64, MemFlags::new(), ptr, 0);
        let dealloc_blk = builder.create_block();
        for (disc, payloads) in payload_tys.iter().enumerate() {
            // Determine which payload fields of this variant are ARC.
            let arc_positions: Vec<(usize, Ty)> = payloads
                .iter()
                .enumerate()
                .filter(|(_, ty)| {
                    is_arc_type(ty, &self.struct_arc_fields, &self.enum_has_payload)
                })
                .map(|(i, ty)| (i, ty.clone()))
                .collect();
            if arc_positions.is_empty() {
                continue;
            }
            let disc_const = builder.ins().iconst(types::I64, disc as i64);
            let is_this = builder.ins().icmp(IntCC::Equal, tag, disc_const);
            let release_blk = builder.create_block();
            let next_blk = builder.create_block();
            builder
                .ins()
                .brif(is_this, release_blk, &[], next_blk, &[]);
            builder.switch_to_block(release_blk);
            builder.seal_block(release_blk);
            for (i, payload_ty) in &arc_positions {
                let raw = builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    ptr,
                    8 + 8 * (*i) as i32,
                );
                let pcty = cranelift_type(payload_ty)?;
                let val = if pcty == types::I64 {
                    raw
                } else {
                    builder.ins().ireduce(pcty, raw)
                };
                self.emit_release_field(&mut builder, payload_ty, val)?;
            }
            builder.ins().jump(dealloc_blk, &[]);
            builder.switch_to_block(next_blk);
            builder.seal_block(next_blk);
        }
        builder.ins().jump(dealloc_blk, &[]);

        builder.switch_to_block(dealloc_blk);
        builder.seal_block(dealloc_blk);
        // Reuse the struct dealloc helper — it handles any
        // `field_size+8` heap block.
        let dealloc_id = self.ensure_runtime_func("struct_dealloc")?;
        let dealloc_local = self.module.declare_func_in_func(dealloc_id, builder.func);
        let size_const = builder.ins().iconst(types::I64, field_size as i64);
        builder.ins().call(dealloc_local, &[ptr, size_const]);
        builder.ins().jump(done, &[]);

        builder.switch_to_block(done);
        builder.seal_block(done);
        builder.ins().return_(&[]);
        builder.finalize();

        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// Build a struct's synthesized release function. Layout:
    ///   load rc at offset `size`
    ///   if rc == -1: return            (sentinel — not yet used by structs)
    ///   rc -= 1; store rc
    ///   if rc > 0: return
    ///   for each ARC field: load + emit_arc_call(release)
    ///   call rune_struct_dealloc(ptr, size)
    ///   return
    fn define_struct_release(
        &mut self,
        struct_sym: SymbolId,
        func_id: FuncId,
    ) -> Result<(), CodegenError> {
        let size = *self.struct_sizes.get(&struct_sym).unwrap_or(&0);
        let arc_fields = self
            .struct_arc_fields
            .get(&struct_sym)
            .cloned()
            .unwrap_or_default();
        let mut ctx = self.module.make_context();
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let ptr = builder.block_params(entry)[0];
        // Load rc and decrement.
        let rc = builder.ins().load(types::I64, MemFlags::new(), ptr, size as i32);
        let one = builder.ins().iconst(types::I64, 1);
        let new_rc = builder.ins().isub(rc, one);
        builder
            .ins()
            .store(MemFlags::new(), new_rc, ptr, size as i32);
        // if new_rc > 0 → just return.
        let zero = builder.ins().iconst(types::I64, 0);
        let alive = builder
            .ins()
            .icmp(IntCC::SignedGreaterThan, new_rc, zero);
        let do_free = builder.create_block();
        let done = builder.create_block();
        builder.ins().brif(alive, done, &[], do_free, &[]);
        // do_free: release ARC fields, then dealloc.
        builder.switch_to_block(do_free);
        builder.seal_block(do_free);
        for (offset, ty) in &arc_fields {
            let cty = cranelift_type(ty)?;
            let field_val = builder
                .ins()
                .load(cty, MemFlags::new(), ptr, *offset as i32);
            // Emit a release call for the field. For a Vec/Str/Enum
            // it's a runtime call; for a nested Struct it's the
            // synthesized release we declared up front.
            self.emit_release_field(&mut builder, ty, field_val)?;
        }
        let dealloc_id = self.ensure_runtime_func("struct_dealloc")?;
        let dealloc_local = self.module.declare_func_in_func(dealloc_id, builder.func);
        let size_const = builder.ins().iconst(types::I64, size as i64);
        builder.ins().call(dealloc_local, &[ptr, size_const]);
        builder.ins().jump(done, &[]);
        // done: return.
        builder.switch_to_block(done);
        builder.seal_block(done);
        builder.ins().return_(&[]);
        builder.finalize();
        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// Build a heap array's release function. Layout: `N` element
    /// slots (`elem_size` bytes each) then the rc, at the 8-aligned
    /// `array_field_size`. Decrement rc; at zero release each ARC
    /// element, then `rune_struct_dealloc`.
    fn define_array_release(
        &mut self,
        array_ty: &Ty,
        func_id: FuncId,
    ) -> Result<(), CodegenError> {
        let Ty::Array(elem, n) = array_ty else {
            return Err(CodegenError("define_array_release: not an array".into()));
        };
        let esize = elem_size(elem)? as i32;
        let field_size = array_field_size(elem, *n)?;
        let elem_arc =
            is_arc_type(elem, &self.struct_arc_fields, &self.enum_has_payload);
        let mut ctx = self.module.make_context();
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let ptr = builder.block_params(entry)[0];
        let rc = builder.ins().load(types::I64, MemFlags::new(), ptr, field_size);
        let one = builder.ins().iconst(types::I64, 1);
        let new_rc = builder.ins().isub(rc, one);
        builder.ins().store(MemFlags::new(), new_rc, ptr, field_size);
        let zero = builder.ins().iconst(types::I64, 0);
        let alive = builder
            .ins()
            .icmp(IntCC::SignedGreaterThan, new_rc, zero);
        let do_free = builder.create_block();
        let done = builder.create_block();
        builder.ins().brif(alive, done, &[], do_free, &[]);
        builder.switch_to_block(do_free);
        builder.seal_block(do_free);
        if elem_arc {
            let elem_cty = cranelift_type(elem)?;
            for i in 0..*n {
                let ev = builder.ins().load(
                    elem_cty,
                    MemFlags::new(),
                    ptr,
                    (i as i32) * esize,
                );
                self.emit_release_field(&mut builder, elem, ev)?;
            }
        }
        let dealloc_id = self.ensure_runtime_func("struct_dealloc")?;
        let dealloc_local = self.module.declare_func_in_func(dealloc_id, builder.func);
        let size_const = builder.ins().iconst(types::I64, field_size as i64);
        builder.ins().call(dealloc_local, &[ptr, size_const]);
        builder.ins().jump(done, &[]);
        builder.switch_to_block(done);
        builder.seal_block(done);
        builder.ins().return_(&[]);
        builder.finalize();
        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// Helper to emit a release call from inside a synthesized
    /// struct-release function. Mirrors `FnCodegen::emit_arc_call`
    /// but operates on a free-standing FunctionBuilder.
    fn emit_release_field(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        ty: &Ty,
        value: Value,
    ) -> Result<(), CodegenError> {
        // A heap array — call its synthesized per-type release.
        if let Ty::Array(..) = ty {
            let func_id = *self
                .array_release_funcs
                .get(ty)
                .ok_or_else(|| CodegenError("missing array release fn".into()))?;
            let local = self.module.declare_func_in_func(func_id, builder.func);
            builder.ins().call(local, &[value]);
            return Ok(());
        }
        // Nested struct or payload enum: call its synthesized release.
        if let Ty::Struct(sym, _) = ty {
            let func_id = *self
                .struct_release_funcs
                .get(sym)
                .ok_or_else(|| CodegenError("nested struct missing release".into()))?;
            let local = self.module.declare_func_in_func(func_id, builder.func);
            builder.ins().call(local, &[value]);
            return Ok(());
        }
        if let Ty::Enum(sym, _) = ty {
            if let Some(&func_id) = self.enum_release_funcs.get(sym) {
                let local = self.module.declare_func_in_func(func_id, builder.func);
                builder.ins().call(local, &[value]);
                return Ok(());
            }
            // Tag-only enum — value is just an i64 discriminant, no
            // heap descriptor, nothing to release. (Shouldn't reach
            // here in practice since is_arc_type returns false.)
            return Ok(());
        }
        // Trait object: the synthesized per-trait release.
        if let Ty::Dyn(sym, _) = ty {
            let func_id = *self
                .dyn_release_funcs
                .get(sym)
                .ok_or_else(|| CodegenError("missing dyn release fn".into()))?;
            let local = self.module.declare_func_in_func(func_id, builder.func);
            builder.ins().call(local, &[value]);
            return Ok(());
        }
        // Vec<T> with an ARC element type: synthesized per-element
        // release. A Vec of non-ARC elements has no entry and falls
        // through to the runtime helper (which frees only the array).
        if let Ty::Vec(elem) = ty {
            if let Some(&func_id) = self.vec_release_funcs.get(&**elem) {
                let local = self.module.declare_func_in_func(func_id, builder.func);
                builder.ins().call(local, &[value]);
                return Ok(());
            }
        }
        // Vec / Str: runtime helper.
        let helper = arc_helper_name("release", ty)?;
        let func_id = self.ensure_runtime_func(helper)?;
        let local = self.module.declare_func_in_func(func_id, builder.func);
        builder.ins().call(local, &[value]);
        Ok(())
    }

    /// Build the per-element-type release function for a `Vec<elem>`
    /// whose elements are ARC-managed. When the strong count is about
    /// to hit zero it walks the live elements releasing each, then
    /// hands off to the runtime `release_vec` (which does the rc
    /// decrement, element-array free, and weak release).
    fn define_vec_release(
        &mut self,
        elem: &Ty,
        func_id: FuncId,
    ) -> Result<(), CodegenError> {
        let mut ctx = self.module.make_context();
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let entry = builder.create_block();
        let walk = builder.create_block();
        let header = builder.create_block();
        let body = builder.create_block();
        let finish = builder.create_block();
        let done = builder.create_block();

        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let p = builder.block_params(entry)[0];
        // Null guard — a null Vec pointer has nothing to release.
        let zero = builder.ins().iconst(types::I64, 0);
        let is_null = builder.ins().icmp(IntCC::Equal, p, zero);
        builder.ins().brif(is_null, done, &[], walk, &[]);

        // walk: release elements only when this call zeroes the rc.
        // (Single-threaded — nothing changes rc between this peek and
        // the `release_vec` call in `finish`.)
        builder.switch_to_block(walk);
        builder.seal_block(walk);
        let i = Variable::new(0);
        builder.declare_var(i, types::I64);
        let zero_i = builder.ins().iconst(types::I64, 0);
        builder.def_var(i, zero_i);
        let rc = builder.ins().load(types::I64, MemFlags::new(), p, 24);
        let one = builder.ins().iconst(types::I64, 1);
        let will_zero = builder.ins().icmp(IntCC::Equal, rc, one);
        builder.ins().brif(will_zero, header, &[], finish, &[]);

        // header: i < len ?
        builder.switch_to_block(header);
        let len = builder.ins().load(types::I64, MemFlags::new(), p, 8);
        let iv = builder.use_var(i);
        let more = builder.ins().icmp(IntCC::SignedLessThan, iv, len);
        builder.ins().brif(more, body, &[], finish, &[]);

        // body: release element[i], then i++.
        builder.switch_to_block(body);
        builder.seal_block(body);
        let arr = builder.ins().load(types::I64, MemFlags::new(), p, 0);
        let iv2 = builder.use_var(i);
        let eight = builder.ins().iconst(types::I64, 8);
        let off = builder.ins().imul(iv2, eight);
        let slot = builder.ins().iadd(arr, off);
        let elem_val = builder.ins().load(types::I64, MemFlags::new(), slot, 0);
        self.emit_release_field(&mut builder, elem, elem_val)?;
        let iv3 = builder.use_var(i);
        let one2 = builder.ins().iconst(types::I64, 1);
        let next = builder.ins().iadd(iv3, one2);
        builder.def_var(i, next);
        builder.ins().jump(header, &[]);
        builder.seal_block(header);

        // finish: standard runtime release (rc--, free array, weak).
        builder.switch_to_block(finish);
        builder.seal_block(finish);
        let rel = self.ensure_runtime_func("release_vec")?;
        let rel_local = self.module.declare_func_in_func(rel, builder.func);
        builder.ins().call(rel_local, &[p]);
        builder.ins().jump(done, &[]);

        builder.switch_to_block(done);
        builder.seal_block(done);
        builder.ins().return_(&[]);
        builder.finalize();
        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// Build a trait object's release function. The `dyn` box is a
    /// heap cell `[fnptr_0..fnptr_{N-1}, data, drop, rc]` — field area
    /// `(N+2)*8`, rc appended by `struct_new`. `N` is the flattened
    /// method count (the trait's own methods + every supertrait
    /// method); a `dyn Sub` and `dyn Super` box thus have different
    /// sizes and ordering even though they ultimately hand off to
    /// the same per-struct release function via the drop slot.
    /// Decrement rc; at zero, call the drop slot (the concrete
    /// struct's synthesized release) on the data pointer, then free
    /// the box itself.
    fn define_dyn_release(
        &mut self,
        trait_sym: SymbolId,
        func_id: FuncId,
    ) -> Result<(), CodegenError> {
        let n = self
            .trait_methods_flat
            .get(&trait_sym)
            .map(|m| m.len())
            .unwrap_or(0);
        let data_off = (n * 8) as i32;
        let drop_off = ((n + 1) * 8) as i32;
        let field_size = ((n + 2) * 8) as i64;
        let rc_off = field_size as i32;
        let mut ctx = self.module.make_context();
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        ctx.func.signature = sig;
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let ptr = builder.block_params(entry)[0];
        // rc -= 1; store.
        let rc = builder.ins().load(types::I64, MemFlags::new(), ptr, rc_off);
        let one = builder.ins().iconst(types::I64, 1);
        let new_rc = builder.ins().isub(rc, one);
        builder.ins().store(MemFlags::new(), new_rc, ptr, rc_off);
        let zero = builder.ins().iconst(types::I64, 0);
        let alive = builder
            .ins()
            .icmp(IntCC::SignedGreaterThan, new_rc, zero);
        let do_free = builder.create_block();
        let done = builder.create_block();
        builder.ins().brif(alive, done, &[], do_free, &[]);
        // do_free: drop the boxed data through the drop slot, free box.
        builder.switch_to_block(do_free);
        builder.seal_block(do_free);
        let data = builder
            .ins()
            .load(types::I64, MemFlags::new(), ptr, data_off);
        let drop_fn = builder
            .ins()
            .load(types::I64, MemFlags::new(), ptr, drop_off);
        let mut drop_sig = self.module.make_signature();
        drop_sig.params.push(AbiParam::new(types::I64));
        let drop_sig_ref = builder.import_signature(drop_sig);
        builder
            .ins()
            .call_indirect(drop_sig_ref, drop_fn, &[data]);
        let dealloc_id = self.ensure_runtime_func("struct_dealloc")?;
        let dealloc_local = self.module.declare_func_in_func(dealloc_id, builder.func);
        let size_const = builder.ins().iconst(types::I64, field_size);
        builder.ins().call(dealloc_local, &[ptr, size_const]);
        builder.ins().jump(done, &[]);
        builder.switch_to_block(done);
        builder.seal_block(done);
        builder.ins().return_(&[]);
        builder.finalize();
        self.module
            .define_function(func_id, &mut ctx)
            .map_err(|e| CodegenError(e.to_string()))?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// Codegen-level mirror of `FnCodegen::ensure_runtime_func` — used
    /// when synthesizing helpers outside of a Rune-function context.
    fn ensure_runtime_func(&mut self, name: &str) -> Result<FuncId, CodegenError> {
        if let Some(&id) = self.builtin_funcs.get(name) {
            return Ok(id);
        }
        let id = declare_builtin(&mut self.module, name)?;
        self.builtin_funcs.insert(name.to_string(), id);
        Ok(id)
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
            struct_arc_fields: &self.struct_arc_fields,
            struct_sizes: &self.struct_sizes,
            struct_release_funcs: &self.struct_release_funcs,
            enum_has_payload: &self.enum_has_payload,
            enum_release_funcs: &self.enum_release_funcs,
            enum_payload_tys: &self.enum_payload_tys,
            vec_release_funcs: &self.vec_release_funcs,
            trait_methods_flat: &self.trait_methods_flat,
            impl_methods: &self.impl_methods,
            dyn_release_funcs: &self.dyn_release_funcs,
            array_release_funcs: &self.array_release_funcs,
            builder,
            var_map: HashMap::new(),
            var_counter: 0,
            arc_locals: Vec::new(),
            loop_exit_stack: Vec::new(),
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
        // Point each runtime symbol at the `runtime.c` function of
        // the same name, linked into this binary by `build.rs`.
        builder.symbol("rune_print_i64", rune_print_i64 as *const u8);
        builder.symbol("rune_print_str", rune_print_str as *const u8);
        builder.symbol("rune_str_eq", rune_str_eq as *const u8);
        builder.symbol("rune_str_concat", rune_str_concat as *const u8);
        builder.symbol("rune_str_slice", rune_str_slice as *const u8);
        builder.symbol("rune_str_starts_with", rune_str_starts_with as *const u8);
        builder.symbol("rune_str_ends_with", rune_str_ends_with as *const u8);
        builder.symbol("rune_str_contains", rune_str_contains as *const u8);
        builder.symbol("rune_vec_new", rune_vec_new as *const u8);
        builder.symbol("rune_vec_push", rune_vec_push as *const u8);
        builder.symbol("rune_vec_get", rune_vec_get as *const u8);
        builder.symbol("rune_vec_len", rune_vec_len as *const u8);
        builder.symbol("rune_panic_bounds", rune_panic_bounds as *const u8);
        builder.symbol("rune_panic_no_match", rune_panic_no_match as *const u8);
        builder.symbol("rune_retain_str", rune_retain_str as *const u8);
        builder.symbol("rune_release_str", rune_release_str as *const u8);
        builder.symbol("rune_retain_vec", rune_retain_vec as *const u8);
        builder.symbol("rune_release_vec", rune_release_vec as *const u8);
        builder.symbol("rune_weak_downgrade_vec", rune_weak_downgrade_vec as *const u8);
        builder.symbol("rune_weak_retain_vec", rune_weak_retain_vec as *const u8);
        builder.symbol("rune_weak_release_vec", rune_weak_release_vec as *const u8);
        builder.symbol("rune_weak_upgrade_vec", rune_weak_upgrade_vec as *const u8);
        builder.symbol("rune_weak_upgrade_or_vec", rune_weak_upgrade_or_vec as *const u8);
        builder.symbol("rune_struct_new", rune_struct_new as *const u8);
        builder.symbol("rune_struct_dealloc", rune_struct_dealloc as *const u8);
        let module = JITModule::new(builder);
        Ok(Self {
            module,
            sym_to_func: HashMap::new(),
            sym_to_sig: HashMap::new(),
            builtin_funcs: HashMap::new(),
            next_str_id: 0,
            struct_arc_fields: HashMap::new(),
            struct_sizes: HashMap::new(),
            struct_release_funcs: HashMap::new(),
            enum_has_payload: std::collections::HashSet::new(),
            enum_payload_tys: HashMap::new(),
            enum_release_funcs: HashMap::new(),
            vec_release_funcs: HashMap::new(),
            trait_methods: HashMap::new(),
            trait_methods_flat: HashMap::new(),
            impl_methods: HashMap::new(),
            dyn_release_funcs: HashMap::new(),
            array_release_funcs: HashMap::new(),
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
            struct_arc_fields: HashMap::new(),
            struct_sizes: HashMap::new(),
            struct_release_funcs: HashMap::new(),
            enum_has_payload: std::collections::HashSet::new(),
            enum_payload_tys: HashMap::new(),
            enum_release_funcs: HashMap::new(),
            vec_release_funcs: HashMap::new(),
            trait_methods: HashMap::new(),
            trait_methods_flat: HashMap::new(),
            impl_methods: HashMap::new(),
            dyn_release_funcs: HashMap::new(),
            array_release_funcs: HashMap::new(),
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
    struct_arc_fields: &'a HashMap<SymbolId, Vec<(u32, Ty)>>,
    struct_sizes: &'a HashMap<SymbolId, u32>,
    struct_release_funcs: &'a HashMap<SymbolId, FuncId>,
    enum_has_payload: &'a std::collections::HashSet<SymbolId>,
    enum_release_funcs: &'a HashMap<SymbolId, FuncId>,
    enum_payload_tys: &'a HashMap<SymbolId, Vec<Vec<Ty>>>,
    vec_release_funcs: &'a HashMap<Ty, FuncId>,
    trait_methods_flat: &'a HashMap<SymbolId, Vec<(SymbolId, String)>>,
    impl_methods: &'a HashMap<(SymbolId, String), SymbolId>,
    dyn_release_funcs: &'a HashMap<SymbolId, FuncId>,
    array_release_funcs: &'a HashMap<Ty, FuncId>,
    builder: FunctionBuilder<'a>,
    var_map: HashMap<SymbolId, Variable>,
    var_counter: u32,
    /// Stack of locals that own a +1 ARC ref and need to be released
    /// at scope exit. Each entry is the Cranelift Variable holding the
    /// pointer plus the ARC type (Vec or Str) to pick the runtime helper.
    arc_locals: Vec<(Variable, Ty)>,
    /// Stack of enclosing loops — `(exit_block, arc_locals_len_at_entry)`.
    /// `Break` jumps to the top entry's exit block after releasing any
    /// ARC locals that were declared since the loop started. Pushed in
    /// `compile_while` / `compile_for` / `compile_for_range` at loop
    /// entry; popped in the same place after the loop body.
    loop_exit_stack: Vec<(Block, usize)>,
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
        let snapshot = self.arc_locals.len();
        let mut last_val: Option<Value> = None;
        let mut tail_escapes_local_arc_ty: Option<Ty> = None;
        for (i, s) in b.stmts.iter().enumerate() {
            last_val = self.compile_stmt(s)?;
            if self.is_filled() {
                break;
            }
            // Detect a tail expression statement (last in the block, no
            // semicolon) whose value is a borrowed Local read of an ARC
            // type. We need to retain it before the scope-exit release
            // below so the caller receives +1.
            if i + 1 == b.stmts.len() {
                if let HirStmt::Expr(e, false) = s {
                    if let HirExprKind::Local(_) = &e.kind {
                        if is_arc_type(&e.ty, self.struct_arc_fields, self.enum_has_payload) {
                            tail_escapes_local_arc_ty = Some(e.ty.clone());
                        }
                    }
                }
            }
        }
        // Retain a borrowed tail value if needed, then release everything
        // pushed during this block's scope.
        if !self.is_filled() {
            if let (Some(v), Some(ty)) = (last_val, tail_escapes_local_arc_ty) {
                self.emit_arc_call("retain", &ty, v)?;
            }
            self.release_arc_locals_to(snapshot)?;
        }
        self.arc_locals.truncate(snapshot);
        Ok(last_val)
    }

    /// Emit a release call on each arc_local from index `target` to the
    /// current end. Does not truncate the vector — callers truncate after
    /// emitting all paths that exit the scope.
    fn release_arc_locals_to(&mut self, target: usize) -> Result<(), CodegenError> {
        for i in (target..self.arc_locals.len()).rev() {
            let (var, ty) = (self.arc_locals[i].0, self.arc_locals[i].1.clone());
            let v = self.builder.use_var(var);
            self.emit_arc_call("release", &ty, v)?;
        }
        Ok(())
    }

    /// Emit retain or release for every active arc_local across all scopes.
    /// Used at return statements where execution leaves the function.
    fn release_all_arc_locals(&mut self) -> Result<(), CodegenError> {
        self.release_arc_locals_to(0)
    }

    fn emit_arc_call(
        &mut self,
        action: &str,
        ty: &Ty,
        value: Value,
    ) -> Result<(), CodegenError> {
        // A heap array is a refcounted block: retain bumps the rc at
        // its trailing slot; release dispatches to the synthesized
        // per-array-type release (walks ARC elements, frees at zero).
        if let Ty::Array(elem, n) = ty {
            match action {
                "retain" => {
                    let off = array_field_size(elem, *n)?;
                    let rc = self.builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        value,
                        off,
                    );
                    let one = self.builder.ins().iconst(types::I64, 1);
                    let new_rc = self.builder.ins().iadd(rc, one);
                    self.builder.ins().store(
                        MemFlags::new(),
                        new_rc,
                        value,
                        off,
                    );
                    return Ok(());
                }
                "release" => {
                    let func_id = *self
                        .array_release_funcs
                        .get(ty)
                        .ok_or_else(|| {
                            CodegenError("missing array release fn".into())
                        })?;
                    let local = self
                        .module
                        .declare_func_in_func(func_id, self.builder.func);
                    self.builder.ins().call(local, &[value]);
                    return Ok(());
                }
                _ => {}
            }
        }
        // Struct values: inline retain (rc++) or call the synthesized
        // per-struct release function (which does rc--, walks ARC
        // fields, and dealloc's at zero).
        if let Ty::Struct(sym, _) = ty {
            let size = *self.struct_sizes.get(sym).unwrap_or(&0);
            match action {
                "retain" => {
                    let rc = self.builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        value,
                        size as i32,
                    );
                    let one = self.builder.ins().iconst(types::I64, 1);
                    let new_rc = self.builder.ins().iadd(rc, one);
                    self.builder.ins().store(
                        MemFlags::new(),
                        new_rc,
                        value,
                        size as i32,
                    );
                    return Ok(());
                }
                "release" => {
                    let func_id = *self
                        .struct_release_funcs
                        .get(sym)
                        .ok_or_else(|| {
                            CodegenError("missing struct release fn".into())
                        })?;
                    let local = self
                        .module
                        .declare_func_in_func(func_id, self.builder.func);
                    self.builder.ins().call(local, &[value]);
                    return Ok(());
                }
                _ => {
                    return Err(CodegenError(format!(
                        "unknown ARC action `{}` on struct",
                        action
                    )));
                }
            }
        }
        // Payload enums: retain inline (rc++ at the per-enum rc
        // offset); release dispatches to the synthesized per-enum
        // function which walks the variant and dealloc's properly.
        if let Ty::Enum(sym, _) = ty {
            if self.enum_has_payload.contains(sym) {
                let max_arity = enum_max_arity(*sym, self.enum_payload_tys);
                let rc_offset = 8 + 8 * max_arity as i32;
                match action {
                    "retain" => {
                        let rc = self.builder.ins().load(
                            types::I64,
                            MemFlags::new(),
                            value,
                            rc_offset,
                        );
                        let one = self.builder.ins().iconst(types::I64, 1);
                        let new_rc = self.builder.ins().iadd(rc, one);
                        self.builder.ins().store(
                            MemFlags::new(),
                            new_rc,
                            value,
                            rc_offset,
                        );
                        return Ok(());
                    }
                    "release" => {
                        let func_id = *self
                            .enum_release_funcs
                            .get(sym)
                            .ok_or_else(|| {
                                CodegenError("missing enum release fn".into())
                            })?;
                        let local = self
                            .module
                            .declare_func_in_func(func_id, self.builder.func);
                        self.builder.ins().call(local, &[value]);
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
        // Trait objects: retain bumps the box's rc (the slot after
        // the N method pointers + data + drop); release dispatches to
        // the synthesized per-trait release, which drops the boxed
        // concrete value and frees the box.
        if let Ty::Dyn(sym, _) = ty {
            let n = self
                .trait_methods_flat
                .get(sym)
                .map(|m| m.len())
                .unwrap_or(0);
            let rc_offset = ((n + 2) * 8) as i32;
            match action {
                "retain" => {
                    let rc = self.builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        value,
                        rc_offset,
                    );
                    let one = self.builder.ins().iconst(types::I64, 1);
                    let new_rc = self.builder.ins().iadd(rc, one);
                    self.builder.ins().store(
                        MemFlags::new(),
                        new_rc,
                        value,
                        rc_offset,
                    );
                    return Ok(());
                }
                "release" => {
                    let func_id = *self
                        .dyn_release_funcs
                        .get(sym)
                        .ok_or_else(|| {
                            CodegenError("missing dyn release fn".into())
                        })?;
                    let local = self
                        .module
                        .declare_func_in_func(func_id, self.builder.func);
                    self.builder.ins().call(local, &[value]);
                    return Ok(());
                }
                _ => {}
            }
        }
        // Vec<T>: retain is the type-agnostic runtime helper (rc++);
        // release dispatches to the synthesized per-element release
        // when the element type is ARC-managed.
        if let Ty::Vec(elem) = ty {
            if action == "release" {
                if let Some(&func_id) = self.vec_release_funcs.get(&**elem) {
                    let local_func = self
                        .module
                        .declare_func_in_func(func_id, self.builder.func);
                    self.builder.ins().call(local_func, &[value]);
                    return Ok(());
                }
            }
        }
        let helper = arc_helper_name(action, ty)?;
        let func_id = self.ensure_runtime_func(helper)?;
        let local_func = self
            .module
            .declare_func_in_func(func_id, self.builder.func);
        self.builder.ins().call(local_func, &[value]);
        Ok(())
    }

    fn compile_stmt(&mut self, s: &HirStmt) -> Result<Option<Value>, CodegenError> {
        match s {
            HirStmt::Let(l) => {
                let cty = cranelift_type(&l.ty)?;
                let var = self.alloc_var();
                self.builder.declare_var(var, cty);
                let mut owns_arc = false;
                if let Some(init) = &l.init {
                    let v = self
                        .compile_expr(init)?
                        .ok_or_else(|| CodegenError("let initializer produced no value".into()))?;
                    // ARC-on-copy: a let from a borrowed Local read retains
                    // so the new binding owns +1. Fresh +1 producers (Call,
                    // Lit, Concat, etc.) need no retain — they already
                    // carry the +1.
                    if is_arc_type(&l.ty, self.struct_arc_fields, self.enum_has_payload) {
                        if let HirExprKind::Local(_) = &init.kind {
                            self.emit_arc_call("retain", &l.ty, v)?;
                        }
                        owns_arc = true;
                    }
                    self.builder.def_var(var, v);
                } else {
                    let z = self.builder.ins().iconst(cty, 0);
                    self.builder.def_var(var, z);
                }
                if let Some(sym) = l.sym {
                    self.var_map.insert(sym, var);
                }
                if owns_arc {
                    self.arc_locals.push((var, l.ty.clone()));
                }
                Ok(None)
            }
            HirStmt::Expr(e, has_semi) => {
                let v = self.compile_expr(e)?;
                if *has_semi {
                    // A discarded expression statement whose value is
                    // a fresh ARC temporary (not a borrowed `Local`)
                    // owns a +1 nobody will reclaim — release it.
                    if let Some(val) = v {
                        if is_arc_type(&e.ty, self.struct_arc_fields, self.enum_has_payload)
                            && !matches!(e.kind, HirExprKind::Local(_))
                        {
                            self.emit_arc_call("release", &e.ty, val)?;
                        }
                    }
                    Ok(None)
                } else {
                    Ok(v)
                }
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
            HirExprKind::Fn(sym) => {
                // Take the function's address. Used to pass a named
                // fn as a value into a struct field or a parameter of
                // `Ty::Fn` type — `Map { iter, f: double }` and
                // similar. The address is an 8-byte pointer; codegen
                // for an `IndirectCall` on this value emits
                // `call_indirect` with the signature built from the
                // expression's `Ty::Fn`.
                let func_id = *self.sym_to_func.get(sym).ok_or_else(|| {
                    CodegenError("fn-value references an unknown function".into())
                })?;
                let fref = self.module.declare_func_in_func(func_id, self.builder.func);
                Ok(Some(self.builder.ins().func_addr(types::I64, fref)))
            }
            HirExprKind::IndirectCall { callee, args } => {
                // Build the call signature from the callee's Ty::Fn,
                // compile each arg, load the function pointer, emit
                // `call_indirect`. Mirrors compile_dyn_call without
                // the box-slot indirection.
                let (param_tys, ret_ty) = match &callee.ty {
                    Ty::Fn { params, ret } => (params.clone(), (**ret).clone()),
                    other => {
                        return Err(CodegenError(format!(
                            "indirect call callee has non-fn type `{}`",
                            other.display()
                        )));
                    }
                };
                let mut arg_vals: Vec<Value> = Vec::with_capacity(args.len());
                for a in args {
                    arg_vals.push(
                        self.compile_expr(a)?
                            .ok_or_else(|| CodegenError("indirect call arg produced no value".into()))?,
                    );
                }
                let fnptr = self
                    .compile_expr(callee)?
                    .ok_or_else(|| CodegenError("indirect call callee produced no value".into()))?;
                let mut sig = self.module.make_signature();
                for t in &param_tys {
                    sig.params.push(AbiParam::new(cranelift_type(t)?));
                }
                if !matches!(ret_ty, Ty::Unit | Ty::Never) {
                    sig.returns.push(AbiParam::new(cranelift_type(&ret_ty)?));
                }
                let sig_ref = self.builder.import_signature(sig);
                let inst = self.builder.ins().call_indirect(sig_ref, fnptr, &arg_vals);
                let results = self.builder.inst_results(inst);
                if results.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(results[0]))
                }
            }
            HirExprKind::EnumVariant { discriminant } => {
                // Unit variant. If the enum has any payload-bearing
                // variant, allocate a per-enum-sized descriptor with
                // tag and rc set; payload slots stay zero. Otherwise
                // the value is just the i64 discriminant.
                if let Ty::Enum(enum_sym, _) = &e.ty {
                    if self.enum_has_payload.contains(enum_sym) {
                        let max_arity = enum_max_arity(*enum_sym, self.enum_payload_tys);
                        let field_size = 8 + 8 * max_arity as i64;
                        let size_const =
                            self.builder.ins().iconst(types::I64, field_size);
                        let alloc_id = self.ensure_runtime_func("struct_new")?;
                        let alloc_local = self
                            .module
                            .declare_func_in_func(alloc_id, self.builder.func);
                        let inst =
                            self.builder.ins().call(alloc_local, &[size_const]);
                        let ptr = self.builder.inst_results(inst)[0];
                        let tag = self
                            .builder
                            .ins()
                            .iconst(types::I64, *discriminant as i64);
                        self.builder.ins().store(MemFlags::new(), tag, ptr, 0);
                        return Ok(Some(ptr));
                    }
                }
                let v = self
                    .builder
                    .ins()
                    .iconst(types::I64, *discriminant as i64);
                Ok(Some(v))
            }
            HirExprKind::EnumPayloadCtor { enum_sym, discriminant, payloads } => {
                let max_arity = enum_max_arity(*enum_sym, self.enum_payload_tys);
                // Layout: tag@0, payload[i]@(8+i*8), rc@(8+max_arity*8).
                // Use rune_struct_new which mallocs `size+8` and inits
                // rc=1 at offset `size`.
                let field_size = 8 + 8 * max_arity as i64;
                let size_const = self.builder.ins().iconst(types::I64, field_size);
                let alloc_id = self.ensure_runtime_func("struct_new")?;
                let alloc_local = self
                    .module
                    .declare_func_in_func(alloc_id, self.builder.func);
                let alloc_inst = self.builder.ins().call(alloc_local, &[size_const]);
                let ptr = self.builder.inst_results(alloc_inst)[0];
                // Tag at offset 0.
                let tag = self
                    .builder
                    .ins()
                    .iconst(types::I64, *discriminant as i64);
                self.builder.ins().store(MemFlags::new(), tag, ptr, 0);
                // Payloads at 8 + i*8, retained if borrowed.
                for (i, p_expr) in payloads.iter().enumerate() {
                    let pay = self
                        .compile_expr(p_expr)?
                        .ok_or_else(|| {
                            CodegenError("variant payload produced no value".into())
                        })?;
                    if is_arc_type(&p_expr.ty, self.struct_arc_fields, self.enum_has_payload) {
                        if let HirExprKind::Local(_) = &p_expr.kind {
                            self.emit_arc_call("retain", &p_expr.ty, pay)?;
                        }
                    }
                    let pcty = cranelift_type(&p_expr.ty)?;
                    let pay_i64 = if pcty == types::I64 {
                        pay
                    } else if matches!(
                        p_expr.ty,
                        Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::ISize)
                    ) {
                        self.builder.ins().sextend(types::I64, pay)
                    } else {
                        self.builder.ins().uextend(types::I64, pay)
                    };
                    let offset = 8 + 8 * i as i32;
                    self.builder
                        .ins()
                        .store(MemFlags::new(), pay_i64, ptr, offset);
                }
                Ok(Some(ptr))
            }
            HirExprKind::Unary { op, expr } => self.compile_unary(*op, expr, &e.ty),
            HirExprKind::Cast { expr } => self.compile_cast(expr, &e.ty),
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
                // ARC swap: retain rhs if it's a borrowed Local read, then
                // release the old value held in the lhs binding, then store.
                if is_arc_type(&rhs.ty, self.struct_arc_fields, self.enum_has_payload) {
                    if let HirExprKind::Local(_) = &rhs.kind {
                        self.emit_arc_call("retain", &rhs.ty, v)?;
                    }
                    let old = self.builder.use_var(var);
                    self.emit_arc_call("release", &rhs.ty, old)?;
                }
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
                // For ARC types (str += str), binop produces a fresh +1
                // (concat allocates). Release the old value before storing.
                if is_arc_type(&rhs.ty, self.struct_arc_fields, self.enum_has_payload) {
                    self.emit_arc_call("release", &rhs.ty, cur)?;
                }
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
                let result = if results.is_empty() {
                    None
                } else {
                    Some(results[0])
                };
                // Owned call arguments: a fresh ARC temporary passed
                // as an argument — a struct literal, a call result, a
                // `dyn` box, anything but a borrowed `Local` — owns a
                // +1 that no binding will release. The callee only
                // borrows it, so the caller reclaims it once the call
                // returns. A `Local` argument stays owned by its
                // binding and is released at that scope's exit.
                for (a, &v) in args.iter().zip(&arg_vals) {
                    if is_arc_type(&a.ty, self.struct_arc_fields, self.enum_has_payload)
                        && !matches!(a.kind, HirExprKind::Local(_))
                    {
                        self.emit_arc_call("release", &a.ty, v)?;
                    }
                }
                Ok(result)
            }
            HirExprKind::BuiltinCall { name, args } => self.compile_builtin_call(name, args),
            HirExprKind::MethodCall { receiver, method, args } => {
                let recv_val = self
                    .compile_expr(receiver)?
                    .ok_or_else(|| CodegenError("method receiver produced no value".into()))?;
                let result =
                    self.compile_method_call(receiver, recv_val, method, args, &e.ty)?;
                self.release_receiver_temp(receiver, recv_val)?;
                Ok(result)
            }
            HirExprKind::DynBox { value, struct_sym, trait_sym } => {
                self.compile_dyn_box(value, *struct_sym, *trait_sym)
            }
            HirExprKind::DynCall { receiver, trait_sym, method, args } => {
                let recv_val = self
                    .compile_expr(receiver)?
                    .ok_or_else(|| CodegenError("dyn receiver produced no value".into()))?;
                let result =
                    self.compile_dyn_call(recv_val, *trait_sym, method, args, &e.ty)?;
                self.release_receiver_temp(receiver, recv_val)?;
                Ok(result)
            }
            HirExprKind::StructLit { sym: _, fields, size } => {
                self.compile_struct_lit(fields, *size)
            }
            HirExprKind::FieldAccess { receiver, offset, field_ty } => {
                self.compile_field_access(receiver, *offset, field_ty)
            }
            HirExprKind::FieldAssign { receiver, offset, field_ty, rhs } => {
                self.compile_field_assign(receiver, *offset, field_ty, rhs)
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
            HirExprKind::Match { scrutinee, arms } => {
                self.compile_match(scrutinee, arms, &e.ty)
            }
            HirExprKind::Return(value) => {
                let vals = if let Some(v) = value {
                    let val = self
                        .compile_expr(v)?
                        .ok_or_else(|| CodegenError("return value produced no value".into()))?;
                    // If the returned value is a borrowed Local read of an
                    // ARC type, retain it so the caller receives +1 after
                    // we release all locals below.
                    if let HirExprKind::Local(_) = &v.kind {
                        if is_arc_type(&v.ty, self.struct_arc_fields, self.enum_has_payload) {
                            self.emit_arc_call("retain", &v.ty, val)?;
                        }
                    }
                    vec![val]
                } else {
                    vec![]
                };
                self.release_all_arc_locals()?;
                self.builder.ins().return_(&vals);
                let after = self.builder.create_block();
                self.builder.switch_to_block(after);
                self.builder.seal_block(after);
                Ok(None)
            }
            HirExprKind::Break => {
                // Jump to the innermost enclosing loop's exit block.
                // Release any ARC locals declared since the loop entry —
                // the snapshot at `loop_exit_stack.last()` is the
                // arc_locals length just before the loop body started,
                // so anything past it is owned by an iteration that's
                // about to be abandoned.
                let &(exit, snapshot) = self.loop_exit_stack.last().ok_or_else(
                    || CodegenError("internal: `break` outside a loop reached codegen".into()),
                )?;
                self.release_arc_locals_to(snapshot)?;
                self.builder.ins().jump(exit, &[]);
                // Continue compiling into an unreachable trailer block,
                // matching `Return`'s pattern at line 1633. Anything in
                // the source position after `break` is dead code.
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
            HirLit::Char(c) => self.builder.ins().iconst(types::I32, *c as i64),
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

        // 2. Build a 24-byte (ptr, len, rc) descriptor on the stack.
        //    rc = -1 marks it as a literal so retain/release become no-ops.
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            24,
            3,
        ));
        self.builder.ins().stack_store(bytes_ptr, slot, 0);
        let len_const = self
            .builder
            .ins()
            .iconst(types::I64, text.len() as i64);
        self.builder.ins().stack_store(len_const, slot, 8);
        let sentinel = self.builder.ins().iconst(types::I64, -1);
        self.builder.ins().stack_store(sentinel, slot, 16);

        Ok(Some(self.builder.ins().stack_addr(types::I64, slot, 0)))
    }

    fn compile_cast(
        &mut self,
        expr: &HirExpr,
        dest_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let src_ty = expr.ty.clone();
        let v = self
            .compile_expr(expr)?
            .ok_or_else(|| CodegenError("cast operand produced no value".into()))?;
        if src_ty == *dest_ty {
            return Ok(Some(v));
        }
        let dest_cty = cranelift_type(dest_ty)?;
        let src_cty = cranelift_type(&src_ty)?;
        let src_is_signed_int = matches!(
            src_ty,
            Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize)
        );
        let src_is_int = matches!(src_ty, Ty::Int(_));
        let src_is_bool = matches!(src_ty, Ty::Bool);
        let src_is_char = matches!(src_ty, Ty::Char);
        let src_is_float = matches!(src_ty, Ty::Float(_));
        let dest_is_signed_int = matches!(
            dest_ty,
            Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize)
        );
        let dest_is_int = matches!(dest_ty, Ty::Int(_));
        let dest_is_bool = matches!(dest_ty, Ty::Bool);
        let dest_is_char = matches!(dest_ty, Ty::Char);
        let dest_is_float = matches!(dest_ty, Ty::Float(_));
        let src_bits = src_cty.bits();
        let dest_bits = dest_cty.bits();

        let result = if (src_is_int || src_is_bool || src_is_char)
            && (dest_is_int || dest_is_char)
        {
            // Integer-shaped → integer-shaped: extend or truncate.
            if dest_bits == src_bits {
                v
            } else if dest_bits > src_bits {
                if src_is_signed_int {
                    self.builder.ins().sextend(dest_cty, v)
                } else {
                    self.builder.ins().uextend(dest_cty, v)
                }
            } else {
                self.builder.ins().ireduce(dest_cty, v)
            }
        } else if (src_is_int || src_is_char) && dest_is_bool {
            // Int/char → bool: zero is false, anything else is true. Emit
            // `(v != 0) as i8`. icmp result is one bit but Cranelift
            // returns it as i8 already.
            let zero = self.builder.ins().iconst(src_cty, 0);
            self.builder.ins().icmp(IntCC::NotEqual, v, zero)
        } else if src_is_bool && dest_is_float {
            // bool → float via int.
            let as_i32 = self.builder.ins().uextend(types::I32, v);
            self.builder.ins().fcvt_from_uint(dest_cty, as_i32)
        } else if src_is_int && dest_is_float {
            if src_is_signed_int {
                self.builder.ins().fcvt_from_sint(dest_cty, v)
            } else {
                self.builder.ins().fcvt_from_uint(dest_cty, v)
            }
        } else if src_is_float && dest_is_int {
            // Saturating to match wrap-free semantics matching most
            // languages' "as" / static_cast on out-of-range floats.
            if dest_is_signed_int {
                self.builder.ins().fcvt_to_sint_sat(dest_cty, v)
            } else {
                self.builder.ins().fcvt_to_uint_sat(dest_cty, v)
            }
        } else if src_is_float && dest_is_float {
            if dest_bits > src_bits {
                self.builder.ins().fpromote(dest_cty, v)
            } else if dest_bits < src_bits {
                self.builder.ins().fdemote(dest_cty, v)
            } else {
                v
            }
        } else {
            return Err(CodegenError(format!(
                "no `as` codegen for `{}` -> `{}`",
                src_ty.display(),
                dest_ty.display()
            )));
        };
        Ok(Some(result))
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

    fn compile_struct_lit(
        &mut self,
        fields: &[(u32, HirExpr)],
        size: u32,
    ) -> Result<Option<Value>, CodegenError> {
        // Heap-allocate the descriptor so the value can escape the
        // function (return by value works). v0.x: the descriptor's
        // bytes leak at scope end — only ARC-managed fields are
        // released. A future session adds an rc + dealloc to close
        // the leak.
        let size_const = self
            .builder
            .ins()
            .iconst(types::I64, size.max(8) as i64);
        let func_id = self.ensure_runtime_func("struct_new")?;
        let local_func = self
            .module
            .declare_func_in_func(func_id, self.builder.func);
        let inst = self.builder.ins().call(local_func, &[size_const]);
        let base = self.builder.inst_results(inst)[0];
        for (offset, value) in fields {
            let v = self
                .compile_expr(value)?
                .ok_or_else(|| CodegenError("struct field produced no value".into()))?;
            // ARC: a field initialized from a borrowed Local read needs
            // a retain so the struct owns its own +1 of that field.
            // Fresh +1 producers (Call, Lit, etc.) already carry a +1.
            if is_arc_type(&value.ty, self.struct_arc_fields, self.enum_has_payload) {
                if let HirExprKind::Local(_) = &value.kind {
                    self.emit_arc_call("retain", &value.ty, v)?;
                }
            }
            self.builder
                .ins()
                .store(MemFlags::new(), v, base, *offset as i32);
        }
        Ok(Some(base))
    }

    fn compile_field_access(
        &mut self,
        receiver: &HirExpr,
        offset: u32,
        field_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let recv = self
            .compile_expr(receiver)?
            .ok_or_else(|| CodegenError("field-access receiver produced no value".into()))?;
        // Generic struct fields can have a TypeVar layout — the
        // monomorphizer only specializes functions, not structs.
        // Treat unresolved TypeVar fields as i64 since v0.x uses
        // 8-byte-per-field padding and most concrete instantiations
        // (Vec, Str, Struct, Enum pointers, i64) are i64-shaped.
        let cty = match field_ty {
            Ty::TypeVar(_) => types::I64,
            _ => cranelift_type(field_ty)?,
        };
        let val = self
            .builder
            .ins()
            .load(cty, MemFlags::new(), recv, offset as i32);
        // An ARC field read produces a new owner of the value, so it
        // gets its own +1 — independent of the field's. This makes a
        // `Field` expression a genuine fresh-+1 producer, matching
        // the fresh/borrowed (`Local`-vs-not) heuristic that `let`,
        // call arguments, struct construction, and `return` rely on.
        if is_arc_type(field_ty, self.struct_arc_fields, self.enum_has_payload) {
            self.emit_arc_call("retain", field_ty, val)?;
        }
        Ok(Some(val))
    }

    fn compile_field_assign(
        &mut self,
        receiver: &HirExpr,
        offset: u32,
        field_ty: &Ty,
        rhs: &HirExpr,
    ) -> Result<Option<Value>, CodegenError> {
        let recv = self
            .compile_expr(receiver)?
            .ok_or_else(|| CodegenError("field-assign receiver produced no value".into()))?;
        let val = self
            .compile_expr(rhs)?
            .ok_or_else(|| CodegenError("field-assign rhs produced no value".into()))?;
        let fcty = match field_ty {
            Ty::TypeVar(_) => types::I64,
            _ => cranelift_type(field_ty)?,
        };
        // ARC field assignment: release the old field value, retain the
        // new one if it's a borrowed Local read. Same retain rule as
        // let / Assign.
        if is_arc_type(field_ty, self.struct_arc_fields, self.enum_has_payload) {
            let old =
                self.builder
                    .ins()
                    .load(fcty, MemFlags::new(), recv, offset as i32);
            self.emit_arc_call("release", field_ty, old)?;
            if let HirExprKind::Local(_) = &rhs.kind {
                self.emit_arc_call("retain", field_ty, val)?;
            }
        }
        self.builder
            .ins()
            .store(MemFlags::new(), val, recv, offset as i32);
        Ok(None)
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
        let length = self
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), recv, 8);
        self.emit_bounds_check(i, length)?;
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

    /// Release a method-call receiver that is a fresh ARC temporary —
    /// anything but a borrowed `Local` read. A method only borrows
    /// its receiver, so a fresh `expr.method()` receiver (a call
    /// result, a field or index read, a `dyn` box) is the caller's
    /// to reclaim once the call returns. The receiver-position mirror
    /// of owned call arguments (session 036).
    fn release_receiver_temp(
        &mut self,
        receiver: &HirExpr,
        recv_val: Value,
    ) -> Result<(), CodegenError> {
        if is_arc_type(&receiver.ty, self.struct_arc_fields, self.enum_has_payload)
            && !matches!(receiver.kind, HirExprKind::Local(_))
        {
            self.emit_arc_call("release", &receiver.ty, recv_val)?;
        }
        Ok(())
    }

    fn compile_method_call(
        &mut self,
        receiver: &HirExpr,
        recv_val: Value,
        method: &str,
        args: &[HirExpr],
        _ret_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
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
            (Ty::Vec(elem), m) if matches!(m, "push" | "get" | "len") => {
                let elem_ty = (**elem).clone();
                let elem_cty = cranelift_type(&elem_ty)?;
                let elem_arc = is_arc_type(
                    &elem_ty,
                    self.struct_arc_fields,
                    self.enum_has_payload,
                );
                let runtime_key = match m {
                    "push" => "vec_push",
                    "get" => "vec_get",
                    "len" => "vec_len",
                    _ => unreachable!(),
                };
                let func_id = self.ensure_runtime_func(runtime_key)?;
                let local_func = self
                    .module
                    .declare_func_in_func(func_id, self.builder.func);
                let mut call_args = vec![recv_val];
                if m == "push" {
                    // Pushing a borrowed ARC element (a Local read)
                    // creates a second owner — retain so the Vec slot
                    // holds its own +1. A fresh +1 producer transfers
                    // its count straight in.
                    if elem_arc {
                        if let HirExprKind::Local(_) = &args[0].kind {
                            self.emit_arc_call("retain", &elem_ty, arg_vals[0])?;
                        }
                    }
                    // Element slots are 8 bytes — widen a narrow value.
                    let stored = if elem_cty == types::I64 {
                        arg_vals[0]
                    } else {
                        self.builder.ins().uextend(types::I64, arg_vals[0])
                    };
                    call_args.push(stored);
                } else {
                    call_args.extend(&arg_vals);
                }
                let inst = self.builder.ins().call(local_func, &call_args);
                let results = self.builder.inst_results(inst);
                if results.is_empty() {
                    return Ok(None);
                }
                let raw = results[0];
                if m == "get" {
                    if elem_arc {
                        // `get` returns a copy of the slot — a new
                        // owner of an ARC element, so retain it.
                        self.emit_arc_call("retain", &elem_ty, raw)?;
                        Ok(Some(raw))
                    } else if elem_cty != types::I64 {
                        Ok(Some(self.builder.ins().ireduce(elem_cty, raw)))
                    } else {
                        Ok(Some(raw))
                    }
                } else {
                    Ok(Some(raw))
                }
            }
            (recv_ty, _) => Err(CodegenError(format!(
                "method `.{}` on `{}` is not implemented",
                method,
                recv_ty.display()
            ))),
        }
    }

    /// Coerce a concrete struct into a `dyn Trait` object. Allocates
    /// a heap cell `[fnptr_0..fnptr_{N-1}, data, drop, rc]` — the
    /// trait's method pointers (per-instance method table), the
    /// concrete data pointer, a drop slot holding the struct's
    /// synthesized release fn, and the ARC refcount (appended by
    /// `struct_new`). The box owns a +1 on the data: it is reclaimed
    /// by ARC, and its release drops the data through the drop slot.
    fn compile_dyn_box(
        &mut self,
        value: &HirExpr,
        struct_sym: SymbolId,
        trait_sym: SymbolId,
    ) -> Result<Option<Value>, CodegenError> {
        let data = self
            .compile_expr(value)?
            .ok_or_else(|| CodegenError("dyn coercion value produced no value".into()))?;
        // The box owns a +1 on the boxed data. A fresh producer (a
        // struct literal, a call) already carries that +1; a borrowed
        // `Local` read does not, so retain it.
        if let HirExprKind::Local(_) = &value.kind {
            self.emit_arc_call("retain", &value.ty, data)?;
        }
        // The box's method-pointer area follows the *flat* layout:
        // the trait's own methods first, then every supertrait
        // method in BFS order (deduped first-wins). For `dyn Dog`
        // where `Dog: Animal`, slot 0 = bark, slot 1 = speak, then
        // data, drop, rc. The owning-trait sym in each entry is
        // ignored at codegen — `impl_methods` is keyed by
        // `(struct_sym, method_name)` so the lookup is uniform.
        let methods = self
            .trait_methods_flat
            .get(&trait_sym)
            .cloned()
            .ok_or_else(|| CodegenError("dyn: unknown trait".into()))?;
        let n = methods.len();
        // `struct_new(size)` allocs `size + 8`, with rc=1 at offset
        // `size`. Field area is the N method ptrs + data + drop slot.
        let cell_size = ((n + 2) * 8) as i64;
        let new_id = self.ensure_runtime_func("struct_new")?;
        let new_local = self.module.declare_func_in_func(new_id, self.builder.func);
        let size_v = self.builder.ins().iconst(types::I64, cell_size);
        let inst = self.builder.ins().call(new_local, &[size_v]);
        let cell = self.builder.inst_results(inst)[0];
        for (i, (_owner, m)) in methods.iter().enumerate() {
            let fn_sym = *self
                .impl_methods
                .get(&(struct_sym, m.clone()))
                .ok_or_else(|| {
                    CodegenError(format!("dyn: method `{}` has no impl", m))
                })?;
            let func_id = *self
                .sym_to_func
                .get(&fn_sym)
                .ok_or_else(|| CodegenError("dyn: impl method not compiled".into()))?;
            let fref = self.module.declare_func_in_func(func_id, self.builder.func);
            let fptr = self.builder.ins().func_addr(types::I64, fref);
            self.builder
                .ins()
                .store(MemFlags::new(), fptr, cell, (i * 8) as i32);
        }
        self.builder
            .ins()
            .store(MemFlags::new(), data, cell, (n * 8) as i32);
        // Drop slot: the concrete struct's synthesized release fn.
        // `define_dyn_release` calls it through this slot when the
        // box's rc reaches zero, reclaiming the boxed data.
        let drop_id = *self
            .struct_release_funcs
            .get(&struct_sym)
            .ok_or_else(|| CodegenError("dyn: struct missing release fn".into()))?;
        let drop_ref = self.module.declare_func_in_func(drop_id, self.builder.func);
        let drop_ptr = self.builder.ins().func_addr(types::I64, drop_ref);
        self.builder
            .ins()
            .store(MemFlags::new(), drop_ptr, cell, ((n + 1) * 8) as i32);
        Ok(Some(cell))
    }

    /// A method call on a `dyn Trait` receiver — load the method
    /// pointer and data pointer from the box and `call_indirect`.
    fn compile_dyn_call(
        &mut self,
        recv_val: Value,
        trait_sym: SymbolId,
        method: &str,
        args: &[HirExpr],
        result_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let cell = recv_val;
        // Look up in the flat list keyed by the call-site trait sym.
        // `d.speak()` on `dyn Dog` (where `Dog: Animal`) finds the
        // supertrait method at its slot in Dog's flat layout — not
        // Animal's flat layout, which would have different offsets.
        let methods = self
            .trait_methods_flat
            .get(&trait_sym)
            .cloned()
            .ok_or_else(|| CodegenError("dyn: unknown trait".into()))?;
        let n = methods.len();
        let index = methods
            .iter()
            .position(|(_owner, m)| m == method)
            .ok_or_else(|| CodegenError(format!("dyn: no method `{}`", method)))?;
        // Compile the explicit args before reading the box slots.
        let mut arg_vals: Vec<Value> = Vec::with_capacity(args.len() + 1);
        let mut compiled = Vec::with_capacity(args.len());
        for a in args {
            compiled.push(
                self.compile_expr(a)?
                    .ok_or_else(|| CodegenError("dyn call arg produced no value".into()))?,
            );
        }
        // `self` = the data pointer in the box's final slot.
        let data = self.builder.ins().load(
            types::I64,
            MemFlags::new(),
            cell,
            (n * 8) as i32,
        );
        arg_vals.push(data);
        arg_vals.extend(compiled);
        let fnptr = self.builder.ins().load(
            types::I64,
            MemFlags::new(),
            cell,
            (index * 8) as i32,
        );
        // Signature of the indirect call: `(self, args...) -> result`.
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        for a in args {
            sig.params.push(AbiParam::new(cranelift_type(&a.ty)?));
        }
        if !matches!(result_ty, Ty::Unit | Ty::Never) {
            sig.returns.push(AbiParam::new(cranelift_type(result_ty)?));
        }
        let sig_ref = self.builder.import_signature(sig);
        let inst = self
            .builder
            .ins()
            .call_indirect(sig_ref, fnptr, &arg_vals);
        let results = self.builder.inst_results(inst);
        if results.is_empty() {
            Ok(None)
        } else {
            Ok(Some(results[0]))
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
        let result = if results.is_empty() { None } else { Some(results[0]) };
        // Owned arguments for borrowing builtins: `print` only reads
        // its argument, so a fresh ARC temporary handed to it (a
        // string concat, a field read) is the caller's to reclaim.
        // `weak` / `upgrade_or` are excluded — they alias their
        // argument's control block, so releasing it would dangle.
        if name == "print_str" {
            for (a, &v) in args.iter().zip(&arg_vals) {
                if is_arc_type(&a.ty, self.struct_arc_fields, self.enum_has_payload)
                    && !matches!(a.kind, HirExprKind::Local(_))
                {
                    self.emit_arc_call("release", &a.ty, v)?;
                }
            }
        }
        Ok(result)
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
        if elems.is_empty() {
            return Err(CodegenError("empty arrays not yet supported".into()));
        }
        let esize = elem_size(elem_ty)? as i32;
        // A heap block: `N` element slots followed by a trailing rc,
        // allocated and rc-initialized to 1 by `rune_struct_new`.
        // Heap-allocated so the array can outlive the frame that
        // built it — returned, or stored in a struct that escapes.
        let field_size = array_field_size(elem_ty, elems.len())?;
        let new_id = self.ensure_runtime_func("struct_new")?;
        let new_local = self.module.declare_func_in_func(new_id, self.builder.func);
        let size_v = self.builder.ins().iconst(types::I64, field_size as i64);
        let inst = self.builder.ins().call(new_local, &[size_v]);
        let base = self.builder.inst_results(inst)[0];
        for (i, elem) in elems.iter().enumerate() {
            let v = self
                .compile_expr(elem)?
                .ok_or_else(|| CodegenError("array element produced no value".into()))?;
            // A borrowed `Local` element gives the array a second
            // owner — retain it; a fresh producer transfers its +1.
            if is_arc_type(elem_ty, self.struct_arc_fields, self.enum_has_payload) {
                if let HirExprKind::Local(_) = &elem.kind {
                    self.emit_arc_call("retain", elem_ty, v)?;
                }
            }
            self.builder
                .ins()
                .store(MemFlags::new(), v, base, (i as i32) * esize);
        }
        Ok(Some(base))
    }

    fn compile_index(
        &mut self,
        array: &HirExpr,
        index: &HirExpr,
        elem_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        // Array length is statically encoded in the type.
        let length = match &array.ty {
            Ty::Array(_, n) => *n,
            _ => {
                return Err(CodegenError(format!(
                    "indexing non-array type `{}`",
                    array.ty.display()
                )));
            }
        };
        let arr_addr = self
            .compile_expr(array)?
            .ok_or_else(|| CodegenError("array operand produced no value".into()))?;
        let idx = self
            .compile_expr(index)?
            .ok_or_else(|| CodegenError("index operand produced no value".into()))?;
        let length_v = self.builder.ins().iconst(types::I64, length as i64);
        self.emit_bounds_check(idx, length_v)?;
        let elem_cty = cranelift_type(elem_ty)?;
        let esize = elem_size(elem_ty)? as i64;
        let esize_const = self.builder.ins().iconst(types::I64, esize);
        let offset = self.builder.ins().imul(idx, esize_const);
        let elem_addr = self.builder.ins().iadd(arr_addr, offset);
        let val = self.builder.ins().load(elem_cty, MemFlags::new(), elem_addr, 0);
        // An ARC element read produces a new owner — retain it, just
        // as `compile_field_access` does for a struct field.
        if is_arc_type(elem_ty, self.struct_arc_fields, self.enum_has_payload) {
            self.emit_arc_call("retain", elem_ty, val)?;
        }
        Ok(Some(val))
    }

    /// Emits `if idx < 0 || idx >= length { rune_panic_bounds(idx, length); }`
    /// inline before a load. After this returns, the builder is positioned in
    /// the ok-block and ready for the actual access.
    fn emit_bounds_check(
        &mut self,
        idx: Value,
        length: Value,
    ) -> Result<(), CodegenError> {
        let panic_func_id = self.ensure_runtime_func("panic_bounds")?;
        let local_panic = self
            .module
            .declare_func_in_func(panic_func_id, self.builder.func);

        let zero = self.builder.ins().iconst(types::I64, 0);
        let lo_ok = self
            .builder
            .ins()
            .icmp(IntCC::SignedGreaterThanOrEqual, idx, zero);
        let hi_ok = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, idx, length);
        let in_bounds = self.builder.ins().band(lo_ok, hi_ok);

        let ok_blk = self.builder.create_block();
        let panic_blk = self.builder.create_block();
        self.builder
            .ins()
            .brif(in_bounds, ok_blk, &[], panic_blk, &[]);

        self.builder.switch_to_block(panic_blk);
        self.builder.seal_block(panic_blk);
        self.builder.ins().call(local_panic, &[idx, length]);
        // The runtime calls exit() — but Cranelift doesn't know that, so
        // the block needs a terminator. `trap` compiles to ud2 on x86_64.
        self.builder.ins().trap(TrapCode::user(1).unwrap());

        self.builder.switch_to_block(ok_blk);
        self.builder.seal_block(ok_blk);
        Ok(())
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

        self.loop_exit_stack.push((exit, self.arc_locals.len()));
        self.compile_block(body)?;
        self.loop_exit_stack.pop();

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

    fn compile_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        result_ty: &Ty,
    ) -> Result<Option<Value>, CodegenError> {
        let scrutinee_val = self
            .compile_expr(scrutinee)?
            .ok_or_else(|| CodegenError("match scrutinee produced no value".into()))?;
        // A fresh ARC scrutinee temporary is owned by the match: arm
        // bindings borrow into it, so it must outlive the arm bodies.
        // Scope-track it so it is released at the merge block, and by
        // `release_all_arc_locals` if an arm diverges via `return`.
        let scrut_snapshot = self.arc_locals.len();
        let scrut_temp = is_arc_type(
            &scrutinee.ty,
            self.struct_arc_fields,
            self.enum_has_payload,
        ) && !matches!(scrutinee.kind, HirExprKind::Local(_));
        if scrut_temp {
            let cty = cranelift_type(&scrutinee.ty)?;
            let var = self.alloc_var();
            self.builder.declare_var(var, cty);
            self.builder.def_var(var, scrutinee_val);
            self.arc_locals.push((var, scrutinee.ty.clone()));
        }

        let merge_blk = self.builder.create_block();
        let produces_value = !matches!(result_ty, Ty::Unit | Ty::Never);
        if produces_value {
            let cty = cranelift_type(result_ty)?;
            self.builder.append_block_param(merge_blk, cty);
        }

        // Pre-create a "next check" block for each arm boundary, plus a
        // fallback block past the last arm.
        let mut next_blks: Vec<Block> = Vec::with_capacity(arms.len() + 1);
        for _ in 0..=arms.len() {
            next_blks.push(self.builder.create_block());
        }

        // The first check block is the current location — jump to it.
        self.builder.ins().jump(next_blks[0], &[]);

        for (i, arm) in arms.iter().enumerate() {
            let check_blk = next_blks[i];
            let next_arm_blk = next_blks[i + 1];
            let body_blk = self.builder.create_block();

            self.builder.switch_to_block(check_blk);
            self.builder.seal_block(check_blk);

            // Or-pattern: try each alternative. First match jumps to body.
            // The last alternative's no-match falls through to next_arm_blk.
            let last_idx = arm.patterns.len() - 1;
            for (p_idx, pattern) in arm.patterns.iter().enumerate() {
                let on_no_match = if p_idx == last_idx {
                    next_arm_blk
                } else {
                    self.builder.create_block()
                };
                self.compile_pattern_check(
                    pattern,
                    scrutinee_val,
                    &scrutinee.ty,
                    body_blk,
                    on_no_match,
                )?;
                if p_idx != last_idx {
                    self.builder.switch_to_block(on_no_match);
                    self.builder.seal_block(on_no_match);
                }
            }

            // Body
            self.builder.switch_to_block(body_blk);
            self.builder.seal_block(body_blk);
            // Bind only applies when the arm has exactly one Bind pattern
            // (the checker rejects Bind inside or-patterns).
            if arm.patterns.len() == 1 {
                if let HirPattern::Bind(sym) = &arm.patterns[0] {
                    let var = self.alloc_var();
                    let cty = cranelift_type(&scrutinee.ty)?;
                    self.builder.declare_var(var, cty);
                    self.builder.def_var(var, scrutinee_val);
                    self.var_map.insert(*sym, var);
                }
            }
            // Optional guard — failing the guard falls through to next arm.
            if let Some(guard) = &arm.guard {
                let guard_val = self
                    .compile_expr(guard)?
                    .ok_or_else(|| CodegenError("match guard produced no value".into()))?;
                let guarded_body = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(guard_val, guarded_body, &[], next_arm_blk, &[]);
                self.builder.switch_to_block(guarded_body);
                self.builder.seal_block(guarded_body);
            }
            // A diverging arm body (`return`, ...) has type `Never`.
            // `Return` codegen leaves a fresh unreachable block behind,
            // so `is_filled()` reads false even though the arm yields
            // no value and must not jump to the merge block.
            let body_diverges = matches!(arm.body.ty, Ty::Never);
            let body_val = self.compile_expr(&arm.body)?;
            if !self.is_filled() {
                if body_diverges {
                    // Terminate the unreachable trailing block so the
                    // function verifies; it contributes no merge value.
                    self.builder.ins().trap(TrapCode::user(2).unwrap());
                } else if produces_value {
                    let v = body_val.ok_or_else(|| {
                        CodegenError("match arm produced no value".into())
                    })?;
                    // Tail-escape retain: an arm body that is a
                    // borrowed `Local` — most often an extracted enum
                    // payload (`Some(x) => x`) — must be retained so
                    // the match value carries its own +1. The arm
                    // analog of `compile_block`'s tail-escape rule.
                    if let HirExprKind::Local(_) = &arm.body.kind {
                        if is_arc_type(
                            &arm.body.ty,
                            self.struct_arc_fields,
                            self.enum_has_payload,
                        ) {
                            self.emit_arc_call("retain", &arm.body.ty, v)?;
                        }
                    }
                    self.builder.ins().jump(merge_blk, &[v]);
                } else {
                    self.builder.ins().jump(merge_blk, &[]);
                }
            }
        }

        // Fallback: no arm matched. Call rune_panic_no_match and trap.
        let fallback_blk = next_blks[arms.len()];
        self.builder.switch_to_block(fallback_blk);
        self.builder.seal_block(fallback_blk);
        let panic_id = self.ensure_runtime_func("panic_no_match")?;
        let local_panic = self
            .module
            .declare_func_in_func(panic_id, self.builder.func);
        self.builder.ins().call(local_panic, &[]);
        self.builder.ins().trap(TrapCode::user(2).unwrap());

        self.builder.switch_to_block(merge_blk);
        self.builder.seal_block(merge_blk);
        let result = if produces_value {
            Some(self.builder.block_params(merge_blk)[0])
        } else {
            None
        };
        // Release the scrutinee temporary — the arm bindings that
        // borrowed into it are now dead. A `return`-diverging arm
        // reclaimed it already via `release_all_arc_locals`.
        if scrut_temp {
            self.release_arc_locals_to(scrut_snapshot)?;
            self.arc_locals.truncate(scrut_snapshot);
        }
        Ok(result)
    }

    fn compile_pattern_check(
        &mut self,
        pattern: &HirPattern,
        scrutinee: Value,
        scrutinee_ty: &Ty,
        on_match: Block,
        on_no_match: Block,
    ) -> Result<(), CodegenError> {
        match pattern {
            HirPattern::Wildcard | HirPattern::Bind(_) => {
                self.builder.ins().jump(on_match, &[]);
            }
            HirPattern::IntLit(v) => {
                let cty = cranelift_type(scrutinee_ty)?;
                let lit = self.builder.ins().iconst(cty, *v);
                let eq = self.builder.ins().icmp(IntCC::Equal, scrutinee, lit);
                self.builder.ins().brif(eq, on_match, &[], on_no_match, &[]);
            }
            HirPattern::BoolLit(b) => {
                let lit = self
                    .builder
                    .ins()
                    .iconst(types::I8, if *b { 1 } else { 0 });
                let eq = self.builder.ins().icmp(IntCC::Equal, scrutinee, lit);
                self.builder.ins().brif(eq, on_match, &[], on_no_match, &[]);
            }
            HirPattern::StrLit(s) => {
                let lit_val = self
                    .compile_str_literal(s)?
                    .ok_or_else(|| CodegenError("pattern str produced no value".into()))?;
                let func_id = self.ensure_runtime_func("str_eq")?;
                let local_func = self
                    .module
                    .declare_func_in_func(func_id, self.builder.func);
                let inst = self.builder.ins().call(local_func, &[scrutinee, lit_val]);
                let eq = self.builder.inst_results(inst)[0];
                self.builder.ins().brif(eq, on_match, &[], on_no_match, &[]);
            }
            HirPattern::EnumVariant { discriminant } => {
                // For payload enums the scrutinee is a pointer to a
                // `{ tag, payload, rc }` descriptor; load the tag at
                // offset 0. For tag-only enums the scrutinee IS the
                // i64 discriminant.
                let has_payload = matches!(
                    scrutinee_ty,
                    Ty::Enum(sym, _) if self.enum_has_payload.contains(sym)
                );
                let tag = if has_payload {
                    self.builder
                        .ins()
                        .load(types::I64, MemFlags::new(), scrutinee, 0)
                } else {
                    scrutinee
                };
                let disc = self
                    .builder
                    .ins()
                    .iconst(types::I64, *discriminant as i64);
                let eq = self.builder.ins().icmp(IntCC::Equal, tag, disc);
                self.builder.ins().brif(eq, on_match, &[], on_no_match, &[]);
            }
            HirPattern::EnumPayload { discriminant, bindings } => {
                // Tag compare + branch. On match, materialize each
                // payload binding by loading from offset (8 + i*8)
                // and storing into a fresh Variable. Bindings of `_`
                // skip the load entirely.
                let tag = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::new(), scrutinee, 0);
                let disc = self
                    .builder
                    .ins()
                    .iconst(types::I64, *discriminant as i64);
                let eq = self.builder.ins().icmp(IntCC::Equal, tag, disc);
                let extract = self.builder.create_block();
                self.builder.ins().brif(eq, extract, &[], on_no_match, &[]);
                self.builder.switch_to_block(extract);
                self.builder.seal_block(extract);
                for (i, (payload_ty, binding)) in bindings.iter().enumerate() {
                    let Some(sym) = binding else { continue };
                    let raw = self.builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        scrutinee,
                        8 + 8 * i as i32,
                    );
                    let pcty = cranelift_type(payload_ty)?;
                    let val = if pcty == types::I64 {
                        raw
                    } else {
                        self.builder.ins().ireduce(pcty, raw)
                    };
                    let var = self.alloc_var();
                    self.builder.declare_var(var, pcty);
                    self.builder.def_var(var, val);
                    self.var_map.insert(*sym, var);
                }
                self.builder.ins().jump(on_match, &[]);
            }
            HirPattern::IntRange { lo, hi, inclusive } => {
                // Lower as: lo <= scrut && scrut [<|<=] hi.
                // Signed vs unsigned icmp follows the scrutinee's type.
                // Char is treated as signed-OK because all valid Unicode
                // scalars fit in the non-negative half of i32.
                let cty = cranelift_type(scrutinee_ty)?;
                let signed = matches!(
                    scrutinee_ty,
                    Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize)
                        | Ty::Char
                );
                let (le_cc, lt_cc) = if signed {
                    (IntCC::SignedLessThanOrEqual, IntCC::SignedLessThan)
                } else {
                    (IntCC::UnsignedLessThanOrEqual, IntCC::UnsignedLessThan)
                };
                let lo_v = self.builder.ins().iconst(cty, *lo);
                let hi_v = self.builder.ins().iconst(cty, *hi);
                let lo_ok = self.builder.ins().icmp(le_cc, lo_v, scrutinee);
                let hi_cc = if *inclusive { le_cc } else { lt_cc };
                let check_hi = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(lo_ok, check_hi, &[], on_no_match, &[]);
                self.builder.switch_to_block(check_hi);
                self.builder.seal_block(check_hi);
                let hi_ok = self.builder.ins().icmp(hi_cc, scrutinee, hi_v);
                self.builder.ins().brif(hi_ok, on_match, &[], on_no_match, &[]);
            }
        }
        Ok(())
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
        self.loop_exit_stack.push((exit, self.arc_locals.len()));
        self.compile_block(body)?;
        self.loop_exit_stack.pop();
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
        // Record the loop's exit block + ARC snapshot so a `break`
        // inside the body releases any ARC locals declared since the
        // loop entry, then jumps to `exit`.
        self.loop_exit_stack.push((exit, self.arc_locals.len()));
        self.compile_block(body)?;
        self.loop_exit_stack.pop();
        if !self.is_filled() {
            self.builder.ins().jump(header, &[]);
        }

        self.builder.seal_block(header);
        self.builder.switch_to_block(exit);
        self.builder.seal_block(exit);
        Ok(None)
    }
}

/// Types whose values are reclaimed by ARC. Vec and Str are always
/// ARC-managed (string literals use the rc=-1 sentinel). Every user-
/// defined struct now carries an rc and participates in ARC (the
/// `struct_arc_fields` map is still used for *field walks* during
/// release, but is_arc_type returns true for any Ty::Struct). An
/// enum is ARC-managed iff it has at least one payload-bearing
/// variant (`enum_has_payload`). A trait object (`dyn Trait`) is a
/// heap box and is always ARC-managed.
/// Max payload arity across an enum's variants. Used to size the
/// heap descriptor `{ tag, payload[max_arity], rc }`. 0 for tag-only
/// enums (which use the i64 representation instead).
fn enum_max_arity(
    sym: SymbolId,
    enum_payload_tys: &HashMap<SymbolId, Vec<Vec<Ty>>>,
) -> usize {
    enum_payload_tys
        .get(&sym)
        .map(|v| v.iter().map(|ps| ps.len()).max().unwrap_or(0))
        .unwrap_or(0)
}

/// A symbol-safe mangling of a type, used to name the synthesized
/// per-element-type Vec release functions. Distinct types must map to
/// distinct strings (the funcs are keyed by element `Ty`), so type
/// arguments are folded in.
fn mangle_ty_name(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "bool".into(),
        Ty::Char => "char".into(),
        Ty::Int(it) => it.name().into(),
        Ty::Float(ft) => ft.name().into(),
        Ty::Str => "str".into(),
        Ty::Unit => "unit".into(),
        Ty::Vec(e) => format!("Vec_{}", mangle_ty_name(e)),
        Ty::Weak(e) => format!("Weak_{}", mangle_ty_name(e)),
        Ty::Array(e, n) => format!("Arr{}_{}", n, mangle_ty_name(e)),
        Ty::Struct(s, args) | Ty::Enum(s, args) => {
            let tag = if matches!(ty, Ty::Struct(_, _)) { "S" } else { "E" };
            if args.is_empty() {
                format!("{}{}", tag, s.0)
            } else {
                let inner: Vec<String> = args.iter().map(mangle_ty_name).collect();
                format!("{}{}_{}", tag, s.0, inner.join("_"))
            }
        }
        Ty::TypeVar(s) => format!("T{}", s.0),
        Ty::Dyn(s, args) => {
            if args.is_empty() {
                format!("dyn{}", s.0)
            } else {
                let inner: Vec<String> = args.iter().map(mangle_ty_name).collect();
                format!("dyn{}_{}", s.0, inner.join("_"))
            }
        }
        Ty::Fn { .. } => "fn".into(),
        // SelfType and Assoc should never reach mangling — they're
        // resolved away in the checker/monomorphizer. Defensive only.
        Ty::SelfType => "Self".into(),
        Ty::Assoc(base, name) => format!("Assoc_{}_{}", mangle_ty_name(base), name),
        Ty::Never => "never".into(),
        Ty::Error => "err".into(),
    }
}

fn is_arc_type(
    ty: &Ty,
    _struct_arc_fields: &HashMap<SymbolId, Vec<(u32, Ty)>>,
    enum_has_payload: &std::collections::HashSet<SymbolId>,
) -> bool {
    match ty {
        Ty::Vec(_) | Ty::Str => true,
        Ty::Struct(_, _) => true,
        Ty::Enum(sym, _) => enum_has_payload.contains(sym),
        Ty::Weak(_) => true,
        // A trait object is a heap box reclaimed by ARC; its release
        // also drops the boxed concrete value.
        Ty::Dyn(_, _) => true,
        // A heap array is a refcounted block in its own right.
        Ty::Array(_, _) => true,
        _ => false,
    }
}

fn arc_helper_name(action: &str, ty: &Ty) -> Result<&'static str, CodegenError> {
    Ok(match (action, ty) {
        ("retain", Ty::Vec(_)) => "retain_vec",
        ("release", Ty::Vec(_)) => "release_vec",
        ("retain", Ty::Str) => "retain_str",
        ("release", Ty::Str) => "release_str",
        // Weak<T> uses the weak-counted helpers per inner type.
        // v0.x only supports Weak<Vec>.
        ("retain", Ty::Weak(inner)) if matches!(**inner, Ty::Vec(_)) => "weak_retain_vec",
        ("release", Ty::Weak(inner)) if matches!(**inner, Ty::Vec(_)) => "weak_release_vec",
        _ => {
            return Err(CodegenError(format!(
                "no ARC helper for action `{}` on `{}`",
                action,
                ty.display()
            )));
        }
    })
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
        // Structs are represented as a pointer to their stack-allocated body.
        Ty::Struct(_, _) => types::I64,
        // Vec is a pointer to a heap-allocated descriptor.
        Ty::Vec(_) => types::I64,
        // Unit-variant enums are stored as their i64 discriminant.
        Ty::Enum(_, _) => types::I64,
        // Weak<T> is also a pointer to the same control block as
        // the strong reference. The distinction lives in which
        // retain/release helpers we call on it.
        Ty::Weak(_) => types::I64,
        // A trait object is a pointer to its boxed method table.
        Ty::Dyn(_, _) => types::I64,
        // A function pointer — same shape as any other pointer.
        Ty::Fn { .. } => types::I64,
        // A projection or Self that survived to codegen means the
        // checker or monomorphizer failed to resolve it. Diagnose
        // clearly rather than masking the bug as "unsupported type".
        Ty::Assoc(_, _) | Ty::SelfType => {
            return Err(CodegenError(format!(
                "internal: unresolved associated-type projection `{}` reached codegen",
                ty.display()
            )));
        }
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
        Ty::Array(_, _) | Ty::Str | Ty::Struct(_, _) | Ty::Vec(_) | Ty::Enum(_, _) | Ty::Weak(_)
        | Ty::Dyn(_, _) => 8,
        _ => {
            return Err(CodegenError(format!(
                "cannot determine size of `{}`",
                ty.display()
            )));
        }
    })
}

/// The element-area size of a heap array `[elem; n]`, rounded up to
/// 8 bytes so the trailing rc word (stored at this offset by
/// `rune_struct_new`) stays 8-aligned even for narrow elements.
fn array_field_size(elem: &Ty, n: usize) -> Result<i32, CodegenError> {
    let raw = (n as i32) * (elem_size(elem)? as i32);
    Ok((raw + 7) & !7)
}

fn declare_builtin<M: Module>(module: &mut M, name: &str) -> Result<FuncId, CodegenError> {
    let (runtime_name, sig) = match name {
        "print_i64" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            ("rune_print_i64", sig)
        }
        "vec_new" => {
            let mut sig = module.make_signature();
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_vec_new", sig)
        }
        "vec_push" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *Vec
            sig.params.push(AbiParam::new(types::I64)); // x
            ("rune_vec_push", sig)
        }
        "vec_get" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_vec_get", sig)
        }
        "vec_len" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_vec_len", sig)
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
        // (idx, len) -> never. Used by the inline bounds-check pattern.
        "panic_bounds" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            ("rune_panic_bounds", sig)
        }
        "retain_str" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *RuneStr
            ("rune_retain_str", sig)
        }
        "release_str" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *RuneStr
            ("rune_release_str", sig)
        }
        "retain_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *RuneVec
            ("rune_retain_vec", sig)
        }
        "release_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // *RuneVec
            ("rune_release_vec", sig)
        }
        "weak_downgrade_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_weak_downgrade_vec", sig)
        }
        "weak_retain_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            ("rune_weak_retain_vec", sig)
        }
        "weak_release_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            ("rune_weak_release_vec", sig)
        }
        "weak_upgrade_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_weak_upgrade_vec", sig)
        }
        "weak_upgrade_or_vec" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_weak_upgrade_or_vec", sig)
        }
        "struct_new" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // size in bytes
            sig.returns.push(AbiParam::new(types::I64));
            ("rune_struct_new", sig)
        }
        "struct_dealloc" => {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(types::I64)); // ptr
            sig.params.push(AbiParam::new(types::I64)); // size
            ("rune_struct_dealloc", sig)
        }
        "panic_no_match" => {
            // () -> never; same trap-after-call shape as panic_bounds.
            let sig = module.make_signature();
            ("rune_panic_no_match", sig)
        }
        _ => return Err(CodegenError(format!("unknown builtin `{}`", name))),
    };
    module
        .declare_function(runtime_name, Linkage::Import, &sig)
        .map_err(|e| CodegenError(e.to_string()))
}
