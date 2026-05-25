# Session 120 — Command-line args (`std::env::args()`)

**Date:** 2026-05-25
**Outcome:** `std::env::args() -> Vec<str>` returns
the OS-provided argv to a running Rune program.
First element is the program name; subsequent
elements are the user-provided arguments. AOT
binaries reach the OS argv via the C main wrapper;
JIT tests get an empty Vec (no argv context). 462
codegen + 43 AOT + 223 typecheck tests green (+2
codegen, +3 AOT from session 119).

```rune
fn main() -> i64 {
    let args: Vec<str> = std::env::args();
    if args.len() < 2 { return 1; }
    let cmd: str = args.get(1);
    if cmd == "--help" {
        print("usage: ...");
        0
    } else { 2 }
}
```

## The decisive observation

argv lives at the boundary between the OS and Rune.
In AOT mode the C main wrapper *receives* `argc /
argv` from libc startup and *forwards* them to a
new runtime helper `rune_argv_init` before invoking
the user's `__rune_main`. The runtime stashes them
in static storage; later, `std::env::args()` calls
build a fresh `Vec<str>` from that storage.

```c
// runtime.c
static int g_argc = 0;
static char** g_argv = NULL;
static struct rune_str* g_arg_descriptors = NULL;

void rune_argv_init(int argc, char** argv) {
    g_argc = argc; g_argv = argv;
    g_arg_descriptors = malloc(sizeof(struct rune_str) * argc);
    for (int i = 0; i < argc; i++) {
        g_arg_descriptors[i].ptr = argv[i];
        g_arg_descriptors[i].len = strlen(argv[i]);
        g_arg_descriptors[i].rc = -1;  // literal — release_str is no-op
    }
}

struct rune_vec* rune_env_args(void) {
    struct rune_vec* v = rune_vec_new();
    for (int i = 0; i < g_argc; i++) {
        rune_vec_push(v, (int64_t)&g_arg_descriptors[i]);
    }
    return v;
}
```

Three subtleties:

1. **`rc = -1` on the descriptors.** argv strings are
   owned by the OS / libc for the process lifetime;
   we mustn't free them. Existing release_str
   special-cases `rc == -1` as "static literal, no
   reclaim" (session 067's mechanism for compiled-
   into-binary string literals). Reusing that
   convention means the Vec's per-elem release walk
   on scope exit is a no-op — exactly what we want.

2. **Static descriptor array, fresh Vec per call.**
   The rune_str *descriptors* are allocated once by
   `rune_argv_init`. Each call to `env::args()`
   builds a *fresh* `Vec<rune_str*>` containing
   pointers to those static descriptors. The Vec
   itself is reference-counted normally (rc=1 on
   construction; freed on scope exit). The
   descriptors live forever.

3. **JIT-mode is empty.** No `rune_argv_init` ever
   runs in JIT, so `g_argc` stays 0 and `env::args()`
   returns an empty Vec. Tests that compile and
   evaluate Rune via `run_main` see this empty
   state — matches "no program-level argv" semantics
   cleanly.

### The AOT C main wrapper change

Before this session, the synthesized C main was
`int main(void)`. Now it's `int main(int argc, char**
argv)` with two block params and an
`rune_argv_init(argc, argv)` call before `__rune_main`:

```rust
// codegen.rs, emit_c_main_wrapper:
sig.params.push(AbiParam::new(types::I32));   // argc
sig.params.push(AbiParam::new(types::I64));   // argv (char**)
sig.returns.push(AbiParam::new(types::I32));

builder.append_block_params_for_function_params(block);
let argc_val = builder.block_params(block)[0];
let argv_val = builder.block_params(block)[1];

// Call rune_argv_init(argc, argv) before __rune_main.
let argv_init_id = declare_builtin(&mut self.module, "argv_init")?;
let argv_init_ref = self.module.declare_func_in_func(argv_init_id, builder.func);
builder.ins().call(argv_init_ref, &[argc_val, argv_val]);

// Then call __rune_main() and forward its return as exit code.
```

C's main signature is forward-compatible: an entry
point declared as `(int, char**)` works correctly
when called by libc startup regardless of the
program's specific contract. All existing AOT tests
(40 of them) still pass with the new signature
because the new params are simply unused by the
existing programs.

### Resolver: `std::env::args` is path-qualified

The builtin is interned with the full path
`"std::env::args"` rather than a bare name. This is
the first nested-namespace builtin (the rest are
top-level: `print`, `read_file`, etc., or `std::Vec` /
`std::HashMap` as types). The lookup machinery
already supports multi-segment paths — no resolver
change needed beyond the intern call.

## The wire-ups

```
runtime.c          (+~30 lines: g_argc / g_argv / g_arg_descriptors
                    static storage, rune_argv_init, rune_env_args)

src/codegen.rs     (+2 extern declarations,
                    +2 JIT symbol registrations,
                    +2 runtime-func signature arms,
                    emit_c_main_wrapper rewritten to take
                     (argc, argv) and call rune_argv_init first)

src/resolver.rs    (+1 BuiltinFn entry under the qualified name
                    "std::env::args")

tests/codegen.rs   (+2 tests: empty-in-JIT, for-loop-safe-on-empty)

tests/aot.rs       (+build_and_run_with_args helper,
                    +3 tests: argv-len, program-name-non-empty,
                     starts-with-prefix-marker)
```

No checker / lowerer / monomorphizer / HIR / std.rn
changes.

## What's tested

Codegen (+2 from session 119's 460):

- `env_args_returns_empty_vec_in_jit` — JIT mode
  shows empty argv (no init was called).
- `env_args_iter_safe_on_empty` — for-loop over
  empty argv doesn't panic, runs zero iterations.

AOT (+3 from session 119's 40):

- `aot_env_args_returns_argv` — three extra args
  passed; argv.len() == 4 (program name + 3).
- `aot_env_args_first_is_program_name` — argv[0]
  is non-empty (the executable path).
- `aot_env_args_content_via_starts_with` — passed
  arg "rune-marker-abc" is visible through
  `.starts_with`.

## Apparent bugs that aren't / explicitly deferred

- **JIT can't simulate argv.** Tests can't pass
  CLI args to JIT-mode programs because there's no
  Rust-side `set_argv()` shim. We'd need to expose
  `rune_argv_init` to Rust and call it before
  `cg.run_main`. Future session if needed; today
  AOT tests cover the argv path.
- **Each call allocates a fresh Vec.** Calling
  `env::args()` multiple times allocates N Vecs.
  Idiomatic usage is "call once, save the result";
  caching at the runtime level would change the
  ownership story (who owns the shared Vec?).
  Leave as-is.
- **No `std::env::var(name) -> str`** for
  environment variables. Mechanical follow-on:
  another runtime helper calling `getenv`, returning
  a rune_str (allocated this time, since env vars
  can change). Future session.
- **No Unicode-aware path on Windows.** argv comes
  in via `(int, char**)`. Windows libc translates
  command-line UTF-16 → process codepage (typically
  ANSI). UTF-8 paths / args work on POSIX out of
  the box; on Windows, a future `wmain` variant
  would help. v0.x prioritizes POSIX correctness.
- **`build_object` doesn't ship a hook to skip the
  main wrapper.** If a Rune program is built as a
  library without main, the wrapper would fail. v0.x
  only supports executables; lib targets are a
  future addition.

## What's next

- **Session 121: Mutable `String` type + builder
  methods.** The lexer needs to accumulate text;
  immutable `str` + `+=` allocates per concat.
  `String::new(); s.push_str(...)` is the natural
  shape.
- **Session 122+**: continued Phase 1 buildout per
  session 117 roadmap.
