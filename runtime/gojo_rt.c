// Gojo runtime implementation.
#include <gojo_rt.h>
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
#ifdef __APPLE__
#include <sys/random.h>
#endif

static int g_argc;
static void box_tls_setup(void);
static char **g_argv;

uint64_t gojo_hash_seed = 0x9e3779b97f4a7c15ULL;
uint64_t gojo_prelude___hashMix(uint64_t h, uint64_t x) { return gojo_hash_mix(h, x); }

void gojo_rt_init(int argc, char **argv) {
    g_argc = argc;
    g_argv = argv;
    // a fresh hash key per process (a fixed one for replayable GOJO_SEED runs)
    const char *seed = getenv("GOJO_SEED");
    uint64_t k = 0;
    if (seed && *seed) k = strtoull(seed, NULL, 10) * 0x9e3779b97f4a7c15ULL + 1;
    else if (getentropy(&k, sizeof k) != 0) k = (uint64_t)time(NULL) ^ ((uint64_t)getpid() << 32) ^ (uint64_t)(uintptr_t)&k;
    gojo_hash_seed = k;
    setvbuf(stdout, NULL, _IOFBF, 1 << 16);
    box_tls_setup();
}

// ------------------------------------------------------------------ panics

static pthread_mutex_t g_panic_lock = PTHREAD_MUTEX_INITIALIZER;

_Noreturn static void panic_end(void) {
    fflush(stdout);
    fflush(stderr);
    _exit(2);
}

_Noreturn void gojo_panic(const char *msg, const char *loc) {
    pthread_mutex_lock(&g_panic_lock);
    fflush(stdout);
    fprintf(stderr, "panic: %s\n    at %s\n", msg, loc);
    panic_end();
}

_Noreturn void gojo_panic_str(gojo_str msg, const char *loc) {
    pthread_mutex_lock(&g_panic_lock);
    fflush(stdout);
    fprintf(stderr, "panic: %.*s\n    at %s\n", (int)msg.len, (const char *)msg.ptr, loc);
    panic_end();
}

_Noreturn void gojo_panic_buf(gojo_string *msg, const char *loc) {
    gojo_str s = { msg->ptr, msg->len };
    gojo_panic_str(s, loc);
}

_Noreturn void gojo_panic_index(int64_t i, int64_t len, const char *loc) {
    char buf[128];
    snprintf(buf, sizeof buf, "index out of range: index %lld, length %lld", (long long)i, (long long)len);
    gojo_panic(buf, loc);
}

_Noreturn void gojo_bce_failed(int slice, int64_t lo, int64_t hi, int64_t len, const char *loc) {
    char buf[200];
    if (!slice) {
        snprintf(buf, sizeof buf, "internal error: a bounds check the compiler removed failed: index %lld, length %lld (please report this)", (long long)lo, (long long)len);
    } else {
        snprintf(buf, sizeof buf, "internal error: a bounds check the compiler removed failed: slice [%lld:%lld], length %lld (please report this)", (long long)lo, (long long)hi, (long long)len);
    }
    gojo_panic(buf, loc);
}

_Noreturn void gojo_panic_overflow(const char *op, const char *loc) {
    char buf[128];
    snprintf(buf, sizeof buf, "integer overflow in %s", op);
    gojo_panic(buf, loc);
}

// ------------------------------------------------------------------ memory

void *gojo_alloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) gojo_panic("out of memory", "<runtime>");
    return p;
}

void *gojo_realloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) gojo_panic("out of memory", "<runtime>");
    return q;
}

// ------------------------------------------------------------------ small-object allocator

static __thread gojo_box_node *t_box_lists[GOJO_BOX_MAX / 16 + 1];
intptr_t gojo_box_tls;
#if defined(__APPLE__)
#include <pthread.h>
static pthread_key_t g_box_key;
#endif

__attribute__((noinline)) gojo_box_node **gojo_box_lists(void) {
    __asm__ volatile("" : : : "memory");
    return t_box_lists;
}

// Every thread that may allocate calls this first.
void gojo_thread_init(void) {
#if defined(__APPLE__)
    pthread_setspecific(g_box_key, gojo_box_lists());
#endif
}

static void box_tls_setup(void) {
#if defined(__linux__) && (defined(__aarch64__) || defined(__x86_64__))
    uintptr_t tp;
#if defined(__aarch64__)
    __asm__ volatile("mrs %0, tpidr_el0" : "=r"(tp));
#else
    __asm__ volatile("movq %%fs:0, %0" : "=r"(tp));
#endif
    gojo_box_tls = (intptr_t)((uintptr_t)gojo_box_lists() - tp);
#elif defined(__APPLE__)
    if (pthread_key_create(&g_box_key, NULL) != 0) gojo_panic("cannot create a thread key", "<runtime>");
    gojo_box_tls = (intptr_t)g_box_key;
#endif
    gojo_thread_init();
}

// Slabs are carved into blocks of one size class. They are kept on a global
// list (never returned to the OS), like most size-class allocators.
#define GOJO_SLAB (64 * 1024)
typedef struct gojo_slab { struct gojo_slab *next; } gojo_slab;
static gojo_slab *g_slabs;
static pthread_mutex_t g_slab_lock = PTHREAD_MUTEX_INITIALIZER;

void *gojo_box_refill(size_t cls) {
    size_t sz = cls << 4;
    gojo_slab *s = gojo_alloc(GOJO_SLAB);
    pthread_mutex_lock(&g_slab_lock);
    s->next = g_slabs;
    g_slabs = s;
    pthread_mutex_unlock(&g_slab_lock);
    // the first 16 bytes hold the slab header; the rest becomes blocks
    uint8_t *start = (uint8_t *)s + 16, *end = (uint8_t *)s + GOJO_SLAB;
    size_t count = (size_t)(end - start) / sz;
    // keep the first block, thread the rest onto the free list
    gojo_box_node **lists = gojo_box_lists();
    gojo_box_node *head = lists[cls];
    for (size_t i = count; i-- > 1;) {
        gojo_box_node *f = (gojo_box_node *)(start + i * sz);
        gojo_box_set_link(f, head);
        head = f;
    }
    lists[cls] = head;
    return start;
}

// ------------------------------------------------------------------ strings

static int64_t grow_cap(int64_t len, int64_t cap, int64_t extra, size_t esize, int64_t first);

static void string_reserve(gojo_string *s, int64_t extra) {
    if (extra <= s->cap - s->len) return;
    int64_t nc = grow_cap(s->len, s->cap, extra, 1, 16);
    s->ptr = gojo_realloc(s->ptr, (size_t)nc);
    s->cap = nc;
}

gojo_string gojo_string_from(gojo_str s) {
    gojo_string r = { NULL, 0, 0 };
    if (s.len) {
        r.ptr = gojo_alloc((size_t)s.len);
        memcpy(r.ptr, s.ptr, (size_t)s.len);
        r.len = r.cap = s.len;
    }
    return r;
}

gojo_string gojo_string_clone(const gojo_string *s) {
    return gojo_string_from(gojo_string_view(s));
}

void gojo_string_push(gojo_string *s, gojo_str t) {
    if (!t.len) return;
    string_reserve(s, t.len);
    memmove(s->ptr + s->len, t.ptr, (size_t)t.len);
    s->len += t.len;
}

void gojo_String_push(gojo_string *s, gojo_str t) { gojo_string_push(s, t); }

void gojo_string_push_byte(gojo_string *s, uint8_t b) {
    string_reserve(s, 1);
    s->ptr[s->len++] = b;
}

void gojo_String_pushRune(gojo_string *s, int32_t r) {
    gojo_string t = gojo_rune_to_string(r);
    gojo_string_push(s, gojo_string_view(&t));
    free(t.ptr);
}

void gojo_String_clear(gojo_string *s) { s->len = 0; }

int gojo_str_cmp(gojo_str a, gojo_str b) {
    int64_t n = a.len < b.len ? a.len : b.len;
    int c = n ? memcmp(a.ptr, b.ptr, (size_t)n) : 0;
    if (c) return c < 0 ? -1 : 1;
    return a.len < b.len ? -1 : a.len > b.len ? 1 : 0;
}

// wyhash (final version 4), keyed with gojo_hash_seed
static inline uint64_t wy_r8(const uint8_t *p) { uint64_t v; memcpy(&v, p, 8); return v; }
static inline uint64_t wy_r4(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static const uint64_t wy_secret[4] = { 0x2d358dccaa6c78a5ULL, 0x8bb84b93962eacc9ULL, 0x4b33a62ed433d4a3ULL, 0x4d5a2da51de1aa47ULL };

uint64_t gojo_str_hash(gojo_str s) {
    const uint8_t *p = s.ptr;
    uint64_t len = (uint64_t)s.len, seed = gojo_hash_seed, a, b;
    seed ^= gojo_wymix(seed ^ wy_secret[0], wy_secret[1]);
    if (len <= 16) {
        if (len >= 4) {
            a = (wy_r4(p) << 32) | wy_r4(p + ((len >> 3) << 2));
            b = (wy_r4(p + len - 4) << 32) | wy_r4(p + len - 4 - ((len >> 3) << 2));
        } else if (len > 0) {
            a = ((uint64_t)p[0] << 16) | ((uint64_t)p[len >> 1] << 8) | p[len - 1];
            b = 0;
        } else {
            a = b = 0;
        }
    } else {
        uint64_t i = len;
        if (i > 48) {
            uint64_t s1 = seed, s2 = seed;
            do {
                seed = gojo_wymix(wy_r8(p) ^ wy_secret[1], wy_r8(p + 8) ^ seed);
                s1 = gojo_wymix(wy_r8(p + 16) ^ wy_secret[2], wy_r8(p + 24) ^ s1);
                s2 = gojo_wymix(wy_r8(p + 32) ^ wy_secret[3], wy_r8(p + 40) ^ s2);
                p += 48;
                i -= 48;
            } while (i > 48);
            seed ^= s1 ^ s2;
        }
        while (i > 16) {
            seed = gojo_wymix(wy_r8(p) ^ wy_secret[1], wy_r8(p + 8) ^ seed);
            i -= 16;
            p += 16;
        }
        a = wy_r8(p + i - 16);
        b = wy_r8(p + i - 8);
    }
    a ^= wy_secret[1];
    b ^= seed;
    __uint128_t r = (__uint128_t)a * b;
    a = (uint64_t)r;
    b = (uint64_t)(r >> 64);
    return gojo_wymix(a ^ wy_secret[0] ^ len, b ^ wy_secret[1]);
}

gojo_string gojo_str_concat(gojo_str a, gojo_str b) {
    gojo_string r = { NULL, 0, 0 };
    string_reserve(&r, a.len + b.len);
    if (a.len) memcpy(r.ptr, a.ptr, (size_t)a.len);
    if (b.len) memcpy(r.ptr + a.len, b.ptr, (size_t)b.len);
    r.len = a.len + b.len;
    return r;
}

gojo_str gojo_str_slice(gojo_str s, int64_t lo, int64_t hi, const char *loc) {
    if (lo < 0 || hi > s.len || lo > hi) {
        char buf[128];
        snprintf(buf, sizeof buf, "slice bounds out of range [%lld:%lld] with length %lld", (long long)lo, (long long)hi, (long long)s.len);
        gojo_panic(buf, loc);
    }
    if ((lo < s.len && (s.ptr[lo] & 0xC0) == 0x80) || (hi < s.len && (s.ptr[hi] & 0xC0) == 0x80))
        gojo_panic("string slice is not on a UTF-8 character boundary", loc);
    gojo_str r = { s.ptr + lo, hi - lo };
    return r;
}

int32_t gojo_utf8_decode(gojo_str s, int64_t *i) {
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

gojo_string gojo_rune_to_string(int32_t r) {
    gojo_string s = { NULL, 0, 0 };
    uint8_t b[4];
    int n;
    uint32_t c = (uint32_t)r;
    if (c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) c = 0xFFFD;
    if (c < 0x80) { b[0] = (uint8_t)c; n = 1; }
    else if (c < 0x800) { b[0] = 0xC0 | (c >> 6); b[1] = 0x80 | (c & 0x3F); n = 2; }
    else if (c < 0x10000) { b[0] = 0xE0 | (c >> 12); b[1] = 0x80 | ((c >> 6) & 0x3F); b[2] = 0x80 | (c & 0x3F); n = 3; }
    else { b[0] = 0xF0 | (c >> 18); b[1] = 0x80 | ((c >> 12) & 0x3F); b[2] = 0x80 | ((c >> 6) & 0x3F); b[3] = 0x80 | (c & 0x3F); n = 4; }
    gojo_str t = { b, n };
    gojo_string_push(&s, t);
    return s;
}

// glibc declares it only with _GNU_SOURCE; every libc we target has it
void *memmem(const void *, size_t, const void *, size_t);

// memchr finds candidates for the first byte, a vectorized scan in libc; a
// long needle goes to memmem (two-way: linear time whatever the text)
static int64_t str_find(gojo_str s, gojo_str sub, int64_t from) {
    if (sub.len == 0) return from;
    if (sub.len > s.len - from) return -1;
    if (sub.len > 16) {
        const uint8_t *q = memmem(s.ptr + from, (size_t)(s.len - from), sub.ptr, (size_t)sub.len);
        return q ? (int64_t)(q - s.ptr) : -1;
    }
    const uint8_t *p = s.ptr + from, *end = s.ptr + s.len - sub.len + 1;
    // memchr matched the first byte; compare only the rest
    const uint8_t *rest = sub.ptr + 1;
    size_t nrest = (size_t)sub.len - 1;
    while (p < end) {
        p = memchr(p, sub.ptr[0], (size_t)(end - p));
        if (!p) return -1;
        if (nrest == 0 || memcmp(p + 1, rest, nrest) == 0) return (int64_t)(p - s.ptr);
        p++;
    }
    return -1;
}

bool gojo_str_contains(gojo_str s, gojo_str sub) { return str_find(s, sub, 0) >= 0; }
bool gojo_str_startsWith(gojo_str s, gojo_str p) { return p.len <= s.len && memcmp(s.ptr, p.ptr, (size_t)p.len) == 0; }
bool gojo_str_endsWith(gojo_str s, gojo_str p) { return p.len <= s.len && memcmp(s.ptr + s.len - p.len, p.ptr, (size_t)p.len) == 0; }
int64_t gojo_prelude___strFind(gojo_str s, gojo_str sub) { return str_find(s, sub, 0); }
int64_t gojo_prelude___strFindFrom(gojo_str s, gojo_str sub, int64_t from) { return str_find(s, sub, from); }

static bool is_space(uint8_t c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f'; }

gojo_str gojo_str_trimStart(gojo_str s) {
    while (s.len && is_space(s.ptr[0])) { s.ptr++; s.len--; }
    return s;
}
gojo_str gojo_str_trimEnd(gojo_str s) {
    while (s.len && is_space(s.ptr[s.len - 1])) s.len--;
    return s;
}
gojo_str gojo_str_trim(gojo_str s) { return gojo_str_trimEnd(gojo_str_trimStart(s)); }

// `s` split at `sep` (not empty) into at most `max` pieces, the last holding
// the rest. Counts first, so the Vec is allocated once at its exact size
// (growing it by realloc cost more than the search); the first cuts found
// are kept, so short strings are searched once.
static gojo_vec split_exact(gojo_str s, gojo_str sep, int64_t max) {
    int64_t cut[32];
    int64_t k = 1, start = 0;
    while (k < max) {
        int64_t j = str_find(s, sep, start);
        if (j < 0) break;
        if (k <= 32) cut[k - 1] = j;
        k++;
        start = j + sep.len;
    }
    gojo_vec v = { NULL, 0, 0 };
    gojo_vec_reserve(&v, k, sizeof(gojo_string));
    gojo_string *out = (gojo_string *)v.ptr;
    start = 0;
    for (int64_t i = 0; i < k - 1; i++) {
        int64_t j = i < 32 ? cut[i] : str_find(s, sep, start);
        gojo_str part = { s.ptr + start, j - start };
        out[i] = gojo_string_from(part);
        start = j + sep.len;
    }
    gojo_str last = { s.ptr + start, s.len - start };
    out[k - 1] = gojo_string_from(last);
    v.len = k;
    return v;
}

static void vec_push_string(gojo_vec *v, gojo_str s) {
    gojo_vec_reserve(v, 1, sizeof(gojo_string));
    ((gojo_string *)v->ptr)[v->len++] = gojo_string_from(s);
}

gojo_vec gojo_str_split(gojo_str s, gojo_str sep) {
    gojo_vec v = { NULL, 0, 0 };
    if (sep.len == 0) {
        int64_t i = 0;
        while (i < s.len) {
            int64_t st = i;
            gojo_utf8_decode(s, &i);
            gojo_str part = { s.ptr + st, i - st };
            vec_push_string(&v, part);
        }
        return v;
    }
    return split_exact(s, sep, INT64_MAX);
}

gojo_vec gojo_str_lines(gojo_str s) {
    gojo_vec v = { NULL, 0, 0 };
    int64_t start = 0;
    for (int64_t i = 0; i < s.len; i++) {
        if (s.ptr[i] == '\n') {
            int64_t end = i;
            if (end > start && s.ptr[end - 1] == '\r') end--;
            gojo_str part = { s.ptr + start, end - start };
            vec_push_string(&v, part);
            start = i + 1;
        }
    }
    if (start < s.len) {
        gojo_str part = { s.ptr + start, s.len - start };
        vec_push_string(&v, part);
    }
    return v;
}

gojo_vec gojo_str_fields(gojo_str s) {
    gojo_vec v = { NULL, 0, 0 };
    int64_t i = 0;
    while (i < s.len) {
        while (i < s.len && is_space(s.ptr[i])) i++;
        int64_t st = i;
        while (i < s.len && !is_space(s.ptr[i])) i++;
        if (i > st) {
            gojo_str part = { s.ptr + st, i - st };
            vec_push_string(&v, part);
        }
    }
    return v;
}

gojo_string gojo_str_toString(gojo_str s) { return gojo_string_from(s); }

gojo_string gojo_str_repeat(gojo_str s, int64_t n) {
    gojo_string r = { NULL, 0, 0 };
    if (n <= 0 || s.len == 0) return r;
    // the whole size first: `"ab".repeat(maxInt)` is a panic, not a long loop
    if (n > PTRDIFF_MAX / s.len) gojo_panic("capacity overflow", "<runtime>");
    string_reserve(&r, s.len * n);
    for (int64_t i = 0; i < n; i++) gojo_string_push(&r, s);
    return r;
}

gojo_string gojo_str_replace(gojo_str s, gojo_str from, gojo_str to) {
    gojo_string r = { NULL, 0, 0 };
    if (from.len == 0) return gojo_string_from(s);
    int64_t start = 0;
    for (;;) {
        int64_t j = str_find(s, from, start);
        if (j < 0) break;
        gojo_str part = { s.ptr + start, j - start };
        gojo_string_push(&r, part);
        gojo_string_push(&r, to);
        start = j + from.len;
    }
    gojo_str last = { s.ptr + start, s.len - start };
    gojo_string_push(&r, last);
    return r;
}

gojo_vec gojo_str_runes(gojo_str s) {
    gojo_vec v = { NULL, 0, 0 };
    int64_t i = 0;
    while (i < s.len) {
        int32_t r = gojo_utf8_decode(s, &i);
        gojo_vec_reserve(&v, 1, sizeof(int32_t));
        ((int32_t *)v.ptr)[v.len++] = r;
    }
    return v;
}

bool gojo_prelude___parseInt(gojo_str s, int64_t *out) {
    char buf[64];
    if (s.len == 0 || s.len >= (int64_t)sizeof buf) return false;
    if (memchr(s.ptr, 0, (size_t)s.len)) return false; // "12\0x" is not a number
    memcpy(buf, s.ptr, (size_t)s.len);
    buf[s.len] = 0;
    char *end;
    errno = 0;
    long long v = strtoll(buf, &end, 10);
    if (errno || *end || isspace((unsigned char)buf[0])) return false;
    *out = v;
    return true;
}

bool gojo_prelude___parseFloat(gojo_str s, double *out) {
    char buf[128];
    if (s.len == 0 || s.len >= (int64_t)sizeof buf) return false;
    if (memchr(s.ptr, 0, (size_t)s.len)) return false; // "12\0x" is not a number
    memcpy(buf, s.ptr, (size_t)s.len);
    buf[s.len] = 0;
    char *end;
    double v = strtod(buf, &end);
    if (*end || isspace((unsigned char)buf[0])) return false;
    *out = v;
    return true;
}

// ------------------------------------------------------------------ formatting

void gojo_fmt_cstr(gojo_string *b, const char *s) {
    gojo_str t = { (const uint8_t *)s, (int64_t)strlen(s) };
    gojo_string_push(b, t);
}

void gojo_fmt_u64(gojo_string *b, uint64_t v) {
    uint8_t buf[24];
    int i = 24;
    do {
        buf[--i] = (uint8_t)('0' + v % 10);
        v /= 10;
    } while (v);
    gojo_str t = { buf + i, 24 - i };
    gojo_string_push(b, t);
}

void gojo_fmt_i64(gojo_string *b, int64_t v) {
    if (v < 0) {
        gojo_string_push_byte(b, '-');
        gojo_fmt_u64(b, (uint64_t)0 - (uint64_t)v);
    } else {
        gojo_fmt_u64(b, (uint64_t)v);
    }
}

void gojo_fmt_f64(gojo_string *b, double v) {
    char buf[64];
    if (isnan(v)) { gojo_fmt_cstr(b, "NaN"); return; }
    if (isinf(v)) { gojo_fmt_cstr(b, v > 0 ? "+Inf" : "-Inf"); return; }
    // the shortest digit count that round-trips. When it is at most 15,
    // rounding to 15 digits gives exactly those digits then zeros (a normal
    // double's half-ulp is below half a unit in the 15th digit); otherwise
    // the nearest 16-digit decimal round-trips if any does, and 17 always
    // does. Subnormals have less precision: they count up from 1.
    int p = fabs(v) < 2.2250738585072014e-308 ? 1 : 15;
    for (; p < 17; p++) {
        snprintf(buf, sizeof buf, "%.*e", p - 1, v);
        if (strtod(buf, NULL) == v) break;
    }
    if (p == 15) {
        // drop the trailing zeros of the mantissa
        char *e = strchr(buf, 'e');
        while (p > 1 && e[-1] == '0') { e--; p--; }
    }
    snprintf(buf, sizeof buf, "%.*e", p - 1, v);
    int exp = atoi(strchr(buf, 'e') + 1);
    if (exp >= -4 && exp < 21) {
        // plain notation, like Go's %v
        int decimals = p - 1 - exp;
        snprintf(buf, sizeof buf, "%.*f", decimals > 0 ? decimals : 0, v);
    } else {
        snprintf(buf, sizeof buf, "%.*g", p, v);
    }
    gojo_fmt_cstr(b, buf);
}

void gojo_fmt_bool(gojo_string *b, bool v) { gojo_fmt_cstr(b, v ? "true" : "false"); }

// ---- fmt.format placeholders

void gojo_fmt_radix(gojo_string *b, int64_t v, bool is_signed, int64_t radix, bool upper) {
    uint64_t u = (uint64_t)v;
    if (is_signed && v < 0) {
        gojo_string_push_byte(b, '-');
        u = (uint64_t)(-(v + 1)) + 1;
    }
    char buf[72];
    int n = 0;
    const char *digits = upper ? "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ" : "0123456789abcdefghijklmnopqrstuvwxyz";
    do {
        buf[n++] = digits[u % (uint64_t)radix];
        u /= (uint64_t)radix;
    } while (u);
    while (n) gojo_string_push_byte(b, (uint8_t)buf[--n]);
}

void gojo_fmt_fixed(gojo_string *b, double v, int64_t prec) {
    if (isnan(v)) { gojo_fmt_cstr(b, "NaN"); return; }
    if (isinf(v)) { gojo_fmt_cstr(b, v > 0 ? "+Inf" : "-Inf"); return; }
    int n = snprintf(NULL, 0, "%.*f", (int)prec, v);
    char *buf = gojo_alloc((size_t)n + 1);
    snprintf(buf, (size_t)n + 1, "%.*f", (int)prec, v);
    gojo_fmt_cstr(b, buf);
    free(buf);
}

static void push_rune(gojo_string *b, uint32_t r) {
    if (r < 0x80) {
        gojo_string_push_byte(b, (uint8_t)r);
    } else if (r < 0x800) {
        gojo_string_push_byte(b, (uint8_t)(0xC0 | (r >> 6)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | (r & 0x3F)));
    } else if (r < 0x10000) {
        gojo_string_push_byte(b, (uint8_t)(0xE0 | (r >> 12)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | ((r >> 6) & 0x3F)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | (r & 0x3F)));
    } else {
        gojo_string_push_byte(b, (uint8_t)(0xF0 | (r >> 18)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | ((r >> 12) & 0x3F)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | ((r >> 6) & 0x3F)));
        gojo_string_push_byte(b, (uint8_t)(0x80 | (r & 0x3F)));
    }
}

// Appends `t` (then frees it) padded to `width` characters. `align` is '<',
// '>', '^' or 0 (numbers right, the rest left); `zero` pads numbers with zeros
// after the sign.
void gojo_fmt_pad(gojo_string *b, gojo_string *t, int64_t width, int64_t align, uint32_t fill, bool zero, bool numeric) {
    int64_t chars = 0;
    for (int64_t i = 0; i < t->len; i++) chars += (t->ptr[i] & 0xC0) != 0x80;
    int64_t pad = width - chars;
    gojo_str whole = { t->ptr, t->len };
    if (pad <= 0) {
        gojo_string_push(b, whole);
    } else if (zero && numeric && !align) {
        int64_t at = t->len > 0 && (t->ptr[0] == '-' || t->ptr[0] == '+') ? 1 : 0;
        gojo_str sign = { t->ptr, at }, rest = { t->ptr + at, t->len - at };
        gojo_string_push(b, sign);
        for (int64_t i = 0; i < pad; i++) gojo_string_push_byte(b, '0');
        gojo_string_push(b, rest);
    } else {
        int64_t a = align ? align : (numeric ? '>' : '<');
        int64_t before = a == '>' ? pad : a == '^' ? pad / 2 : 0;
        for (int64_t i = 0; i < before; i++) push_rune(b, fill);
        gojo_string_push(b, whole);
        for (int64_t i = 0; i < pad - before; i++) push_rune(b, fill);
    }
    free(t->ptr);
}

void gojo_fmt_str(gojo_string *b, gojo_str s, int nested) {
    if (!nested) { gojo_string_push(b, s); return; }
    gojo_string_push_byte(b, '"');
    for (int64_t i = 0; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        switch (c) {
        case '"': gojo_fmt_cstr(b, "\\\""); break;
        case '\\': gojo_fmt_cstr(b, "\\\\"); break;
        case '\n': gojo_fmt_cstr(b, "\\n"); break;
        case '\t': gojo_fmt_cstr(b, "\\t"); break;
        case '\r': gojo_fmt_cstr(b, "\\r"); break;
        default:
            if (c < 0x20) {
                char buf[8];
                snprintf(buf, sizeof buf, "\\x%02x", c);
                gojo_fmt_cstr(b, buf);
            } else gojo_string_push_byte(b, c);
        }
    }
    gojo_string_push_byte(b, '"');
}

void gojo_fmt_ptr(gojo_string *b, const void *p) {
    char buf[32];
    snprintf(buf, sizeof buf, "%p", p);
    gojo_fmt_cstr(b, buf);
}

static pthread_mutex_t g_out_lock = PTHREAD_MUTEX_INITIALIZER;

void gojo_write(int fd, gojo_string *b) {
    // a write to a full pipe blocks: mark it like a C call
    gojo_ffi_enter();
    pthread_mutex_lock(&g_out_lock);
    FILE *f = fd == 2 ? stderr : stdout;
    if (fd == 2) fflush(stdout);
    fwrite(b->ptr, 1, (size_t)b->len, f);
    if (fd == 2) fflush(stderr);
    pthread_mutex_unlock(&g_out_lock);
    gojo_ffi_exit();
    free(b->ptr);
}

// ------------------------------------------------------------------ vectors

// The capacity for `len + extra` elements: at least double the old one, and
// never more than fits in memory (a larger request is a panic, not a short
// buffer).
static int64_t grow_cap(int64_t len, int64_t cap, int64_t extra, size_t esize, int64_t first) {
    int64_t max = (int64_t)(PTRDIFF_MAX / (esize ? esize : 1));
    if (extra < 0 || extra > max - len) gojo_panic("capacity overflow", "<runtime>");
    int64_t need = len + extra;
    int64_t nc = cap ? cap : first;
    while (nc < need) nc = nc > max / 2 ? max : nc * 2;
    return nc;
}

void gojo_vec_grow(gojo_vec *v, int64_t extra, size_t esize) {
    int64_t nc = grow_cap(v->len, v->cap, extra, esize, 4);
    v->ptr = gojo_realloc(v->ptr, (size_t)nc * esize);
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

void gojo_sort(void *base, int64_t n, size_t size, int (*cmp)(const void *, const void *, void *), void *ctx) {
    if (n < 2) return;
    uint8_t *tmp = gojo_alloc((size_t)n * size);
    merge_sort(base, tmp, n, size, cmp, ctx);
    free(tmp);
}

// ------------------------------------------------------------------ maps
// Insertion-ordered: an array of entries plus an open-addressing index.
// Entry layout: [uint64 hash][int64 live][K][V] as described by gojo_map_desc.

#define ENT(m, d, i) ((m)->entries + (size_t)(i) * (d)->esize)
#define EHASH(e) (*(uint64_t *)(e))
#define ELIVE(e) (*(int64_t *)((uint8_t *)(e) + 8))

static void map_reindex(gojo_map *m, const gojo_map_desc *d, int64_t icap) {
    free(m->index);
    m->index = gojo_alloc((size_t)icap * sizeof(int64_t));
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

static void map_compact(gojo_map *m, const gojo_map_desc *d) {
    int64_t k = 0;
    for (int64_t i = 0; i < m->n_entries; i++) {
        uint8_t *e = ENT(m, d, i);
        if (!ELIVE(e)) continue;
        if (k != i) memcpy(ENT(m, d, k), e, d->esize);
        k++;
    }
    m->n_entries = k;
}

static int64_t map_lookup(const gojo_map *m, const gojo_map_desc *d, const void *key, uint64_t h, int64_t *slot_out) {
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

void *gojo_map_find(const gojo_map *m, const gojo_map_desc *d, const void *key) {
    if (!m->len) return NULL;
    uint64_t h = d->hash(key);
    int64_t ix = map_lookup(m, d, key, h, NULL);
    return ix >= 0 ? ENT(m, d, ix) : NULL;
}

// Make room for one more entry: compact away removed entries or grow, and keep
// the index below 75% load. Needs only the stored hashes, not the key type.
void gojo_map_reserve1(gojo_map *m, size_t esize) {
    gojo_map_desc d = { esize, 0, 0, NULL, NULL };
    if (m->n_entries == m->cap_entries) {
        if (m->len < m->n_entries * 3 / 4) {
            map_compact(m, &d);
            map_reindex(m, &d, m->index_cap);
        } else {
            int64_t nc = m->cap_entries ? m->cap_entries * 2 : 8;
            m->entries = gojo_realloc(m->entries, (size_t)nc * esize);
            m->cap_entries = nc;
        }
    }
    if ((m->n_entries + 1) * 4 > m->index_cap * 3) {
        int64_t ic = m->index_cap ? m->index_cap * 2 : 16;
        while ((m->n_entries + 1) * 4 > ic * 3) ic *= 2;
        map_reindex(m, &d, ic);
    }
}

void *gojo_map_slot(gojo_map *m, const gojo_map_desc *d, const void *key, bool *existed) {
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
            m->entries = gojo_realloc(m->entries, (size_t)nc * d->esize);
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

bool gojo_map_remove(gojo_map *m, const gojo_map_desc *d, const void *key, void *out_entry) {
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

void gojo_map_free(gojo_map *m) {
    free(m->entries);
    free(m->index);
}

// ------------------------------------------------------------------ os

gojo_vec gojo_os_args(void) {
    gojo_vec v = { NULL, 0, 0 };
    for (int i = 0; i < g_argc; i++) {
        gojo_str s = { (const uint8_t *)g_argv[i], (int64_t)strlen(g_argv[i]) };
        vec_push_string(&v, s);
    }
    return v;
}

_Noreturn void gojo_os_exit(int64_t code) {
    fflush(stdout);
    fflush(stderr);
    exit((int)code);
}

// A path, name or argument can't contain a NUL byte: C would see only what
// comes before it (`"a.txt\0/x"` would open `a.txt`), so such calls fail with
// EINVAL instead.
static bool has_nul(gojo_str s) { return s.len && memchr(s.ptr, 0, (size_t)s.len); }

static char *cpath(gojo_str p) {
    char *s = gojo_alloc((size_t)p.len + 1);
    memcpy(s, p.ptr, (size_t)p.len);
    s[p.len] = 0;
    return s;
}

static int64_t os_readFile_impl(gojo_str path, gojo_vec *out) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    FILE *f = fopen(p, "rb");
    free(p);
    if (!f) return errno ? errno : 1;
    uint8_t buf[65536];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        gojo_vec_reserve(out, (int64_t)n, 1);
        memcpy((uint8_t *)out->ptr + out->len, buf, n);
        out->len += (int64_t)n;
    }
    int bad = ferror(f);
    fclose(f);
    return bad ? EIO : 0;
}

static int64_t os_writeFile_impl(gojo_str path, gojo_slice data) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    FILE *f = fopen(p, "wb");
    free(p);
    if (!f) return errno ? errno : 1;
    size_t n = fwrite(data.ptr, 1, (size_t)data.len, f);
    int bad = n != (size_t)data.len || ferror(f);
    if (fclose(f) != 0) bad = 1;
    return bad ? EIO : 0;
}


bool gojo_os___getenv(gojo_str name, gojo_string *out) {
    if (has_nul(name)) return false;
    char *n = cpath(name);
    const char *v = getenv(n);
    free(n);
    if (!v) return false;
    gojo_str t = { (const uint8_t *)v, (int64_t)strlen(v) };
    gojo_string_push(out, t);
    return true;
}

// ------------------------------------------------------------------ time

// Simulation (deterministic tests): one thread, a seeded scheduler and a
// virtual clock that jumps to the next timer when every task waits.
int gojo_sim_on;
int64_t gojo_sim_clock;
// Debug builds: lock-order checking.
int gojo_debug_checks;

int64_t gojo_time_now(void) {
    if (gojo_sim_on) return __atomic_load_n(&gojo_sim_clock, __ATOMIC_RELAXED);
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// ------------------------------------------------------------------ ffi

void *gojo_dlsym(const char *lib, const char *name) {
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
            gojo_panic(msg, "<ffi>");
        }
    }
    void *f = dlsym(h, name);
    if (!f) {
        char msg[512];
        snprintf(msg, sizeof msg, "cannot find symbol `%s`%s%s", name, lib ? " in " : "", lib ? lib : "");
        gojo_panic(msg, "<ffi>");
    }
    return f;
}


// File I/O blocks its thread: give the CPU slot to another worker meanwhile.
int64_t gojo_os___readFile(gojo_str path, gojo_vec *out) {
    int f = gojo_sim_disk_fault(-1, false);
    if (f) return f;
    gojo_blocking_enter();
    int64_t r = os_readFile_impl(path, out);
    gojo_ffi_exit();
    return r;
}

int64_t gojo_os___writeFile(gojo_str path, gojo_slice data) {
    int f = gojo_sim_disk_fault(-1, true);
    if (f) {
        // a failed write may leave part of the file
        gojo_slice part = { data.ptr, gojo_sim_short_read(data.len + 1) - 1 };
        os_writeFile_impl(path, part);
        return f;
    }
    gojo_blocking_enter();
    int64_t r = os_writeFile_impl(path, data);
    gojo_ffi_exit();
    return r;
}
