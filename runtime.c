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
