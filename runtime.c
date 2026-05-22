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
