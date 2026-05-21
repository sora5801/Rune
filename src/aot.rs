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
    int64_t     rc;   // ARC refcount; -1 = literal (never reclaim)
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
    result->rc = 1;
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

static int64_t clamp_i64(int64_t v, int64_t lo, int64_t hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

struct rune_str* rune_str_slice(const struct rune_str* s, int64_t start, int64_t end) {
    start = clamp_i64(start, 0, s->len);
    end   = clamp_i64(end,   start, s->len);
    int64_t new_len = end - start;
    struct rune_str* result = (struct rune_str*)malloc(sizeof(struct rune_str));
    result->rc = 1;
    if (new_len == 0) {
        result->ptr = (const char*)0;
        result->len = 0;
        return result;
    }
    char* bytes = (char*)malloc((size_t)new_len);
    memcpy(bytes, s->ptr + start, (size_t)new_len);
    result->ptr = bytes;
    result->len = new_len;
    return result;
}

int8_t rune_str_starts_with(const struct rune_str* s, const struct rune_str* prefix) {
    if (prefix->len > s->len) return 0;
    if (prefix->len == 0) return 1;
    return (int8_t)(memcmp(s->ptr, prefix->ptr, (size_t)prefix->len) == 0);
}

int8_t rune_str_ends_with(const struct rune_str* s, const struct rune_str* suffix) {
    if (suffix->len > s->len) return 0;
    if (suffix->len == 0) return 1;
    return (int8_t)(memcmp(s->ptr + (s->len - suffix->len), suffix->ptr, (size_t)suffix->len) == 0);
}

int8_t rune_str_contains(const struct rune_str* s, const struct rune_str* needle) {
    if (needle->len == 0) return 1;
    if (needle->len > s->len) return 0;
    int64_t last = s->len - needle->len;
    for (int64_t i = 0; i <= last; i++) {
        if (memcmp(s->ptr + i, needle->ptr, (size_t)needle->len) == 0) return 1;
    }
    return 0;
}

struct rune_vec {
    int64_t* ptr;
    int64_t  len;
    int64_t  cap;
    int64_t  rc;          // strong refcount
    int64_t  weak_count;  // weak refs + 1 (strong refs share that 1)
};

struct rune_vec* rune_vec_new(void) {
    struct rune_vec* v = (struct rune_vec*)malloc(sizeof(struct rune_vec));
    v->ptr = (int64_t*)0;
    v->len = 0;
    v->cap = 0;
    v->rc = 1;
    v->weak_count = 1;
    return v;
}

void rune_vec_push(struct rune_vec* v, int64_t x) {
    if (v->len == v->cap) {
        int64_t new_cap = v->cap == 0 ? 4 : v->cap * 2;
        v->ptr = (int64_t*)realloc(v->ptr, (size_t)(new_cap * (int64_t)sizeof(int64_t)));
        v->cap = new_cap;
    }
    v->ptr[v->len++] = x;
}

int64_t rune_vec_get(const struct rune_vec* v, int64_t i) {
    if (i < 0 || i >= v->len) return 0;
    return v->ptr[i];
}

int64_t rune_vec_len(const struct rune_vec* v) {
    return v->len;
}

void rune_panic_bounds(int64_t idx, int64_t len) {
    fprintf(stderr, "rune: index %lld out of range for length %lld\n",
            (long long)idx, (long long)len);
    exit(1);
}

void rune_retain_str(struct rune_str* s) {
    if (s == NULL || s->rc == -1) return;
    s->rc += 1;
}

void rune_release_str(struct rune_str* s) {
    if (s == NULL || s->rc == -1) return;
    s->rc -= 1;
    if (s->rc > 0) return;
    if (s->ptr != NULL) free((void*)s->ptr);
    free(s);
}

void rune_retain_vec(struct rune_vec* v) {
    if (v == NULL || v->rc == -1) return;
    v->rc += 1;
}

void rune_weak_release_vec(struct rune_vec* v) {
    if (v == NULL || v->weak_count == -1) return;
    v->weak_count -= 1;
    if (v->weak_count > 0) return;
    free(v);
}

void rune_release_vec(struct rune_vec* v) {
    if (v == NULL || v->rc == -1) return;
    v->rc -= 1;
    if (v->rc > 0) return;
    if (v->ptr != NULL) free(v->ptr);
    v->ptr = (int64_t*)0;
    v->cap = 0;
    v->len = 0;
    rune_weak_release_vec(v);
}

struct rune_vec* rune_weak_downgrade_vec(struct rune_vec* v) {
    if (v == NULL || v->weak_count == -1) return v;
    v->weak_count += 1;
    return v;
}

void rune_weak_retain_vec(struct rune_vec* v) {
    if (v == NULL || v->weak_count == -1) return;
    v->weak_count += 1;
}

struct rune_vec* rune_weak_upgrade_vec(struct rune_vec* v) {
    if (v == NULL || v->rc <= 0) return (struct rune_vec*)0;
    v->rc += 1;
    return v;
}

void rune_panic_no_match(void) {
    fprintf(stderr, "rune: no match arm matched\n");
    exit(1);
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
