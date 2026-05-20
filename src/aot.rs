//! Ahead-of-time compilation helpers — produce a native object file from
//! a Rune HIR and shell out to a C-style linker driver to produce an
//! executable.

use std::path::Path;
use std::process::Command;

use crate::codegen::{Codegen, CodegenError, OptLevel};
use crate::hir::{HirItem, HirModule};
use crate::ty::SymbolId;

/// Minimal Rune runtime: defines C symbols the codegen imports.
///
/// `struct rune_str` here matches the layout emitted by codegen.rs:
/// a 16-byte (pointer, length) descriptor on the stack.
const RUNTIME_C: &str = r#"
#include <stdio.h>
#include <stdint.h>
#include <string.h>

struct rune_str {
    const char* ptr;
    int64_t     len;
};

void rune_print_i64(int64_t x) {
    printf("%lld\n", (long long)x);
}

void rune_print_str(const struct rune_str* s) {
    fwrite(s->ptr, 1, (size_t)s->len, stdout);
    fputc('\n', stdout);
}

int8_t rune_str_eq(const struct rune_str* a, const struct rune_str* b) {
    if (a->len != b->len) return 0;
    return (int8_t)(memcmp(a->ptr, b->ptr, (size_t)a->len) == 0);
}

#include <stdlib.h>

struct rune_str* rune_str_concat(const struct rune_str* a, const struct rune_str* b) {
    int64_t total_len = a->len + b->len;
    struct rune_str* result = (struct rune_str*)malloc(sizeof(struct rune_str));
    if (total_len == 0) {
        result->ptr = (const char*)0;
        result->len = 0;
        return result;
    }
    char* bytes = (char*)malloc((size_t)total_len);
    if (a->len > 0) memcpy(bytes, a->ptr, (size_t)a->len);
    if (b->len > 0) memcpy(bytes + a->len, b->ptr, (size_t)b->len);
    result->ptr = bytes;
    result->len = total_len;
    return result;
}
"#;

/// Compile a Rune module to a native object file (returned as bytes).
///
/// Renames Rune's `main` to `__rune_main` in place and synthesizes a
/// C-compatible `int main(void)` that calls it and truncates the i64
/// return to the i32 exit code.
pub fn build_object(
    hir: &mut HirModule,
    module_name: &str,
    opt: OptLevel,
) -> Result<Vec<u8>, CodegenError> {
    let rune_main_sym = rename_main(hir).ok_or_else(|| {
        CodegenError("no `main` function in module".into())
    })?;
    let mut cg = Codegen::new_object(module_name, opt)?;
    cg.compile_module(hir)?;
    let rune_main_id = cg
        .func_id(rune_main_sym)
        .ok_or_else(|| CodegenError("main was not declared".into()))?;
    cg.emit_c_main_wrapper(rune_main_id)?;
    cg.finish()
}

fn rename_main(hir: &mut HirModule) -> Option<SymbolId> {
    for item in &mut hir.items {
        let HirItem::Fn(f) = item;
        if f.name == "main" {
            let sym = f.sym;
            f.name = "__rune_main".to_string();
            return Some(sym);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct LinkError {
    pub tried: Vec<String>,
    pub errors: Vec<String>,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "no working linker found among {:?}.", self.tried)?;
        for e in &self.errors {
            writeln!(f, "  {}", e)?;
        }
        Ok(())
    }
}

impl std::error::Error for LinkError {}

/// Invoke a C-style linker driver to produce `output` from `obj`, plus
/// the embedded runtime C source.
///
/// Tries `$RUNE_LINKER` first if set, then `clang`, `gcc`, `cc` in order.
/// The first one that succeeds wins; returns the linker that worked.
///
/// The runtime defines `rune_print_i64` and any other host-provided
/// builtins the codegen imports. We pass it as a `.c` input so the linker
/// driver compiles + links it in one shot.
pub fn link(obj: &Path, output: &Path) -> Result<String, LinkError> {
    let runtime_path = obj.with_extension("rt.c");
    if let Err(e) = std::fs::write(&runtime_path, RUNTIME_C) {
        return Err(LinkError {
            tried: Vec::new(),
            errors: vec![format!("writing runtime to {}: {}", runtime_path.display(), e)],
        });
    }
    let candidates: Vec<String> = if let Ok(custom) = std::env::var("RUNE_LINKER") {
        vec![custom]
    } else {
        vec!["clang".into(), "gcc".into(), "cc".into()]
    };
    let mut errors = Vec::new();
    for cand in &candidates {
        let result = Command::new(cand)
            .arg(obj)
            .arg(&runtime_path)
            .arg("-o")
            .arg(output)
            .output();
        match result {
            Ok(out) if out.status.success() => return Ok(cand.clone()),
            Ok(out) => errors.push(format!(
                "{} exited with {:?}: {}",
                cand,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("{}: {}", cand, e)),
        }
    }
    Err(LinkError { tried: candidates, errors })
}
