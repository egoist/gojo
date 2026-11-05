// Kite runtime implementation.
#include "kite_rt.h"
#include <stdio.h>
#include <errno.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <ctype.h>

static int g_argc;
static char **g_argv;

void kite_rt_init(int argc, char **argv) {
    g_argc = argc;
    g_argv = argv;
    setvbuf(stdout, NULL, _IOFBF, 1 << 16);
}

// ------------------------------------------------------------------ panics

static pthread_mutex_t g_panic_lock = PTHREAD_MUTEX_INITIALIZER;

_Noreturn static void panic_end(void) {
    fflush(stdout);
    fflush(stderr);
    _exit(2);
}

_Noreturn void kite_panic(const char *msg, const char *loc) {
    pthread_mutex_lock(&g_panic_lock);
    fflush(stdout);
    fprintf(stderr, "panic: %s\n    at %s\n", msg, loc);
    panic_end();
}

_Noreturn void kite_panic_str(kite_str msg, const char *loc) {
    pthread_mutex_lock(&g_panic_lock);
    fflush(stdout);
    fprintf(stderr, "panic: %.*s\n    at %s\n", (int)msg.len, (const char *)msg.ptr, loc);
    panic_end();
}

_Noreturn void kite_panic_buf(kite_string *msg, const char *loc) {
    kite_str s = { msg->ptr, msg->len };
    kite_panic_str(s, loc);
}

_Noreturn void kite_panic_index(int64_t i, int64_t len, const char *loc) {
    char buf[128];
    snprintf(buf, sizeof buf, "index out of range: index %lld, length %lld", (long long)i, (long long)len);
    kite_panic(buf, loc);
}

_Noreturn void kite_panic_overflow(const char *op, const char *loc) {
    char buf[128];
    snprintf(buf, sizeof buf, "integer overflow in %s", op);
    kite_panic(buf, loc);
}

// ------------------------------------------------------------------ memory

void *kite_alloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) kite_panic("out of memory", "<runtime>");
    return p;
}

void *kite_realloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) kite_panic("out of memory", "<runtime>");
    return q;
}

// ------------------------------------------------------------------ small-object allocator

__thread kite_box_node *kite_box_lists[KITE_BOX_MAX / 16 + 1];

// Slabs are carved into blocks of one size class. They are kept on a global
// list (never returned to the OS), like most size-class allocators.
#define KITE_SLAB (64 * 1024)
typedef struct kite_slab { struct kite_slab *next; } kite_slab;
static kite_slab *g_slabs;
static pthread_mutex_t g_slab_lock = PTHREAD_MUTEX_INITIALIZER;

void *kite_box_refill(size_t cls) {
    size_t sz = cls << 4;
    kite_slab *s = kite_alloc(KITE_SLAB);
    pthread_mutex_lock(&g_slab_lock);
    s->next = g_slabs;
    g_slabs = s;
    pthread_mutex_unlock(&g_slab_lock);
    // the first 16 bytes hold the slab header; the rest becomes blocks
    uint8_t *start = (uint8_t *)s + 16, *end = (uint8_t *)s + KITE_SLAB;
    size_t count = (size_t)(end - start) / sz;
    // keep the first block, thread the rest onto the free list
    kite_box_node *head = kite_box_lists[cls];
    for (size_t i = count; i-- > 1;) {
        kite_box_node *f = (kite_box_node *)(start + i * sz);
        f->next = head;
        head = f;
    }
    kite_box_lists[cls] = head;
    return start;
}

// ------------------------------------------------------------------ strings

static void string_reserve(kite_string *s, int64_t extra) {
    if (s->len + extra <= s->cap) return;
    int64_t nc = s->cap ? s->cap * 2 : 16;
    while (nc < s->len + extra) nc *= 2;
    s->ptr = kite_realloc(s->ptr, (size_t)nc);
    s->cap = nc;
}

kite_string kite_string_from(kite_str s) {
    kite_string r = { NULL, 0, 0 };
    if (s.len) {
        r.ptr = kite_alloc((size_t)s.len);
        memcpy(r.ptr, s.ptr, (size_t)s.len);
        r.len = r.cap = s.len;
    }
    return r;
}

kite_string kite_string_clone(const kite_string *s) {
    return kite_string_from(kite_string_view(s));
}

void kite_string_push(kite_string *s, kite_str t) {
    if (!t.len) return;
    string_reserve(s, t.len);
    memmove(s->ptr + s->len, t.ptr, (size_t)t.len);
    s->len += t.len;
}

void kite_String_push(kite_string *s, kite_str t) { kite_string_push(s, t); }

void kite_string_push_byte(kite_string *s, uint8_t b) {
    string_reserve(s, 1);
    s->ptr[s->len++] = b;
}

void kite_String_pushRune(kite_string *s, int32_t r) {
    kite_string t = kite_rune_to_string(r);
    kite_string_push(s, kite_string_view(&t));
    free(t.ptr);
}

void kite_String_clear(kite_string *s) { s->len = 0; }

bool kite_str_eq(kite_str a, kite_str b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.ptr, b.ptr, (size_t)a.len) == 0);
}

int kite_str_cmp(kite_str a, kite_str b) {
    int64_t n = a.len < b.len ? a.len : b.len;
    int c = n ? memcmp(a.ptr, b.ptr, (size_t)n) : 0;
    if (c) return c < 0 ? -1 : 1;
    return a.len < b.len ? -1 : a.len > b.len ? 1 : 0;
}

uint64_t kite_str_hash(kite_str s) {
    // 8 bytes per step, then a final avalanche
    uint64_t h = 0x9e3779b97f4a7c15ULL ^ (uint64_t)s.len;
    const uint8_t *p = s.ptr;
    int64_t n = s.len;
    while (n >= 8) {
        uint64_t w;
        memcpy(&w, p, 8);
        h = (h ^ w) * 0xbf58476d1ce4e5b9ULL;
        h ^= h >> 31;
        p += 8;
        n -= 8;
    }
    uint64_t w = 0;
    for (int64_t i = 0; i < n; i++) w |= (uint64_t)p[i] << (8 * i);
    h = (h ^ w) * 0x94d049bb133111ebULL;
    return kite_hash_u64(h);
}

kite_string kite_str_concat(kite_str a, kite_str b) {
    kite_string r = { NULL, 0, 0 };
    string_reserve(&r, a.len + b.len);
    if (a.len) memcpy(r.ptr, a.ptr, (size_t)a.len);
    if (b.len) memcpy(r.ptr + a.len, b.ptr, (size_t)b.len);
    r.len = a.len + b.len;
    return r;
}

kite_str kite_str_slice(kite_str s, int64_t lo, int64_t hi, const char *loc) {
    if (lo < 0 || hi > s.len || lo > hi) {
        char buf[128];
        snprintf(buf, sizeof buf, "slice bounds out of range [%lld:%lld] with length %lld", (long long)lo, (long long)hi, (long long)s.len);
        kite_panic(buf, loc);
    }
    if ((lo < s.len && (s.ptr[lo] & 0xC0) == 0x80) || (hi < s.len && (s.ptr[hi] & 0xC0) == 0x80))
        kite_panic("string slice is not on a UTF-8 character boundary", loc);
    kite_str r = { s.ptr + lo, hi - lo };
    return r;
}

int32_t kite_utf8_decode(kite_str s, int64_t *i) {
    const uint8_t *p = s.ptr + *i;
    int64_t left = s.len - *i;
    uint8_t c = p[0];
    if (c < 0x80) { *i += 1; return c; }
    int n = (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 1;
    if (n == 1 || left < n) { *i += 1; return 0xFFFD; }
    int32_t r = c & (0xFF >> (n + 1));
    for (int k = 1; k < n; k++) {
        if ((p[k] & 0xC0) != 0x80) { *i += 1; return 0xFFFD; }
        r = (r << 6) | (p[k] & 0x3F);
    }
    *i += n;
    return r;
}

kite_string kite_rune_to_string(int32_t r) {
    kite_string s = { NULL, 0, 0 };
    uint8_t b[4];
    int n;
    uint32_t c = (uint32_t)r;
    if (c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) c = 0xFFFD;
    if (c < 0x80) { b[0] = (uint8_t)c; n = 1; }
    else if (c < 0x800) { b[0] = 0xC0 | (c >> 6); b[1] = 0x80 | (c & 0x3F); n = 2; }
    else if (c < 0x10000) { b[0] = 0xE0 | (c >> 12); b[1] = 0x80 | ((c >> 6) & 0x3F); b[2] = 0x80 | (c & 0x3F); n = 3; }
    else { b[0] = 0xF0 | (c >> 18); b[1] = 0x80 | ((c >> 12) & 0x3F); b[2] = 0x80 | ((c >> 6) & 0x3F); b[3] = 0x80 | (c & 0x3F); n = 4; }
    kite_str t = { b, n };
    kite_string_push(&s, t);
    return s;
}

static int64_t str_find(kite_str s, kite_str sub, int64_t from) {
    if (sub.len == 0) return from;
    for (int64_t i = from; i + sub.len <= s.len; i++)
        if (s.ptr[i] == sub.ptr[0] && memcmp(s.ptr + i, sub.ptr, (size_t)sub.len) == 0) return i;
    return -1;
}

bool kite_str_contains(kite_str s, kite_str sub) { return str_find(s, sub, 0) >= 0; }
bool kite_str_startsWith(kite_str s, kite_str p) { return p.len <= s.len && memcmp(s.ptr, p.ptr, (size_t)p.len) == 0; }
bool kite_str_endsWith(kite_str s, kite_str p) { return p.len <= s.len && memcmp(s.ptr + s.len - p.len, p.ptr, (size_t)p.len) == 0; }
int64_t kite_prelude___strFind(kite_str s, kite_str sub) { return str_find(s, sub, 0); }

static bool is_space(uint8_t c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f'; }

kite_str kite_str_trimStart(kite_str s) {
    while (s.len && is_space(s.ptr[0])) { s.ptr++; s.len--; }
    return s;
}
kite_str kite_str_trimEnd(kite_str s) {
    while (s.len && is_space(s.ptr[s.len - 1])) s.len--;
    return s;
}
kite_str kite_str_trim(kite_str s) { return kite_str_trimEnd(kite_str_trimStart(s)); }

static void vec_push_string(kite_vec *v, kite_str s) {
    kite_vec_reserve(v, 1, sizeof(kite_string));
    ((kite_string *)v->ptr)[v->len++] = kite_string_from(s);
}

kite_vec kite_str_split(kite_str s, kite_str sep) {
    kite_vec v = { NULL, 0, 0 };
    if (sep.len == 0) {
        int64_t i = 0;
        while (i < s.len) {
            int64_t st = i;
            kite_utf8_decode(s, &i);
            kite_str part = { s.ptr + st, i - st };
            vec_push_string(&v, part);
        }
        return v;
    }
    int64_t start = 0;
    for (;;) {
        int64_t j = str_find(s, sep, start);
        if (j < 0) break;
        kite_str part = { s.ptr + start, j - start };
        vec_push_string(&v, part);
        start = j + sep.len;
    }
    kite_str last = { s.ptr + start, s.len - start };
    vec_push_string(&v, last);
    return v;
}

kite_vec kite_str_lines(kite_str s) {
    kite_vec v = { NULL, 0, 0 };
    int64_t start = 0;
    for (int64_t i = 0; i < s.len; i++) {
        if (s.ptr[i] == '\n') {
            int64_t end = i;
            if (end > start && s.ptr[end - 1] == '\r') end--;
            kite_str part = { s.ptr + start, end - start };
            vec_push_string(&v, part);
            start = i + 1;
        }
    }
    if (start < s.len) {
        kite_str part = { s.ptr + start, s.len - start };
        vec_push_string(&v, part);
    }
    return v;
}

kite_vec kite_str_fields(kite_str s) {
    kite_vec v = { NULL, 0, 0 };
    int64_t i = 0;
    while (i < s.len) {
        while (i < s.len && is_space(s.ptr[i])) i++;
        int64_t st = i;
        while (i < s.len && !is_space(s.ptr[i])) i++;
        if (i > st) {
            kite_str part = { s.ptr + st, i - st };
            vec_push_string(&v, part);
        }
    }
    return v;
}

kite_string kite_str_toUpper(kite_str s) {
    kite_string r = kite_string_from(s);
    for (int64_t i = 0; i < r.len; i++) r.ptr[i] = (uint8_t)toupper(r.ptr[i]);
    return r;
}
kite_string kite_str_toLower(kite_str s) {
    kite_string r = kite_string_from(s);
    for (int64_t i = 0; i < r.len; i++) r.ptr[i] = (uint8_t)tolower(r.ptr[i]);
    return r;
}
kite_string kite_str_toString(kite_str s) { return kite_string_from(s); }

kite_string kite_str_repeat(kite_str s, int64_t n) {
    kite_string r = { NULL, 0, 0 };
    for (int64_t i = 0; i < n; i++) kite_string_push(&r, s);
    return r;
}

kite_string kite_str_replace(kite_str s, kite_str from, kite_str to) {
    kite_string r = { NULL, 0, 0 };
    if (from.len == 0) return kite_string_from(s);
    int64_t start = 0;
    for (;;) {
        int64_t j = str_find(s, from, start);
        if (j < 0) break;
        kite_str part = { s.ptr + start, j - start };
        kite_string_push(&r, part);
        kite_string_push(&r, to);
        start = j + from.len;
    }
    kite_str last = { s.ptr + start, s.len - start };
    kite_string_push(&r, last);
    return r;
}

kite_vec kite_str_runes(kite_str s) {
    kite_vec v = { NULL, 0, 0 };
    int64_t i = 0;
    while (i < s.len) {
        int32_t r = kite_utf8_decode(s, &i);
        kite_vec_reserve(&v, 1, sizeof(int32_t));
        ((int32_t *)v.ptr)[v.len++] = r;
    }
    return v;
}

bool kite_prelude___parseInt(kite_str s, int64_t *out) {
    char buf[64];
    if (s.len == 0 || s.len >= (int64_t)sizeof buf) return false;
    memcpy(buf, s.ptr, (size_t)s.len);
    buf[s.len] = 0;
    char *end;
    errno = 0;
    long long v = strtoll(buf, &end, 10);
    if (errno || *end || isspace((unsigned char)buf[0])) return false;
    *out = v;
    return true;
}

bool kite_prelude___parseFloat(kite_str s, double *out) {
    char buf[128];
    if (s.len == 0 || s.len >= (int64_t)sizeof buf) return false;
    memcpy(buf, s.ptr, (size_t)s.len);
    buf[s.len] = 0;
    char *end;
    double v = strtod(buf, &end);
    if (*end || isspace((unsigned char)buf[0])) return false;
    *out = v;
    return true;
}

// ------------------------------------------------------------------ formatting

void kite_fmt_cstr(kite_string *b, const char *s) {
    kite_str t = { (const uint8_t *)s, (int64_t)strlen(s) };
    kite_string_push(b, t);
}

void kite_fmt_u64(kite_string *b, uint64_t v) {
    uint8_t buf[24];
    int i = 24;
    do {
        buf[--i] = (uint8_t)('0' + v % 10);
        v /= 10;
    } while (v);
    kite_str t = { buf + i, 24 - i };
    kite_string_push(b, t);
}

void kite_fmt_i64(kite_string *b, int64_t v) {
    if (v < 0) {
        kite_string_push_byte(b, '-');
        kite_fmt_u64(b, (uint64_t)0 - (uint64_t)v);
    } else {
        kite_fmt_u64(b, (uint64_t)v);
    }
}

void kite_fmt_f64(kite_string *b, double v) {
    char buf[64];
    if (isnan(v)) { kite_fmt_cstr(b, "NaN"); return; }
    if (isinf(v)) { kite_fmt_cstr(b, v > 0 ? "+Inf" : "-Inf"); return; }
    // shortest representation that round-trips
    for (int p = 1; p <= 17; p++) {
        snprintf(buf, sizeof buf, "%.*g", p, v);
        if (strtod(buf, NULL) == v) break;
    }
    kite_fmt_cstr(b, buf);
}

void kite_fmt_bool(kite_string *b, bool v) { kite_fmt_cstr(b, v ? "true" : "false"); }

void kite_fmt_str(kite_string *b, kite_str s, int nested) {
    if (!nested) { kite_string_push(b, s); return; }
    kite_string_push_byte(b, '"');
    for (int64_t i = 0; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        switch (c) {
        case '"': kite_fmt_cstr(b, "\\\""); break;
        case '\\': kite_fmt_cstr(b, "\\\\"); break;
        case '\n': kite_fmt_cstr(b, "\\n"); break;
        case '\t': kite_fmt_cstr(b, "\\t"); break;
        case '\r': kite_fmt_cstr(b, "\\r"); break;
        default:
            if (c < 0x20) {
                char buf[8];
                snprintf(buf, sizeof buf, "\\x%02x", c);
                kite_fmt_cstr(b, buf);
            } else kite_string_push_byte(b, c);
        }
    }
    kite_string_push_byte(b, '"');
}

void kite_fmt_ptr(kite_string *b, const void *p) {
    char buf[32];
    snprintf(buf, sizeof buf, "%p", p);
    kite_fmt_cstr(b, buf);
}

static pthread_mutex_t g_out_lock = PTHREAD_MUTEX_INITIALIZER;

void kite_write(int fd, kite_string *b) {
    pthread_mutex_lock(&g_out_lock);
    FILE *f = fd == 2 ? stderr : stdout;
    if (fd == 2) fflush(stdout);
    fwrite(b->ptr, 1, (size_t)b->len, f);
    if (fd == 2) fflush(stderr);
    pthread_mutex_unlock(&g_out_lock);
    free(b->ptr);
}

// ------------------------------------------------------------------ vectors

void kite_vec_grow(kite_vec *v, int64_t need, size_t esize) {
    int64_t nc = v->cap ? v->cap * 2 : 4;
    while (nc < need) nc *= 2;
    v->ptr = kite_realloc(v->ptr, (size_t)nc * esize);
    v->cap = nc;
}

static void merge_sort(uint8_t *a, uint8_t *tmp, int64_t n, size_t sz, int (*cmp)(const void *, const void *, void *), void *ctx) {
    if (n < 2) return;
    if (n <= 12) {
        // insertion sort
        for (int64_t i = 1; i < n; i++) {
            memcpy(tmp, a + i * sz, sz);
            int64_t j = i;
            while (j > 0 && cmp(tmp, a + (j - 1) * sz, ctx) < 0) {
                memcpy(a + j * sz, a + (j - 1) * sz, sz);
                j--;
            }
            memcpy(a + j * sz, tmp, sz);
        }
        return;
    }
    int64_t m = n / 2;
    merge_sort(a, tmp, m, sz, cmp, ctx);
    merge_sort(a + m * sz, tmp, n - m, sz, cmp, ctx);
    if (cmp(a + (m - 1) * sz, a + m * sz, ctx) <= 0) return;
    memcpy(tmp, a, (size_t)n * sz);
    int64_t i = 0, j = m, k = 0;
    while (i < m && j < n) {
        if (cmp(tmp + j * sz, tmp + i * sz, ctx) < 0) memcpy(a + (k++) * sz, tmp + (j++) * sz, sz);
        else memcpy(a + (k++) * sz, tmp + (i++) * sz, sz);
    }
    while (i < m) memcpy(a + (k++) * sz, tmp + (i++) * sz, sz);
    while (j < n) memcpy(a + (k++) * sz, tmp + (j++) * sz, sz);
}

void kite_sort(void *base, int64_t n, size_t size, int (*cmp)(const void *, const void *, void *), void *ctx) {
    if (n < 2) return;
    uint8_t *tmp = kite_alloc((size_t)n * size);
    merge_sort(base, tmp, n, size, cmp, ctx);
    free(tmp);
}

// ------------------------------------------------------------------ maps
// Insertion-ordered: an array of entries plus an open-addressing index.
// Entry layout: [uint64 hash][int64 live][K][V] as described by kite_map_desc.

#define ENT(m, d, i) ((m)->entries + (size_t)(i) * (d)->esize)
#define EHASH(e) (*(uint64_t *)(e))
#define ELIVE(e) (*(int64_t *)((uint8_t *)(e) + 8))

static void map_reindex(kite_map *m, const kite_map_desc *d, int64_t icap) {
    free(m->index);
    m->index = kite_alloc((size_t)icap * sizeof(int64_t));
    for (int64_t i = 0; i < icap; i++) m->index[i] = -1;
    m->index_cap = icap;
    for (int64_t i = 0; i < m->n_entries; i++) {
        uint8_t *e = ENT(m, d, i);
        if (!ELIVE(e)) continue;
        uint64_t h = EHASH(e);
        int64_t s = (int64_t)(h & (uint64_t)(icap - 1));
        while (m->index[s] >= 0) s = (s + 1) & (icap - 1);
        m->index[s] = i;
    }
}

static void map_compact(kite_map *m, const kite_map_desc *d) {
    int64_t k = 0;
    for (int64_t i = 0; i < m->n_entries; i++) {
        uint8_t *e = ENT(m, d, i);
        if (!ELIVE(e)) continue;
        if (k != i) memcpy(ENT(m, d, k), e, d->esize);
        k++;
    }
    m->n_entries = k;
}

static int64_t map_lookup(const kite_map *m, const kite_map_desc *d, const void *key, uint64_t h, int64_t *slot_out) {
    if (!m->index_cap) { if (slot_out) *slot_out = -1; return -1; }
    int64_t s = (int64_t)(h & (uint64_t)(m->index_cap - 1));
    int64_t first_tomb = -1;
    for (;;) {
        int64_t ix = m->index[s];
        if (ix == -1) {
            if (slot_out) *slot_out = first_tomb >= 0 ? first_tomb : s;
            return -1;
        }
        if (ix == -2) {
            if (first_tomb < 0) first_tomb = s;
        } else {
            uint8_t *e = ENT(m, d, ix);
            if (EHASH(e) == h && d->eq(e + d->koff, key)) {
                if (slot_out) *slot_out = s;
                return ix;
            }
        }
        s = (s + 1) & (m->index_cap - 1);
    }
}

void *kite_map_find(const kite_map *m, const kite_map_desc *d, const void *key) {
    if (!m->len) return NULL;
    uint64_t h = d->hash(key);
    int64_t ix = map_lookup(m, d, key, h, NULL);
    return ix >= 0 ? ENT(m, d, ix) : NULL;
}

// Make room for one more entry: compact away removed entries or grow, and keep
// the index below 75% load. Needs only the stored hashes, not the key type.
void kite_map_reserve1(kite_map *m, size_t esize) {
    kite_map_desc d = { esize, 0, 0, NULL, NULL };
    if (m->n_entries == m->cap_entries) {
        if (m->len < m->n_entries * 3 / 4) {
            map_compact(m, &d);
            map_reindex(m, &d, m->index_cap);
        } else {
            int64_t nc = m->cap_entries ? m->cap_entries * 2 : 8;
            m->entries = kite_realloc(m->entries, (size_t)nc * esize);
            m->cap_entries = nc;
        }
    }
    if ((m->n_entries + 1) * 4 > m->index_cap * 3) {
        int64_t ic = m->index_cap ? m->index_cap * 2 : 16;
        while ((m->n_entries + 1) * 4 > ic * 3) ic *= 2;
        map_reindex(m, &d, ic);
    }
}

void *kite_map_slot(kite_map *m, const kite_map_desc *d, const void *key, bool *existed) {
    uint64_t h = d->hash(key);
    int64_t ix = map_lookup(m, d, key, h, NULL);
    if (ix >= 0) {
        *existed = true;
        return ENT(m, d, ix);
    }
    *existed = false;
    if (m->n_entries == m->cap_entries) {
        if (m->len < m->n_entries * 3 / 4) {
            map_compact(m, d);
            map_reindex(m, d, m->index_cap);
        } else {
            int64_t nc = m->cap_entries ? m->cap_entries * 2 : 8;
            m->entries = kite_realloc(m->entries, (size_t)nc * d->esize);
            m->cap_entries = nc;
        }
    }
    if ((m->n_entries + 1) * 4 > m->index_cap * 3) {
        int64_t ic = m->index_cap ? m->index_cap * 2 : 16;
        while ((m->n_entries + 1) * 4 > ic * 3) ic *= 2;
        map_reindex(m, d, ic);
    }
    int64_t slot;
    map_lookup(m, d, key, h, &slot);
    int64_t ni = m->n_entries++;
    m->index[slot] = ni;
    uint8_t *e = ENT(m, d, ni);
    EHASH(e) = h;
    ELIVE(e) = 1;
    m->len++;
    return e;
}

bool kite_map_remove(kite_map *m, const kite_map_desc *d, const void *key, void *out_entry) {
    if (!m->len) return false;
    uint64_t h = d->hash(key);
    int64_t slot;
    int64_t ix = map_lookup(m, d, key, h, &slot);
    if (ix < 0) return false;
    uint8_t *e = ENT(m, d, ix);
    memcpy(out_entry, e, d->esize);
    ELIVE(e) = 0;
    m->index[slot] = -2;
    m->len--;
    if (ix == m->n_entries - 1) m->n_entries--;
    return true;
}

void kite_map_free(kite_map *m) {
    free(m->entries);
    free(m->index);
}

// ------------------------------------------------------------------ goroutines

typedef struct { void (*fn)(void *); void *arg; } go_start;

static void *go_main(void *p) {
    go_start s = *(go_start *)p;
    free(p);
    s.fn(s.arg);
    return NULL;
}

void kite_go(void (*fn)(void *), void *arg) {
    go_start *s = kite_alloc(sizeof *s);
    s->fn = fn;
    s->arg = arg;
    pthread_t t;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&attr, 8 << 20);
    if (pthread_create(&t, &attr, go_main, s) != 0) kite_panic("cannot start goroutine", "<runtime>");
    pthread_attr_destroy(&attr);
}

// ------------------------------------------------------------------ select support

static pthread_mutex_t g_sel_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_sel_cond = PTHREAD_COND_INITIALIZER;
static uint64_t g_sel_gen;

static void select_notify(void) {
    pthread_mutex_lock(&g_sel_lock);
    g_sel_gen++;
    pthread_cond_broadcast(&g_sel_cond);
    pthread_mutex_unlock(&g_sel_lock);
}

void kite_select_wait(void) {
    pthread_mutex_lock(&g_sel_lock);
    uint64_t gen = g_sel_gen;
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_nsec += 2000000;
    if (ts.tv_nsec >= 1000000000) { ts.tv_sec++; ts.tv_nsec -= 1000000000; }
    while (gen == g_sel_gen) {
        if (pthread_cond_timedwait(&g_sel_cond, &g_sel_lock, &ts) != 0) break;
    }
    pthread_mutex_unlock(&g_sel_lock);
}

// ------------------------------------------------------------------ channels

struct kite_chan {
    pthread_mutex_t mu;
    pthread_cond_t can_send, can_recv, taken;
    uint8_t *buf;
    size_t esize;
    int64_t cap;     // user capacity (0 = unbuffered)
    int64_t slots;   // max(cap, 1)
    int64_t head, len;
    uint64_t sent, received;
    bool closed;
    int64_t refs;
};

kite_chan *kite_chan_new(size_t esize, int64_t cap) {
    if (cap < 0) kite_panic("negative channel capacity", "<runtime>");
    kite_chan *c = kite_alloc(sizeof *c);
    memset(c, 0, sizeof *c);
    pthread_mutex_init(&c->mu, NULL);
    pthread_cond_init(&c->can_send, NULL);
    pthread_cond_init(&c->can_recv, NULL);
    pthread_cond_init(&c->taken, NULL);
    c->esize = esize ? esize : 1;
    c->cap = cap;
    c->slots = cap ? cap : 1;
    c->buf = kite_alloc((size_t)c->slots * c->esize);
    c->refs = 1;
    return c;
}

void kite_chan_retain(kite_chan *c) { __atomic_add_fetch(&c->refs, 1, __ATOMIC_RELAXED); }

void kite_chan_release(kite_chan *c, void (*drop_elem)(void *)) {
    if (!c) return;
    if (__atomic_sub_fetch(&c->refs, 1, __ATOMIC_ACQ_REL) != 0) return;
    if (drop_elem)
        for (int64_t i = 0; i < c->len; i++) drop_elem(c->buf + ((c->head + i) % c->slots) * c->esize);
    free(c->buf);
    pthread_mutex_destroy(&c->mu);
    pthread_cond_destroy(&c->can_send);
    pthread_cond_destroy(&c->can_recv);
    pthread_cond_destroy(&c->taken);
    free(c);
}

static void chan_push(kite_chan *c, const void *elem) {
    memcpy(c->buf + ((c->head + c->len) % c->slots) * c->esize, elem, c->esize);
    c->len++;
    c->sent++;
}

void kite_chan_send(kite_chan *c, const void *elem, const char *loc) {
    pthread_mutex_lock(&c->mu);
    while (!c->closed && c->len == c->slots) pthread_cond_wait(&c->can_send, &c->mu);
    if (c->closed) {
        pthread_mutex_unlock(&c->mu);
        kite_panic("send on closed channel", loc);
    }
    chan_push(c, elem);
    uint64_t my = c->sent;
    pthread_cond_signal(&c->can_recv);
    if (c->cap == 0) {
        // unbuffered: wait until a receiver takes the value
        while (c->received < my && !c->closed) pthread_cond_wait(&c->taken, &c->mu);
    }
    pthread_mutex_unlock(&c->mu);
    select_notify();
}

bool kite_chan_try_send(kite_chan *c, const void *elem, const char *loc) {
    pthread_mutex_lock(&c->mu);
    if (c->closed) {
        pthread_mutex_unlock(&c->mu);
        kite_panic("send on closed channel", loc);
    }
    if (c->len == c->slots) {
        pthread_mutex_unlock(&c->mu);
        return false;
    }
    chan_push(c, elem);
    pthread_cond_signal(&c->can_recv);
    pthread_mutex_unlock(&c->mu);
    select_notify();
    return true;
}

static void chan_pop(kite_chan *c, void *out) {
    memcpy(out, c->buf + c->head * c->esize, c->esize);
    c->head = (c->head + 1) % c->slots;
    c->len--;
    c->received++;
    pthread_cond_signal(&c->can_send);
    pthread_cond_broadcast(&c->taken);
}

bool kite_chan_recv(kite_chan *c, void *out) {
    pthread_mutex_lock(&c->mu);
    while (c->len == 0 && !c->closed) pthread_cond_wait(&c->can_recv, &c->mu);
    if (c->len == 0) {
        pthread_mutex_unlock(&c->mu);
        return false;
    }
    chan_pop(c, out);
    pthread_mutex_unlock(&c->mu);
    select_notify();
    return true;
}

// 1 = received, 2 = closed, 0 = would block
int kite_chan_try_recv(kite_chan *c, void *out) {
    pthread_mutex_lock(&c->mu);
    if (c->len == 0) {
        int r = c->closed ? 2 : 0;
        pthread_mutex_unlock(&c->mu);
        return r;
    }
    chan_pop(c, out);
    pthread_mutex_unlock(&c->mu);
    select_notify();
    return 1;
}

void kite_chan_close(kite_chan *c, const char *loc) {
    pthread_mutex_lock(&c->mu);
    if (c->closed) {
        pthread_mutex_unlock(&c->mu);
        kite_panic("close of closed channel", loc);
    }
    c->closed = true;
    pthread_cond_broadcast(&c->can_recv);
    pthread_cond_broadcast(&c->can_send);
    pthread_cond_broadcast(&c->taken);
    pthread_mutex_unlock(&c->mu);
    select_notify();
}

int64_t kite_chan_len(kite_chan *c) {
    pthread_mutex_lock(&c->mu);
    int64_t n = c->len;
    pthread_mutex_unlock(&c->mu);
    return c->cap == 0 ? 0 : n;
}

// ------------------------------------------------------------------ mutex

struct kite_mutex { pthread_mutex_t m; };

kite_mutex *kite_mutex_new(void) {
    kite_mutex *m = kite_alloc(sizeof *m);
    pthread_mutex_init(&m->m, NULL);
    return m;
}
void kite_mutex_free(kite_mutex *m) {
    if (!m) return;
    pthread_mutex_destroy(&m->m);
    free(m);
}
void kite_mutex_lock(kite_mutex *m) { pthread_mutex_lock(&m->m); }
void kite_mutex_unlock(kite_mutex *m) { pthread_mutex_unlock(&m->m); }

// ------------------------------------------------------------------ task groups

typedef struct group_task { pthread_t t; struct group_task *next; } group_task;

struct kite_group {
    pthread_mutex_t mu;
    group_task *tasks;
    bool failed;
};

kite_group *kite_group_new(void) {
    kite_group *g = kite_alloc(sizeof *g);
    pthread_mutex_init(&g->mu, NULL);
    g->tasks = NULL;
    g->failed = false;
    return g;
}

void kite_group_go(kite_group *g, void (*fn)(void *), void *arg) {
    go_start *s = kite_alloc(sizeof *s);
    s->fn = fn;
    s->arg = arg;
    group_task *t = kite_alloc(sizeof *t);
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 8 << 20);
    if (pthread_create(&t->t, &attr, go_main, s) != 0) kite_panic("cannot start task", "<runtime>");
    pthread_attr_destroy(&attr);
    pthread_mutex_lock(&g->mu);
    t->next = g->tasks;
    g->tasks = t;
    pthread_mutex_unlock(&g->mu);
}

// Returns true for the first failing task (which records its error).
bool kite_group_fail(kite_group *g) {
    pthread_mutex_lock(&g->mu);
    bool first = !g->failed;
    g->failed = true;
    pthread_mutex_unlock(&g->mu);
    return first;
}

void kite_group_wait(kite_group *g) {
    for (;;) {
        pthread_mutex_lock(&g->mu);
        group_task *t = g->tasks;
        g->tasks = NULL;
        pthread_mutex_unlock(&g->mu);
        if (!t) break;
        while (t) {
            pthread_join(t->t, NULL);
            group_task *n = t->next;
            free(t);
            t = n;
        }
    }
    pthread_mutex_destroy(&g->mu);
    free(g);
}

// ------------------------------------------------------------------ os

kite_vec kite_os_args(void) {
    kite_vec v = { NULL, 0, 0 };
    for (int i = 0; i < g_argc; i++) {
        kite_str s = { (const uint8_t *)g_argv[i], (int64_t)strlen(g_argv[i]) };
        vec_push_string(&v, s);
    }
    return v;
}

_Noreturn void kite_os_exit(int64_t code) {
    fflush(stdout);
    fflush(stderr);
    exit((int)code);
}

static char *cpath(kite_str p) {
    char *s = kite_alloc((size_t)p.len + 1);
    memcpy(s, p.ptr, (size_t)p.len);
    s[p.len] = 0;
    return s;
}

int64_t kite_os___readFile(kite_str path, kite_vec *out) {
    char *p = cpath(path);
    FILE *f = fopen(p, "rb");
    free(p);
    if (!f) return errno ? errno : 1;
    uint8_t buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        kite_vec_reserve(out, (int64_t)n, 1);
        memcpy((uint8_t *)out->ptr + out->len, buf, n);
        out->len += (int64_t)n;
    }
    int bad = ferror(f);
    fclose(f);
    return bad ? EIO : 0;
}

int64_t kite_os___writeFile(kite_str path, kite_slice data) {
    char *p = cpath(path);
    FILE *f = fopen(p, "wb");
    free(p);
    if (!f) return errno ? errno : 1;
    size_t n = fwrite(data.ptr, 1, (size_t)data.len, f);
    int bad = n != (size_t)data.len || ferror(f);
    if (fclose(f) != 0) bad = 1;
    return bad ? EIO : 0;
}

kite_string kite_os___errMsg(kite_str path, int64_t code) {
    kite_string s = { NULL, 0, 0 };
    kite_string_push(&s, path);
    kite_fmt_cstr(&s, ": ");
    kite_fmt_cstr(&s, strerror((int)code));
    return s;
}

bool kite_os___getenv(kite_str name, kite_string *out) {
    char *n = cpath(name);
    const char *v = getenv(n);
    free(n);
    if (!v) return false;
    kite_str t = { (const uint8_t *)v, (int64_t)strlen(v) };
    kite_string_push(out, t);
    return true;
}

// ------------------------------------------------------------------ time

void kite_time_sleep(int64_t ns) {
    if (ns <= 0) return;
    struct timespec ts = { (time_t)(ns / 1000000000), (long)(ns % 1000000000) };
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {}
}

int64_t kite_time_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// ------------------------------------------------------------------ ffi

void *kite_dlsym(const char *lib, const char *name) {
    void *h = RTLD_DEFAULT;
    if (lib && *lib) {
        const char *cands[6];
        char b1[256], b2[256], b3[256], b4[256];
        snprintf(b1, sizeof b1, "%s.dylib", lib);
        snprintf(b2, sizeof b2, "%s.so", lib);
        snprintf(b3, sizeof b3, "lib%s.dylib", lib);
        snprintf(b4, sizeof b4, "lib%s.so", lib);
        cands[0] = lib; cands[1] = b1; cands[2] = b2; cands[3] = b3; cands[4] = b4; cands[5] = NULL;
        h = NULL;
        for (int i = 0; cands[i] && !h; i++) h = dlopen(cands[i], RTLD_NOW | RTLD_GLOBAL);
        if (!h) {
            char msg[512];
            snprintf(msg, sizeof msg, "cannot load library `%s`", lib);
            kite_panic(msg, "<ffi>");
        }
    }
    void *f = dlsym(h, name);
    if (!f) {
        char msg[512];
        snprintf(msg, sizeof msg, "cannot find symbol `%s`%s%s", name, lib ? " in " : "", lib ? lib : "");
        kite_panic(msg, "<ffi>");
    }
    return f;
}

// ------------------------------------------------------------------ tests

int kite_run_test(void (*fn)(void), const char *name) {
    fflush(stdout);
    fflush(stderr);
    int64_t t0 = kite_time_now();
    pid_t pid = fork();
    if (pid == 0) {
        fn();
        fflush(stdout);
        _exit(0);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    double ms = (double)(kite_time_now() - t0) / 1e6;
    bool ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    fprintf(stderr, "%s %s (%.1f ms)\n", ok ? "  ok  " : "  FAIL", name, ms);
    return ok ? 0 : 1;
}
