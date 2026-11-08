// Gojo runtime interface. Included by every generated C file.
#ifndef GOJO_RT_H
#define GOJO_RT_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>

typedef uint8_t gojo_unit;

typedef struct { const uint8_t *ptr; int64_t len; } gojo_str;
typedef struct { uint8_t *ptr; int64_t len; int64_t cap; } gojo_string;
typedef struct { void *ptr; int64_t len; int64_t cap; } gojo_vec;
typedef struct { void *ptr; int64_t len; } gojo_slice;
typedef struct { void *fn; void *env; void (*drop)(void *); } gojo_func;
typedef struct { void *data; const void *vt; } gojo_iface;

// Common prefix of every interface vtable.
typedef struct {
    void (*drop)(void *);
    void (*fmt)(gojo_string *, void *, int);
    uint64_t tid;
    size_t size;
} gojo_vt_base;

typedef struct {
    uint8_t *entries;
    int64_t *index;
    int64_t len;
    int64_t n_entries;
    int64_t cap_entries;
    int64_t index_cap;
} gojo_map;

typedef struct {
    size_t esize, koff, voff;
    uint64_t (*hash)(const void *);
    bool (*eq)(const void *, const void *);
} gojo_map_desc;

typedef struct gojo_chan gojo_chan;
typedef struct gojo_mutex gojo_mutex;
typedef struct gojo_group gojo_group;
typedef struct gojo_task gojo_task;

// ---- panics
_Noreturn void gojo_panic(const char *msg, const char *loc);
_Noreturn void gojo_panic_str(gojo_str msg, const char *loc);
_Noreturn void gojo_panic_buf(gojo_string *msg, const char *loc);
_Noreturn void gojo_panic_index(int64_t i, int64_t len, const char *loc);
_Noreturn void gojo_panic_overflow(const char *op, const char *loc);

static inline int64_t gojo_idx(int64_t i, int64_t len, const char *loc) {
    if ((uint64_t)i >= (uint64_t)len) gojo_panic_index(i, len, loc);
    return i;
}

// ---- checked arithmetic
#define GOJO_ARITH(N, T, MIN, BITS) \
    static inline T gojo_add_##N(T a, T b, const char *loc) { T r; if (__builtin_add_overflow(a, b, &r)) gojo_panic_overflow("addition", loc); return r; } \
    static inline T gojo_sub_##N(T a, T b, const char *loc) { T r; if (__builtin_sub_overflow(a, b, &r)) gojo_panic_overflow("subtraction", loc); return r; } \
    static inline T gojo_mul_##N(T a, T b, const char *loc) { T r; if (__builtin_mul_overflow(a, b, &r)) gojo_panic_overflow("multiplication", loc); return r; } \
    static inline T gojo_div_##N(T a, T b, const char *loc) { if (b == 0) gojo_panic("division by zero", loc); if (MIN != 0 && a == (T)MIN && b == (T)-1) gojo_panic_overflow("division", loc); return a / b; } \
    static inline T gojo_rem_##N(T a, T b, const char *loc) { if (b == 0) gojo_panic("division by zero", loc); if (MIN != 0 && a == (T)MIN && b == (T)-1) return 0; return a % b; } \
    static inline T gojo_neg_##N(T a, const char *loc) { if (MIN != 0 && a == (T)MIN) gojo_panic_overflow("negation", loc); return (T)(0 - a); } \
    static inline T gojo_shl_##N(T a, int64_t s, const char *loc) { if (s < 0 || s >= BITS) gojo_panic("shift amount out of range", loc); return (T)((uint64_t)a << s); } \
    static inline T gojo_shr_##N(T a, int64_t s, const char *loc) { if (s < 0 || s >= BITS) gojo_panic("shift amount out of range", loc); return (T)(a >> s); }

GOJO_ARITH(i8, int8_t, INT8_MIN, 8)
GOJO_ARITH(i16, int16_t, INT16_MIN, 16)
GOJO_ARITH(i32, int32_t, INT32_MIN, 32)
GOJO_ARITH(i64, int64_t, INT64_MIN, 64)
GOJO_ARITH(u8, uint8_t, 0, 8)
GOJO_ARITH(u16, uint16_t, 0, 16)
GOJO_ARITH(u32, uint32_t, 0, 32)
GOJO_ARITH(u64, uint64_t, 0, 64)

static inline int64_t gojo_check_range(double v, double lo, double hi, const char *loc) {
    if (!(v >= lo && v <= hi)) gojo_panic("numeric conversion out of range", loc);
    return (int64_t)v;
}
static inline void gojo_check_irange(int64_t v, int64_t lo, int64_t hi, const char *loc) {
    if (v < lo || v > hi) gojo_panic("integer conversion out of range", loc);
}
static inline void gojo_check_urange(uint64_t v, uint64_t hi, const char *loc) {
    if (v > hi) gojo_panic("integer conversion out of range", loc);
}

// ---- memory
void *gojo_alloc(size_t n);
void *gojo_realloc(void *p, size_t n);
static inline void gojo_free(void *p) { free(p); }

// Small-object allocator for Box / rc / arc / interface boxes. Ownership tells
// the compiler the exact size at every free, so blocks go back to a per-thread
// free list for their size class instead of through malloc.
#define GOJO_BOX_MAX 256
// A free block holds the link to the next one in its first bytes. The block
// held (and will hold) values of other types, so the link is only ever read
// and written with memcpy, which may alias anything: a typed `node->next`
// access could be reordered with the accesses of the box's own type around it
// once these functions are inlined (strict aliasing, see spec 11.1).
typedef struct gojo_box_node gojo_box_node;
static inline gojo_box_node *gojo_box_link(const void *f) { gojo_box_node *n; memcpy(&n, f, sizeof n); return n; }
static inline void gojo_box_set_link(void *f, gojo_box_node *n) { memcpy(f, &n, sizeof n); }
gojo_box_node **gojo_box_lists(void);
// The free lists live in thread-local storage. A goroutine can resume on another
// thread, and a compiler may keep a thread-local's address across calls, so the
// thread register is re-read on every use (volatile asm is never merged or moved
// across a call). Linux: offset from the thread pointer; macOS: a pthread key.
extern intptr_t gojo_box_tls;
static inline gojo_box_node **gojo_box_head(void) {
#if defined(__linux__) && defined(__aarch64__)
    uintptr_t tp;
    __asm__ volatile("mrs %0, tpidr_el0" : "=r"(tp));
    return (gojo_box_node **)(tp + gojo_box_tls);
#elif defined(__linux__) && defined(__x86_64__)
    uintptr_t tp;
    __asm__ volatile("movq %%fs:0, %0" : "=r"(tp));
    return (gojo_box_node **)(tp + gojo_box_tls);
#elif defined(__APPLE__) && defined(__aarch64__)
    uintptr_t tp;
    __asm__ volatile("mrs %0, tpidrro_el0" : "=r"(tp));
    return ((gojo_box_node ***)(tp & ~(uintptr_t)7))[gojo_box_tls];
#elif defined(__APPLE__) && defined(__x86_64__)
    gojo_box_node **p;
    __asm__ volatile("movq %%gs:(,%1,8), %0" : "=r"(p) : "r"(gojo_box_tls));
    return p;
#else
    return gojo_box_lists();
#endif
}
void *gojo_box_refill(size_t cls);
#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#define GOJO_NO_BOX_POOL 1
#endif
// TypeSanitizer only forgets a block's type when it is freed
#if __has_feature(type_sanitizer)
#define GOJO_NO_BOX_POOL 1
#endif
#endif
#if defined(__SANITIZE_ADDRESS__)
#define GOJO_NO_BOX_POOL 1
#endif
static inline void *gojo_box_alloc(size_t n) {
#ifdef GOJO_NO_BOX_POOL
    return gojo_alloc(n);   // let the sanitizer see every block
#endif
    if (n > GOJO_BOX_MAX) return gojo_alloc(n);
    size_t cls = n ? (n + 15) >> 4 : 1;
    gojo_box_node **lists = gojo_box_head();
    gojo_box_node *f = lists[cls];
    if (__builtin_expect(f != NULL, 1)) {
        lists[cls] = gojo_box_link(f);
        return f;
    }
    return gojo_box_refill(cls);
}
static inline void gojo_box_free(void *p, size_t n) {
    if (!p) return;
#ifdef GOJO_NO_BOX_POOL
    free(p);
    return;
#endif
    if (n > GOJO_BOX_MAX) { free(p); return; }
    size_t cls = n ? (n + 15) >> 4 : 1;
    gojo_box_node **lists = gojo_box_head();
    gojo_box_set_link(p, lists[cls]);
    lists[cls] = (gojo_box_node *)p;
}

// ---- strings
static inline gojo_str gojo_str_lit(const char *s, int64_t n) { gojo_str r = { (const uint8_t *)s, n }; return r; }
static inline gojo_str gojo_string_view(const gojo_string *s) { gojo_str r = { s->ptr, s->len }; return r; }
gojo_string gojo_string_from(gojo_str s);
gojo_string gojo_string_clone(const gojo_string *s);
static inline void gojo_string_drop(gojo_string *s) { free(s->ptr); }
void gojo_string_push(gojo_string *s, gojo_str t);
void gojo_String_push(gojo_string *s, gojo_str t);
void gojo_string_push_byte(gojo_string *s, uint8_t b);
void gojo_fmt_radix(gojo_string *b, int64_t v, bool is_signed, int64_t radix, bool upper);
void gojo_fmt_fixed(gojo_string *b, double v, int64_t prec);
void gojo_fmt_pad(gojo_string *b, gojo_string *t, int64_t width, int64_t align, uint32_t fill, bool zero, bool numeric);
void gojo_String_pushRune(gojo_string *s, int32_t r);
void gojo_String_clear(gojo_string *s);
bool gojo_str_eq(gojo_str a, gojo_str b);
int gojo_str_cmp(gojo_str a, gojo_str b);
uint64_t gojo_str_hash(gojo_str s);
gojo_string gojo_str_concat(gojo_str a, gojo_str b);
gojo_str gojo_str_slice(gojo_str s, int64_t lo, int64_t hi, const char *loc);
int32_t gojo_utf8_decode(gojo_str s, int64_t *i);
gojo_string gojo_rune_to_string(int32_t r);

bool gojo_str_contains(gojo_str s, gojo_str sub);
bool gojo_str_startsWith(gojo_str s, gojo_str p);
bool gojo_str_endsWith(gojo_str s, gojo_str p);
int64_t gojo_prelude___strFind(gojo_str s, gojo_str sub);
gojo_str gojo_str_trim(gojo_str s);
gojo_str gojo_str_trimStart(gojo_str s);
gojo_str gojo_str_trimEnd(gojo_str s);
gojo_vec gojo_str_split(gojo_str s, gojo_str sep);
gojo_vec gojo_str_lines(gojo_str s);
gojo_vec gojo_str_fields(gojo_str s);
gojo_string gojo_str_toUpper(gojo_str s);
gojo_string gojo_str_toLower(gojo_str s);
gojo_string gojo_str_toString(gojo_str s);
gojo_string gojo_str_repeat(gojo_str s, int64_t n);
gojo_string gojo_str_replace(gojo_str s, gojo_str from, gojo_str to);
gojo_vec gojo_str_runes(gojo_str s);
bool gojo_prelude___parseInt(gojo_str s, int64_t *out);
bool gojo_prelude___parseFloat(gojo_str s, double *out);
int64_t gojo_prelude___strRFind(gojo_str s, gojo_str sub);
int64_t gojo_prelude___strFindFrom(gojo_str s, gojo_str sub, int64_t from);
int64_t gojo_prelude___decodeAt(gojo_str s, int64_t i);
int32_t gojo_regex___fold(int32_t r);
int64_t gojo_regex___bits(int32_t r);
bool gojo_regex___exec(gojo_slice code, gojo_slice classes, gojo_slice meta, gojo_str prefix, gojo_str text, int64_t pos, int64_t nslots, gojo_vec *caps);
int64_t gojo_str_count(gojo_str s, gojo_str sub);
gojo_vec gojo_str_splitN(gojo_str s, gojo_str sep, int64_t n);
gojo_str gojo_str_trimChars(gojo_str s, gojo_str chars);
bool gojo_str_equalFold(gojo_str a, gojo_str b);
int64_t gojo_str_runeCount(gojo_str s);
bool gojo_str_isAscii(gojo_str s);
bool gojo_str_isValidUtf8(gojo_str s);
void gojo_prelude___stringInsert(gojo_string *s, int64_t at, gojo_str t);
void gojo_prelude___stringSetLen(gojo_string *s, int64_t n);
int64_t gojo_prelude___stringPopRune(gojo_string *s);
void gojo_String_reserve(gojo_string *s, int64_t n);
int64_t gojo_String_cap(gojo_string *s);
int64_t gojo_prelude___runeBits(int32_t r);
int32_t gojo_prelude___runeUpper(int32_t r);
int32_t gojo_prelude___runeLower(int32_t r);
bool gojo_parse_int(gojo_str s, int64_t radix, bool is_signed, uint64_t max, uint64_t *out);
uint64_t gojo_rand___next(void);
int64_t gojo_rand___secureFill(gojo_vec *v, int64_t n);
gojo_string gojo_hex___encode(gojo_slice d, bool upper);
int64_t gojo_hex___decode(gojo_str s, gojo_vec *out);
gojo_string gojo_base64___encode(gojo_slice d, bool url, bool pad);
int64_t gojo_base64___decode(gojo_str s, bool url, gojo_vec *out);
gojo_string gojo_url___escape(gojo_str s, int64_t mode);
bool gojo_url___unescape(gojo_str s, bool plus, bool lenient, gojo_string *out);
void gojo_crypto___init(gojo_vec *st, int64_t algo);
void gojo_crypto___update(gojo_vec *st, gojo_slice data);
void gojo_crypto___finish(gojo_vec *st, gojo_vec *out);
bool gojo_crypto___equal(gojo_slice a, gojo_slice b);
uint32_t gojo_crc32_update(uint32_t crc, gojo_slice data);
int64_t gojo_tls___client(int64_t fd, gojo_str name, gojo_str ca, bool verify, gojo_string *msg);
int64_t gojo_tls___serverCtx(gojo_str cert, gojo_str key, gojo_string *msg);
void gojo_tls___freeCtx(int64_t ctx);
int64_t gojo_tls___server(int64_t ctx, int64_t fd, gojo_string *msg);
int64_t gojo_tls___handshake(int64_t h, gojo_string *msg);
int64_t gojo_tls___read(int64_t h, gojo_vec *buf, int64_t max, gojo_string *msg);
int64_t gojo_tls___write(int64_t h, gojo_slice data, gojo_string *msg);
void gojo_tls___close(int64_t h);
uint64_t gojo_prelude___f64Bits(double x);
double gojo_prelude___f64FromBits(uint64_t b);
uint32_t gojo_prelude___f32Bits(float x);
float gojo_prelude___f32FromBits(uint32_t b);

// ---- formatting
void gojo_fmt_i64(gojo_string *b, int64_t v);
void gojo_fmt_u64(gojo_string *b, uint64_t v);
void gojo_fmt_f64(gojo_string *b, double v);
void gojo_fmt_bool(gojo_string *b, bool v);
void gojo_fmt_str(gojo_string *b, gojo_str s, int nested);
void gojo_fmt_cstr(gojo_string *b, const char *s);
void gojo_fmt_ptr(gojo_string *b, const void *p);
void gojo_write(int fd, gojo_string *b);

// ---- vectors
void gojo_vec_grow(gojo_vec *v, int64_t extra, size_t esize);
// room for `extra` more elements (`extra` > 0; no overflow in `len + extra`)
static inline void gojo_vec_reserve(gojo_vec *v, int64_t extra, size_t esize) {
    if (extra > v->cap - v->len) gojo_vec_grow(v, extra, esize);
}
void gojo_sort(void *base, int64_t n, size_t size, int (*cmp)(const void *, const void *, void *), void *ctx);

// ---- maps
void *gojo_map_find(const gojo_map *m, const gojo_map_desc *d, const void *key);
void *gojo_map_slot(gojo_map *m, const gojo_map_desc *d, const void *key, bool *existed);
bool gojo_map_remove(gojo_map *m, const gojo_map_desc *d, const void *key, void *out_entry);
void gojo_map_free(gojo_map *m);
void gojo_map_reserve1(gojo_map *m, size_t esize);
// Hashes are keyed with a random seed chosen at startup, so the keys of a
// Map built from untrusted input (headers, JSON objects) can't be picked to
// collide: the wyhash mix, a 64x64->128 bit multiply folded to 64 bits.
extern uint64_t gojo_hash_seed;
static inline uint64_t gojo_wymix(uint64_t a, uint64_t b) {
    __uint128_t r = (__uint128_t)a * b;
    return (uint64_t)r ^ (uint64_t)(r >> 64);
}
static inline uint64_t gojo_hash_u64(uint64_t x) {
    return gojo_wymix(x ^ 0x2d358dccaa6c78a5ULL, gojo_hash_seed ^ 0x8bb84b93962eacc9ULL);
}
static inline uint64_t gojo_hash_mix(uint64_t h, uint64_t x) { return gojo_wymix(h ^ 0x4b33a62ed433d4a3ULL, x ^ gojo_hash_seed); }
uint64_t gojo_prelude___hashMix(uint64_t h, uint64_t x);

// A fact for the optimizer, e.g. that a loop's block end is within the loop.
#if defined(__clang__)
#define GOJO_ASSUME(x) __builtin_assume(x)
#else
#define GOJO_ASSUME(x) do { if (!(x)) __builtin_unreachable(); } while (0)
#endif

// The assembler name of a C symbol (macOS prefixes an underscore).
#define GOJO_ASM_STR2(x) #x
#define GOJO_ASM_STR(x) GOJO_ASM_STR2(x)
#define GOJO_ASM_NAME(n) GOJO_ASM_STR(__USER_LABEL_PREFIX__) n

// ---- concurrency (runtime/gojo_sched.c)
// A goroutine waiting in a channel queue. Lives on the waiting goroutine's stack.
typedef struct gojo_sudog {
    struct gojo_g *g;
    gojo_chan *c;
    void *elem;
    uint64_t token;
    bool ok, linked;
    int idx;        // which select case
    struct gojo_sudog *next, *prev;
} gojo_sudog;
typedef struct {
    gojo_chan *c;
    void *elem;     // value to send, or where to receive into
    bool send;
    bool ok;
} gojo_sel_case;
_Noreturn void gojo_sched_main(void (*entry)(void));
// Preemption: generated code polls this flag at loop heads and on entry to
// functions that may recurse.
extern int gojo_preempt_flag;
void gojo_preempt_point(void);
// The slow path goes through an assembly stub that saves all registers, from
// an asm statement that changes nothing as far as the C compiler knows (see
// gojo_sched.c), so a poll doesn't force values out of registers.
#if defined(__aarch64__)
#define GOJO_PREEMPT_CALL() __asm__ volatile("bl " GOJO_ASM_NAME("gojo_preempt_tramp") ::: "x30", "cc")
#elif defined(__x86_64__)
#define GOJO_PREEMPT_CALL() __asm__ volatile("leaq -128(%%rsp), %%rsp\n\tcall " GOJO_ASM_NAME("gojo_preempt_tramp") "\n\tleaq 128(%%rsp), %%rsp" ::: "cc")
#else
#define GOJO_PREEMPT_CALL() gojo_preempt_point()
#endif
#ifdef GOJO_NO_POLL
#define GOJO_POLL() do {} while (0)
#else
#define GOJO_POLL() do { if (__builtin_expect(__atomic_load_n(&gojo_preempt_flag, __ATOMIC_RELAXED), 0)) GOJO_PREEMPT_CALL(); } while (0)
#endif
// Around C calls: the worker's CPU slot goes to another worker if the call takes long.
void gojo_ffi_enter(void);
void gojo_ffi_exit(void);
void gojo_blocking_enter(void);
void gojo_go(void (*fn)(void *), void *arg);
// A channel has one sender and one receiver when made; ends are counted
// separately: the last sender closes it, the last receiver makes sends fail.
gojo_chan *gojo_chan_new(size_t esize, int64_t cap, const char *loc);
void gojo_chan_retain_tx(gojo_chan *c);
void gojo_chan_retain_rx(gojo_chan *c);
void gojo_chan_release_tx(gojo_chan *c, void (*drop_elem)(void *));
void gojo_chan_release_rx(gojo_chan *c, void (*drop_elem)(void *));
bool gojo_chan_send(gojo_chan *c, const void *elem, const char *loc);
bool gojo_chan_recv(gojo_chan *c, void *out);
int64_t gojo_chan_len(gojo_chan *c);
int gojo_select(gojo_sel_case *cases, int n, gojo_task **tasks, int nt, int64_t deadline, bool want_cancel, bool has_default, const char *loc);
gojo_mutex *gojo_mutex_new(const char *loc);
void gojo_mutex_free(gojo_mutex *m);
void gojo_mutex_lock(gojo_mutex *m);
void gojo_mutex_unlock(gojo_mutex *m);
// RwLock: many readers or one writer; tasks wait parked.
uint8_t *gojo_prelude___rwNew(void);
void gojo_prelude___rwFree(uint8_t *l);
void gojo_rw_rlock(uint8_t *l);
void gojo_rw_runlock(uint8_t *l);
void gojo_rw_wlock(uint8_t *l);
void gojo_rw_wunlock(uint8_t *l);
// `with a, b = lock(m1, m2)`: all of them, in address order.
void gojo_mutex_lock_all(gojo_mutex **ms, int n, const char *loc);
gojo_group *gojo_group_new(const char *loc);
void gojo_group_go(gojo_group *g, void (*fn)(void *), void *arg, const char *loc);
void gojo_group_go_env(gojo_group *g, void (*fn)(void *), void *env, void (*drop)(void *), const char *loc);
void gojo_group_wait(gojo_group *g);
bool gojo_group_fail(gojo_group *g);
void gojo_group_cancel(gojo_group *g);
void gojo_group_cancel_failed(gojo_group *g);
uint8_t *gojo_drop_shield(void);
void gojo_drop_unshield(uint8_t *sh);
// `let t = go f()`: a task with a handle and room for its result.
gojo_task *gojo_task_new(size_t result_size);
void *gojo_task_result(gojo_task *t);
void gojo_group_go_task(gojo_group *g, gojo_task *t, void (*fn)(void *), void *arg, const char *loc);
bool gojo_task_finish(gojo_task *t);
void gojo_task_join(gojo_task *t);
void gojo_task_cancel(gojo_task *t);
bool gojo_task_abandon(gojo_task *t);
void gojo_task_free(gojo_task *t);
// `par for`: body(ctx, i) for i in [0, n), on up to one task per CPU.
void gojo_par_for(int64_t n, void (*body)(void *, int64_t), void *ctx);
bool gojo_time___sleep(int64_t ns);
bool gojo_task___cancelled(void);
uint8_t *gojo_task___scopeEnter(int64_t ns);
uint8_t *gojo_task___shieldEnter(int64_t ns);
void gojo_task___shieldExit(uint8_t *s);
int64_t gojo_task___deadline(void);
int64_t gojo_task___reason(void);
int64_t gojo_task___newLocal(void);
uint8_t *gojo_task_bind_enter(int64_t id, void *val);
void gojo_task_bind_exit(uint8_t *s);
void *gojo_task_lookup(int64_t id);
bool gojo_task___scopeExit(uint8_t *s);
void gojo_task___groupCancel(uint8_t *state);
void gojo_task_yieldNow(void);
int64_t gojo_task___numGoroutines(void);
int64_t gojo_task___numThreads(void);
int64_t gojo_task___maxProcs(void);
int64_t gojo_task___numHibernated(void);
void *gojo_ptr_escape(const void *p);
int64_t gojo_task___setMaxProcs(int64_t n);
int64_t gojo_net___listen(gojo_str addr);
int64_t gojo_net___connect(gojo_str addr);
int64_t gojo_net___accept(int64_t fd);
int64_t gojo_net___read(int64_t fd, gojo_vec *buf, int64_t max);
int64_t gojo_net___write(int64_t fd, gojo_slice data);
void gojo_net___close(int64_t fd);
void gojo_net___shutdownWrite(int64_t fd);
int64_t gojo_net___port(int64_t fd);
int64_t gojo_net___udpListen(gojo_str addr);
int64_t gojo_net___udpDial(gojo_str addr);
int64_t gojo_net___sendTo(int64_t fd, gojo_slice data, gojo_str addr);
int64_t gojo_net___recvFrom(int64_t fd, gojo_vec *buf, int64_t max, gojo_string *from);
int64_t gojo_net___lookup(gojo_str host, gojo_vec *out);

// ---- os / time / ffi
void gojo_rt_init(int argc, char **argv);
void gojo_thread_init(void);
gojo_vec gojo_os_args(void);
_Noreturn void gojo_os_exit(int64_t code);
int64_t gojo_os___readFile(gojo_str path, gojo_vec *out);
int64_t gojo_os___writeFile(gojo_str path, gojo_slice data);
int64_t gojo_io___kind(int64_t code);
gojo_string gojo_io___strerror(int64_t code);
int64_t gojo_os___open(gojo_str path, int64_t mode);
int64_t gojo_os___read(int64_t fd, gojo_vec *buf, int64_t max);
int64_t gojo_os___write(int64_t fd, gojo_slice data);
void gojo_os___close(int64_t fd);
void gojo_os___flush(int64_t fd);
int64_t gojo_os___stat(gojo_str path, bool follow, int64_t *size, int64_t *mtime, int64_t *kind, int64_t *mode);
int64_t gojo_os___readDir(gojo_str path, gojo_vec *names);
int64_t gojo_os___mkdir(gojo_str path);
int64_t gojo_os___remove(gojo_str path);
int64_t gojo_os___rename(gojo_str from, gojo_str to);
int64_t gojo_os___cwd(gojo_string *out);
int64_t gojo_os___chdir(gojo_str path);
int64_t gojo_os___setenv(gojo_str name, gojo_str value, bool unset);
void gojo_os___tempDir(gojo_string *out);
int64_t gojo_os___run(gojo_str prog, gojo_slice args, gojo_slice input, bool capture, gojo_vec *out, gojo_vec *errout, int64_t *status);
int64_t gojo_time___unixNanos(void);
int64_t gojo_time___localOffset(int64_t secs);
bool gojo_os___getenv(gojo_str name, gojo_string *out);
int64_t gojo_time_now(void);
extern int gojo_sim_on;
extern int64_t gojo_sim_clock;
extern int gojo_debug_checks;
// A package-level `let`: computed once, by the first task to use it.
typedef struct { int state; void *owner; } gojo_once;
void gojo_once_run(gojo_once *o, void (*init)(void), const char *name);
// A scheduling point for the simulated scheduler (atomics).
void gojo_sim_point(void);
// A task tree with what each task waits for (`task.dump()`, SIGQUIT).
void gojo_task_dump(void);
void *gojo_dlsym(const char *lib, const char *name);
int gojo_run_test(void (*fn)(void), const char *name);

// ---- json (runtime/gojo_lib.c)
typedef struct {
    const uint8_t *s;
    int64_t len, pos;
    int64_t depth;
    // -1 while there is no error
    int64_t errpos;
    gojo_string err, path, buf, kbuf;
} gojo_jdec;
void gojo_jenc_str(gojo_string *b, gojo_str s);
void gojo_jenc_f64(gojo_string *b, double v);
void gojo_json___indent(gojo_str s, int64_t indent, gojo_string *out);
void gojo_json___quote(gojo_string *b, gojo_str s);
void gojo_jdec_init(gojo_jdec *d, gojo_str s);
void gojo_jdec_free(gojo_jdec *d);
bool gojo_jdec_fail(gojo_jdec *d, const char *msg);
bool gojo_jdec_expected(gojo_jdec *d, const char *what);
bool gojo_jdec_fail_str(gojo_jdec *d, const char *a, gojo_str s, const char *b);
bool gojo_jdec_null(gojo_jdec *d);
bool gojo_jdec_bool(gojo_jdec *d, bool *out);
bool gojo_jdec_i64(gojo_jdec *d, int64_t *out, int64_t lo, int64_t hi, const char *ty);
bool gojo_jdec_u64(gojo_jdec *d, uint64_t *out, uint64_t hi, const char *ty);
bool gojo_jdec_f64(gojo_jdec *d, double *out);
bool gojo_jdec_f32(gojo_jdec *d, float *out);
bool gojo_jdec_str(gojo_jdec *d, gojo_string *out);
int gojo_jdec_peek(gojo_jdec *d);
bool gojo_jdec_strview(gojo_jdec *d, gojo_str *out);
bool gojo_jdec_open(gojo_jdec *d, uint8_t c);
int gojo_jdec_item(gojo_jdec *d, int64_t i);
int gojo_jdec_key(gojo_jdec *d, int64_t i, gojo_str *key);
bool gojo_jdec_skip(gojo_jdec *d);
bool gojo_jdec_end(gojo_jdec *d);
void gojo_jdec_at_key(gojo_jdec *d, const uint8_t *k, int64_t n);
void gojo_jdec_at_index(gojo_jdec *d, int64_t i);
int64_t gojo_jdec_result(gojo_jdec *d, gojo_string *msg);
void gojo_jdec_linecol(gojo_str s, int64_t pos, int64_t *line, int64_t *col);

// ---- processes and signals (runtime/gojo_lib.c)
int64_t gojo_os___signalCode(int64_t signo);
int64_t gojo_os___getpid(void);
int64_t gojo_os___spawn(gojo_str prog, gojo_slice args, gojo_slice env, bool setEnv, gojo_str dir, gojo_slice modes, gojo_vec *fds, int64_t *pid);
int64_t gojo_os___wait(int64_t pid);
int64_t gojo_os___tryWait(int64_t pid);
int64_t gojo_os___kill(int64_t pid, int64_t code);
void gojo_os___environ(gojo_vec *out);
int64_t gojo_os___notify(gojo_slice codes, gojo_vec *fds);
void gojo_os___unnotify(gojo_slice codes, int64_t wfd);

#endif
