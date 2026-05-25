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

// Session 119: byte-level string accessors + split. byte_at
// returns the byte at index i, or 0 if out-of-range (mirroring
// rune_vec_get's policy of "no panic, surface zero"). find
// returns the byte offset of needle's first occurrence in s, or
// -1 if not present (the int64_t sentinel; v0.x doesn't return
// Option<i64> from builtins).

uint8_t rune_str_byte_at(const struct rune_str* s, int64_t i) {
    if (i < 0 || i >= s->len) return 0;
    return (uint8_t)s->ptr[i];
}

int64_t rune_str_find(const struct rune_str* s, const struct rune_str* needle) {
    if (needle->len == 0) return 0;
    if (needle->len > s->len) return -1;
    int64_t last = s->len - needle->len;
    for (int64_t i = 0; i <= last; i++) {
        if (memcmp(s->ptr + i, needle->ptr, (size_t)needle->len) == 0) return i;
    }
    return -1;
}

// Forward decls — split builds a Vec<rune_str*> using the existing
// vec runtime.
struct rune_vec;
struct rune_vec* rune_vec_new(void);
void rune_vec_push(struct rune_vec* v, int64_t x);

// Helper for split: allocate a fresh rune_str holding the byte
// range [a, b) from src. Empty range allowed (ptr=NULL, len=0).
static struct rune_str* str_slice_owned(const char* src_ptr, int64_t a, int64_t b) {
    struct rune_str* r = (struct rune_str*)malloc(sizeof(struct rune_str));
    r->rc = 1;
    int64_t n = b - a;
    if (n <= 0) {
        r->ptr = (const char*)0;
        r->len = 0;
        return r;
    }
    char* bytes = (char*)malloc((size_t)n);
    memcpy(bytes, src_ptr + a, (size_t)n);
    r->ptr = bytes;
    r->len = n;
    return r;
}

// Session 121: mutable, heap-grown String type. Same descriptor
// shape as rune_str but with a `cap` field for amortized growth.
// Allocations follow the Vec doubling pattern: cap = 4 minimum,
// doubles when len would exceed cap. The runtime owns the byte
// buffer (free on release). Conversion `.to_str()` copies bytes
// into a fresh immutable rune_str — the String stays mutable
// after the read.
struct rune_string {
    char*   ptr;
    int64_t len;
    int64_t cap;
    int64_t rc;
};

struct rune_string* rune_string_new(void) {
    struct rune_string* s = (struct rune_string*)malloc(sizeof(struct rune_string));
    s->ptr = (char*)0;
    s->len = 0;
    s->cap = 0;
    s->rc = 1;
    return s;
}

static void rune_string_reserve(struct rune_string* s, int64_t need) {
    if (s->cap >= need) return;
    int64_t new_cap = s->cap == 0 ? 8 : s->cap * 2;
    while (new_cap < need) new_cap *= 2;
    char* nb = (char*)malloc((size_t)new_cap);
    if (s->len > 0) memcpy(nb, s->ptr, (size_t)s->len);
    if (s->ptr != (char*)0) free(s->ptr);
    s->ptr = nb;
    s->cap = new_cap;
}

void rune_string_push_str(struct rune_string* s, const struct rune_str* x) {
    if (x->len <= 0) return;
    rune_string_reserve(s, s->len + x->len);
    memcpy(s->ptr + s->len, x->ptr, (size_t)x->len);
    s->len += x->len;
}

void rune_string_push_byte(struct rune_string* s, uint8_t b) {
    rune_string_reserve(s, s->len + 1);
    s->ptr[s->len] = (char)b;
    s->len += 1;
}

int64_t rune_string_len(const struct rune_string* s) {
    return s->len;
}

struct rune_str* rune_string_to_str(const struct rune_string* s) {
    struct rune_str* r = (struct rune_str*)malloc(sizeof(struct rune_str));
    r->rc = 1;
    if (s->len <= 0) {
        r->ptr = (const char*)0;
        r->len = 0;
        return r;
    }
    char* bytes = (char*)malloc((size_t)s->len);
    memcpy(bytes, s->ptr, (size_t)s->len);
    r->ptr = bytes;
    r->len = s->len;
    return r;
}

void rune_retain_string(struct rune_string* s) {
    if (s == (struct rune_string*)0 || s->rc == -1) return;
    s->rc += 1;
}

void rune_release_string(struct rune_string* s) {
    if (s == (struct rune_string*)0 || s->rc == -1) return;
    s->rc -= 1;
    if (s->rc == 0) {
        if (s->ptr != (char*)0) free(s->ptr);
        free(s);
    }
}

// Session 122: String::from(s: str) — construct a fresh mutable
// String pre-populated with `s`'s bytes. Equivalent to
// `String::new().push_str(s)` but in one allocation.
struct rune_string* rune_string_from(const struct rune_str* s) {
    struct rune_string* out = rune_string_new();
    if (s->len > 0) {
        rune_string_reserve(out, s->len);
        memcpy(out->ptr, s->ptr, (size_t)s->len);
        out->len = s->len;
    }
    return out;
}

// Session 123: i64::from_str — inverse of i64::to_str. Parses
// the str's bytes as a base-10 decimal integer with optional
// leading `-` or `+`. On any error (empty input, non-digit
// characters, out-of-range value) returns 0. Callers that need
// to distinguish "parsed zero" from "parse error" should pre-
// validate the input (`s.byte_at(0) >= 48u8` etc.) before
// calling, or wait for an Option<i64>-returning variant.
//
// The copy-to-stack-buffer pattern matches the path_to_cstr
// helper from session 118 — Rune `str` isn't NUL-terminated,
// so strtoll needs us to copy + terminate before parsing.
int64_t rune_i64_from_str(const struct rune_str* s) {
    if (s->len <= 0) return 0;
    char buf[32];
    size_t n = (size_t)s->len > 31 ? 31 : (size_t)s->len;
    memcpy(buf, s->ptr, n);
    buf[n] = '\0';
    char* end = (char*)0;
    long long v = strtoll(buf, &end, 10);
    // No digits consumed → invalid.
    if (end == buf) return 0;
    return (int64_t)v;
}

// Session 122: integer formatting. i64::to_str renders the value
// as decimal (with leading `-` for negatives) into a fresh +1
// rune_str. snprintf produces the digits into a stack buffer
// large enough for any i64 (20 digits + sign + NUL = 22 bytes);
// the rune_str descriptor stores only the digits (no NUL).
struct rune_str* rune_i64_to_str(int64_t v) {
    char buf[32];
    int n = snprintf(buf, sizeof(buf), "%lld", (long long)v);
    if (n < 0) n = 0;
    struct rune_str* r = (struct rune_str*)malloc(sizeof(struct rune_str));
    r->rc = 1;
    if (n == 0) {
        r->ptr = (const char*)0;
        r->len = 0;
        return r;
    }
    char* bytes = (char*)malloc((size_t)n);
    memcpy(bytes, buf, (size_t)n);
    r->ptr = bytes;
    r->len = (int64_t)n;
    return r;
}

// Session 120: command-line args. The AOT C main wrapper calls
// rune_argv_init at process start with the OS-provided argc/argv;
// std::env::args() then returns a fresh Vec<str> per call that
// aliases the OS-owned argv strings via rc=-1 ("literal") rune_str
// descriptors stored in a static array. JIT-mode tests never call
// rune_argv_init, so g_argc stays 0 and env_args() returns empty —
// matches the "no program-level argv available" model.
static int g_argc = 0;
static char** g_argv = (char**)0;
static struct rune_str* g_arg_descriptors = (struct rune_str*)0;

void rune_argv_init(int argc, char** argv) {
    if (g_arg_descriptors != (struct rune_str*)0) {
        // Idempotent: second init replaces the previous binding
        // (test harnesses may call this multiple times).
        free(g_arg_descriptors);
        g_arg_descriptors = (struct rune_str*)0;
    }
    g_argc = argc;
    g_argv = argv;
    if (argc > 0) {
        g_arg_descriptors = (struct rune_str*)malloc(
            sizeof(struct rune_str) * (size_t)argc);
        for (int i = 0; i < argc; i++) {
            g_arg_descriptors[i].ptr = argv[i];
            g_arg_descriptors[i].len = (int64_t)strlen(argv[i]);
            g_arg_descriptors[i].rc = -1;  // literal — release_str is no-op
        }
    }
}

struct rune_vec* rune_env_args(void) {
    struct rune_vec* v = rune_vec_new();
    for (int i = 0; i < g_argc; i++) {
        rune_vec_push(v, (int64_t)&g_arg_descriptors[i]);
    }
    return v;
}

struct rune_vec* rune_str_split(const struct rune_str* s, const struct rune_str* sep) {
    struct rune_vec* v = rune_vec_new();
    // Empty separator → return [whole_str] (no-split convention).
    // Avoids the alternative of "split into individual chars" which
    // requires UTF-8 decoding and isn't this session's scope.
    if (sep->len == 0) {
        struct rune_str* whole = str_slice_owned(s->ptr, 0, s->len);
        rune_vec_push(v, (int64_t)whole);
        return v;
    }
    int64_t start = 0;
    int64_t i = 0;
    int64_t last = s->len - sep->len;
    while (i <= last) {
        if (memcmp(s->ptr + i, sep->ptr, (size_t)sep->len) == 0) {
            struct rune_str* piece = str_slice_owned(s->ptr, start, i);
            rune_vec_push(v, (int64_t)piece);
            i += sep->len;
            start = i;
        } else {
            i++;
        }
    }
    // Final piece (may be empty if separator is at the end — keeps
    // semantics consistent with Rust's split which yields a trailing
    // empty str for "a,b,".split(",") = ["a", "b", ""]).
    struct rune_str* tail = str_slice_owned(s->ptr, start, s->len);
    rune_vec_push(v, (int64_t)tail);
    return v;
}

// Session 118: file I/O builtins. Both functions take rune_str
// descriptors and return rune_str (read) / int8_t (write). The
// path is read as a NUL-terminated C string — Rune `str` isn't
// NUL-terminated, so we copy onto the stack with a length cap.
// Errors are surfaced as the simplest possible signal: read_file
// returns an empty rune_str on failure (callers check `.is_empty()`
// or `.len() > 0`); write_file returns 0 on failure, 1 on success.
// A proper Result<str, IoErr> shape can come once Rune has a
// std::io::Error type — out of scope for v0.x.

static int path_to_cstr(const struct rune_str* path, char* buf, size_t cap) {
    if ((size_t)path->len + 1 > cap) return 0;
    if (path->len > 0) memcpy(buf, path->ptr, (size_t)path->len);
    buf[path->len] = '\0';
    return 1;
}

struct rune_str* rune_read_file(const struct rune_str* path) {
    struct rune_str* result = (struct rune_str*)malloc(sizeof(struct rune_str));
    result->rc = 1;
    result->ptr = (const char*)0;
    result->len = 0;
    char cpath[4096];
    if (!path_to_cstr(path, cpath, sizeof(cpath))) return result;
    FILE* f = fopen(cpath, "rb");
    if (!f) return result;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return result; }
    long sz = ftell(f);
    if (sz < 0) { fclose(f); return result; }
    if (fseek(f, 0, SEEK_SET) != 0) { fclose(f); return result; }
    if (sz == 0) { fclose(f); return result; }  // empty file → empty str
    char* bytes = (char*)malloc((size_t)sz);
    size_t got = fread(bytes, 1, (size_t)sz, f);
    fclose(f);
    if (got != (size_t)sz) {
        free(bytes);
        return result;
    }
    result->ptr = bytes;
    result->len = sz;
    return result;
}

int8_t rune_write_file(const struct rune_str* path, const struct rune_str* contents) {
    char cpath[4096];
    if (!path_to_cstr(path, cpath, sizeof(cpath))) return 0;
    FILE* f = fopen(cpath, "wb");
    if (!f) return 0;
    if (contents->len > 0) {
        size_t wrote = fwrite(contents->ptr, 1, (size_t)contents->len, f);
        if (wrote != (size_t)contents->len) { fclose(f); return 0; }
    }
    fclose(f);
    return 1;
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

struct rune_vec* rune_weak_upgrade_or_vec(struct rune_vec* w, struct rune_vec* def) {
    if (w != NULL && w->rc > 0) {
        w->rc += 1;
        return w;
    }
    // The weak target is dead — return the default, retained so the
    // caller owns its own strong reference.
    rune_retain_vec(def);
    return def;
}

void rune_panic_no_match(void) {
    fprintf(stderr, "rune: no match arm matched\n");
    exit(1);
}

// Heap block of `size` field bytes + an 8-byte trailing rc (set to
// 1). Backs structs, payload enums, dyn boxes, and heap arrays.
void* rune_struct_new(int64_t size) {
    char* p = (char*)malloc((size_t)size + 8);
    *(int64_t*)(p + size) = 1;
    return p;
}

void rune_struct_dealloc(void* p, int64_t size) {
    (void)size;
    if (p) free(p);
}

// ---- HashMap ----
//
// Open-addressing linear-probed table with i64 keys + 8-byte values
// (any Rune type that fits a slot — primitives, struct/vec/str
// pointers, fn-pointers, dyn boxes). Initial cap 8, doubles when
// occupancy exceeds 75%. Tombstone-based deletion: `occupied` is
// a tri-state byte — 0=empty, 1=live, 2=tombstone. Probes continue
// past tombstones (since the key being looked up may have been
// inserted after a now-removed neighbor and lives further down the
// chain), but matching only happens against live slots. Insert
// reuses the first tombstone seen along the probe path. Grow drops
// tombstones (they don't carry over to the rehashed table). The
// per-V release walk synthesized at codegen time matches occupied
// == 1 specifically to skip tombstoned slots.
struct rune_hashmap {
    int64_t* keys;
    int64_t* vals;
    int8_t*  occupied;
    int64_t  len;
    int64_t  cap;
    int64_t  rc;
    int64_t  weak_count;
    // Session 069: 0 = i64 keys (slot stores the i64 directly),
    // 1 = str keys (slot stores a rune_str* and the runtime
    // owns a +1 on each key — retain on fresh insert, release
    // on remove or final descriptor drop). Hash + equality
    // branch on this field per probe step.
    int64_t  key_kind;
};

// Multiplicative mix; same shape Rust uses for its default i64
// hasher's finalizer. Distributes adjacent keys (the most common
// real-world pattern) reasonably across the bucket table.
static uint64_t rune_hashmap_hash_i64(int64_t k) {
    uint64_t x = (uint64_t)k;
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33;
    x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33;
    return x;
}

// FNV-1a over the byte content of a rune_str. Good distribution for
// short ASCII strings; collisions for adversarial inputs are
// possible but the hashmap is in-process (not exposed to network
// input), so DoS isn't a concern.
static uint64_t rune_hashmap_hash_str(const struct rune_str* s) {
    if (s == NULL) return 0;
    uint64_t h = 0xcbf29ce484222325ULL;
    for (int64_t i = 0; i < s->len; i++) {
        h ^= (uint64_t)(uint8_t)s->ptr[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

// Branch on the descriptor's key_kind. Both branches receive the
// raw int64 key as the codegen passes it — for str keys, that's
// the rune_str* cast to int64.
static uint64_t rune_hashmap_hash_key(
    const struct rune_hashmap* m,
    int64_t k
) {
    if (m->key_kind == 1) {
        return rune_hashmap_hash_str((const struct rune_str*)k);
    }
    return rune_hashmap_hash_i64(k);
}

// Content equality. For str keys, two slots are equal when their
// `rune_str_eq` returns nonzero (length matches, memcmp succeeds).
static int rune_hashmap_keys_equal(
    const struct rune_hashmap* m,
    int64_t a,
    int64_t b
) {
    if (m->key_kind == 1) {
        return rune_str_eq(
            (const struct rune_str*)a,
            (const struct rune_str*)b
        ) ? 1 : 0;
    }
    return a == b ? 1 : 0;
}

struct rune_hashmap* rune_hashmap_new(void) {
    struct rune_hashmap* m =
        (struct rune_hashmap*)malloc(sizeof(struct rune_hashmap));
    m->keys = (int64_t*)0;
    m->vals = (int64_t*)0;
    m->occupied = (int8_t*)0;
    m->len = 0;
    m->cap = 0;
    m->rc = 1;
    m->weak_count = 1;
    m->key_kind = 0;
    return m;
}

// Session 069: str-key variant. Same descriptor shape; just the
// key_kind tag differs. All other methods (insert/get/contains/
// remove/release) branch on key_kind internally.
struct rune_hashmap* rune_hashmap_str_new(void) {
    struct rune_hashmap* m = rune_hashmap_new();
    m->key_kind = 1;
    return m;
}

// Find the bucket index for `k` — the slot holding `k` (occupied==1
// && key matches), the first empty slot (occupied==0), or the first
// tombstone if no later match is found. Caller distinguishes by
// reading `occupied[i]`. Linear probe stops at empty; continues
// past tombstones (the key may have been inserted before a removal
// later in the chain).
static int64_t rune_hashmap_probe(
    const struct rune_hashmap* m,
    int64_t k
) {
    uint64_t mask = (uint64_t)(m->cap - 1);
    uint64_t i = rune_hashmap_hash_key(m, k) & mask;
    while (m->occupied[i] != 0) {
        if (m->occupied[i] == 1 && rune_hashmap_keys_equal(m, m->keys[i], k)) {
            return (int64_t)i;
        }
        i = (i + 1) & mask;
    }
    return (int64_t)i;
}

// Like probe but tracks the first tombstone seen — so an insert
// can reuse it instead of extending the probe chain. Returns the
// insertion slot directly: if `k` is already live, that slot; if
// not but a tombstone was passed, that one; otherwise the first
// empty slot.
static int64_t rune_hashmap_probe_for_insert(
    const struct rune_hashmap* m,
    int64_t k
) {
    uint64_t mask = (uint64_t)(m->cap - 1);
    uint64_t i = rune_hashmap_hash_key(m, k) & mask;
    int64_t first_tomb = -1;
    while (m->occupied[i] != 0) {
        if (m->occupied[i] == 1 && rune_hashmap_keys_equal(m, m->keys[i], k)) {
            return (int64_t)i;
        }
        if (m->occupied[i] == 2 && first_tomb < 0) {
            first_tomb = (int64_t)i;
        }
        i = (i + 1) & mask;
    }
    if (first_tomb >= 0) return first_tomb;
    return (int64_t)i;
}

static void rune_hashmap_grow(struct rune_hashmap* m) {
    int64_t new_cap = m->cap == 0 ? 8 : m->cap * 2;
    int64_t* new_keys = (int64_t*)malloc((size_t)new_cap * sizeof(int64_t));
    int64_t* new_vals = (int64_t*)malloc((size_t)new_cap * sizeof(int64_t));
    int8_t*  new_occ  = (int8_t*) calloc((size_t)new_cap, sizeof(int8_t));
    // Rehash live entries into the new table; tombstones are
    // dropped (they're not real data and don't carry over). The
    // probe chains shrink because tombstone-extended chains
    // collapse.
    uint64_t mask = (uint64_t)(new_cap - 1);
    for (int64_t i = 0; i < m->cap; i++) {
        if (m->occupied[i] != 1) continue;
        int64_t k = m->keys[i];
        uint64_t j = rune_hashmap_hash_key(m, k) & mask;
        while (new_occ[j]) j = (j + 1) & mask;
        new_keys[j] = k;
        new_vals[j] = m->vals[i];
        new_occ[j] = 1;
    }
    if (m->keys) free(m->keys);
    if (m->vals) free(m->vals);
    if (m->occupied) free(m->occupied);
    m->keys = new_keys;
    m->vals = new_vals;
    m->occupied = new_occ;
    m->cap = new_cap;
}

int64_t rune_hashmap_insert(struct rune_hashmap* m, int64_t k, int64_t v) {
    // Grow before insert when occupancy would exceed 75% — also
    // covers the cap=0 initial state. Keep load factor low so
    // probe chains stay short.
    if (m->cap == 0 || (m->len + 1) * 4 > m->cap * 3) {
        rune_hashmap_grow(m);
    }
    int64_t i = rune_hashmap_probe_for_insert(m, k);
    int64_t prev = 0;
    if (m->occupied[i] != 1) {
        // Empty or tombstone — fresh insert. For str keys, the
        // runtime owns the key's +1: retain on store, release on
        // remove or final descriptor drop. (Caller's `k` may be a
        // borrowed Local; the runtime side is the source of truth
        // for what the slot owns. Symmetric with how the slot's
        // value ARC is handled by codegen rather than the runtime,
        // but here the runtime has the type info — only str keys
        // exist for now — so it can act directly.)
        m->occupied[i] = 1;
        m->keys[i] = k;
        m->len += 1;
        if (m->key_kind == 1) {
            rune_retain_str((struct rune_str*)k);
        }
    } else {
        // Overwrite: hand the previous value back to the codegen
        // caller so it can release the +1 the old slot owned.
        // Returning 0 for the fresh-slot case is safe — V's
        // release helpers all null-check, and non-ARC V types
        // never use this return anyway (codegen discards it).
        prev = m->vals[i];
    }
    m->vals[i] = v;
    return prev;
}

int64_t rune_hashmap_get(const struct rune_hashmap* m, int64_t k) {
    if (m->cap == 0) return 0;
    int64_t i = rune_hashmap_probe(m, k);
    return m->occupied[i] == 1 ? m->vals[i] : 0;
}

int8_t rune_hashmap_contains_key(const struct rune_hashmap* m, int64_t k) {
    if (m->cap == 0) return 0;
    int64_t i = rune_hashmap_probe(m, k);
    return m->occupied[i] == 1 ? 1 : 0;
}

int64_t rune_hashmap_len(const struct rune_hashmap* m) {
    return m->len;
}

// Remove the entry for `k`. Returns the value that was stored
// (or 0 if `k` wasn't present). The slot becomes a tombstone so
// later probes still continue past it for keys inserted after a
// neighboring removal — but the slot is reusable on insert.
// Callers releasing ARC values should read the returned val and
// release it themselves; the runtime doesn't know V's type.
int64_t rune_hashmap_remove(struct rune_hashmap* m, int64_t k) {
    if (m->cap == 0) return 0;
    int64_t i = rune_hashmap_probe(m, k);
    if (m->occupied[i] != 1) return 0;
    int64_t v = m->vals[i];
    if (m->key_kind == 1) {
        // The slot owned a +1 on its str key; release it now that
        // the slot is being tombstoned. The lookup arg `k` is a
        // separate borrowed pointer the caller still holds.
        rune_release_str((struct rune_str*)m->keys[i]);
    }
    m->occupied[i] = 2;
    // Don't clear keys[i] / vals[i] — probe checks occupied first.
    m->len -= 1;
    return v;
}

// Low-level inspectors used by the HashMapKeysIter (defined in
// std.rn) to walk occupied slots without going through the
// hash-driven probe path. `cap` and `is_live_at` are read-only;
// `key_at` returns the slot's key (caller's responsibility to
// only invoke when is_live_at returned 1).
int64_t rune_hashmap_cap(const struct rune_hashmap* m) {
    return m->cap;
}

int8_t rune_hashmap_is_live_at(const struct rune_hashmap* m, int64_t i) {
    if (m->cap == 0 || i < 0 || i >= m->cap) return 0;
    return m->occupied[i] == 1 ? 1 : 0;
}

int64_t rune_hashmap_key_at(const struct rune_hashmap* m, int64_t i) {
    if (m->cap == 0 || i < 0 || i >= m->cap) return 0;
    return m->keys[i];
}

// Session 075: companion to rune_hashmap_key_at — returns the
// raw 8-byte value at slot `i` (caller must have already
// checked `rune_hashmap_is_live_at`). For ARC value types, the
// codegen-side retain when consuming the entry is the caller's
// responsibility, mirroring how hashmap_get's value is treated.
int64_t rune_hashmap_val_at(const struct rune_hashmap* m, int64_t i) {
    if (m->cap == 0 || i < 0 || i >= m->cap) return 0;
    return m->vals[i];
}

void rune_retain_hashmap(struct rune_hashmap* m) {
    if (m == NULL || m->rc == -1) return;
    m->rc += 1;
}

void rune_weak_release_hashmap(struct rune_hashmap* m) {
    if (m == NULL || m->weak_count == -1) return;
    m->weak_count -= 1;
    if (m->weak_count > 0) return;
    free(m);
}

void rune_release_hashmap(struct rune_hashmap* m) {
    if (m == NULL || m->rc == -1) return;
    m->rc -= 1;
    if (m->rc > 0) return;
    // Session 069: at the final drop, release each live key for
    // str-keyed maps. The synthesized per-V release walk (in
    // codegen) ran first and released vals; now we own the key
    // ARC, so walk and release. The corresponding codegen walk
    // for values uses `occupied == 1` to skip tombstones — we
    // match that here.
    if (m->key_kind == 1 && m->occupied != NULL) {
        for (int64_t i = 0; i < m->cap; i++) {
            if (m->occupied[i] == 1) {
                rune_release_str((struct rune_str*)m->keys[i]);
            }
        }
    }
    if (m->keys) free(m->keys);
    if (m->vals) free(m->vals);
    if (m->occupied) free(m->occupied);
    m->keys = (int64_t*)0;
    m->vals = (int64_t*)0;
    m->occupied = (int8_t*)0;
    m->cap = 0;
    m->len = 0;
    rune_weak_release_hashmap(m);
}
