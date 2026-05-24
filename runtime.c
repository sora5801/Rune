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
// occupancy exceeds 75%. No deletion in v0.x (no tombstones).
// Layout: `occupied` byte (0 = empty, 1 = live), parallel `keys`
// and `vals` arrays, plus the standard rc + weak_count for ARC.
//
// Value-side ARC is the user's responsibility — the runtime stores
// raw 8-byte slots and doesn't retain/release the values it holds.
// A future session can add a "drop helper fn pointer" field per
// hashmap instance, but v0.x keeps the type i64-only for both
// position aware analysis (`is_arc_type(Ty::HashMap(_,_))` returns
// true for the *container itself*; the user explicitly retains any
// reference-counted V before insert if they want correctness).
struct rune_hashmap {
    int64_t* keys;
    int64_t* vals;
    int8_t*  occupied;
    int64_t  len;
    int64_t  cap;
    int64_t  rc;
    int64_t  weak_count;
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
    return m;
}

// Find the bucket index for `k`: either the slot already holding
// `k` (occupied=1, key matches), or the first empty slot reached
// during linear probing. Caller checks `occupied[i]` to know which.
static int64_t rune_hashmap_probe(
    const struct rune_hashmap* m,
    int64_t k
) {
    uint64_t mask = (uint64_t)(m->cap - 1);
    uint64_t i = rune_hashmap_hash_i64(k) & mask;
    while (m->occupied[i] && m->keys[i] != k) {
        i = (i + 1) & mask;
    }
    return (int64_t)i;
}

static void rune_hashmap_grow(struct rune_hashmap* m) {
    int64_t new_cap = m->cap == 0 ? 8 : m->cap * 2;
    int64_t* new_keys = (int64_t*)malloc((size_t)new_cap * sizeof(int64_t));
    int64_t* new_vals = (int64_t*)malloc((size_t)new_cap * sizeof(int64_t));
    int8_t*  new_occ  = (int8_t*) calloc((size_t)new_cap, sizeof(int8_t));
    // Rehash live entries into the new table by probing from the
    // hash. The relative order changes but the contents don't.
    uint64_t mask = (uint64_t)(new_cap - 1);
    for (int64_t i = 0; i < m->cap; i++) {
        if (!m->occupied[i]) continue;
        int64_t k = m->keys[i];
        uint64_t j = rune_hashmap_hash_i64(k) & mask;
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

void rune_hashmap_insert(struct rune_hashmap* m, int64_t k, int64_t v) {
    // Grow before insert when occupancy would exceed 75% — also
    // covers the cap=0 initial state. Keep load factor low so
    // probe chains stay short.
    if (m->cap == 0 || (m->len + 1) * 4 > m->cap * 3) {
        rune_hashmap_grow(m);
    }
    int64_t i = rune_hashmap_probe(m, k);
    if (!m->occupied[i]) {
        m->occupied[i] = 1;
        m->keys[i] = k;
        m->len += 1;
    }
    m->vals[i] = v;
}

int64_t rune_hashmap_get(const struct rune_hashmap* m, int64_t k) {
    if (m->cap == 0) return 0;
    int64_t i = rune_hashmap_probe(m, k);
    return m->occupied[i] ? m->vals[i] : 0;
}

int8_t rune_hashmap_contains_key(const struct rune_hashmap* m, int64_t k) {
    if (m->cap == 0) return 0;
    int64_t i = rune_hashmap_probe(m, k);
    return m->occupied[i] ? 1 : 0;
}

int64_t rune_hashmap_len(const struct rune_hashmap* m) {
    return m->len;
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
