// M:N scheduler: goroutines are stackful coroutines multiplexed onto worker threads.
//
//  - Each worker has a bounded run queue that other workers can steal from, plus
//    a `runnext` slot, so a goroutine that was just woken runs soon. Overflow goes
//    to a global queue.
//  - A blocked goroutine is parked: it registers somewhere (channel queue, timer
//    heap, poller) and switches back to the worker's scheduler loop.
//  - A monitor, in the poller thread, keeps the CPU shared:
//      * a goroutine that has run for 10 ms while others wait is asked to yield;
//        generated code polls a global flag at loop heads and on entry to
//        functions that may recurse (one load and a branch);
//      * a worker inside a C call (FFI, file I/O) gives up its CPU slot, so
//        another worker runs the queued goroutines; known-blocking calls give
//        it up at once;
//      * a goroutine that doesn't answer a request to yield (a long loop in C)
//        is treated like one in a C call.
//    So there are as many workers running Kite code as CPUs, plus one per
//    goroutine that is in C.
//  - Goroutine stacks are large virtual reservations with a guard region; only
//    touched pages use memory. Stacks never move, so tasks can borrow from them.
//
// Thread-locals: the compiler may cache a thread-local's address across a call, but
// a goroutine can resume on another thread. Always go through cur_worker().
#include <sys/mman.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <signal.h>
#include <sched.h>
#if defined(__APPLE__)
#include <sys/event.h>
#include <mach/mach.h>
#else
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#endif

#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#define KITE_ASAN 1
#endif
#if __has_feature(thread_sanitizer)
#define KITE_TSAN 1
#endif
#endif
#if defined(__SANITIZE_ADDRESS__) && !defined(KITE_ASAN)
#define KITE_ASAN 1
#endif
#if defined(__SANITIZE_THREAD__) && !defined(KITE_TSAN)
#define KITE_TSAN 1
#endif
#ifdef KITE_ASAN
void __sanitizer_start_switch_fiber(void **fake_stack_save, const void *bottom, size_t size);
void __sanitizer_finish_switch_fiber(void *fake_stack_save, const void **bottom_old, size_t *size_old);
#endif
#ifdef KITE_TSAN
void *__tsan_get_current_fiber(void);
void *__tsan_create_fiber(unsigned flags);
void __tsan_destroy_fiber(void *fiber);
void __tsan_switch_to_fiber(void *fiber, unsigned flags);
#endif

#define KITE_LOAD(p) __atomic_load_n((p), __ATOMIC_ACQUIRE)
#define KITE_STORE(p, v) __atomic_store_n((p), (v), __ATOMIC_RELEASE)
#define KITE_RELAXED(p) __atomic_load_n((p), __ATOMIC_RELAXED)
#define KITE_SET_RELAXED(p, v) __atomic_store_n((p), (v), __ATOMIC_RELAXED)
#define KITE_SC_LOAD(p) __atomic_load_n((p), __ATOMIC_SEQ_CST)
#define KITE_SC_STORE(p, v) __atomic_store_n((p), (v), __ATOMIC_SEQ_CST)
#define KITE_ADD(p, n) __atomic_add_fetch((p), (n), __ATOMIC_SEQ_CST)
#define KITE_FENCE() __atomic_thread_fence(__ATOMIC_SEQ_CST)

static inline void cpu_relax(void) {
#if defined(__x86_64__)
    __asm__ volatile("pause");
#elif defined(__aarch64__)
    __asm__ volatile("yield");
#endif
}

typedef int kite_spin;

static inline void spin_lock(kite_spin *l) {
    int spins = 0;
    for (;;) {
        if (__atomic_exchange_n(l, 1, __ATOMIC_ACQUIRE) == 0) return;
        while (KITE_RELAXED(l)) {
            if (++spins < 256) cpu_relax();
            else sched_yield();
        }
    }
}
static inline void spin_unlock(kite_spin *l) { __atomic_store_n(l, 0, __ATOMIC_RELEASE); }

// ------------------------------------------------------------------ context switch

// void kite_ctx_switch(void **save_sp, void *load_sp)
#define KITE_STR2(x) #x
#define KITE_STR(x) KITE_STR2(x)
#define KITE_SYM(name) KITE_STR(__USER_LABEL_PREFIX__) #name

#if defined(__aarch64__)
#define KITE_CTX_FRAME 176
#define KITE_CTX_RET_SLOT 11   // x30, in words from the frame base
__asm__(
    ".text\n.p2align 2\n"
#if !defined(__APPLE__)
    ".type kite_ctx_switch, %function\n"
#endif
    ".globl " KITE_SYM(kite_ctx_switch) "\n"
    KITE_SYM(kite_ctx_switch) ":\n"
    "    sub sp, sp, #176\n"
    "    stp x19, x20, [sp, #0]\n"
    "    stp x21, x22, [sp, #16]\n"
    "    stp x23, x24, [sp, #32]\n"
    "    stp x25, x26, [sp, #48]\n"
    "    stp x27, x28, [sp, #64]\n"
    "    stp x29, x30, [sp, #80]\n"
    "    stp d8, d9, [sp, #96]\n"
    "    stp d10, d11, [sp, #112]\n"
    "    stp d12, d13, [sp, #128]\n"
    "    stp d14, d15, [sp, #144]\n"
    "    mov x9, sp\n"
    "    str x9, [x0]\n"
    "    mov sp, x1\n"
    "    ldp x19, x20, [sp, #0]\n"
    "    ldp x21, x22, [sp, #16]\n"
    "    ldp x23, x24, [sp, #32]\n"
    "    ldp x25, x26, [sp, #48]\n"
    "    ldp x27, x28, [sp, #64]\n"
    "    ldp x29, x30, [sp, #80]\n"
    "    ldp d8, d9, [sp, #96]\n"
    "    ldp d10, d11, [sp, #112]\n"
    "    ldp d12, d13, [sp, #128]\n"
    "    ldp d14, d15, [sp, #144]\n"
    "    add sp, sp, #176\n"
    "    ret\n");
#elif defined(__x86_64__)
#define KITE_CTX_FRAME 64
#define KITE_CTX_RET_SLOT 7    // return address, in words from the frame base
__asm__(
    ".text\n.p2align 4\n"
#if !defined(__APPLE__)
    ".type kite_ctx_switch, @function\n"
#endif
    ".globl " KITE_SYM(kite_ctx_switch) "\n"
    KITE_SYM(kite_ctx_switch) ":\n"
    "    pushq %rbp\n"
    "    pushq %rbx\n"
    "    pushq %r12\n"
    "    pushq %r13\n"
    "    pushq %r14\n"
    "    pushq %r15\n"
    "    subq $8, %rsp\n"
    "    stmxcsr 0(%rsp)\n"
    "    fnstcw 4(%rsp)\n"
    "    movq %rsp, (%rdi)\n"
    "    movq %rsi, %rsp\n"
    "    ldmxcsr 0(%rsp)\n"
    "    fldcw 4(%rsp)\n"
    "    addq $8, %rsp\n"
    "    popq %r15\n"
    "    popq %r14\n"
    "    popq %r13\n"
    "    popq %r12\n"
    "    popq %rbx\n"
    "    popq %rbp\n"
    "    ret\n");
#else
#error "the Kite runtime supports x86_64 and aarch64"
#endif

extern void kite_ctx_switch(void **save_sp, void *load_sp);

// ------------------------------------------------------------------ preemption trampoline
//
// Generated code reaches kite_preempt_point through this stub, from an asm
// statement that tells the C compiler nothing changes: no registers (besides
// the return address and flags) and no memory. That keeps the poll from
// forcing values out of registers in the loops it sits in. It's sound because
// a preemption point is not a synchronization point: Kite code only sees
// other goroutines' writes through locks and channels, which are real calls.
// The stub saves every register the C call could change, vector state included.
#if defined(__aarch64__)
__asm__(
    ".text\n.p2align 2\n"
    ".globl " KITE_SYM(kite_preempt_tramp) "\n"
    KITE_SYM(kite_preempt_tramp) ":\n"
    // stay below the caller's 128-byte red zone
    "    sub sp, sp, #816\n"
    "    stp x0, x1, [sp, #0]\n"
    "    stp x2, x3, [sp, #16]\n"
    "    stp x4, x5, [sp, #32]\n"
    "    stp x6, x7, [sp, #48]\n"
    "    stp x8, x9, [sp, #64]\n"
    "    stp x10, x11, [sp, #80]\n"
    "    stp x12, x13, [sp, #96]\n"
    "    stp x14, x15, [sp, #112]\n"
    "    stp x16, x17, [sp, #128]\n"
    "    stp x29, x30, [sp, #144]\n"
    "    mrs x0, fpsr\n"
    "    mrs x1, fpcr\n"
    "    stp x0, x1, [sp, #160]\n"
    "    stp q0, q1, [sp, #176]\n"
    "    stp q2, q3, [sp, #208]\n"
    "    stp q4, q5, [sp, #240]\n"
    "    stp q6, q7, [sp, #272]\n"
    "    stp q8, q9, [sp, #304]\n"
    "    stp q10, q11, [sp, #336]\n"
    "    stp q12, q13, [sp, #368]\n"
    "    stp q14, q15, [sp, #400]\n"
    "    stp q16, q17, [sp, #432]\n"
    "    stp q18, q19, [sp, #464]\n"
    "    stp q20, q21, [sp, #496]\n"
    "    stp q22, q23, [sp, #528]\n"
    "    stp q24, q25, [sp, #560]\n"
    "    stp q26, q27, [sp, #592]\n"
    "    stp q28, q29, [sp, #624]\n"
    "    stp q30, q31, [sp, #656]\n"
    "    add x29, sp, #144\n"
    "    bl " KITE_SYM(kite_preempt_point) "\n"
    "    ldp q0, q1, [sp, #176]\n"
    "    ldp q2, q3, [sp, #208]\n"
    "    ldp q4, q5, [sp, #240]\n"
    "    ldp q6, q7, [sp, #272]\n"
    "    ldp q8, q9, [sp, #304]\n"
    "    ldp q10, q11, [sp, #336]\n"
    "    ldp q12, q13, [sp, #368]\n"
    "    ldp q14, q15, [sp, #400]\n"
    "    ldp q16, q17, [sp, #432]\n"
    "    ldp q18, q19, [sp, #464]\n"
    "    ldp q20, q21, [sp, #496]\n"
    "    ldp q22, q23, [sp, #528]\n"
    "    ldp q24, q25, [sp, #560]\n"
    "    ldp q26, q27, [sp, #592]\n"
    "    ldp q28, q29, [sp, #624]\n"
    "    ldp q30, q31, [sp, #656]\n"
    "    ldp x0, x1, [sp, #160]\n"
    "    msr fpsr, x0\n"
    "    msr fpcr, x1\n"
    "    ldp x0, x1, [sp, #0]\n"
    "    ldp x2, x3, [sp, #16]\n"
    "    ldp x4, x5, [sp, #32]\n"
    "    ldp x6, x7, [sp, #48]\n"
    "    ldp x8, x9, [sp, #64]\n"
    "    ldp x10, x11, [sp, #80]\n"
    "    ldp x12, x13, [sp, #96]\n"
    "    ldp x14, x15, [sp, #112]\n"
    "    ldp x16, x17, [sp, #128]\n"
    "    ldp x29, x30, [sp, #144]\n"
    "    add sp, sp, #816\n"
    "    ret\n");
#elif defined(__x86_64__)
// Set at startup: XSAVE area size (0: use FXSAVE).
int64_t kite_xsave_size;
__asm__(
    ".text\n.p2align 4\n"
    ".globl " KITE_SYM(kite_preempt_tramp) "\n"
    KITE_SYM(kite_preempt_tramp) ":\n"
    // the caller already stepped over its red zone
    "    pushq %rbp\n"
    "    movq %rsp, %rbp\n"
    "    pushq %rax\n"
    "    pushq %rcx\n"
    "    pushq %rdx\n"
    "    pushq %rsi\n"
    "    pushq %rdi\n"
    "    pushq %r8\n"
    "    pushq %r9\n"
    "    pushq %r10\n"
    "    pushq %r11\n"
    "    andq $-64, %rsp\n"
    "    movq " KITE_SYM(kite_xsave_size) "(%rip), %rcx\n"
    "    testq %rcx, %rcx\n"
    "    jz 1f\n"
    "    subq %rcx, %rsp\n"
    // the XSAVE header must start out zero
    "    movq %rsp, %rdi\n"
    "    xorl %eax, %eax\n"
    "    movq %rax, 512(%rdi)\n"
    "    movq %rax, 520(%rdi)\n"
    "    movq %rax, 528(%rdi)\n"
    "    movq %rax, 536(%rdi)\n"
    "    movq %rax, 544(%rdi)\n"
    "    movq %rax, 552(%rdi)\n"
    "    movq %rax, 560(%rdi)\n"
    "    movq %rax, 568(%rdi)\n"
    "    movl $-1, %eax\n"
    "    movl $-1, %edx\n"
    "    xsave64 (%rsp)\n"
    "    call " KITE_SYM(kite_preempt_point) "\n"
    "    movl $-1, %eax\n"
    "    movl $-1, %edx\n"
    "    xrstor64 (%rsp)\n"
    "    jmp 2f\n"
    "1:\n"
    "    subq $512, %rsp\n"
    "    fxsave64 (%rsp)\n"
    "    call " KITE_SYM(kite_preempt_point) "\n"
    "    fxrstor64 (%rsp)\n"
    "2:\n"
    "    leaq -72(%rbp), %rsp\n"
    "    popq %r11\n"
    "    popq %r10\n"
    "    popq %r9\n"
    "    popq %r8\n"
    "    popq %rdi\n"
    "    popq %rsi\n"
    "    popq %rdx\n"
    "    popq %rcx\n"
    "    popq %rax\n"
    "    popq %rbp\n"
    "    ret\n");

static void xsave_setup(void) {
    unsigned a, b, c, d;
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    if (!(c & (1u << 27))) return;      // no OSXSAVE: FXSAVE it is
    __asm__ volatile("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(0xd), "c"(0));
    if (b < 576) return;
    kite_xsave_size = ((int64_t)b + 63) & ~(int64_t)63;
}
#endif


// ------------------------------------------------------------------ core types

enum { WAIT_NONE = 0, WAIT_CANCELLABLE = 1 };
enum { WAKE_NONE, WAKE_CANCEL, WAKE_CHAN, WAKE_TIMER, WAKE_IO };
enum { POST_NONE, POST_DEAD, POST_YIELD };

typedef struct kite_scope kite_scope;

typedef struct kite_g {
    void *sp;
    int on_cpu;             // the goroutine is still on its stack; don't resume it yet
    uint32_t epoch;
    uint64_t wait;          // epoch << 2 | wait kind; claimed by CAS back to WAIT_NONE
    int wake;               // why the goroutine woke; written by whoever claimed it
    void *wake_data;
    void (*fn)(void *);
    void *arg;
    struct kite_g *qnext;   // global queue, free lists, mutex waiters
    kite_scope *scope;
    struct kite_g *scope_prev, *scope_next;
    kite_group *group;
    uint8_t *stack;         // lowest usable address
    size_t stack_size;
    uint8_t *mapping;       // start of the whole mapping, guard included
    size_t mapping_size;
    uint32_t idx;
#ifdef KITE_ASAN
    void *fake_stack;
#endif
#ifdef KITE_TSAN
    void *tsan_fiber;
#endif
} kite_g;

#define RUNQ_SIZE 256

typedef struct kite_worker {
    void *sched_sp;
    kite_g *cur;
    int post;
    uint32_t rq_head, rq_tail;
    kite_g *rq[RUNQ_SIZE];
    kite_g *runnext;
    pthread_cond_t cv;
    bool notified;
    struct kite_worker *idle_next;
    bool spinning;
    uint64_t tick;          // goroutines started; read by the monitor
    int preempt;            // the monitor asks the running goroutine to yield
    bool yielded;           // the last goroutine yielded: check the global queue first
    int insys;              // in a C call
    uint64_t sysseq;        // C calls made, to tell one long call from many short ones
    int released;           // its CPU slot was handed to another worker
    uint64_t seen_tick, seen_sysseq;    // monitor-only
    int64_t seen_since, preempt_since;  // monitor-only
    int64_t preempt_cpu;                // monitor-only: thread CPU time at the request
    int cpu_ok;                         // the thread's CPU clock below is set
#if defined(__APPLE__)
    thread_t mthread;
#else
    clockid_t cpuclock;
#endif
    uint32_t schedtick;
    uint64_t rng;
    kite_g *free_gs;
    int nfree;
    uint32_t id;
#ifdef KITE_ASAN
    void *fake_stack;
    const void *stack_bottom;
    size_t stack_size;
#endif
#ifdef KITE_TSAN
    void *tsan_fiber;
#endif
} kite_worker;

struct kite_scope {
    kite_spin lock;
    int cancelled;
    kite_scope *parent;
    kite_scope *first_child, *next_sib, *prev_sib;
    kite_g *members;
};

#define MAX_WORKERS 10000

static pthread_mutex_t s_lock = PTHREAD_MUTEX_INITIALIZER;
static kite_g *s_gq_head, *s_gq_tail;
static int64_t s_gq_size;
static kite_worker *s_idle;
static int s_nidle, s_nstarted, s_nspinning, s_nreleased, s_nprocs;
int kite_preempt_flag;   // outstanding requests to yield; polled by generated code
static int64_t s_ngoroutines;
static kite_worker **s_workers;
static kite_g *s_free_gs;
static int64_t s_nfree_global;
static int64_t s_timers_pending, s_io_waiters;
static size_t s_stack_size = 2 << 20;
static const size_t s_main_stack_size = (size_t)1 << 30;
static const size_t s_guard_size = 256 << 10;

// Goroutines are addressed by index in poller events; slots live forever.
#define G_CHUNK_BITS 12
#define G_CHUNK (1u << G_CHUNK_BITS)
#define G_MAX_CHUNKS (1u << 18)
static kite_g **s_g_chunks;
static uint32_t s_g_count;
static kite_spin s_g_lock;

static __thread kite_worker *t_worker;
static __thread uint8_t *t_alt_stack;

// Not inlined, and opaque to the optimizer: see the note about thread-locals above.
__attribute__((noinline)) static kite_worker *cur_worker(void) {
    __asm__ volatile("" : : : "memory");
    return t_worker;
}

static inline kite_g *cur_g(void) { return cur_worker()->cur; }

static void wake_one(void);
static void ensure_poller(void);
static void poller_wake(void);
static void worker_setup_signals(void);

// ------------------------------------------------------------------ run queues

static uint64_t next_rand(kite_worker *w) {
    uint64_t x = w->rng;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    return w->rng = x;
}

static void global_put_locked(kite_g *g) {
    g->qnext = NULL;
    if (s_gq_tail) s_gq_tail->qnext = g;
    else s_gq_head = g;
    s_gq_tail = g;
    KITE_SET_RELAXED(&s_gq_size, s_gq_size + 1);
}

// Goroutines woken by timers and the network go first: they have waited already.
static void global_put_front(kite_g *g) {
    pthread_mutex_lock(&s_lock);
    g->qnext = s_gq_head;
    s_gq_head = g;
    if (!s_gq_tail) s_gq_tail = g;
    KITE_SET_RELAXED(&s_gq_size, s_gq_size + 1);
    pthread_mutex_unlock(&s_lock);
}

static void global_put(kite_g *g) {
    pthread_mutex_lock(&s_lock);
    global_put_locked(g);
    pthread_mutex_unlock(&s_lock);
}

// Moves half of a full local queue, plus g, to the global queue.
static bool runq_put_slow(kite_worker *w, kite_g *g, uint32_t h, uint32_t t) {
    uint32_t n = (t - h) / 2;
    kite_g *batch[RUNQ_SIZE / 2 + 1];
    for (uint32_t i = 0; i < n; i++) batch[i] = KITE_RELAXED(&w->rq[(h + i) % RUNQ_SIZE]);
    if (!__atomic_compare_exchange_n(&w->rq_head, &h, h + n, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return false;
    batch[n] = g;
    pthread_mutex_lock(&s_lock);
    for (uint32_t i = 0; i <= n; i++) global_put_locked(batch[i]);
    pthread_mutex_unlock(&s_lock);
    return true;
}

static void runq_put(kite_worker *w, kite_g *g, bool next) {
    if (next) {
        kite_g *old = __atomic_exchange_n(&w->runnext, g, __ATOMIC_ACQ_REL);
        if (!old) return;
        g = old;
    }
    for (;;) {
        uint32_t h = KITE_LOAD(&w->rq_head);
        uint32_t t = KITE_RELAXED(&w->rq_tail);
        if (t - h < RUNQ_SIZE) {
            KITE_SET_RELAXED(&w->rq[t % RUNQ_SIZE], g);
            KITE_STORE(&w->rq_tail, t + 1);
            return;
        }
        if (runq_put_slow(w, g, h, t)) return;
    }
}

static kite_g *runq_get(kite_worker *w) {
    kite_g *next = KITE_LOAD(&w->runnext);
    while (next) {
        if (__atomic_compare_exchange_n(&w->runnext, &next, NULL, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) return next;
    }
    for (;;) {
        uint32_t h = KITE_LOAD(&w->rq_head);
        uint32_t t = KITE_LOAD(&w->rq_tail);
        if (t == h) return NULL;
        kite_g *g = KITE_RELAXED(&w->rq[h % RUNQ_SIZE]);
        if (__atomic_compare_exchange_n(&w->rq_head, &h, h + 1, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return g;
    }
}

static inline bool runq_empty(kite_worker *w) {
    return KITE_LOAD(&w->rq_head) == KITE_LOAD(&w->rq_tail) && !KITE_LOAD(&w->runnext);
}

static uint32_t runq_grab(kite_worker *v, kite_g **batch, bool steal_next) {
    for (;;) {
        uint32_t h = KITE_LOAD(&v->rq_head);
        uint32_t t = KITE_LOAD(&v->rq_tail);
        uint32_t n = t - h;
        n = n - n / 2;
        if (n == 0) {
            if (steal_next) {
                kite_g *next = KITE_LOAD(&v->runnext);
                if (next) {
                    // give the owner a moment: it is probably about to run it
                    if (KITE_RELAXED(&v->cur))
                        for (int i = 0; i < 100; i++) cpu_relax();
                    if (__atomic_compare_exchange_n(&v->runnext, &next, NULL, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
                        batch[0] = next;
                        return 1;
                    }
                    continue;
                }
            }
            return 0;
        }
        if (n > RUNQ_SIZE / 2) continue; // inconsistent snapshot
        for (uint32_t i = 0; i < n; i++) batch[i] = KITE_RELAXED(&v->rq[(h + i) % RUNQ_SIZE]);
        if (__atomic_compare_exchange_n(&v->rq_head, &h, h + n, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return n;
    }
}

static kite_g *runq_steal(kite_worker *w, kite_worker *v, bool steal_next) {
    kite_g *batch[RUNQ_SIZE / 2];
    uint32_t n = runq_grab(v, batch, steal_next);
    if (n == 0) return NULL;
    kite_g *g = batch[--n];
    if (n == 0) return g;
    uint32_t t = KITE_RELAXED(&w->rq_tail);
    for (uint32_t i = 0; i < n; i++) KITE_SET_RELAXED(&w->rq[(t + i) % RUNQ_SIZE], batch[i]);
    KITE_STORE(&w->rq_tail, t + n);
    return g;
}

static kite_g *global_get_locked(kite_worker *w) {
    int64_t size = s_gq_size;
    if (size == 0) return NULL;
    int64_t n = size / s_nprocs + 1;
    if (n > size) n = size;
    if (n > RUNQ_SIZE / 2) n = RUNQ_SIZE / 2;
    // only what fits: overflowing the local queue would need s_lock again
    uint32_t h = KITE_LOAD(&w->rq_head);
    uint32_t t = KITE_RELAXED(&w->rq_tail);
    int64_t room = RUNQ_SIZE - (int64_t)(t - h);
    if (n > room + 1) n = room + 1;
    kite_g *first = NULL;
    for (int64_t i = 0; i < n; i++) {
        kite_g *g = s_gq_head;
        s_gq_head = g->qnext;
        if (!s_gq_head) s_gq_tail = NULL;
        g->qnext = NULL;
        if (!first) {
            first = g;
        } else {
            KITE_SET_RELAXED(&w->rq[t % RUNQ_SIZE], g);
            t++;
        }
    }
    KITE_STORE(&w->rq_tail, t);
    KITE_SET_RELAXED(&s_gq_size, size - n);
    return first;
}

static inline bool can_wake(void) {
    int active = KITE_SC_LOAD(&s_nstarted) - KITE_SC_LOAD(&s_nidle) - KITE_SC_LOAD(&s_nreleased);
    return active < s_nprocs;
}

static inline void maybe_wake(void) {
    KITE_FENCE();
    if (KITE_SC_LOAD(&s_nspinning) == 0 && can_wake()) wake_one();
}

// Makes a parked goroutine runnable. Whoever calls this owns the wake-up.
static void g_ready(kite_g *g) {
    kite_worker *w = cur_worker();
    if (w) runq_put(w, g, true);
    else global_put_front(g);
    maybe_wake();
}

// ------------------------------------------------------------------ goroutine stacks

static kite_g *g_slot(uint32_t idx) { return &s_g_chunks[idx >> G_CHUNK_BITS][idx & (G_CHUNK - 1)]; }

static kite_g *g_new_slot(void) {
    spin_lock(&s_g_lock);
    uint32_t idx = s_g_count;
    if ((idx >> G_CHUNK_BITS) >= G_MAX_CHUNKS) kite_panic("too many goroutines", "<runtime>");
    if ((idx & (G_CHUNK - 1)) == 0) {
        kite_g *chunk = calloc(G_CHUNK, sizeof(kite_g));
        if (!chunk) kite_panic("out of memory", "<runtime>");
        __atomic_store_n(&s_g_chunks[idx >> G_CHUNK_BITS], chunk, __ATOMIC_RELEASE);
    }
    kite_g *g = g_slot(idx);
    g->idx = idx;
    __atomic_store_n(&s_g_count, idx + 1, __ATOMIC_RELEASE);
    spin_unlock(&s_g_lock);
    return g;
}

static void g_map_stack(kite_g *g, size_t size) {
    size_t total = size + s_guard_size;
    int flags = MAP_PRIVATE | MAP_ANON;
#ifdef MAP_NORESERVE
    flags |= MAP_NORESERVE;
#endif
#ifdef MAP_STACK
    flags |= MAP_STACK;
#endif
    uint8_t *m = mmap(NULL, total, PROT_READ | PROT_WRITE, flags, -1, 0);
    if (m == MAP_FAILED) kite_panic("cannot allocate a goroutine stack: out of address space or memory (set KITE_GOROUTINE_STACK to use smaller stacks)", "<runtime>");
#if defined(__linux__)
    // Linux 6.13+: a guard region that doesn't split the mapping, so stacks next
    // to each other merge into one mapping instead of two per goroutine
    static int guard_install = 1;
    if (KITE_RELAXED(&guard_install)) {
        if (madvise(m, s_guard_size, 102 /* MADV_GUARD_INSTALL */) == 0) goto guarded;
        KITE_SET_RELAXED(&guard_install, 0);
    }
#endif
    if (mprotect(m, s_guard_size, PROT_NONE) != 0) {
        kite_panic("cannot protect a goroutine stack: too many memory mappings (on Linux before 6.13, raise vm.max_map_count; each goroutine needs two)", "<runtime>");
    }
#if defined(__linux__)
guarded:
#endif
    g->mapping = m;
    g->mapping_size = total;
    g->stack = m + s_guard_size;
    g->stack_size = size;
}

static void g_entry(void);

static void g_prepare(kite_g *g) {
    uint8_t *top = g->stack + g->stack_size;
    top = (uint8_t *)((uintptr_t)top & ~(uintptr_t)15);
#if defined(__aarch64__)
    uint8_t *sp = top - KITE_CTX_FRAME;
    memset(sp, 0, KITE_CTX_FRAME);
    ((void **)sp)[KITE_CTX_RET_SLOT] = (void *)g_entry;
#else
    // on entry to g_entry: (rsp + 8) must be 16-byte aligned
    uint8_t *sp = top - 8 - KITE_CTX_FRAME;
    memset(sp, 0, KITE_CTX_FRAME);
    ((void **)sp)[KITE_CTX_RET_SLOT] = (void *)g_entry;
    unsigned int mxcsr = 0;
    unsigned short fcw = 0;
    __asm__ volatile("stmxcsr %0" : "=m"(mxcsr));
    __asm__ volatile("fnstcw %0" : "=m"(fcw));
    *(unsigned int *)sp = mxcsr;
    *(unsigned short *)(sp + 4) = fcw;
#endif
    g->sp = sp;
}

static kite_g *g_alloc(kite_worker *w, size_t size) {
    kite_g *g = NULL;
    if (size == s_stack_size) {
        g = w->free_gs;
        if (g) {
            w->free_gs = g->qnext;
            w->nfree--;
        } else if (__atomic_load_n(&s_free_gs, __ATOMIC_RELAXED)) {
            pthread_mutex_lock(&s_lock);
            for (int i = 0; i < 128 && s_free_gs; i++) {
                kite_g *f = s_free_gs;
                __atomic_store_n(&s_free_gs, f->qnext, __ATOMIC_RELAXED);
                KITE_SET_RELAXED(&s_nfree_global, s_nfree_global - 1);
                if (!g) {
                    g = f;
                } else {
                    f->qnext = w->free_gs;
                    w->free_gs = f;
                    w->nfree++;
                }
            }
            pthread_mutex_unlock(&s_lock);
        }
    }
    if (!g) {
        g = g_new_slot();
#ifdef KITE_TSAN
        g->tsan_fiber = NULL;
#endif
        g_map_stack(g, size);
    }
    g->qnext = NULL;
    g->scope = NULL;
    g->group = NULL;
    g->wake = WAKE_NONE;
    g->wake_data = NULL;
    KITE_SET_RELAXED(&g->on_cpu, 0);
    KITE_SET_RELAXED(&g->wait, (uint64_t)g->epoch << 2);
    g_prepare(g);
#ifdef KITE_TSAN
    if (!g->tsan_fiber) g->tsan_fiber = __tsan_create_fiber(0);
#endif
    return g;
}

// Runs on the scheduler stack, after the goroutine has switched away for good.
static void g_recycle(kite_worker *w, kite_g *g) {
#ifdef KITE_ASAN
    // the dead goroutine's frames are still poisoned; munmap lets ASan forget them
    munmap(g->mapping, g->mapping_size);
    g_map_stack(g, s_stack_size);
#endif
    g->qnext = w->free_gs;
    w->free_gs = g;
    w->nfree++;
    if (w->nfree > 512) {
        // stacks freed here are often needed by the worker that spawns: share
        // half; give memory back only when the shared pool itself grows large
        kite_g *keep = w->free_gs;
        for (int i = 1; i < 256; i++) keep = keep->qnext;
        kite_g *rest = keep->qnext;
        keep->qnext = NULL;
        int moved = w->nfree - 256;
        w->nfree = 256;
        kite_g *last = rest;
        bool trim = KITE_RELAXED(&s_nfree_global) > 4096;
        for (;;) {
            if (trim) {
#if defined(__APPLE__)
                madvise(last->stack, last->stack_size, MADV_FREE);
#else
                madvise(last->stack, last->stack_size, MADV_DONTNEED);
#endif
            }
            if (!last->qnext) break;
            last = last->qnext;
        }
        pthread_mutex_lock(&s_lock);
        last->qnext = s_free_gs;
        __atomic_store_n(&s_free_gs, rest, __ATOMIC_RELAXED);
        KITE_SET_RELAXED(&s_nfree_global, s_nfree_global + moved);
        pthread_mutex_unlock(&s_lock);
    }
}

// ------------------------------------------------------------------ switching

static inline void switch_to_g(kite_worker *w, kite_g *g) {
#ifdef KITE_ASAN
    __sanitizer_start_switch_fiber(&w->fake_stack, g->stack, g->stack_size);
#endif
#ifdef KITE_TSAN
    __tsan_switch_to_fiber(g->tsan_fiber, 0);
#endif
    kite_ctx_switch(&w->sched_sp, g->sp);
#ifdef KITE_ASAN
    __sanitizer_finish_switch_fiber(w->fake_stack, NULL, NULL);
#endif
}

// Back to the scheduler. On return the goroutine may be on another worker.
static inline void switch_to_sched(kite_g *g, kite_worker *w, bool dying) {
#ifdef KITE_ASAN
    __sanitizer_start_switch_fiber(dying ? NULL : &g->fake_stack, w->stack_bottom, w->stack_size);
#endif
#ifdef KITE_TSAN
    __tsan_switch_to_fiber(w->tsan_fiber, 0);
#endif
    kite_ctx_switch(&g->sp, w->sched_sp);
#ifdef KITE_ASAN
    __sanitizer_finish_switch_fiber(g->fake_stack, NULL, NULL);
#endif
}

// The caller has arranged for someone to g_ready() this goroutine.
static void g_park(void) {
    kite_worker *w = cur_worker();
    switch_to_sched(w->cur, w, false);
}

// ------------------------------------------------------------------ waits and cancellation

static inline uint64_t wait_begin(kite_g *g) {
    g->epoch++;
    uint64_t t = ((uint64_t)g->epoch << 2) | WAIT_CANCELLABLE;
    g->wake = WAKE_NONE;
    g->wake_data = NULL;
    KITE_SC_STORE(&g->wait, t);
    return t;
}

static inline bool wait_claim(kite_g *g, uint64_t token) {
    uint64_t expected = token;
    return __atomic_compare_exchange_n(&g->wait, &expected, token & ~(uint64_t)3, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED);
}

static inline bool scope_cancelled(kite_scope *s) { return s && KITE_SC_LOAD(&s->cancelled); }

// Parks unless the task is already cancelled. Afterwards g->wake says why it woke.
static void wait_park(kite_g *g, uint64_t token) {
    if (scope_cancelled(g->scope) && wait_claim(g, token)) {
        g->wake = WAKE_CANCEL;
        return;
    }
    g_park();
}

static void scope_cancel(kite_scope *s) {
    spin_lock(&s->lock);
    if (KITE_RELAXED(&s->cancelled)) {
        spin_unlock(&s->lock);
        return;
    }
    KITE_SC_STORE(&s->cancelled, 1);
    for (kite_g *g = s->members; g; g = g->scope_next) {
        uint64_t w = KITE_SC_LOAD(&g->wait);
        if ((w & 3) == WAIT_CANCELLABLE && wait_claim(g, w)) {
            g->wake = WAKE_CANCEL;
            g_ready(g);
        }
    }
    for (kite_scope *c = s->first_child; c; c = c->next_sib) scope_cancel(c);
    spin_unlock(&s->lock);
}

static void scope_join(kite_scope *s, kite_g *g) {
    g->scope = s;
    if (!s) return;
    spin_lock(&s->lock);
    g->scope_prev = NULL;
    g->scope_next = s->members;
    if (s->members) s->members->scope_prev = g;
    s->members = g;
    spin_unlock(&s->lock);
}

static void scope_leave(kite_g *g) {
    kite_scope *s = g->scope;
    g->scope = NULL;
    if (!s) return;
    spin_lock(&s->lock);
    if (g->scope_prev) g->scope_prev->scope_next = g->scope_next;
    else s->members = g->scope_next;
    if (g->scope_next) g->scope_next->scope_prev = g->scope_prev;
    spin_unlock(&s->lock);
}

// A new scope under the goroutine's current one; the goroutine moves into it.
static void scope_enter(kite_scope *s, kite_g *g) {
    kite_scope *p = g->scope;
    memset(s, 0, sizeof *s);
    s->parent = p;
    if (p) {
        spin_lock(&p->lock);
        s->cancelled = p->cancelled;
        s->next_sib = p->first_child;
        if (p->first_child) p->first_child->prev_sib = s;
        p->first_child = s;
        spin_unlock(&p->lock);
    }
    scope_leave(g);
    scope_join(s, g);
}

// The goroutine goes back to the parent scope; s stays in the tree.
static void scope_step_out(kite_scope *s, kite_g *g) {
    scope_leave(g);
    scope_join(s->parent, g);
}

static void scope_unlink(kite_scope *s) {
    kite_scope *p = s->parent;
    if (p) {
        spin_lock(&p->lock);
        if (s->prev_sib) s->prev_sib->next_sib = s->next_sib;
        else p->first_child = s->next_sib;
        if (s->next_sib) s->next_sib->prev_sib = s->prev_sib;
        spin_unlock(&p->lock);
    }
}

static void scope_exit(kite_scope *s, kite_g *g) {
    scope_step_out(s, g);
    scope_unlink(s);
}

bool kite_task___cancelled(void) { return scope_cancelled(cur_g()->scope); }

void kite_task_yieldNow(void) {
    kite_worker *w = cur_worker();
    w->post = POST_YIELD;
    switch_to_sched(w->cur, w, false);
}

// ------------------------------------------------------------------ the scheduler

static void worker_park(kite_worker *w) {
    // hand the queue over: an idle worker has no business holding work
    kite_g *g;
    while ((g = runq_get(w))) global_put(g);
    pthread_mutex_lock(&s_lock);
    w->idle_next = s_idle;
    s_idle = w;
    KITE_ADD(&s_nidle, 1);
    if (s_nidle == s_nstarted && s_gq_size == 0 && KITE_SC_LOAD(&s_timers_pending) == 0 && KITE_SC_LOAD(&s_io_waiters) == 0) {
        kite_panic("all goroutines are asleep: deadlock", "<runtime>");
    }
    while (!w->notified) pthread_cond_wait(&w->cv, &s_lock);
    w->notified = false;
    pthread_mutex_unlock(&s_lock);
}

// Wakes an idle worker, or starts a new one, as a spinning worker.
static void wake_one(void) {
    pthread_mutex_lock(&s_lock);
    if (!can_wake() || KITE_SC_LOAD(&s_nspinning) > 0) {
        pthread_mutex_unlock(&s_lock);
        return;
    }
    kite_worker *w = s_idle;
    if (w) {
        s_idle = w->idle_next;
        KITE_ADD(&s_nidle, -1);
        KITE_ADD(&s_nspinning, 1);
        w->spinning = true;
        w->notified = true;
        pthread_cond_signal(&w->cv);
        pthread_mutex_unlock(&s_lock);
        return;
    }
    extern void kite_start_worker(void);
    if (KITE_RELAXED(&s_nstarted) < MAX_WORKERS) kite_start_worker();
    pthread_mutex_unlock(&s_lock);
}

static inline void stop_spinning(kite_worker *w) {
    if (!w->spinning) return;
    w->spinning = false;
    // this worker found work; someone else should look for more
    if (KITE_ADD(&s_nspinning, -1) == 0) maybe_wake();
}

// The worker's goroutine is in C: let another worker have its CPU slot.
static void worker_release(kite_worker *w) {
    int expected = 0;
    if (!__atomic_compare_exchange_n(&w->released, &expected, 1, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED)) return;
    KITE_ADD(&s_nreleased, 1);
    maybe_wake();
}

static bool worker_reclaim(kite_worker *w) {
    int expected = 1;
    if (!__atomic_compare_exchange_n(&w->released, &expected, 0, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED)) return false;
    KITE_ADD(&s_nreleased, -1);
    return true;
}

static void cancel_preempt(kite_worker *w) {
    if (__atomic_exchange_n(&w->preempt, 0, __ATOMIC_ACQ_REL)) KITE_ADD(&kite_preempt_flag, -1);
}

// Generated code calls this when kite_preempt_flag is set.
void kite_preempt_point(void) {
    kite_worker *w = cur_worker();
    if (!w || !__atomic_exchange_n(&w->preempt, 0, __ATOMIC_ACQ_REL)) return;
    KITE_ADD(&kite_preempt_flag, -1);
    kite_task_yieldNow();
}

// Around C calls that might take a while. Nothing switches goroutines in
// between, so enter and exit run on the same worker.
void kite_ffi_enter(void) {
    kite_worker *w = cur_worker();
    if (!w) return;
    KITE_SET_RELAXED(&w->sysseq, w->sysseq + 1);
    KITE_SC_STORE(&w->insys, 1);
}

void kite_ffi_exit(void) {
    kite_worker *w = cur_worker();
    if (!w) return;
    KITE_SC_STORE(&w->insys, 0);
    if (KITE_SC_LOAD(&w->released) && worker_reclaim(w)) {
        // more workers than CPUs running Kite code now: get back in line
        int active = KITE_SC_LOAD(&s_nstarted) - KITE_SC_LOAD(&s_nidle) - KITE_SC_LOAD(&s_nreleased);
        if (active > s_nprocs) kite_task_yieldNow();
    }
}

// For calls known to block (file I/O): hand the slot over right away.
void kite_blocking_enter(void) {
    kite_ffi_enter();
    kite_worker *w = cur_worker();
    if (w) worker_release(w);
}

static kite_g *find_work(kite_worker *w) {
    for (;;) {
        // back in the scheduler: the worker takes its CPU slot again (or parks
        // below if all are taken)
        worker_reclaim(w);
        int active = KITE_SC_LOAD(&s_nstarted) - KITE_SC_LOAD(&s_nidle) - KITE_SC_LOAD(&s_nreleased);
        if (active > s_nprocs && !w->spinning) {
            worker_park(w);
            continue;
        }
        // fairness: now and then, and right after a goroutine was asked to yield,
        // look at the global queue first; woken goroutines wait at its front
        bool global_first = w->yielded || ++w->schedtick % 61 == 0;
        w->yielded = false;
        if (global_first && KITE_RELAXED(&s_gq_size) > 0) {
            pthread_mutex_lock(&s_lock);
            kite_g *g = global_get_locked(w);
            pthread_mutex_unlock(&s_lock);
            if (g) return g;
        }
        kite_g *g = runq_get(w);
        if (g) return g;
        if (KITE_RELAXED(&s_gq_size) > 0) {
            pthread_mutex_lock(&s_lock);
            g = global_get_locked(w);
            pthread_mutex_unlock(&s_lock);
            if (g) return g;
        }
        int started = KITE_LOAD(&s_nstarted);
        if (!w->spinning && 2 * KITE_SC_LOAD(&s_nspinning) < started - KITE_SC_LOAD(&s_nidle)) {
            w->spinning = true;
            KITE_ADD(&s_nspinning, 1);
        }
        if (w->spinning) {
            for (int round = 0; round < 4; round++) {
                uint32_t start = (uint32_t)next_rand(w);
                for (int i = 0; i < started; i++) {
                    kite_worker *v = KITE_LOAD(&s_workers[(start + (uint32_t)i) % (uint32_t)started]);
                    if (!v || v == w) continue;
                    g = runq_steal(w, v, round == 3);
                    if (g) return g;
                }
            }
            w->spinning = false;
            KITE_ADD(&s_nspinning, -1);
            // anything queued between our last look and dropping `spinning`?
            KITE_FENCE();
            bool found = KITE_RELAXED(&s_gq_size) > 0;
            for (int i = 0; i < started && !found; i++) {
                kite_worker *v = KITE_LOAD(&s_workers[i]);
                if (v && v != w && !runq_empty(v)) found = true;
            }
            if (found) continue;
        }
        pthread_mutex_lock(&s_lock);
        if (s_gq_size > 0) {
            pthread_mutex_unlock(&s_lock);
            continue;
        }
        pthread_mutex_unlock(&s_lock);
        worker_park(w);
    }
}

static void schedule_loop(kite_worker *w) {
    for (;;) {
        kite_g *g = find_work(w);
        stop_spinning(w);
        // the waker may have been faster than the goroutine's switch away from its stack
        while (KITE_LOAD(&g->on_cpu)) cpu_relax();
        KITE_SET_RELAXED(&g->on_cpu, 1);
        KITE_SET_RELAXED(&w->cur, g);
        KITE_SET_RELAXED(&w->tick, w->tick + 1);
        switch_to_g(w, g);
        KITE_SET_RELAXED(&w->cur, NULL);
        int post = w->post;
        w->post = POST_NONE;
        KITE_STORE(&g->on_cpu, 0);
        if (post == POST_DEAD) {
            g_recycle(w, g);
        } else if (post == POST_YIELD) {
            w->yielded = true;
            // to the back of the line: others get a turn even on one worker
            global_put(g);
            maybe_wake();
        }
    }
}

static void g_entry(void) {
    kite_worker *w = cur_worker();
#ifdef KITE_ASAN
    __sanitizer_finish_switch_fiber(NULL, &w->stack_bottom, &w->stack_size);
#endif
    kite_g *g = w->cur;
    g->fn(g->arg);
    w = cur_worker();
    g = w->cur;
    scope_leave(g);
    kite_group *grp = g->group;
    if (grp) {
        extern void kite_group_task_done(kite_group *);
        kite_group_task_done(grp);
        w = cur_worker();
    }
    KITE_ADD(&s_ngoroutines, -1);
    w->post = POST_DEAD;
    switch_to_sched(g, w, true);
    __builtin_unreachable();
}

static void worker_init(kite_worker *w) {
    pthread_cond_init(&w->cv, NULL);
    t_worker = w;
    w->rng = ((uint64_t)w->id + 1) * 0x9E3779B97F4A7C15ULL ^ (uint64_t)kite_time_now();
    if (!w->rng) w->rng = 1;
#ifdef KITE_TSAN
    w->tsan_fiber = __tsan_get_current_fiber();
#endif
    worker_setup_signals();
    kite_thread_init();
#if defined(__APPLE__)
    w->mthread = pthread_mach_thread_np(pthread_self());
    KITE_STORE(&w->cpu_ok, 1);
#else
    if (pthread_getcpuclockid(pthread_self(), &w->cpuclock) == 0) KITE_STORE(&w->cpu_ok, 1);
#endif
}

// CPU time the worker's thread has used, in ns; -1 if unknown. The monitor uses
// it to tell a goroutine that doesn't answer a preemption request apart from a
// thread the OS isn't running (when the machine is overloaded): only the first
// is helped by handing its slot to another thread.
static int64_t worker_cpu_ns(kite_worker *w) {
    if (!KITE_LOAD(&w->cpu_ok)) return -1;
#if defined(__APPLE__)
    thread_basic_info_data_t info;
    mach_msg_type_number_t n = THREAD_BASIC_INFO_COUNT;
    if (thread_info(w->mthread, THREAD_BASIC_INFO, (thread_info_t)&info, &n) != KERN_SUCCESS) return -1;
    return ((int64_t)info.user_time.seconds + info.system_time.seconds) * 1000000000LL + ((int64_t)info.user_time.microseconds + info.system_time.microseconds) * 1000LL;
#else
    struct timespec ts;
    if (clock_gettime(w->cpuclock, &ts) != 0) return -1;
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
#endif
}

static void *worker_main(void *p) {
    kite_worker *w = p;
    worker_init(w);
    schedule_loop(w);
    return NULL;
}

// Called with s_lock held. The new worker starts out spinning.
void kite_start_worker(void) {
    kite_worker *w = calloc(1, sizeof *w);
    if (!w) kite_panic("out of memory", "<runtime>");
    w->id = (uint32_t)KITE_RELAXED(&s_nstarted);
    w->spinning = true;
    KITE_ADD(&s_nspinning, 1);
    __atomic_store_n(&s_workers[w->id], w, __ATOMIC_RELEASE);
    KITE_ADD(&s_nstarted, 1);
    pthread_t t;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&attr, 256 << 10);
    if (pthread_create(&t, &attr, worker_main, w) != 0) kite_panic("cannot start a worker thread", "<runtime>");
    pthread_attr_destroy(&attr);
}

// ------------------------------------------------------------------ stack overflow

static void on_fault(int sig, siginfo_t *si, void *uc) {
    kite_worker *w = t_worker;
    uint8_t *addr = si->si_addr;
    if (w && w->cur) {
        kite_g *g = w->cur;
        if (addr >= g->mapping && addr < g->mapping + s_guard_size + (64 << 10)) {
            static const char msg[] = "panic: stack overflow\n    goroutine stacks are limited by KITE_GOROUTINE_STACK (default 2 MB; the main goroutine has 1 GB)\n";
            fflush(stdout);
            ssize_t n = write(2, msg, sizeof msg - 1);
            (void)n;
            _exit(2);
        }
    }
    signal(sig, SIG_DFL);
    raise(sig);
}

static void worker_setup_signals(void) {
#ifndef KITE_ASAN
    size_t alt = 64 << 10;
    t_alt_stack = malloc(alt);
    if (!t_alt_stack) return;
    stack_t ss;
    memset(&ss, 0, sizeof ss);
    ss.ss_sp = t_alt_stack;
    ss.ss_size = alt;
    sigaltstack(&ss, NULL);
#endif
}

static void install_fault_handler(void) {
#if !defined(KITE_ASAN) && !defined(KITE_TSAN)
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
#endif
}

// ------------------------------------------------------------------ startup

typedef void (*kite_entry_fn)(void);

static void main_thunk(void *p) {
    ((kite_entry_fn)p)();
    fflush(stdout);
    fflush(stderr);
    exit(0);
}

_Noreturn void kite_sched_main(void (*entry)(void)) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    s_nprocs = n > 0 ? (int)n : 1;
    const char *env = getenv("KITE_MAXPROCS");
    if (env && atoi(env) > 0) s_nprocs = atoi(env);
    env = getenv("KITE_GOROUTINE_STACK");
    if (env && atoll(env) >= (64 << 10)) s_stack_size = (size_t)atoll(env);
    size_t ps = (size_t)sysconf(_SC_PAGESIZE);
    s_stack_size = (s_stack_size + ps - 1) & ~(ps - 1);
#if defined(__x86_64__)
    xsave_setup();
#endif
    s_workers = calloc(MAX_WORKERS, sizeof *s_workers);
    s_g_chunks = calloc(G_MAX_CHUNKS, sizeof *s_g_chunks);
    if (!s_workers || !s_g_chunks) kite_panic("out of memory", "<runtime>");
    install_fault_handler();
    kite_worker *w = calloc(1, sizeof *w);
    if (!w) kite_panic("out of memory", "<runtime>");
    s_workers[0] = w;
    s_nstarted = 1;
    worker_init(w);
    kite_g *g = g_alloc(w, s_main_stack_size);
    g->fn = main_thunk;
    g->arg = (void *)entry;
    runq_put(w, g, false);
    schedule_loop(w);
    __builtin_unreachable();
}

void kite_go(void (*fn)(void *), void *arg) {
    ensure_poller();
    KITE_ADD(&s_ngoroutines, 1);
    kite_g *g = g_alloc(cur_worker(), s_stack_size);
    g->fn = fn;
    g->arg = arg;
    g_ready(g);
}

// ------------------------------------------------------------------ channels

struct kite_chan {
    kite_spin lock;
    bool closed;
    uint8_t *buf;
    size_t esize;
    int64_t cap;
    int64_t head, len;
    kite_sudog *recv_head, *recv_tail;
    kite_sudog *send_head, *send_tail;
    int64_t refs;
};

kite_chan *kite_chan_new(size_t esize, int64_t cap) {
    if (cap < 0) kite_panic("negative channel capacity", "<runtime>");
    if (cap > (int64_t)(PTRDIFF_MAX / (esize ? esize : 1))) kite_panic("channel capacity too large", "<runtime>");
    kite_chan *c = kite_alloc(sizeof *c);
    memset(c, 0, sizeof *c);
    c->esize = esize ? esize : 1;
    c->cap = cap;
    c->buf = cap ? kite_alloc((size_t)cap * c->esize) : NULL;
    c->refs = 1;
    return c;
}

void kite_chan_retain(kite_chan *c) { __atomic_add_fetch(&c->refs, 1, __ATOMIC_RELAXED); }

void kite_chan_release(kite_chan *c, void (*drop_elem)(void *)) {
    if (!c) return;
    if (__atomic_sub_fetch(&c->refs, 1, __ATOMIC_ACQ_REL) != 0) return;
    if (drop_elem)
        for (int64_t i = 0; i < c->len; i++) drop_elem(c->buf + ((c->head + i) % c->cap) * c->esize);
    free(c->buf);
    free(c);
}

static void sq_push(kite_sudog **head, kite_sudog **tail, kite_sudog *s) {
    s->next = NULL;
    s->prev = *tail;
    if (*tail) (*tail)->next = s;
    else *head = s;
    *tail = s;
    s->linked = true;
}

static void sq_remove(kite_sudog **head, kite_sudog **tail, kite_sudog *s) {
    if (!s->linked) return;
    if (s->prev) s->prev->next = s->next;
    else *head = s->next;
    if (s->next) s->next->prev = s->prev;
    else *tail = s->prev;
    s->linked = false;
}

// Next waiter still in a position to be served; it stays parked until g_ready.
static kite_sudog *sq_pop(kite_sudog **head, kite_sudog **tail) {
    for (;;) {
        kite_sudog *s = *head;
        if (!s) return NULL;
        sq_remove(head, tail, s);
        kite_g *g = s->g;
        if (wait_claim(g, s->token)) {
            g->wake = WAKE_CHAN;
            g->wake_data = s;
            return s;
        }
    }
}

static inline void *chan_slot(kite_chan *c, int64_t i) { return c->buf + ((c->head + i) % c->cap) * c->esize; }

// Non-blocking attempts. c is locked; the lock is released only on success (1).
static int chan_try_send_locked(kite_chan *c, const void *elem, const char *loc) {
    if (c->closed) {
        spin_unlock(&c->lock);
        kite_panic("send on closed channel", loc);
    }
    kite_sudog *r = sq_pop(&c->recv_head, &c->recv_tail);
    if (r) {
        memcpy(r->elem, elem, c->esize);
        r->ok = true;
        spin_unlock(&c->lock);
        g_ready(r->g);
        return 1;
    }
    if (c->len < c->cap) {
        memcpy(chan_slot(c, c->len), elem, c->esize);
        c->len++;
        spin_unlock(&c->lock);
        return 1;
    }
    return 0;
}

// Returns 1 = received (unlocked), 2 = closed and drained (unlocked), 0 = would block (locked).
static int chan_try_recv_locked(kite_chan *c, void *out) {
    if (c->len > 0) {
        memcpy(out, chan_slot(c, 0), c->esize);
        c->head = (c->head + 1) % c->cap;
        c->len--;
        // a blocked sender can now use the free slot
        kite_sudog *s = sq_pop(&c->send_head, &c->send_tail);
        if (s) {
            memcpy(chan_slot(c, c->len), s->elem, c->esize);
            c->len++;
            s->ok = true;
            spin_unlock(&c->lock);
            g_ready(s->g);
            return 1;
        }
        spin_unlock(&c->lock);
        return 1;
    }
    kite_sudog *s = sq_pop(&c->send_head, &c->send_tail);
    if (s) {
        memcpy(out, s->elem, c->esize);
        s->ok = true;
        spin_unlock(&c->lock);
        g_ready(s->g);
        return 1;
    }
    if (c->closed) {
        spin_unlock(&c->lock);
        return 2;
    }
    return 0;
}

// False: the task was cancelled while blocked and the value was not sent.
bool kite_chan_send(kite_chan *c, const void *elem, const char *loc) {
    spin_lock(&c->lock);
    if (chan_try_send_locked(c, elem, loc)) return true;
    kite_g *g = cur_g();
    kite_sudog s;
    memset(&s, 0, sizeof s);
    s.g = g;
    s.c = c;
    s.elem = (void *)elem;
    s.token = wait_begin(g);
    sq_push(&c->send_head, &c->send_tail, &s);
    spin_unlock(&c->lock);
    wait_park(g, s.token);
    if (g->wake == WAKE_CANCEL) {
        spin_lock(&c->lock);
        sq_remove(&c->send_head, &c->send_tail, &s);
        spin_unlock(&c->lock);
        return false;
    }
    if (!s.ok) kite_panic("send on closed channel", loc);
    return true;
}

bool kite_chan_recv(kite_chan *c, void *out) {
    spin_lock(&c->lock);
    int r = chan_try_recv_locked(c, out);
    if (r) return r == 1;
    kite_g *g = cur_g();
    kite_sudog s;
    memset(&s, 0, sizeof s);
    s.g = g;
    s.c = c;
    s.elem = out;
    s.token = wait_begin(g);
    sq_push(&c->recv_head, &c->recv_tail, &s);
    spin_unlock(&c->lock);
    wait_park(g, s.token);
    if (g->wake == WAKE_CANCEL) {
        spin_lock(&c->lock);
        sq_remove(&c->recv_head, &c->recv_tail, &s);
        spin_unlock(&c->lock);
        return false;
    }
    return s.ok;
}

void kite_chan_close(kite_chan *c, const char *loc) {
    spin_lock(&c->lock);
    if (c->closed) {
        spin_unlock(&c->lock);
        kite_panic("close of closed channel", loc);
    }
    c->closed = true;
    // collect first: the sudogs live on stacks that resume once readied
    kite_g *wake = NULL;
    kite_sudog *s;
    while ((s = sq_pop(&c->recv_head, &c->recv_tail))) {
        s->ok = false;
        s->g->qnext = wake;
        wake = s->g;
    }
    while ((s = sq_pop(&c->send_head, &c->send_tail))) {
        s->ok = false;
        s->g->qnext = wake;
        wake = s->g;
    }
    spin_unlock(&c->lock);
    while (wake) {
        kite_g *n = wake->qnext;
        wake->qnext = NULL;
        g_ready(wake);
        wake = n;
    }
}

int64_t kite_chan_len(kite_chan *c) {
    spin_lock(&c->lock);
    int64_t n = c->len;
    spin_unlock(&c->lock);
    return n;
}

// Returns the arm taken, or -1 for the default arm. cases[i].ok reports the outcome:
// a received value, or a sent one. False on a receive means closed or cancelled;
// false on a send means cancelled, and the value was not sent.
int kite_select(kite_sel_case *cases, int n, bool has_default, const char *loc) {
    kite_g *g = cur_g();
    int order[n], locks[n];
    kite_worker *w = cur_worker();
    for (int i = 0; i < n; i++) {
        int j = (int)(next_rand(w) % (uint64_t)(i + 1));
        order[i] = order[j];
        order[j] = i;
    }
    // lock order: by address, each channel once
    int nlocks = 0;
    for (int i = 0; i < n; i++) {
        int k = nlocks++;
        while (k > 0 && cases[locks[k - 1]].c > cases[i].c) {
            locks[k] = locks[k - 1];
            k--;
        }
        locks[k] = i;
    }
#define SEL_LOCK_ALL() do { kite_chan *prev_ = NULL; for (int k_ = 0; k_ < nlocks; k_++) { kite_chan *c_ = cases[locks[k_]].c; if (c_ != prev_) spin_lock(&c_->lock); prev_ = c_; } } while (0)
#define SEL_UNLOCK_ALL() do { kite_chan *prev_ = NULL; for (int k_ = nlocks - 1; k_ >= 0; k_--) { kite_chan *c_ = cases[locks[k_]].c; if (c_ != prev_) spin_unlock(&c_->lock); prev_ = c_; } } while (0)
    SEL_LOCK_ALL();
    for (int i = 0; i < n; i++) {
        kite_sel_case *cs = &cases[order[i]];
        kite_chan *c = cs->c;
        if (cs->send) {
            if (c->closed) {
                SEL_UNLOCK_ALL();
                kite_panic("send on closed channel", loc);
            }
            kite_sudog *r = sq_pop(&c->recv_head, &c->recv_tail);
            if (r) {
                memcpy(r->elem, cs->elem, c->esize);
                r->ok = true;
                SEL_UNLOCK_ALL();
                g_ready(r->g);
                cs->ok = true;
                return order[i];
            }
            if (c->len < c->cap) {
                memcpy(chan_slot(c, c->len), cs->elem, c->esize);
                c->len++;
                SEL_UNLOCK_ALL();
                cs->ok = true;
                return order[i];
            }
        } else {
            kite_sudog *s = NULL;
            if (c->len > 0) {
                memcpy(cs->elem, chan_slot(c, 0), c->esize);
                c->head = (c->head + 1) % c->cap;
                c->len--;
                s = sq_pop(&c->send_head, &c->send_tail);
                if (s) {
                    memcpy(chan_slot(c, c->len), s->elem, c->esize);
                    c->len++;
                    s->ok = true;
                }
            } else {
                s = sq_pop(&c->send_head, &c->send_tail);
                if (s) {
                    memcpy(cs->elem, s->elem, c->esize);
                    s->ok = true;
                } else if (c->closed) {
                    SEL_UNLOCK_ALL();
                    cs->ok = false;
                    return order[i];
                } else {
                    continue;
                }
            }
            SEL_UNLOCK_ALL();
            if (s) g_ready(s->g);
            cs->ok = true;
            return order[i];
        }
    }
    if (has_default) {
        SEL_UNLOCK_ALL();
        return -1;
    }
    // nothing ready: wait on every channel
    uint64_t token = wait_begin(g);
    for (int i = 0; i < n; i++) {
        kite_sudog *s = &cases[i].s;
        memset(s, 0, sizeof *s);
        s->g = g;
        s->c = cases[i].c;
        s->elem = cases[i].elem;
        s->token = token;
        if (cases[i].send) sq_push(&s->c->send_head, &s->c->send_tail, s);
        else sq_push(&s->c->recv_head, &s->c->recv_tail, s);
    }
    SEL_UNLOCK_ALL();
    wait_park(g, token);
    kite_sudog *fired = g->wake == WAKE_CHAN ? g->wake_data : NULL;
    SEL_LOCK_ALL();
    for (int i = 0; i < n; i++) {
        kite_sudog *s = &cases[i].s;
        if (cases[i].send) sq_remove(&s->c->send_head, &s->c->send_tail, s);
        else sq_remove(&s->c->recv_head, &s->c->recv_tail, s);
    }
    SEL_UNLOCK_ALL();
#undef SEL_LOCK_ALL
#undef SEL_UNLOCK_ALL
    if (fired) {
        int i = (int)((kite_sel_case *)((char *)fired - offsetof(kite_sel_case, s)) - cases);
        if (cases[i].send && !fired->ok) kite_panic("send on closed channel", loc);
        cases[i].ok = fired->ok;
        return i;
    }
    // cancelled: behave as if a receive arm's channel was closed; with only send
    // arms, take none of them
    for (int i = 0; i < n; i++) {
        if (!cases[i].send) {
            cases[i].ok = false;
            return i;
        }
    }
    return -2;
}

// ------------------------------------------------------------------ mutex

// A goroutine-aware lock: waiters park instead of blocking their worker, and
// ownership can move between threads with the goroutine.
struct kite_mutex {
    int state;              // 0 free, 1 locked, 2 locked with waiters
    kite_spin lock;
    kite_g *head, *tail;
};

kite_mutex *kite_mutex_new(void) {
    kite_mutex *m = kite_alloc(sizeof *m);
    memset(m, 0, sizeof *m);
    return m;
}

void kite_mutex_free(kite_mutex *m) { free(m); }

void kite_mutex_lock(kite_mutex *m) {
    int expected = 0;
    if (__atomic_compare_exchange_n(&m->state, &expected, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return;
    for (int i = 0; i < 64; i++) {
        cpu_relax();
        if (KITE_RELAXED(&m->state) == 0) {
            expected = 0;
            if (__atomic_compare_exchange_n(&m->state, &expected, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return;
        }
    }
    kite_g *g = cur_g();
    spin_lock(&m->lock);
    // mark contended; if it turns out to be free, take it
    int old = __atomic_exchange_n(&m->state, 2, __ATOMIC_ACQUIRE);
    if (old == 0) {
        // nobody else queued (they would keep the state at 2 under this lock)
        if (!m->head) __atomic_store_n(&m->state, 1, __ATOMIC_RELAXED);
        spin_unlock(&m->lock);
        return;
    }
    g->qnext = NULL;
    if (m->tail) m->tail->qnext = g;
    else m->head = g;
    m->tail = g;
    spin_unlock(&m->lock);
    g_park();
    // the unlocker handed the lock over
}

void kite_mutex_unlock(kite_mutex *m) {
    int expected = 1;
    if (__atomic_compare_exchange_n(&m->state, &expected, 0, false, __ATOMIC_RELEASE, __ATOMIC_RELAXED)) return;
    spin_lock(&m->lock);
    kite_g *g = m->head;
    if (g) {
        m->head = g->qnext;
        if (!m->head) {
            m->tail = NULL;
            __atomic_store_n(&m->state, 1, __ATOMIC_RELEASE);
        }
        g->qnext = NULL;
        spin_unlock(&m->lock);
        g_ready(g);
        return;
    }
    __atomic_store_n(&m->state, 0, __ATOMIC_RELEASE);
    spin_unlock(&m->lock);
}

// ------------------------------------------------------------------ task groups

struct kite_group {
    kite_scope scope;
    kite_spin lock;
    int64_t live;
    kite_g *waiter;
    bool failed;
};

kite_group *kite_group_new(void) {
    ensure_poller();
    kite_group *g = kite_alloc(sizeof *g);
    memset(g, 0, sizeof *g);
    // the body runs in the group: a failing task cancels it too
    scope_enter(&g->scope, cur_g());
    return g;
}

void kite_group_go(kite_group *grp, void (*fn)(void *), void *arg) {
    kite_g *g = g_alloc(cur_worker(), s_stack_size);
    g->fn = fn;
    g->arg = arg;
    g->group = grp;
    KITE_ADD(&s_ngoroutines, 1);
    spin_lock(&grp->lock);
    grp->live++;
    spin_unlock(&grp->lock);
    scope_join(&grp->scope, g);
    g_ready(g);
}

void kite_group_task_done(kite_group *grp) {
    spin_lock(&grp->lock);
    kite_g *w = --grp->live == 0 ? grp->waiter : NULL;
    if (w) grp->waiter = NULL;
    spin_unlock(&grp->lock);
    if (w) g_ready(w);
}

// True for the first failure: that task records its error. Also cancels the group.
bool kite_group_fail(kite_group *grp) {
    spin_lock(&grp->lock);
    bool first = !grp->failed;
    grp->failed = true;
    spin_unlock(&grp->lock);
    scope_cancel(&grp->scope);
    return first;
}

void kite_group_cancel(kite_group *grp) { scope_cancel(&grp->scope); }

// Always waits for every task, cancelled or not: tasks may borrow from this frame.
void kite_group_wait(kite_group *grp) {
    kite_g *g = cur_g();
    // the body is done; its tasks stay reachable by cancellation from above
    scope_step_out(&grp->scope, g);
    spin_lock(&grp->lock);
    if (grp->live > 0) {
        grp->waiter = g;
        spin_unlock(&grp->lock);
        g_park();
    } else {
        spin_unlock(&grp->lock);
    }
    // the last task's kite_group_task_done may still be releasing the lock
    spin_lock(&grp->lock);
    spin_unlock(&grp->lock);
    scope_unlink(&grp->scope);
    free(grp);
}

// ------------------------------------------------------------------ timers

typedef struct kite_timer {
    int64_t when;
    int idx;                // position in the heap, -1 when not in it
    kite_g *g;              // wake this goroutine...
    uint64_t token;
    kite_scope *scope;      // ...or cancel this scope
} kite_timer;

static pthread_mutex_t s_timer_lock = PTHREAD_MUTEX_INITIALIZER;
static kite_timer **s_heap;
static int s_heap_len, s_heap_cap;

static void heap_set(int i, kite_timer *t) {
    s_heap[i] = t;
    t->idx = i;
}

static void heap_up(int i) {
    kite_timer *t = s_heap[i];
    while (i > 0) {
        int p = (i - 1) / 2;
        if (s_heap[p]->when <= t->when) break;
        heap_set(i, s_heap[p]);
        i = p;
    }
    heap_set(i, t);
}

static void heap_down(int i) {
    kite_timer *t = s_heap[i];
    for (;;) {
        int l = 2 * i + 1;
        if (l >= s_heap_len) break;
        int m = l + 1 < s_heap_len && s_heap[l + 1]->when < s_heap[l]->when ? l + 1 : l;
        if (s_heap[m]->when >= t->when) break;
        heap_set(i, s_heap[m]);
        i = m;
    }
    heap_set(i, t);
}

static void heap_remove(kite_timer *t) {
    int i = t->idx;
    if (i < 0) return;
    t->idx = -1;
    KITE_ADD(&s_timers_pending, -1);
    kite_timer *last = s_heap[--s_heap_len];
    if (last != t) {
        heap_set(i, last);
        heap_up(i);
        heap_down(last->idx);
    }
}

static void timer_add(kite_timer *t) {
    ensure_poller();
    pthread_mutex_lock(&s_timer_lock);
    if (s_heap_len == s_heap_cap) {
        s_heap_cap = s_heap_cap ? s_heap_cap * 2 : 64;
        s_heap = kite_realloc(s_heap, (size_t)s_heap_cap * sizeof *s_heap);
    }
    KITE_ADD(&s_timers_pending, 1);
    s_heap[s_heap_len++] = t;
    t->idx = s_heap_len - 1;
    heap_up(t->idx);
    bool earliest = t->idx == 0;
    pthread_mutex_unlock(&s_timer_lock);
    if (earliest) poller_wake();
}

static void timer_remove(kite_timer *t) {
    pthread_mutex_lock(&s_timer_lock);
    heap_remove(t);
    pthread_mutex_unlock(&s_timer_lock);
}

// False: the task was cancelled before the time was up.
bool kite_time___sleep(int64_t ns) {
    kite_g *g = cur_g();
    if (scope_cancelled(g->scope)) return false;
    if (ns <= 0) {
        kite_task_yieldNow();
        return !scope_cancelled(cur_g()->scope);
    }
    kite_timer t;
    memset(&t, 0, sizeof t);
    t.when = kite_time_now() + ns;
    t.g = g;
    t.token = wait_begin(g);
    t.idx = -1;
    timer_add(&t);
    wait_park(g, t.token);
    if (g->wake == WAKE_TIMER) return true;
    timer_remove(&t);
    return false;
}

// A cancel scope with a deadline, for task.timeout.
uint8_t *kite_task___scopeEnter(int64_t ns) {
    kite_g *g = cur_g();
    struct { kite_scope s; kite_timer t; } *sc = kite_alloc(sizeof *sc);
    scope_enter(&sc->s, g);
    memset(&sc->t, 0, sizeof sc->t);
    sc->t.idx = -1;
    sc->t.scope = &sc->s;
    if (ns <= 0) {
        scope_cancel(&sc->s);
    } else {
        sc->t.when = kite_time_now() + ns;
        timer_add(&sc->t);
    }
    return (uint8_t *)sc;
}

// True: the scope was cancelled, by its deadline or from outside.
bool kite_task___scopeExit(uint8_t *p) {
    struct { kite_scope s; kite_timer t; } *sc = (void *)p;
    timer_remove(&sc->t);
    bool cancelled = KITE_SC_LOAD(&sc->s.cancelled) != 0;
    scope_exit(&sc->s, cur_g());
    free(sc);
    return cancelled;
}

void kite_task___groupCancel(uint8_t *state) { kite_group_cancel(*(kite_group **)state); }

// ------------------------------------------------------------------ poller and monitor

static int s_poll_fd = -1;
static int s_wake_fd = -1;
static int s_timer_fd = -1;     // Linux: epoll timeouts are in milliseconds; timerfd isn't
static int s_poller_started;

static void poller_wake(void) {
#if defined(__APPLE__)
    struct kevent ev;
    EV_SET(&ev, 1, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    kevent(s_poll_fd, &ev, 1, NULL, 0, NULL);
#else
    uint64_t one = 1;
    ssize_t r = write(s_wake_fd, &one, sizeof one);
    (void)r;
#endif
}

#define PREEMPT_AFTER (10 * 1000000)    // a goroutine's time slice while others wait
#define UNRESPONSIVE (20 * 1000000)     // no answer to a request to yield: it's in C
#define SYSCALL_AFTER (1 * 1000000)     // a C call this long gives up its slot

static void monitor_tick(void) {
    int64_t now = kite_time_now();
    int started = KITE_LOAD(&s_nstarted);
    bool queued = KITE_RELAXED(&s_gq_size) > 0;
    for (int i = 0; i < started && !queued; i++) {
        kite_worker *w = KITE_LOAD(&s_workers[i]);
        if (w && !runq_empty(w)) queued = true;
    }
    for (int i = 0; i < started; i++) {
        kite_worker *w = KITE_LOAD(&s_workers[i]);
        if (!w) continue;
        uint64_t tick = KITE_RELAXED(&w->tick);
        uint64_t seq = KITE_RELAXED(&w->sysseq);
        if (tick != w->seen_tick || !KITE_RELAXED(&w->cur)) {
            // a new goroutine since the last look
            w->seen_tick = tick;
            w->seen_since = now;
            w->seen_sysseq = seq;
            w->preempt_since = 0;
            cancel_preempt(w);
            continue;
        }
        if (KITE_SC_LOAD(&w->insys)) {
            // one C call has been running since the last look
            if (seq == w->seen_sysseq && now - w->seen_since >= SYSCALL_AFTER) {
                worker_release(w);
                // it may have returned in the meantime
                if (!KITE_SC_LOAD(&w->insys)) worker_reclaim(w);
            } else if (seq != w->seen_sysseq) {
                w->seen_sysseq = seq;
                w->seen_since = now;
            }
            continue;
        }
        if (KITE_RELAXED(&w->released)) continue;
        if (!queued || now - w->seen_since < PREEMPT_AFTER) continue;
        if (!w->preempt_since) {
            w->preempt_since = now;
            w->preempt_cpu = worker_cpu_ns(w);
            if (!__atomic_exchange_n(&w->preempt, 1, __ATOMIC_ACQ_REL)) KITE_ADD(&kite_preempt_flag, 1);
        } else if (now - w->preempt_since >= UNRESPONSIVE) {
            // Not polling: a long loop inside C, like a big sort -- if the thread
            // has really been running. One the OS has kept off the CPU (an
            // overloaded machine) would answer once it runs, and another thread
            // would only add to the overload.
            int64_t cpu = worker_cpu_ns(w);
            if (cpu >= 0 && w->preempt_cpu >= 0 && cpu - w->preempt_cpu < UNRESPONSIVE / 2) continue;
            cancel_preempt(w);
            worker_release(w);
        }
    }
    if (queued && can_wake() && KITE_SC_LOAD(&s_nspinning) == 0) wake_one();
}

static void deliver_token(uint64_t data) {
    uint32_t idx = (uint32_t)(data >> 34);
    uint64_t low = data & (((uint64_t)1 << 34) - 1);
    if (idx >= KITE_LOAD(&s_g_count)) return;
    kite_g *g = g_slot(idx);
    uint64_t w = KITE_SC_LOAD(&g->wait);
    if ((w & (((uint64_t)1 << 34) - 1)) != low) return; // a stale event
    if (wait_claim(g, w)) {
        g->wake = WAKE_IO;
        g_ready(g);
        KITE_ADD(&s_io_waiters, -1);
    }
}

static void *poller_main(void *arg) {
    (void)arg;
    sigset_t all;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, NULL);
    for (;;) {
        int64_t now = kite_time_now();
        int64_t next = -1;
        pthread_mutex_lock(&s_timer_lock);
        while (s_heap_len > 0 && s_heap[0]->when <= now) {
            kite_timer *t = s_heap[0];
            if (t->scope) {
                heap_remove(t);
                scope_cancel(t->scope);
            } else {
                // keep it counted until the goroutine is queued, for deadlock detection
                KITE_ADD(&s_timers_pending, 1);
                heap_remove(t);
                if (wait_claim(t->g, t->token)) {
                    t->g->wake = WAKE_TIMER;
                    g_ready(t->g);
                }
                KITE_ADD(&s_timers_pending, -1);
            }
        }
        if (s_heap_len > 0) next = s_heap[0]->when - now;
        pthread_mutex_unlock(&s_timer_lock);
        monitor_tick();
        // while anything runs, tick for the monitor; when all is idle, only events matter
        bool idle = KITE_SC_LOAD(&s_nidle) == KITE_SC_LOAD(&s_nstarted);
        if (!idle && (next < 0 || next > 2000000)) next = 2000000;
        if (next < 0 || next > 100000000) next = 100000000;
#if defined(__APPLE__)
        struct kevent evs[64];
        struct timespec ts;
        if (next >= 0) {
            ts.tv_sec = (time_t)(next / 1000000000);
            ts.tv_nsec = (long)(next % 1000000000);
        }
        int n = kevent(s_poll_fd, NULL, 0, evs, 64, next >= 0 ? &ts : NULL);
        for (int i = 0; i < n; i++) {
            if (evs[i].filter == EVFILT_USER) continue;
            deliver_token((uint64_t)evs[i].udata);
        }
#else
        struct epoll_event evs[64];
        int ms = -1;
        if (next >= 0) {
            // the earliest timer, to the nanosecond; the epoll timeout is only a backstop
            int64_t at = kite_time_now() + next;
            struct itimerspec its;
            memset(&its, 0, sizeof its);
            its.it_value.tv_sec = (time_t)(at / 1000000000);
            its.it_value.tv_nsec = (long)(at % 1000000000);
            if (next == 0) its.it_value.tv_nsec += 1;
            timerfd_settime(s_timer_fd, TFD_TIMER_ABSTIME, &its, NULL);
            ms = (int)(next / 1000000) + 1;
        }
        int n = epoll_wait(s_poll_fd, evs, 64, ms);
        for (int i = 0; i < n; i++) {
            if (evs[i].data.u64 <= 1) {
                uint64_t v;
                ssize_t r = read(evs[i].data.u64 == 0 ? s_wake_fd : s_timer_fd, &v, sizeof v);
                (void)r;
                continue;
            }
            deliver_token(evs[i].data.u64);
        }
#endif
    }
    return NULL;
}

static void ensure_poller(void) {
    if (KITE_LOAD(&s_poller_started)) return;
    pthread_mutex_lock(&s_lock);
    if (!s_poller_started) {
#if defined(__APPLE__)
        s_poll_fd = kqueue();
        struct kevent ev;
        EV_SET(&ev, 1, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
        kevent(s_poll_fd, &ev, 1, NULL, 0, NULL);
#else
        s_poll_fd = epoll_create1(EPOLL_CLOEXEC);
        s_wake_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
        struct epoll_event ev;
        memset(&ev, 0, sizeof ev);
        ev.events = EPOLLIN;
        ev.data.u64 = 0;
        epoll_ctl(s_poll_fd, EPOLL_CTL_ADD, s_wake_fd, &ev);
        s_timer_fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK | TFD_CLOEXEC);
        ev.data.u64 = 1;
        epoll_ctl(s_poll_fd, EPOLL_CTL_ADD, s_timer_fd, &ev);
#endif
        if (s_poll_fd < 0) kite_panic("cannot create the event queue", "<runtime>");
        pthread_t t;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
        pthread_attr_setstacksize(&attr, 256 << 10);
        if (pthread_create(&t, &attr, poller_main, NULL) != 0) kite_panic("cannot start the poller thread", "<runtime>");
        pthread_attr_destroy(&attr);
        KITE_STORE(&s_poller_started, 1);
    }
    pthread_mutex_unlock(&s_lock);
}

// Parks until fd is ready. False: the task was cancelled.
static bool wait_fd(int fd, bool for_write) {
    ensure_poller();
    kite_g *g = cur_g();
    if (scope_cancelled(g->scope)) return false;
    uint64_t token = wait_begin(g);
    uint64_t data = ((uint64_t)g->idx << 34) | (token & (((uint64_t)1 << 34) - 1));
    KITE_ADD(&s_io_waiters, 1);
#if defined(__APPLE__)
    struct kevent ev;
    EV_SET(&ev, fd, for_write ? EVFILT_WRITE : EVFILT_READ, EV_ADD | EV_ONESHOT, 0, 0, (void *)data);
    if (kevent(s_poll_fd, &ev, 1, NULL, 0, NULL) != 0) kite_panic("cannot register a file descriptor with the poller", "<runtime>");
#else
    struct epoll_event ev;
    memset(&ev, 0, sizeof ev);
    ev.events = (for_write ? EPOLLOUT : EPOLLIN) | EPOLLONESHOT | EPOLLRDHUP;
    ev.data.u64 = data;
    if (epoll_ctl(s_poll_fd, EPOLL_CTL_MOD, fd, &ev) != 0) {
        if (errno != ENOENT || epoll_ctl(s_poll_fd, EPOLL_CTL_ADD, fd, &ev) != 0) kite_panic("cannot register a file descriptor with the poller", "<runtime>");
    }
#endif
    wait_park(g, token);
    if (g->wake == WAKE_IO) return true;
    // cancelled: the registration is stale now, and a late event won't match the token
    KITE_ADD(&s_io_waiters, -1);
#if defined(__APPLE__)
    EV_SET(&ev, fd, for_write ? EVFILT_WRITE : EVFILT_READ, EV_DELETE, 0, 0, NULL);
    kevent(s_poll_fd, &ev, 1, NULL, 0, NULL);
#else
    ev.events = 0;
    epoll_ctl(s_poll_fd, EPOLL_CTL_MOD, fd, &ev);
#endif
    return false;
}

// ------------------------------------------------------------------ TCP

static void sock_setup(int fd) {
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
#if defined(SO_NOSIGPIPE)
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
#endif
}

// "host:port" or ":port"; an empty host means all interfaces when listening.
static int resolve(kite_str addr, bool passive, struct addrinfo **out) {
    int64_t colon = -1;
    for (int64_t i = addr.len - 1; i >= 0; i--) {
        if (addr.ptr[i] == ':') {
            colon = i;
            break;
        }
    }
    if (colon < 0) return EINVAL;
    char host[256], port[32];
    if (colon >= (int64_t)sizeof host || addr.len - colon - 1 >= (int64_t)sizeof port) return EINVAL;
    memcpy(host, addr.ptr, (size_t)colon);
    host[colon] = 0;
    memcpy(port, addr.ptr + colon + 1, (size_t)(addr.len - colon - 1));
    port[addr.len - colon - 1] = 0;
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (passive) hints.ai_flags = AI_PASSIVE;
    // name lookups can take seconds: give the CPU slot away meanwhile
    kite_blocking_enter();
    int r = getaddrinfo(colon ? host : NULL, port, &hints, out);
    kite_ffi_exit();
    return r == 0 ? 0 : EHOSTUNREACH;
}

int64_t kite_net___listen(kite_str addr) {
    if (addr.len && memchr(addr.ptr, 0, (size_t)addr.len)) return -EINVAL;
    struct addrinfo *ai;
    int r = resolve(addr, true, &ai);
    if (r) return -r;
    int err = EADDRNOTAVAIL;
    for (struct addrinfo *a = ai; a; a = a->ai_next) {
        int fd = socket(a->ai_family, a->ai_socktype, a->ai_protocol);
        if (fd < 0) {
            err = errno;
            continue;
        }
        int one = 1;
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
        if (bind(fd, a->ai_addr, a->ai_addrlen) == 0 && listen(fd, 1024) == 0) {
            sock_setup(fd);
            freeaddrinfo(ai);
            return fd;
        }
        err = errno;
        close(fd);
    }
    freeaddrinfo(ai);
    return -err;
}

int64_t kite_net___connect(kite_str addr) {
    if (addr.len && memchr(addr.ptr, 0, (size_t)addr.len)) return -EINVAL;
    struct addrinfo *ai;
    int r = resolve(addr, false, &ai);
    if (r) return -r;
    int err = EHOSTUNREACH;
    for (struct addrinfo *a = ai; a; a = a->ai_next) {
        int fd = socket(a->ai_family, a->ai_socktype, a->ai_protocol);
        if (fd < 0) {
            err = errno;
            continue;
        }
        sock_setup(fd);
        int c = connect(fd, a->ai_addr, a->ai_addrlen);
        if (c != 0 && errno == EINPROGRESS) {
            if (!wait_fd(fd, true)) {
                close(fd);
                freeaddrinfo(ai);
                return -ECANCELED;
            }
            socklen_t len = sizeof c;
            getsockopt(fd, SOL_SOCKET, SO_ERROR, &c, &len);
            if (c != 0) errno = c;
        }
        if (c == 0) {
            freeaddrinfo(ai);
            return fd;
        }
        err = errno;
        close(fd);
    }
    freeaddrinfo(ai);
    return -err;
}

int64_t kite_net___accept(int64_t lfd) {
    for (;;) {
        int fd = accept((int)lfd, NULL, NULL);
        if (fd >= 0) {
            sock_setup(fd);
            return fd;
        }
        if (errno == EINTR || errno == ECONNABORTED) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) return -errno;
        if (!wait_fd((int)lfd, false)) return -ECANCELED;
    }
}

int64_t kite_net___read(int64_t fd, kite_vec *buf, int64_t max) {
    if (max <= 0) return 0;
    kite_vec_reserve(buf, max, 1);
    for (;;) {
        ssize_t n = read((int)fd, (uint8_t *)buf->ptr + buf->len, (size_t)max);
        if (n >= 0) {
            buf->len += n;
            return n;
        }
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) return -errno;
        if (!wait_fd((int)fd, false)) return -ECANCELED;
    }
}

int64_t kite_net___write(int64_t fd, kite_slice data) {
#if defined(MSG_NOSIGNAL)
    int flags = MSG_NOSIGNAL;
#else
    int flags = 0;
#endif
    for (;;) {
        ssize_t n = send((int)fd, data.ptr, (size_t)data.len, flags);
        if (n >= 0) return n;
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) return -errno;
        if (!wait_fd((int)fd, true)) return -ECANCELED;
    }
}

void kite_net___close(int64_t fd) { close((int)fd); }

void kite_net___shutdownWrite(int64_t fd) { shutdown((int)fd, SHUT_WR); }

int64_t kite_net___port(int64_t fd) {
    struct sockaddr_storage ss;
    socklen_t len = sizeof ss;
    if (getsockname((int)fd, (struct sockaddr *)&ss, &len) != 0) return -errno;
    if (ss.ss_family == AF_INET) return ntohs(((struct sockaddr_in *)&ss)->sin_port);
    if (ss.ss_family == AF_INET6) return ntohs(((struct sockaddr_in6 *)&ss)->sin6_port);
    return -EINVAL;
}


// ------------------------------------------------------------------ tests

// Each test gets a process, and a scheduler, of its own.
int kite_run_test(void (*fn)(void), const char *name) {
    fflush(stdout);
    fflush(stderr);
    int64_t t0 = kite_time_now();
    pid_t pid = fork();
    if (pid == 0) {
        kite_sched_main(fn);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    double ms = (double)(kite_time_now() - t0) / 1e6;
    bool ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    fprintf(stderr, "%s %s (%.1f ms)\n", ok ? "  ok  " : "  FAIL", name, ms);
    return ok ? 0 : 1;
}

// ------------------------------------------------------------------ task stats

int64_t kite_task___numGoroutines(void) { return KITE_SC_LOAD(&s_ngoroutines) + 1; }
int64_t kite_task___numThreads(void) { return KITE_SC_LOAD(&s_nstarted); }
int64_t kite_task___maxProcs(void) { return KITE_RELAXED(&s_nprocs); }

int64_t kite_task___setMaxProcs(int64_t n) {
    if (n < 1) n = 1;
    if (n > MAX_WORKERS) n = MAX_WORKERS;
    int old = __atomic_exchange_n(&s_nprocs, (int)n, __ATOMIC_SEQ_CST);
    // more room: start using it; less: workers park as they reach the scheduler
    if (n > old) maybe_wake();
    return old;
}
