// Kite runtime interface. Included by every generated C file.
#ifndef KITE_RT_H
#define KITE_RT_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>

typedef uint8_t kite_unit;

typedef struct { const uint8_t *ptr; int64_t len; } kite_str;
typedef struct { uint8_t *ptr; int64_t len; int64_t cap; } kite_string;
typedef struct { void *ptr; int64_t len; int64_t cap; } kite_vec;
typedef struct { void *ptr; int64_t len; } kite_slice;
typedef struct { void *fn; void *env; void (*drop)(void *); } kite_func;
typedef struct { void *data; const void *vt; } kite_iface;

// Common prefix of every interface vtable.
typedef struct {
    void (*drop)(void *);
    void (*fmt)(kite_string *, void *, int);
    uint64_t tid;
    size_t size;
} kite_vt_base;

typedef struct {
    uint8_t *entries;
    int64_t *index;
    int64_t len;
    int64_t n_entries;
    int64_t cap_entries;
    int64_t index_cap;
} kite_map;

typedef struct {
    size_t esize, koff, voff;
    uint64_t (*hash)(const void *);
    bool (*eq)(const void *, const void *);
} kite_map_desc;

typedef struct kite_chan kite_chan;
typedef struct kite_mutex kite_mutex;
typedef struct kite_group kite_group;

// ---- panics
_Noreturn void kite_panic(const char *msg, const char *loc);
_Noreturn void kite_panic_str(kite_str msg, const char *loc);
_Noreturn void kite_panic_buf(kite_string *msg, const char *loc);
_Noreturn void kite_panic_index(int64_t i, int64_t len, const char *loc);
_Noreturn void kite_panic_overflow(const char *op, const char *loc);

static inline int64_t kite_idx(int64_t i, int64_t len, const char *loc) {
    if ((uint64_t)i >= (uint64_t)len) kite_panic_index(i, len, loc);
    return i;
}

// ---- checked arithmetic
#define KITE_ARITH(N, T, MIN, BITS) \
    static inline T kite_add_##N(T a, T b, const char *loc) { T r; if (__builtin_add_overflow(a, b, &r)) kite_panic_overflow("addition", loc); return r; } \
    static inline T kite_sub_##N(T a, T b, const char *loc) { T r; if (__builtin_sub_overflow(a, b, &r)) kite_panic_overflow("subtraction", loc); return r; } \
    static inline T kite_mul_##N(T a, T b, const char *loc) { T r; if (__builtin_mul_overflow(a, b, &r)) kite_panic_overflow("multiplication", loc); return r; } \
    static inline T kite_div_##N(T a, T b, const char *loc) { if (b == 0) kite_panic("division by zero", loc); if (MIN != 0 && a == (T)MIN && b == (T)-1) kite_panic_overflow("division", loc); return a / b; } \
    static inline T kite_rem_##N(T a, T b, const char *loc) { if (b == 0) kite_panic("division by zero", loc); if (MIN != 0 && a == (T)MIN && b == (T)-1) return 0; return a % b; } \
    static inline T kite_neg_##N(T a, const char *loc) { if (MIN != 0 && a == (T)MIN) kite_panic_overflow("negation", loc); return (T)(0 - a); } \
    static inline T kite_shl_##N(T a, int64_t s, const char *loc) { if (s < 0 || s >= BITS) kite_panic("shift amount out of range", loc); return (T)((uint64_t)a << s); } \
    static inline T kite_shr_##N(T a, int64_t s, const char *loc) { if (s < 0 || s >= BITS) kite_panic("shift amount out of range", loc); return (T)(a >> s); }

KITE_ARITH(i8, int8_t, INT8_MIN, 8)
KITE_ARITH(i16, int16_t, INT16_MIN, 16)
KITE_ARITH(i32, int32_t, INT32_MIN, 32)
KITE_ARITH(i64, int64_t, INT64_MIN, 64)
KITE_ARITH(u8, uint8_t, 0, 8)
KITE_ARITH(u16, uint16_t, 0, 16)
KITE_ARITH(u32, uint32_t, 0, 32)
KITE_ARITH(u64, uint64_t, 0, 64)

static inline int64_t kite_check_range(double v, double lo, double hi, const char *loc) {
    if (!(v >= lo && v <= hi)) kite_panic("numeric conversion out of range", loc);
    return (int64_t)v;
}
static inline void kite_check_irange(int64_t v, int64_t lo, int64_t hi, const char *loc) {
    if (v < lo || v > hi) kite_panic("integer conversion out of range", loc);
}
static inline void kite_check_urange(uint64_t v, uint64_t hi, const char *loc) {
    if (v > hi) kite_panic("integer conversion out of range", loc);
}

// ---- memory
void *kite_alloc(size_t n);
void *kite_realloc(void *p, size_t n);
static inline void kite_free(void *p) { free(p); }

// ---- strings
static inline kite_str kite_str_lit(const char *s, int64_t n) { kite_str r = { (const uint8_t *)s, n }; return r; }
static inline kite_str kite_string_view(const kite_string *s) { kite_str r = { s->ptr, s->len }; return r; }
kite_string kite_string_from(kite_str s);
kite_string kite_string_clone(const kite_string *s);
static inline void kite_string_drop(kite_string *s) { free(s->ptr); }
void kite_string_push(kite_string *s, kite_str t);
void kite_String_push(kite_string *s, kite_str t);
void kite_string_push_byte(kite_string *s, uint8_t b);
void kite_String_pushRune(kite_string *s, int32_t r);
void kite_String_clear(kite_string *s);
bool kite_str_eq(kite_str a, kite_str b);
int kite_str_cmp(kite_str a, kite_str b);
uint64_t kite_str_hash(kite_str s);
kite_string kite_str_concat(kite_str a, kite_str b);
kite_str kite_str_slice(kite_str s, int64_t lo, int64_t hi, const char *loc);
int32_t kite_utf8_decode(kite_str s, int64_t *i);
kite_string kite_rune_to_string(int32_t r);

bool kite_str_contains(kite_str s, kite_str sub);
bool kite_str_startsWith(kite_str s, kite_str p);
bool kite_str_endsWith(kite_str s, kite_str p);
int64_t kite_prelude___strFind(kite_str s, kite_str sub);
kite_str kite_str_trim(kite_str s);
kite_str kite_str_trimStart(kite_str s);
kite_str kite_str_trimEnd(kite_str s);
kite_vec kite_str_split(kite_str s, kite_str sep);
kite_vec kite_str_lines(kite_str s);
kite_vec kite_str_fields(kite_str s);
kite_string kite_str_toUpper(kite_str s);
kite_string kite_str_toLower(kite_str s);
kite_string kite_str_toString(kite_str s);
kite_string kite_str_repeat(kite_str s, int64_t n);
kite_string kite_str_replace(kite_str s, kite_str from, kite_str to);
kite_vec kite_str_runes(kite_str s);
bool kite_prelude___parseInt(kite_str s, int64_t *out);
bool kite_prelude___parseFloat(kite_str s, double *out);

// ---- formatting
void kite_fmt_i64(kite_string *b, int64_t v);
void kite_fmt_u64(kite_string *b, uint64_t v);
void kite_fmt_f64(kite_string *b, double v);
void kite_fmt_bool(kite_string *b, bool v);
void kite_fmt_str(kite_string *b, kite_str s, int nested);
void kite_fmt_cstr(kite_string *b, const char *s);
void kite_fmt_ptr(kite_string *b, const void *p);
void kite_write(int fd, kite_string *b);

// ---- vectors
void kite_vec_grow(kite_vec *v, int64_t need, size_t esize);
static inline void kite_vec_reserve(kite_vec *v, int64_t extra, size_t esize) {
    if (v->len + extra > v->cap) kite_vec_grow(v, v->len + extra, esize);
}
void kite_sort(void *base, int64_t n, size_t size, int (*cmp)(const void *, const void *, void *), void *ctx);

// ---- maps
void *kite_map_find(const kite_map *m, const kite_map_desc *d, const void *key);
void *kite_map_slot(kite_map *m, const kite_map_desc *d, const void *key, bool *existed);
bool kite_map_remove(kite_map *m, const kite_map_desc *d, const void *key, void *out_entry);
void kite_map_free(kite_map *m);
static inline uint64_t kite_hash_u64(uint64_t x) {
    x ^= x >> 33; x *= 0xff51afd7ed558ccdULL; x ^= x >> 33; x *= 0xc4ceb9fe1a85ec53ULL; x ^= x >> 33;
    return x;
}
static inline uint64_t kite_hash_mix(uint64_t h, uint64_t x) { return kite_hash_u64(h ^ (x + 0x9e3779b97f4a7c15ULL + (h << 6) + (h >> 2))); }

// ---- concurrency
void kite_go(void (*fn)(void *), void *arg);
kite_chan *kite_chan_new(size_t esize, int64_t cap);
void kite_chan_retain(kite_chan *c);
void kite_chan_release(kite_chan *c, void (*drop_elem)(void *));
void kite_chan_send(kite_chan *c, const void *elem, const char *loc);
bool kite_chan_recv(kite_chan *c, void *out);
int kite_chan_try_recv(kite_chan *c, void *out);
bool kite_chan_try_send(kite_chan *c, const void *elem, const char *loc);
void kite_chan_close(kite_chan *c, const char *loc);
int64_t kite_chan_len(kite_chan *c);
void kite_select_wait(void);
kite_mutex *kite_mutex_new(void);
void kite_mutex_free(kite_mutex *m);
void kite_mutex_lock(kite_mutex *m);
void kite_mutex_unlock(kite_mutex *m);
kite_group *kite_group_new(void);
void kite_group_go(kite_group *g, void (*fn)(void *), void *arg);
void kite_group_wait(kite_group *g);
bool kite_group_fail(kite_group *g);

// ---- os / time / ffi
void kite_rt_init(int argc, char **argv);
kite_vec kite_os_args(void);
_Noreturn void kite_os_exit(int64_t code);
int64_t kite_os___readFile(kite_str path, kite_vec *out);
int64_t kite_os___writeFile(kite_str path, kite_slice data);
kite_string kite_os___errMsg(kite_str path, int64_t code);
bool kite_os___getenv(kite_str name, kite_string *out);
void kite_time_sleep(int64_t ns);
int64_t kite_time_now(void);
void *kite_dlsym(const char *lib, const char *name);
int kite_run_test(void (*fn)(void), const char *name);

#endif
