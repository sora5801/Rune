//! Compiles `runtime.c` — the single source for the Rune runtime —
//! and links it into the `rune` binary so the JIT can resolve the
//! `rune_*` symbols. The AOT path compiles the same `runtime.c`
//! when it links a target executable.

fn main() {
    cc::Build::new()
        .file("runtime.c")
        .compile("rune_runtime");
    println!("cargo:rerun-if-changed=runtime.c");
}
