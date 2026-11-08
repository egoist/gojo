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
//    So there are as many workers running Gojo code as CPUs, plus one per
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
#include <poll.h>
#if defined(__APPLE__)
#include <sys/event.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#else
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#endif

#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#define GOJO_ASAN 1
#endif
#if __has_feature(thread_sanitizer)
#define GOJO_TSAN 1
#endif
#if __has_feature(type_sanitizer)
#define GOJO_TYSAN 1
#endif
#endif
#if defined(__SANITIZE_ADDRESS__) && !defined(GOJO_ASAN)
#define GOJO_ASAN 1
#endif
#if defined(__SANITIZE_THREAD__) && !defined(GOJO_TSAN)
#define GOJO_TSAN 1
#endif
#ifdef GOJO_ASAN
void __sanitizer_start_switch_fiber(void **fake_stack_save, const void *bottom, size_t size);
void __sanitizer_finish_switch_fiber(void *fake_stack_save, const void **bottom_old, size_t *size_old);
#endif
#ifdef GOJO_TSAN
void *__tsan_get_current_fiber(void);
void *__tsan_create_fiber(unsigned flags);
void __tsan_destroy_fiber(void *fiber);
void __tsan_switch_to_fiber(void *fiber, unsigned flags);
#endif

#define GOJO_LOAD(p) __atomic_load_n((p), __ATOMIC_ACQUIRE)
#define GOJO_STORE(p, v) __atomic_store_n((p), (v), __ATOMIC_RELEASE)
#define GOJO_RELAXED(p) __atomic_load_n((p), __ATOMIC_RELAXED)
#define GOJO_SET_RELAXED(p, v) __atomic_store_n((p), (v), __ATOMIC_RELAXED)
#define GOJO_SC_LOAD(p) __atomic_load_n((p), __ATOMIC_SEQ_CST)
#define GOJO_SC_STORE(p, v) __atomic_store_n((p), (v), __ATOMIC_SEQ_CST)
#define GOJO_ADD(p, n) __atomic_add_fetch((p), (n), __ATOMIC_SEQ_CST)
#define GOJO_FENCE() __atomic_thread_fence(__ATOMIC_SEQ_CST)

static inline void cpu_relax(void) {
#if defined(__x86_64__)
    __asm__ volatile("pause");
#elif defined(__aarch64__)
    __asm__ volatile("yield");
#endif
}

typedef int gojo_spin;

static inline void spin_lock(gojo_spin *l) {
    int spins = 0;
    for (;;) {
        if (__atomic_exchange_n(l, 1, __ATOMIC_ACQUIRE) == 0) return;
        while (GOJO_RELAXED(l)) {
            if (++spins < 256) cpu_relax();
            else sched_yield();
        }
    }
}
static inline void spin_unlock(gojo_spin *l) { __atomic_store_n(l, 0, __ATOMIC_RELEASE); }
static inline bool spin_trylock(gojo_spin *l) { return __atomic_exchange_n(l, 1, __ATOMIC_ACQUIRE) == 0; }

// ------------------------------------------------------------------ context switch

// void gojo_ctx_switch(void **save_sp, void *load_sp)
#define GOJO_STR2(x) #x
#define GOJO_STR(x) GOJO_STR2(x)
#define GOJO_SYM(name) GOJO_STR(__USER_LABEL_PREFIX__) #name

#if defined(__aarch64__)
#define GOJO_CTX_FRAME 176
#define GOJO_CTX_RET_SLOT 11   // x30, in words from the frame base
__asm__(
    ".text\n.p2align 2\n"
#if !defined(__APPLE__)
    ".type gojo_ctx_switch, %function\n"
#endif
    ".globl " GOJO_SYM(gojo_ctx_switch) "\n"
    GOJO_SYM(gojo_ctx_switch) ":\n"
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
#define GOJO_CTX_FRAME 64
#define GOJO_CTX_RET_SLOT 7    // return address, in words from the frame base
__asm__(
    ".text\n.p2align 4\n"
#if !defined(__APPLE__)
    ".type gojo_ctx_switch, @function\n"
#endif
    ".globl " GOJO_SYM(gojo_ctx_switch) "\n"
    GOJO_SYM(gojo_ctx_switch) ":\n"
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
#error "the Gojo runtime supports x86_64 and aarch64"
#endif

extern void gojo_ctx_switch(void **save_sp, void *load_sp);

// ------------------------------------------------------------------ preemption trampoline
//
// Generated code reaches gojo_preempt_point through this stub, from an asm
// statement that tells the C compiler nothing changes: no registers (besides
// the return address and flags) and no memory. That keeps the poll from
// forcing values out of registers in the loops it sits in. It's sound because
// a preemption point is not a synchronization point: Gojo code only sees
// other goroutines' writes through locks and channels, which are real calls.
// The stub saves every register the C call could change, vector state included.
#if defined(__aarch64__)
__asm__(
    ".text\n.p2align 2\n"
    ".globl " GOJO_SYM(gojo_preempt_tramp) "\n"
    GOJO_SYM(gojo_preempt_tramp) ":\n"
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
    "    bl " GOJO_SYM(gojo_preempt_point) "\n"
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
int64_t gojo_xsave_size;
__asm__(
    ".text\n.p2align 4\n"
    ".globl " GOJO_SYM(gojo_preempt_tramp) "\n"
    GOJO_SYM(gojo_preempt_tramp) ":\n"
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
    "    movq " GOJO_SYM(gojo_xsave_size) "(%rip), %rcx\n"
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
    "    call " GOJO_SYM(gojo_preempt_point) "\n"
    "    movl $-1, %eax\n"
    "    movl $-1, %edx\n"
    "    xrstor64 (%rsp)\n"
    "    jmp 2f\n"
    "1:\n"
    "    subq $512, %rsp\n"
    "    fxsave64 (%rsp)\n"
    "    call " GOJO_SYM(gojo_preempt_point) "\n"
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
    gojo_xsave_size = ((int64_t)b + 63) & ~(int64_t)63;
}
#endif


// ------------------------------------------------------------------ core types

// (WAIT_DRAIN: waiting for an io_uring operation to finish after a cancel;
// cancellation doesn't wake it)
enum { WAIT_NONE = 0, WAIT_CANCELLABLE = 1, WAIT_DRAIN = 2 };
enum { WAKE_NONE, WAKE_CANCEL, WAKE_CHAN, WAKE_TIMER, WAKE_IO, WAKE_TASK };
enum { POST_NONE, POST_DEAD, POST_YIELD };
enum { BLK_NONE, BLK_RECV, BLK_SEND, BLK_SELECT, BLK_LOCK, BLK_JOIN, BLK_SCOPE, BLK_SLEEP, BLK_IO, BLK_RWLOCK, BLK_FILE };

typedef struct gojo_scope gojo_scope;

typedef struct gojo_timer {
    int64_t when;
    int idx;                // position in the heap, -1 when not in it
    struct gojo_g *g;       // wake this goroutine...
    uint64_t token;
    gojo_scope *scope;      // ...or cancel this scope
} gojo_timer;

// Wait records live in the goroutine's descriptor (or on the heap), never on
// its stack: a parked stack may be put away (see "hibernation"), and others
// mustn't read it then.
typedef struct gojo_g {
    void *sp;
    int on_cpu;             // the goroutine is still on its stack; don't resume it yet
    uint32_t epoch;
    uint64_t wait;          // epoch << 2 | wait kind; claimed by CAS back to WAIT_NONE
    int wake;               // why the goroutine woke; written by whoever claimed it
    void *wake_data;
    void (*fn)(void *);
    void *arg;
    void (*arg_drop)(void *);   // called on arg once fn returns (a closure's environment)
    struct gojo_g *qnext;   // global queue, free lists, mutex waiters
    gojo_scope *scope;
    struct gojo_g *scope_prev, *scope_next;
    bool scope_linked;      // in scope->members: joined when it first waits
    int scope_shard;        // which of the scope's member lists
    gojo_group *group;
    uint8_t *stack;         // lowest usable address
    size_t stack_size;
    uint8_t *mapping;       // start of the whole mapping, guard included
    size_t mapping_size;
    uint32_t idx;
    // (above: what starting and switching touch; below: rarely used)
    bool alive;             // for task dumps and deadlock reports
    const char *spawn_loc;  // where `go` started it
    int blocked;            // BLK_*: what it is parked on
    const void *blocked_on;
    int64_t blocked_since;
    // locks held, by where they were made (debug builds: lock-order checking)
    const char *held[8];
    int nheld;
    bool slow_noted;        // debug builds: told that its scope waits long
    gojo_sudog sud;         // a channel send or receive
    gojo_timer timer;       // a sleep, or a select's deadline
    // hibernation: the used part of the stack, copied out, pages released
    gojo_spin hib_lock;
    int hib;
    int touching;           // a channel is copying a value into or out of its stack
    uint8_t *hib_buf;
    size_t hib_size;
    struct gojo_group *open_groups; // scopes it opened: their tasks may borrow its stack
    bool rw_write;          // waiting for an RwLock: as a writer
    // an io_uring operation: its number (in its user_data), its result, and
    // the number of the last one finished; while one is in flight the kernel
    // may write into the stack, so it isn't put away
    uint32_t io_seq;
    uint32_t io_done;
    int32_t io_res;
    int io_inflight;
    // C was given a pointer into its stack (see gojo_ptr_escape): C may keep it
    // and use it while the goroutine is parked, so the stack stays where it is
    int pinned;
#ifdef GOJO_ASAN
    void *fake_stack;
#endif
#ifdef GOJO_TSAN
    void *tsan_fiber;
#endif
} gojo_g;

#define RUNQ_SIZE 256

typedef struct gojo_worker {
    void *sched_sp;
    gojo_g *cur;
    int post;
    uint32_t rq_head, rq_tail;
    gojo_g *rq[RUNQ_SIZE];
    gojo_g *runnext;
    pthread_cond_t cv;
    bool notified;
    struct gojo_worker *idle_next;
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
    gojo_g *free_gs;
    int nfree;
    uint32_t id;
#ifdef GOJO_ASAN
    void *fake_stack;
    const void *stack_bottom;
    size_t stack_size;
#endif
#ifdef GOJO_TSAN
    void *tsan_fiber;
#endif
} gojo_worker;

// why a scope was cancelled (std/task.gojo `Reason`, plus one)
enum { CANCEL_FAILED = 1, CANCEL_DEADLINE, CANCEL_EXPLICIT, CANCEL_SHUTDOWN };

// Members (goroutines that have waited in the scope, for cancellation to
// wake) are kept in a few lists, one per group of workers, so that tasks on
// different threads don't contend for one lock.
#define SCOPE_SHARDS 8

struct gojo_scope {
    gojo_spin lock;
    int cancelled;
    int reason;             // CANCEL_*, once cancelled
    int64_t deadline;       // task.timeout's, 0 for none
    gojo_scope *parent;
    gojo_scope *first_child, *next_sib, *prev_sib;
    struct { gojo_spin lock; gojo_g *head; } members[SCOPE_SHARDS];
    // task-local values: a binding made by `task.with`; lookups go up the tree
    // (through `shielded`, the scope a shield left)
    int64_t local_id;
    void *local_val;
    gojo_scope *shielded;
};

#define MAX_WORKERS 10000

static pthread_mutex_t s_lock = PTHREAD_MUTEX_INITIALIZER;
static gojo_g *s_gq_head, *s_gq_tail;
static int64_t s_gq_size;
static gojo_worker *s_idle;
static int s_nidle, s_nstarted, s_nspinning, s_nreleased, s_nprocs;
int gojo_preempt_flag;   // outstanding requests to yield; polled by generated code
static int64_t s_ngoroutines;
static gojo_worker **s_workers;
static gojo_g *s_free_gs;
static int64_t s_nfree_global;
static int64_t s_timers_pending, s_io_waiters;
static size_t s_stack_size = 2 << 20;
static const size_t s_main_stack_size = (size_t)1 << 30;
static const size_t s_guard_size = 256 << 10;

// Goroutines are addressed by index in poller events; slots live forever.
#define G_CHUNK_BITS 12
#define G_CHUNK (1u << G_CHUNK_BITS)
#define G_MAX_CHUNKS (1u << 18)
static gojo_g **s_g_chunks;
static uint32_t s_g_count;
static gojo_spin s_g_lock;

static __thread gojo_worker *t_worker;
static __thread uint8_t *t_alt_stack;

// Not inlined, and opaque to the optimizer: see the note about thread-locals above.
__attribute__((noinline)) static gojo_worker *cur_worker(void) {
    __asm__ volatile("" : : : "memory");
    return t_worker;
}

static inline gojo_g *cur_g(void) { return cur_worker()->cur; }

// When g was first seen parked (at the latest `now`).
static inline int64_t blocked_since(gojo_g *g, int64_t now) {
    int64_t t = GOJO_RELAXED(&g->blocked_since);
    if (t == 0) {
        int64_t z = 0;
        __atomic_compare_exchange_n(&g->blocked_since, &z, now, false, __ATOMIC_RELAXED, __ATOMIC_RELAXED);
        t = GOJO_RELAXED(&g->blocked_since);
    }
    return t;
}

static _Noreturn void deadlock(void);
static void on_quit(int sig);
static void sim_push(gojo_g *g);
static gojo_g *sim_find_work(gojo_worker *w);
static uint64_t sim_rand(void);
void gojo_sim_setup(uint64_t seed);

static void scope_cancel_r(gojo_scope *s, int reason);

// ------------------------------------------------------------------ hibernation
//
// Stacks can't move (tasks borrow from each other's stacks), but a stack can
// be put away and brought back at the same address: the bytes in use are
// copied to the heap and the pages released; when the goroutine is about to
// run, or someone has to write into its stack (a channel handing it a value),
// the pages come back and the bytes are copied back. The address doesn't
// change, so every pointer into the stack stays valid. Only goroutines parked
// for a while, among many, and that no task may be borrowing from (none of the
// scopes they opened has a task running), are put away.

static gojo_g *g_slot(uint32_t idx);
static bool g_borrowed(gojo_g *g);
static int64_t s_drop_grace = 1000000000;   // GOJO_DROP_GRACE (ms)
static bool s_hib_on;
static int64_t s_nhib;                      // stacks put away now
static int64_t s_hib_after = 200000000;     // parked this long (ns)
#define HIB_MIN_GOROUTINES 256              // only worth it with many

static void stack_release(uint8_t *lo, uint8_t *hi) {
    if (hi <= lo) return;
#if defined(__APPLE__)
    madvise(lo, (size_t)(hi - lo), MADV_FREE_REUSABLE);
#else
    madvise(lo, (size_t)(hi - lo), MADV_DONTNEED);
#endif
}

// With g->hib_lock held.
static void thaw_locked(gojo_g *g) {
    if (!g->hib) return;
    uint8_t *top = g->stack + g->stack_size;
    uint8_t *sp = top - g->hib_size;
#if defined(__APPLE__)
    size_t ps = (size_t)getpagesize();
    uint8_t *lo = (uint8_t *)((uintptr_t)sp & ~(uintptr_t)(ps - 1));
    madvise(lo, (size_t)(top - lo), MADV_FREE_REUSE);
#endif
    memcpy(sp, g->hib_buf, g->hib_size);
    free(g->hib_buf);
    g->hib_buf = NULL;
    GOJO_SC_STORE(&g->hib, 0);
    GOJO_ADD(&s_nhib, -1);
}

// Brings g's stack back if it is put away.
static void g_touch(gojo_g *g) {
    if (!s_hib_on) return;
    spin_lock(&g->hib_lock);
    thaw_locked(g);
    spin_unlock(&g->hib_lock);
}

// Around writing into, or reading from, a parked goroutine's stack. The
// hibernator gives up on a goroutine that is being touched (it stores `hib`
// then loads `touching`; this stores `touching` then loads `hib`: one of them
// sees the other).
static inline void g_touch_begin(gojo_g *g) {
    if (!s_hib_on) return;
    GOJO_SC_STORE(&g->touching, 1);
    if (GOJO_SC_LOAD(&g->hib)) g_touch(g);
}
static inline void g_touch_end(gojo_g *g) {
    if (s_hib_on) GOJO_STORE(&g->touching, 0);
}

// Puts g's stack away if it is parked and nobody may be using it. Runs on the
// monitor thread.
static bool hibernate(gojo_g *g, int64_t now) {
    if (g->hib || g->stack_size != s_stack_size || !GOJO_RELAXED(&g->alive)) return false;
    int b = GOJO_RELAXED(&g->blocked);
    if (b == BLK_NONE || b == BLK_SCOPE || now - blocked_since(g, now) < s_hib_after) return false;
    if (!spin_trylock(&g->hib_lock)) return false;
    bool done = false;
    // still parked (a wait that was claimed means it is about to run)
    bool waiting = b == BLK_LOCK || b == BLK_RWLOCK || b == BLK_JOIN || (GOJO_SC_LOAD(&g->wait) & 3) == WAIT_CANCELLABLE;
    // (set before its wait starts, cleared after it ends)
    if (GOJO_SC_LOAD(&g->io_inflight)) waiting = false;
    if (!g->hib && waiting && GOJO_RELAXED(&g->blocked) == b) {
        GOJO_SC_STORE(&g->hib, 1);
        // now it can't run (a worker that picks it waits for this lock), so its
        // open scopes stay as they are: do their tasks borrow from this stack?
        bool busy = GOJO_SC_LOAD(&g->on_cpu) || GOJO_SC_LOAD(&g->touching);
        bool borrowed = !busy && g_borrowed(g);
        // (after the borrow check: a task that pinned this stack stored that
        // before it finished, and finishing is what made the stack free)
        bool pinned = !busy && !borrowed && GOJO_SC_LOAD(&g->pinned);
        if (busy || borrowed || pinned) {
            // a worker is about to run it, its tasks may be reading its stack,
            // or C may hold a pointer into it
            GOJO_SC_STORE(&g->hib, 0);
        } else {
            uint8_t *top = g->stack + g->stack_size;
            uint8_t *sp = (uint8_t *)g->sp;
            size_t used = (size_t)(top - sp);
            g->hib_buf = malloc(used);
            if (!g->hib_buf) {
                GOJO_SC_STORE(&g->hib, 0);
            } else {
                memcpy(g->hib_buf, sp, used);
                g->hib_size = used;
                stack_release(g->stack, top);
                GOJO_ADD(&s_nhib, 1);
                done = true;
            }
        }
    }
    spin_unlock(&g->hib_lock);
    return done;
}

static void hibernate_parked(int64_t now) {
    static int64_t last;
    int64_t every = s_hib_after > 10000000 ? s_hib_after : 10000000;
    if (!s_hib_on || now - last < every) return;
    last = now;
    if (GOJO_RELAXED(&s_ngoroutines) < HIB_MIN_GOROUTINES) return;
    // at most 20 ms of this thread per round
    uint32_t n = GOJO_LOAD(&s_g_count);
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    int64_t t0 = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
    static uint32_t next;
    if (next >= n) next = 0;
    int k = 0;
    for (uint32_t c = 0; c < n; c++) {
        uint32_t i = (next + c) % n;
        if (hibernate(g_slot(i), now) && ++k % 256 == 0) {
            clock_gettime(CLOCK_MONOTONIC, &ts);
            if ((int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec - t0 > 20000000) {
                next = i + 1;
                return;
            }
        }
    }
}

static int s_poller_started;

// Ctrl-C / SIGTERM: cancel `main`'s scope; a second one ends the program.
static gojo_scope *s_root_scope;
static gojo_spin s_root_lock;
static volatile sig_atomic_t s_shutdown;
static volatile sig_atomic_t s_shutdown_sig;
static void on_shutdown(int sig) {
    signal(sig, SIG_DFL);
    // nothing runs that could wind the program down (no task, timer or
    // network wait has started the poller): end it as the signal would
    if (!GOJO_LOAD(&s_poller_started)) raise(sig);
    s_shutdown_sig = sig;
    s_shutdown = 1;
}
static void check_shutdown(void) {
    if (s_shutdown != 1) return;
    spin_lock(&s_root_lock);
    gojo_scope *root = s_root_scope;
    if (root) {
        s_shutdown = 2;
        scope_cancel_r(root, CANCEL_SHUTDOWN);
    }
    spin_unlock(&s_root_lock);
    // no scope to cancel (`main` starts no tasks itself): end as the signal would
    if (!root) raise(s_shutdown_sig);
}

// simulation state (see "simulation")
static uint64_t s_sim_rng;
static gojo_g **s_simq;
static int s_simq_len, s_simq_cap;
static int s_sim_budget = 1;
static bool s_sim_sleeping;

// A scheduling point: maybe let another task run.
static inline void sim_point(void) {
    if (!gojo_sim_on) return;
    gojo_worker *w = cur_worker();
    if (!w || !w->cur) return;
    if (sim_rand() % 4 == 0) gojo_task_yieldNow();
}


static void wake_one(void);
static void ensure_poller(void);
static void poller_wake(void);
static void worker_setup_signals(void);

// ------------------------------------------------------------------ run queues

static uint64_t next_rand(gojo_worker *w) {
    if (gojo_sim_on) return sim_rand();
    uint64_t x = w->rng;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    return w->rng = x;
}

static void global_put_locked(gojo_g *g) {
    g->qnext = NULL;
    if (s_gq_tail) s_gq_tail->qnext = g;
    else s_gq_head = g;
    s_gq_tail = g;
    GOJO_SET_RELAXED(&s_gq_size, s_gq_size + 1);
}

// Goroutines woken by timers and the network go first: they have waited already.
static void global_put_front(gojo_g *g) {
    if (gojo_sim_on) {
        sim_push(g);
        return;
    }
    pthread_mutex_lock(&s_lock);
    g->qnext = s_gq_head;
    s_gq_head = g;
    if (!s_gq_tail) s_gq_tail = g;
    GOJO_SET_RELAXED(&s_gq_size, s_gq_size + 1);
    pthread_mutex_unlock(&s_lock);
}

static void global_put(gojo_g *g) {
    if (gojo_sim_on) {
        sim_push(g);
        return;
    }
    pthread_mutex_lock(&s_lock);
    global_put_locked(g);
    pthread_mutex_unlock(&s_lock);
}

// Moves half of a full local queue, plus g, to the global queue.
static bool runq_put_slow(gojo_worker *w, gojo_g *g, uint32_t h, uint32_t t) {
    uint32_t n = (t - h) / 2;
    gojo_g *batch[RUNQ_SIZE / 2 + 1];
    for (uint32_t i = 0; i < n; i++) batch[i] = GOJO_RELAXED(&w->rq[(h + i) % RUNQ_SIZE]);
    if (!__atomic_compare_exchange_n(&w->rq_head, &h, h + n, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return false;
    batch[n] = g;
    pthread_mutex_lock(&s_lock);
    for (uint32_t i = 0; i <= n; i++) global_put_locked(batch[i]);
    pthread_mutex_unlock(&s_lock);
    return true;
}

static void runq_put(gojo_worker *w, gojo_g *g, bool next) {
    if (gojo_sim_on) {
        sim_push(g);
        return;
    }
    if (next) {
        gojo_g *old = __atomic_exchange_n(&w->runnext, g, __ATOMIC_ACQ_REL);
        if (!old) return;
        g = old;
    }
    for (;;) {
        uint32_t h = GOJO_LOAD(&w->rq_head);
        uint32_t t = GOJO_RELAXED(&w->rq_tail);
        if (t - h < RUNQ_SIZE) {
            GOJO_SET_RELAXED(&w->rq[t % RUNQ_SIZE], g);
            GOJO_STORE(&w->rq_tail, t + 1);
            return;
        }
        if (runq_put_slow(w, g, h, t)) return;
    }
}

static gojo_g *runq_get(gojo_worker *w) {
    gojo_g *next = GOJO_LOAD(&w->runnext);
    while (next) {
        if (__atomic_compare_exchange_n(&w->runnext, &next, NULL, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) return next;
    }
    for (;;) {
        uint32_t h = GOJO_LOAD(&w->rq_head);
        uint32_t t = GOJO_LOAD(&w->rq_tail);
        if (t == h) return NULL;
        gojo_g *g = GOJO_RELAXED(&w->rq[h % RUNQ_SIZE]);
        if (__atomic_compare_exchange_n(&w->rq_head, &h, h + 1, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return g;
    }
}

static inline bool runq_empty(gojo_worker *w) {
    return GOJO_LOAD(&w->rq_head) == GOJO_LOAD(&w->rq_tail) && !GOJO_LOAD(&w->runnext);
}

static uint32_t runq_grab(gojo_worker *v, gojo_g **batch, bool steal_next) {
    for (;;) {
        uint32_t h = GOJO_LOAD(&v->rq_head);
        uint32_t t = GOJO_LOAD(&v->rq_tail);
        uint32_t n = t - h;
        n = n - n / 2;
        if (n == 0) {
            if (steal_next) {
                gojo_g *next = GOJO_LOAD(&v->runnext);
                if (next) {
                    // give the owner a moment: it is probably about to run it
                    if (GOJO_RELAXED(&v->cur))
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
        for (uint32_t i = 0; i < n; i++) batch[i] = GOJO_RELAXED(&v->rq[(h + i) % RUNQ_SIZE]);
        if (__atomic_compare_exchange_n(&v->rq_head, &h, h + n, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return n;
    }
}

static gojo_g *runq_steal(gojo_worker *w, gojo_worker *v, bool steal_next) {
    gojo_g *batch[RUNQ_SIZE / 2];
    uint32_t n = runq_grab(v, batch, steal_next);
    if (n == 0) return NULL;
    gojo_g *g = batch[--n];
    if (n == 0) return g;
    uint32_t t = GOJO_RELAXED(&w->rq_tail);
    for (uint32_t i = 0; i < n; i++) GOJO_SET_RELAXED(&w->rq[(t + i) % RUNQ_SIZE], batch[i]);
    GOJO_STORE(&w->rq_tail, t + n);
    return g;
}

static gojo_g *global_get_locked(gojo_worker *w) {
    int64_t size = s_gq_size;
    if (size == 0) return NULL;
    int64_t n = size / s_nprocs + 1;
    if (n > size) n = size;
    if (n > RUNQ_SIZE / 2) n = RUNQ_SIZE / 2;
    // only what fits: overflowing the local queue would need s_lock again
    uint32_t h = GOJO_LOAD(&w->rq_head);
    uint32_t t = GOJO_RELAXED(&w->rq_tail);
    int64_t room = RUNQ_SIZE - (int64_t)(t - h);
    if (n > room + 1) n = room + 1;
    gojo_g *first = NULL;
    for (int64_t i = 0; i < n; i++) {
        gojo_g *g = s_gq_head;
        s_gq_head = g->qnext;
        if (!s_gq_head) s_gq_tail = NULL;
        g->qnext = NULL;
        if (!first) {
            first = g;
        } else {
            GOJO_SET_RELAXED(&w->rq[t % RUNQ_SIZE], g);
            t++;
        }
    }
    GOJO_STORE(&w->rq_tail, t);
    GOJO_SET_RELAXED(&s_gq_size, size - n);
    return first;
}

static inline bool can_wake(void) {
    int active = GOJO_SC_LOAD(&s_nstarted) - GOJO_SC_LOAD(&s_nidle) - GOJO_SC_LOAD(&s_nreleased);
    return active < s_nprocs;
}

static inline void maybe_wake(void) {
    GOJO_FENCE();
    if (GOJO_SC_LOAD(&s_nspinning) == 0 && can_wake()) wake_one();
}

// Makes a parked goroutine runnable. Whoever calls this owns the wake-up.
static void g_ready(gojo_g *g) {
    if (gojo_sim_on) {
        sim_push(g);
        return;
    }
    gojo_worker *w = cur_worker();
    if (w) runq_put(w, g, true);
    else global_put_front(g);
    maybe_wake();
}

// ------------------------------------------------------------------ goroutine stacks

static gojo_g *g_slot(uint32_t idx) { return &s_g_chunks[idx >> G_CHUNK_BITS][idx & (G_CHUNK - 1)]; }

static gojo_g *g_new_slot(void) {
    spin_lock(&s_g_lock);
    uint32_t idx = s_g_count;
    if ((idx >> G_CHUNK_BITS) >= G_MAX_CHUNKS) gojo_panic("too many goroutines", "<runtime>");
    if ((idx & (G_CHUNK - 1)) == 0) {
        gojo_g *chunk = calloc(G_CHUNK, sizeof(gojo_g));
        if (!chunk) gojo_panic("out of memory", "<runtime>");
        __atomic_store_n(&s_g_chunks[idx >> G_CHUNK_BITS], chunk, __ATOMIC_RELEASE);
    }
    gojo_g *g = g_slot(idx);
    g->idx = idx;
    __atomic_store_n(&s_g_count, idx + 1, __ATOMIC_RELEASE);
    spin_unlock(&s_g_lock);
    return g;
}

// TypeSanitizer's mmap interceptor clears the shadow of the whole mapping,
// eight bytes of it per byte mapped, so every 2 MB goroutine stack would cost
// 16 MB of memory at once (and the main goroutine's 1 GB stack 8 GB). A stack
// holds only frames, whose types TypeSanitizer resets as they are entered, so
// under it stacks are mapped with the system call itself.
static void *stack_mmap(size_t len, int flags) {
#if defined(GOJO_TYSAN) && defined(__linux__)
    return (void *)syscall(SYS_mmap, NULL, len, PROT_READ | PROT_WRITE, flags, -1, 0);
#elif defined(GOJO_TYSAN) && defined(__APPLE__)
    // zero-filled and lazily backed, like the anonymous mapping
    (void)flags;
    mach_vm_address_t a = 0;
    if (mach_vm_allocate(mach_task_self(), &a, len, VM_FLAGS_ANYWHERE) != KERN_SUCCESS) return MAP_FAILED;
    return (void *)(uintptr_t)a;
#else
    return mmap(NULL, len, PROT_READ | PROT_WRITE, flags, -1, 0);
#endif
}

static void g_map_stack(gojo_g *g, size_t size) {
    size_t total = size + s_guard_size;
    int flags = MAP_PRIVATE | MAP_ANON;
#ifdef MAP_NORESERVE
    flags |= MAP_NORESERVE;
#endif
#ifdef MAP_STACK
    flags |= MAP_STACK;
#endif
    uint8_t *m = stack_mmap(total, flags);
    if (m == MAP_FAILED) gojo_panic("cannot allocate a goroutine stack: out of address space or memory (set GOJO_GOROUTINE_STACK to use smaller stacks)", "<runtime>");
#if defined(__linux__)
    // Linux 6.13+: a guard region that doesn't split the mapping, so stacks next
    // to each other merge into one mapping instead of two per goroutine
    static int guard_install = 1;
    if (GOJO_RELAXED(&guard_install)) {
        if (madvise(m, s_guard_size, 102 /* MADV_GUARD_INSTALL */) == 0) goto guarded;
        GOJO_SET_RELAXED(&guard_install, 0);
    }
#endif
    if (mprotect(m, s_guard_size, PROT_NONE) != 0) {
        gojo_panic("cannot protect a goroutine stack: too many memory mappings (on Linux before 6.13, raise vm.max_map_count; each goroutine needs two)", "<runtime>");
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

static void g_prepare(gojo_g *g) {
    uint8_t *top = g->stack + g->stack_size;
    top = (uint8_t *)((uintptr_t)top & ~(uintptr_t)15);
#if defined(__aarch64__)
    uint8_t *sp = top - GOJO_CTX_FRAME;
    memset(sp, 0, GOJO_CTX_FRAME);
    ((void **)sp)[GOJO_CTX_RET_SLOT] = (void *)g_entry;
#else
    // on entry to g_entry: (rsp + 8) must be 16-byte aligned
    uint8_t *sp = top - 8 - GOJO_CTX_FRAME;
    memset(sp, 0, GOJO_CTX_FRAME);
    ((void **)sp)[GOJO_CTX_RET_SLOT] = (void *)g_entry;
    unsigned int mxcsr = 0;
    unsigned short fcw = 0;
    __asm__ volatile("stmxcsr %0" : "=m"(mxcsr));
    __asm__ volatile("fnstcw %0" : "=m"(fcw));
    *(unsigned int *)sp = mxcsr;
    *(unsigned short *)(sp + 4) = fcw;
#endif
    g->sp = sp;
}

static gojo_g *g_alloc(gojo_worker *w, size_t size) {
    gojo_g *g = NULL;
    if (size == s_stack_size) {
        g = w->free_gs;
        if (g) {
            w->free_gs = g->qnext;
            w->nfree--;
        } else if (__atomic_load_n(&s_free_gs, __ATOMIC_RELAXED)) {
            pthread_mutex_lock(&s_lock);
            for (int i = 0; i < 128 && s_free_gs; i++) {
                gojo_g *f = s_free_gs;
                __atomic_store_n(&s_free_gs, f->qnext, __ATOMIC_RELAXED);
                GOJO_SET_RELAXED(&s_nfree_global, s_nfree_global - 1);
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
#ifdef GOJO_TSAN
        g->tsan_fiber = NULL;
#endif
        g_map_stack(g, size);
    }
    // (a finished goroutine left blocked, nheld, open_groups, hib and
    // scope_linked at their zero values; a new slot is zeroed)
    g->qnext = NULL;
    g->arg_drop = NULL;
    g->scope = NULL;
    g->group = NULL;
    GOJO_SET_RELAXED(&g->pinned, 0);
    GOJO_SET_RELAXED(&g->alive, true);
    g->spawn_loc = NULL;
    g->wake = WAKE_NONE;
    g->wake_data = NULL;
    GOJO_SET_RELAXED(&g->on_cpu, 0);
    GOJO_SET_RELAXED(&g->wait, (uint64_t)g->epoch << 2);
    g_prepare(g);
#ifdef GOJO_TSAN
    if (!g->tsan_fiber) g->tsan_fiber = __tsan_create_fiber(0);
#endif
    return g;
}

// Runs on the scheduler stack, after the goroutine has switched away for good.
static void g_recycle(gojo_worker *w, gojo_g *g) {
    GOJO_SET_RELAXED(&g->alive, false);
#ifdef GOJO_ASAN
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
        gojo_g *keep = w->free_gs;
        for (int i = 1; i < 256; i++) keep = keep->qnext;
        gojo_g *rest = keep->qnext;
        keep->qnext = NULL;
        int moved = w->nfree - 256;
        w->nfree = 256;
        gojo_g *last = rest;
        bool trim = GOJO_RELAXED(&s_nfree_global) > 4096;
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
        GOJO_SET_RELAXED(&s_nfree_global, s_nfree_global + moved);
        pthread_mutex_unlock(&s_lock);
    }
}

// ------------------------------------------------------------------ switching

static inline void switch_to_g(gojo_worker *w, gojo_g *g) {
#ifdef GOJO_ASAN
    __sanitizer_start_switch_fiber(&w->fake_stack, g->stack, g->stack_size);
#endif
#ifdef GOJO_TSAN
    __tsan_switch_to_fiber(g->tsan_fiber, 0);
#endif
    gojo_ctx_switch(&w->sched_sp, g->sp);
#ifdef GOJO_ASAN
    __sanitizer_finish_switch_fiber(w->fake_stack, NULL, NULL);
#endif
}

// Back to the scheduler. On return the goroutine may be on another worker.
static inline void switch_to_sched(gojo_g *g, gojo_worker *w, bool dying) {
#ifdef GOJO_ASAN
    __sanitizer_start_switch_fiber(dying ? NULL : &g->fake_stack, w->stack_bottom, w->stack_size);
#endif
#ifdef GOJO_TSAN
    __tsan_switch_to_fiber(w->tsan_fiber, 0);
#endif
    gojo_ctx_switch(&g->sp, w->sched_sp);
#ifdef GOJO_ASAN
    __sanitizer_finish_switch_fiber(g->fake_stack, NULL, NULL);
#endif
}

// The caller has arranged for someone to g_ready() this goroutine.
static void g_park(void) {
    gojo_worker *w = cur_worker();
    switch_to_sched(w->cur, w, false);
}

// ------------------------------------------------------------------ waits and cancellation

static inline void scope_link(gojo_g *g);

static inline uint64_t wait_begin(gojo_g *g) {
    scope_link(g);
    g->epoch++;
    uint64_t t = ((uint64_t)g->epoch << 2) | WAIT_CANCELLABLE;
    g->wake = WAKE_NONE;
    g->wake_data = NULL;
    GOJO_SC_STORE(&g->wait, t);
    return t;
}

static inline bool wait_claim(gojo_g *g, uint64_t token) {
    uint64_t expected = token;
    return __atomic_compare_exchange_n(&g->wait, &expected, token & ~(uint64_t)3, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED);
}

static inline bool scope_cancelled(gojo_scope *s) { return s && GOJO_SC_LOAD(&s->cancelled); }

// What a parked goroutine waits for, for task dumps and deadlock reports.
// (read by the monitor thread: relaxed atomics)
// `blocked_since` is stamped by whoever first looks (the monitor, a dump):
// parking doesn't read the clock.
static inline void blk(gojo_g *g, int kind, const void *on) {
    GOJO_SET_RELAXED(&g->slow_noted, false);
    GOJO_SET_RELAXED(&g->blocked_on, on);
    GOJO_SET_RELAXED(&g->blocked_since, 0);
    GOJO_SC_STORE(&g->blocked, kind);
}


static inline void unblk(gojo_g *g) { GOJO_SC_STORE(&g->blocked, BLK_NONE); }

// Parks unless the task is already cancelled. Afterwards g->wake says why it woke.
static void wait_park(gojo_g *g, uint64_t token) {
    if (scope_cancelled(g->scope) && wait_claim(g, token)) {
        g->wake = WAKE_CANCEL;
        return;
    }
    g_park();
}

static void scope_cancel(gojo_scope *s) { scope_cancel_r(s, CANCEL_EXPLICIT); }

static void scope_cancel_r(gojo_scope *s, int reason) {
    spin_lock(&s->lock);
    if (GOJO_RELAXED(&s->cancelled)) {
        spin_unlock(&s->lock);
        return;
    }
    s->reason = reason;
    GOJO_SC_STORE(&s->cancelled, 1);
    for (int k = 0; k < SCOPE_SHARDS; k++) {
        spin_lock(&s->members[k].lock);
        for (gojo_g *g = s->members[k].head; g; g = g->scope_next) {
            uint64_t w = GOJO_SC_LOAD(&g->wait);
            if ((w & 3) == WAIT_CANCELLABLE && wait_claim(g, w)) {
                g->wake = WAKE_CANCEL;
                g_ready(g);
            }
        }
        spin_unlock(&s->members[k].lock);
    }
    for (gojo_scope *c = s->first_child; c; c = c->next_sib) scope_cancel_r(c, reason);
    spin_unlock(&s->lock);
}

// A goroutine belongs to a scope from the start, but is listed in its
// members (which cancellation walks to wake waiters) only once it first waits:
// most short tasks never wait, and then cost the scope's lock nothing.
static void scope_join(gojo_scope *s, gojo_g *g) {
    g->scope = s;
    g->scope_linked = false;
}

// Before a cancellable wait (from wait_begin): be findable by scope_cancel.
static inline void scope_link(gojo_g *g) {
    gojo_scope *s = g->scope;
    if (g->scope_linked || !s) return;
    gojo_worker *w = cur_worker();
    int k = w ? (int)(w->id % SCOPE_SHARDS) : 0;
    spin_lock(&s->members[k].lock);
    g->scope_prev = NULL;
    g->scope_next = s->members[k].head;
    if (g->scope_next) g->scope_next->scope_prev = g;
    s->members[k].head = g;
    g->scope_shard = k;
    g->scope_linked = true;
    spin_unlock(&s->members[k].lock);
}

static void scope_leave(gojo_g *g) {
    gojo_scope *s = g->scope;
    g->scope = NULL;
    if (!s || !g->scope_linked) return;
    g->scope_linked = false;
    int k = g->scope_shard;
    spin_lock(&s->members[k].lock);
    if (g->scope_prev) g->scope_prev->scope_next = g->scope_next;
    else s->members[k].head = g->scope_next;
    if (g->scope_next) g->scope_next->scope_prev = g->scope_prev;
    spin_unlock(&s->members[k].lock);
}

// A new scope under the goroutine's current one; the goroutine moves into it.
static void scope_enter(gojo_scope *s, gojo_g *g) {
    gojo_scope *p = g->scope;
    memset(s, 0, sizeof *s);
    s->parent = p;
    if (p) {
        spin_lock(&p->lock);
        s->cancelled = p->cancelled;
        s->reason = p->reason;
        s->next_sib = p->first_child;
        if (p->first_child) p->first_child->prev_sib = s;
        p->first_child = s;
        spin_unlock(&p->lock);
    }
    scope_leave(g);
    scope_join(s, g);
}

// The goroutine goes back to the parent scope; s stays in the tree.
static void scope_step_out(gojo_scope *s, gojo_g *g) {
    scope_leave(g);
    scope_join(s->parent, g);
}

static void scope_unlink(gojo_scope *s) {
    gojo_scope *p = s->parent;
    if (p) {
        spin_lock(&p->lock);
        if (s->prev_sib) s->prev_sib->next_sib = s->next_sib;
        else p->first_child = s->next_sib;
        if (s->next_sib) s->next_sib->prev_sib = s->prev_sib;
        spin_unlock(&p->lock);
    }
}

static void scope_exit(gojo_scope *s, gojo_g *g) {
    scope_step_out(s, g);
    scope_unlink(s);
}

bool gojo_task___cancelled(void) { return scope_cancelled(cur_g()->scope); }

void gojo_task_yieldNow(void) {
    gojo_worker *w = cur_worker();
    w->post = POST_YIELD;
    switch_to_sched(w->cur, w, false);
}

// ------------------------------------------------------------------ the scheduler

static void worker_park(gojo_worker *w) {
    // hand the queue over: an idle worker has no business holding work
    gojo_g *g;
    while ((g = runq_get(w))) global_put(g);
    pthread_mutex_lock(&s_lock);
    w->idle_next = s_idle;
    s_idle = w;
    GOJO_ADD(&s_nidle, 1);
    if (s_nidle == s_nstarted && s_gq_size == 0 && GOJO_SC_LOAD(&s_timers_pending) == 0 && GOJO_SC_LOAD(&s_io_waiters) == 0) {
        deadlock();
    }
    while (!w->notified) pthread_cond_wait(&w->cv, &s_lock);
    w->notified = false;
    pthread_mutex_unlock(&s_lock);
}

// Wakes an idle worker, or starts a new one, as a spinning worker.
static void wake_one(void) {
    if (gojo_sim_on) return;
    pthread_mutex_lock(&s_lock);
    if (!can_wake() || GOJO_SC_LOAD(&s_nspinning) > 0) {
        pthread_mutex_unlock(&s_lock);
        return;
    }
    gojo_worker *w = s_idle;
    if (w) {
        s_idle = w->idle_next;
        GOJO_ADD(&s_nidle, -1);
        GOJO_ADD(&s_nspinning, 1);
        w->spinning = true;
        w->notified = true;
        pthread_cond_signal(&w->cv);
        pthread_mutex_unlock(&s_lock);
        return;
    }
    extern void gojo_start_worker(void);
    if (GOJO_RELAXED(&s_nstarted) < MAX_WORKERS) gojo_start_worker();
    pthread_mutex_unlock(&s_lock);
}

static inline void stop_spinning(gojo_worker *w) {
    if (!w->spinning) return;
    w->spinning = false;
    // this worker found work; someone else should look for more
    if (GOJO_ADD(&s_nspinning, -1) == 0) maybe_wake();
}

// The worker's goroutine is in C: let another worker have its CPU slot.
static void worker_release(gojo_worker *w) {
    // simulated: one thread, blocking calls block it
    if (gojo_sim_on) return;
    int expected = 0;
    if (!__atomic_compare_exchange_n(&w->released, &expected, 1, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED)) return;
    GOJO_ADD(&s_nreleased, 1);
    maybe_wake();
}

static bool worker_reclaim(gojo_worker *w) {
    int expected = 1;
    if (!__atomic_compare_exchange_n(&w->released, &expected, 0, false, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED)) return false;
    GOJO_ADD(&s_nreleased, -1);
    return true;
}

static void cancel_preempt(gojo_worker *w) {
    if (__atomic_exchange_n(&w->preempt, 0, __ATOMIC_ACQ_REL)) GOJO_ADD(&gojo_preempt_flag, -1);
}

// Generated code calls this when gojo_preempt_flag is set.
void gojo_preempt_point(void) {
    if (gojo_sim_on) {
        // a random preemption every few thousand checkpoints
        if (--s_sim_budget <= 0) {
            s_sim_budget = 1 + (int)(sim_rand() % 4096);
            gojo_worker *sw = cur_worker();
            if (sw && sw->cur) gojo_task_yieldNow();
        }
        return;
    }
    gojo_worker *w = cur_worker();
    if (!w || !__atomic_exchange_n(&w->preempt, 0, __ATOMIC_ACQ_REL)) return;
    GOJO_ADD(&gojo_preempt_flag, -1);
    gojo_task_yieldNow();
}

// Around C calls that might take a while. Nothing switches goroutines in
// between, so enter and exit run on the same worker.
void gojo_ffi_enter(void) {
    gojo_worker *w = cur_worker();
    if (!w) return;
    GOJO_SET_RELAXED(&w->sysseq, w->sysseq + 1);
    GOJO_SC_STORE(&w->insys, 1);
}

void gojo_ffi_exit(void) {
    gojo_worker *w = cur_worker();
    if (!w) return;
    GOJO_SC_STORE(&w->insys, 0);
    if (GOJO_SC_LOAD(&w->released) && worker_reclaim(w)) {
        // more workers than CPUs running Gojo code now: get back in line
        int active = GOJO_SC_LOAD(&s_nstarted) - GOJO_SC_LOAD(&s_nidle) - GOJO_SC_LOAD(&s_nreleased);
        if (active > s_nprocs) gojo_task_yieldNow();
    }
}

// For calls known to block (file I/O): hand the slot over right away.
void gojo_blocking_enter(void) {
    gojo_ffi_enter();
    gojo_worker *w = cur_worker();
    if (w) worker_release(w);
}

static gojo_g *find_work(gojo_worker *w) {
    if (gojo_sim_on) return sim_find_work(w);
    for (;;) {
        // back in the scheduler: the worker takes its CPU slot again (or parks
        // below if all are taken)
        worker_reclaim(w);
        int active = GOJO_SC_LOAD(&s_nstarted) - GOJO_SC_LOAD(&s_nidle) - GOJO_SC_LOAD(&s_nreleased);
        if (active > s_nprocs && !w->spinning) {
            worker_park(w);
            continue;
        }
        // fairness: now and then, and right after a goroutine was asked to yield,
        // look at the global queue first; woken goroutines wait at its front
        bool global_first = w->yielded || ++w->schedtick % 61 == 0;
        w->yielded = false;
        if (global_first && GOJO_RELAXED(&s_gq_size) > 0) {
            pthread_mutex_lock(&s_lock);
            gojo_g *g = global_get_locked(w);
            pthread_mutex_unlock(&s_lock);
            if (g) return g;
        }
        gojo_g *g = runq_get(w);
        if (g) return g;
        if (GOJO_RELAXED(&s_gq_size) > 0) {
            pthread_mutex_lock(&s_lock);
            g = global_get_locked(w);
            pthread_mutex_unlock(&s_lock);
            if (g) return g;
        }
        int started = GOJO_LOAD(&s_nstarted);
        if (!w->spinning && 2 * GOJO_SC_LOAD(&s_nspinning) < started - GOJO_SC_LOAD(&s_nidle)) {
            w->spinning = true;
            GOJO_ADD(&s_nspinning, 1);
        }
        if (w->spinning) {
            for (int round = 0; round < 4; round++) {
                uint32_t start = (uint32_t)next_rand(w);
                for (int i = 0; i < started; i++) {
                    gojo_worker *v = GOJO_LOAD(&s_workers[(start + (uint32_t)i) % (uint32_t)started]);
                    if (!v || v == w) continue;
                    g = runq_steal(w, v, round == 3);
                    if (g) return g;
                }
            }
            w->spinning = false;
            GOJO_ADD(&s_nspinning, -1);
            // anything queued between our last look and dropping `spinning`?
            GOJO_FENCE();
            bool found = GOJO_RELAXED(&s_gq_size) > 0;
            for (int i = 0; i < started && !found; i++) {
                gojo_worker *v = GOJO_LOAD(&s_workers[i]);
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

static void schedule_loop(gojo_worker *w) {
    for (;;) {
        gojo_g *g = find_work(w);
        stop_spinning(w);
        // the waker may have been faster than the goroutine's switch away from its stack
        while (GOJO_LOAD(&g->on_cpu)) cpu_relax();
        GOJO_SC_STORE(&g->on_cpu, 1);
        if (GOJO_SC_LOAD(&g->hib)) g_touch(g);
        GOJO_SET_RELAXED(&w->cur, g);
        GOJO_SET_RELAXED(&w->tick, w->tick + 1);
        switch_to_g(w, g);
        GOJO_SET_RELAXED(&w->cur, NULL);
        int post = w->post;
        w->post = POST_NONE;
        GOJO_STORE(&g->on_cpu, 0);
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
    gojo_worker *w = cur_worker();
#ifdef GOJO_ASAN
    __sanitizer_finish_switch_fiber(NULL, &w->stack_bottom, &w->stack_size);
#endif
    gojo_g *g = w->cur;
    g->fn(g->arg);
    w = cur_worker();
    g = w->cur;
    if (g->arg_drop) g->arg_drop(g->arg);
    scope_leave(g);
    gojo_group *grp = g->group;
    if (grp) {
        extern void gojo_group_task_done(gojo_group *);
        gojo_group_task_done(grp);
        w = cur_worker();
    }
    GOJO_ADD(&s_ngoroutines, -1);
    w->post = POST_DEAD;
    switch_to_sched(g, w, true);
    __builtin_unreachable();
}

static void worker_init(gojo_worker *w) {
    pthread_cond_init(&w->cv, NULL);
    t_worker = w;
    w->rng = ((uint64_t)w->id + 1) * 0x9E3779B97F4A7C15ULL ^ (uint64_t)gojo_time_now();
    if (!w->rng) w->rng = 1;
#ifdef GOJO_TSAN
    w->tsan_fiber = __tsan_get_current_fiber();
#endif
    worker_setup_signals();
    gojo_thread_init();
#if defined(__APPLE__)
    w->mthread = pthread_mach_thread_np(pthread_self());
    GOJO_STORE(&w->cpu_ok, 1);
#else
    if (pthread_getcpuclockid(pthread_self(), &w->cpuclock) == 0) GOJO_STORE(&w->cpu_ok, 1);
#endif
}

// CPU time the worker's thread has used, in ns; -1 if unknown. The monitor uses
// it to tell a goroutine that doesn't answer a preemption request apart from a
// thread the OS isn't running (when the machine is overloaded): only the first
// is helped by handing its slot to another thread.
static int64_t worker_cpu_ns(gojo_worker *w) {
    if (!GOJO_LOAD(&w->cpu_ok)) return -1;
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
    gojo_worker *w = p;
    worker_init(w);
    schedule_loop(w);
    return NULL;
}

// Starts a detached runtime thread with a small stack. glibc carves static TLS
// out of the requested size and fails with EINVAL when it doesn't fit (zig's
// debug builds, for one, link a UBSan runtime with 256 KB of TLS, and C
// libraries can bring more), so the size doubles until the thread starts.
static bool start_thread(void *(*fn)(void *), void *arg) {
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    int r = EINVAL;
    for (size_t size = 256 << 10; size <= (64u << 20) && r == EINVAL; size *= 2) {
        if (pthread_attr_setstacksize(&attr, size) != 0) continue;
        pthread_t t;
        r = pthread_create(&t, &attr, fn, arg);
    }
    pthread_attr_destroy(&attr);
    return r == 0;
}

// Called with s_lock held. The new worker starts out spinning.
void gojo_start_worker(void) {
    gojo_worker *w = calloc(1, sizeof *w);
    if (!w) gojo_panic("out of memory", "<runtime>");
    w->id = (uint32_t)GOJO_RELAXED(&s_nstarted);
    w->spinning = true;
    GOJO_ADD(&s_nspinning, 1);
    __atomic_store_n(&s_workers[w->id], w, __ATOMIC_RELEASE);
    GOJO_ADD(&s_nstarted, 1);
    if (!start_thread(worker_main, w)) gojo_panic("cannot start a worker thread", "<runtime>");
}

// ------------------------------------------------------------------ stack overflow

static void on_fault(int sig, siginfo_t *si, void *uc) {
    gojo_worker *w = t_worker;
    uint8_t *addr = si->si_addr;
    if (w && w->cur) {
        gojo_g *g = w->cur;
        if (addr >= g->mapping && addr < g->mapping + s_guard_size + (64 << 10)) {
            static const char msg[] = "panic: stack overflow\n    goroutine stacks are limited by GOJO_GOROUTINE_STACK (default 2 MB; the main goroutine has 1 GB)\n";
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
#ifndef GOJO_ASAN
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
#if !defined(GOJO_ASAN) && !defined(GOJO_TSAN)
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

typedef void (*gojo_entry_fn)(void);

static void main_thunk(void *p) {
    ((gojo_entry_fn)p)();
    fflush(stdout);
    fflush(stderr);
    // stopped by Ctrl-C (and finished cleanly)
    exit(s_shutdown ? 130 : 0);
}

_Noreturn void gojo_sched_main(void (*entry)(void)) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    s_nprocs = n > 0 ? (int)n : 1;
    const char *env = getenv("GOJO_MAXPROCS");
    if (env && atoi(env) > 0) s_nprocs = atoi(env);
    env = getenv("GOJO_SEED");
    if (env && *env && !gojo_sim_on) gojo_sim_setup(strtoull(env, NULL, 10));
    if (gojo_sim_on) s_nprocs = 1;
    env = getenv("GOJO_DROP_GRACE");
    if (env && *env) s_drop_grace = atoll(env) * 1000000;
    // stack hibernation; not under sanitizers, which track stacks themselves
#if !defined(GOJO_ASAN) && !defined(GOJO_TSAN)
    env = getenv("GOJO_HIBERNATE");
    s_hib_on = !gojo_sim_on && !(env && strcmp(env, "0") == 0);
    if (env && atoll(env) > 1) s_hib_after = atoll(env) * 1000000;
#endif
    signal(SIGQUIT, on_quit);
    if (!gojo_sim_on) {
        signal(SIGINT, on_shutdown);
        signal(SIGTERM, on_shutdown);
    }
    env = getenv("GOJO_GOROUTINE_STACK");
    if (env && atoll(env) >= (64 << 10)) s_stack_size = (size_t)atoll(env);
    size_t ps = (size_t)sysconf(_SC_PAGESIZE);
    s_stack_size = (s_stack_size + ps - 1) & ~(ps - 1);
#if defined(__x86_64__)
    xsave_setup();
#endif
    s_workers = calloc(MAX_WORKERS, sizeof *s_workers);
    s_g_chunks = calloc(G_MAX_CHUNKS, sizeof *s_g_chunks);
    if (!s_workers || !s_g_chunks) gojo_panic("out of memory", "<runtime>");
    install_fault_handler();
    gojo_worker *w = calloc(1, sizeof *w);
    if (!w) gojo_panic("out of memory", "<runtime>");
    s_workers[0] = w;
    s_nstarted = 1;
    worker_init(w);
    gojo_g *g = g_alloc(w, s_main_stack_size);
    g->fn = main_thunk;
    g->arg = (void *)entry;
    runq_put(w, g, false);
    schedule_loop(w);
    __builtin_unreachable();
}

void gojo_go(void (*fn)(void *), void *arg) {
    ensure_poller();
    GOJO_ADD(&s_ngoroutines, 1);
    gojo_g *g = g_alloc(cur_worker(), s_stack_size);
    g->fn = fn;
    g->arg = arg;
    g_ready(g);
}

// ------------------------------------------------------------------ waits on timers and tasks

static void timer_add(gojo_timer *t);
static void timer_remove(gojo_timer *t);

// A task started with a handle (see "task handles" below).
struct gojo_task {
    gojo_scope scope;
    gojo_spin lock;
    bool done, handle_gone;
    gojo_g *waiter;         // `join`
    gojo_g *sel_g;          // a `select` with an arm on this task
    uint64_t sel_token;
    const char *loc;        // where `go` started it
};

// ------------------------------------------------------------------ channels

struct gojo_chan {
    gojo_spin lock;
    bool closed;            // every sender is gone: receivers drain, then get none
    bool deaf;              // every receiver is gone: sends fail
    uint8_t *buf;
    size_t esize;
    int64_t cap;
    int64_t head, len;
    gojo_sudog *recv_head, *recv_tail;
    gojo_sudog *send_head, *send_tail;
    int64_t senders, receivers;  // atomic; the lock only for the last of either
    int64_t ends;           // senders + receivers: freed at zero
    const char *loc;        // where it was made
};

gojo_chan *gojo_chan_new(size_t esize, int64_t cap, const char *loc) {
    if (cap < 0) gojo_panic("negative channel capacity", "<runtime>");
    if (cap > (int64_t)(PTRDIFF_MAX / (esize ? esize : 1))) gojo_panic("channel capacity too large", "<runtime>");
    gojo_chan *c = gojo_alloc(sizeof *c);
    memset(c, 0, sizeof *c);
    c->esize = esize ? esize : 1;
    c->cap = cap;
    c->buf = cap ? gojo_alloc((size_t)cap * c->esize) : NULL;
    c->senders = 1;
    c->receivers = 1;
    c->ends = 2;
    c->loc = loc;
    return c;
}

void gojo_chan_retain_tx(gojo_chan *c) {
    GOJO_ADD(&c->ends, 1);
    GOJO_ADD(&c->senders, 1);
}

void gojo_chan_retain_rx(gojo_chan *c) {
    GOJO_ADD(&c->ends, 1);
    GOJO_ADD(&c->receivers, 1);
}

static void chan_free(gojo_chan *c, void (*drop_elem)(void *));
static void wake_list(gojo_g *wake);
static gojo_g *chan_fail_waiters(gojo_sudog **head, gojo_sudog **tail, gojo_g *wake);

// The last sender gone: the channel closes. Waiting receivers get none.
void gojo_chan_release_tx(gojo_chan *c, void (*drop_elem)(void *)) {
    if (!c) return;
    if (GOJO_ADD(&c->senders, -1) == 0) {
        spin_lock(&c->lock);
        c->closed = true;
        gojo_g *wake = chan_fail_waiters(&c->recv_head, &c->recv_tail, NULL);
        spin_unlock(&c->lock);
        wake_list(wake);
    }
    if (GOJO_ADD(&c->ends, -1) == 0) chan_free(c, drop_elem);
}

// The last receiver gone: nothing can be received any more. Buffered values
// are dropped; waiting senders fail, and drop their values themselves.
void gojo_chan_release_rx(gojo_chan *c, void (*drop_elem)(void *)) {
    if (!c) return;
    if (GOJO_ADD(&c->receivers, -1) == 0) {
        spin_lock(&c->lock);
        c->deaf = true;
        if (drop_elem)
            for (int64_t i = 0; i < c->len; i++) drop_elem(c->buf + ((c->head + i) % c->cap) * c->esize);
        c->len = 0;
        gojo_g *wake = chan_fail_waiters(&c->send_head, &c->send_tail, NULL);
        spin_unlock(&c->lock);
        wake_list(wake);
    }
    if (GOJO_ADD(&c->ends, -1) == 0) chan_free(c, drop_elem);
}

static void chan_free(gojo_chan *c, void (*drop_elem)(void *)) {
    if (drop_elem)
        for (int64_t i = 0; i < c->len; i++) drop_elem(c->buf + ((c->head + i) % c->cap) * c->esize);
    free(c->buf);
    free(c);
}

static void sq_push(gojo_sudog **head, gojo_sudog **tail, gojo_sudog *s) {
    s->next = NULL;
    s->prev = *tail;
    if (*tail) (*tail)->next = s;
    else *head = s;
    *tail = s;
    s->linked = true;
}

static void sq_remove(gojo_sudog **head, gojo_sudog **tail, gojo_sudog *s) {
    if (!s->linked) return;
    if (s->prev) s->prev->next = s->next;
    else *head = s->next;
    if (s->next) s->next->prev = s->prev;
    else *tail = s->prev;
    s->linked = false;
}

// Next waiter still in a position to be served; it stays parked until g_ready.
static gojo_sudog *sq_pop(gojo_sudog **head, gojo_sudog **tail) {
    for (;;) {
        gojo_sudog *s = *head;
        if (!s) return NULL;
        sq_remove(head, tail, s);
        gojo_g *g = s->g;
        if (wait_claim(g, s->token)) {
            g->wake = WAKE_CHAN;
            g->wake_data = s;
            return s;
        }
    }
}

static inline void *chan_slot(gojo_chan *c, int64_t i) { return c->buf + ((c->head + i) % c->cap) * c->esize; }

// Non-blocking attempts. c is locked; the lock is released unless it would
// block. 1: sent; 2: nobody can receive it (the caller keeps the value); 0: would block.
static int chan_try_send_locked(gojo_chan *c, const void *elem, const char *loc) {
    (void)loc;
    if (c->deaf) {
        spin_unlock(&c->lock);
        return 2;
    }
    gojo_sudog *r = sq_pop(&c->recv_head, &c->recv_tail);
    if (r) {
        g_touch_begin(r->g);
        memcpy(r->elem, elem, c->esize);
        g_touch_end(r->g);
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
static int chan_try_recv_locked(gojo_chan *c, void *out) {
    if (c->len > 0) {
        memcpy(out, chan_slot(c, 0), c->esize);
        c->head = (c->head + 1) % c->cap;
        c->len--;
        // a blocked sender can now use the free slot
        gojo_sudog *s = sq_pop(&c->send_head, &c->send_tail);
        if (s) {
            g_touch_begin(s->g);
            memcpy(chan_slot(c, c->len), s->elem, c->esize);
            g_touch_end(s->g);
            c->len++;
            s->ok = true;
            spin_unlock(&c->lock);
            g_ready(s->g);
            return 1;
        }
        spin_unlock(&c->lock);
        return 1;
    }
    gojo_sudog *s = sq_pop(&c->send_head, &c->send_tail);
    if (s) {
        g_touch_begin(s->g);
        memcpy(out, s->elem, c->esize);
        g_touch_end(s->g);
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

// False: the value was not sent (every receiver is gone, or the task was
// cancelled while it waited); the caller drops it.
bool gojo_chan_send(gojo_chan *c, const void *elem, const char *loc) {
    sim_point();
    spin_lock(&c->lock);
    int r = chan_try_send_locked(c, elem, loc);
    if (r) return r == 1;
    gojo_g *g = cur_g();
    gojo_sudog *s = &g->sud;
    memset(s, 0, sizeof *s);
    s->g = g;
    s->c = c;
    s->elem = (void *)elem;
    s->token = wait_begin(g);
    sq_push(&c->send_head, &c->send_tail, s);
    spin_unlock(&c->lock);
    blk(g, BLK_SEND, c);
    wait_park(g, s->token);
    unblk(g);
    if (g->wake == WAKE_CANCEL) {
        spin_lock(&c->lock);
        sq_remove(&c->send_head, &c->send_tail, s);
        spin_unlock(&c->lock);
        return false;
    }
    return s->ok;
}

bool gojo_chan_recv(gojo_chan *c, void *out) {
    sim_point();
    spin_lock(&c->lock);
    int r = chan_try_recv_locked(c, out);
    if (r) return r == 1;
    gojo_g *g = cur_g();
    gojo_sudog *s = &g->sud;
    memset(s, 0, sizeof *s);
    s->g = g;
    s->c = c;
    s->elem = out;
    s->token = wait_begin(g);
    sq_push(&c->recv_head, &c->recv_tail, s);
    spin_unlock(&c->lock);
    blk(g, BLK_RECV, c);
    wait_park(g, s->token);
    unblk(g);
    if (g->wake == WAKE_CANCEL) {
        spin_lock(&c->lock);
        sq_remove(&c->recv_head, &c->recv_tail, s);
        spin_unlock(&c->lock);
        return false;
    }
    return s->ok;
}

// Takes every waiter off a queue, failed (ok = false), into a list to wake
// once the lock is released: the sudogs live on stacks that resume when readied.
static gojo_g *chan_fail_waiters(gojo_sudog **head, gojo_sudog **tail, gojo_g *wake) {
    gojo_sudog *s;
    while ((s = sq_pop(head, tail))) {
        s->ok = false;
        s->g->qnext = wake;
        wake = s->g;
    }
    return wake;
}

static void wake_list(gojo_g *wake) {
    while (wake) {
        gojo_g *n = wake->qnext;
        wake->qnext = NULL;
        g_ready(wake);
        wake = n;
    }
}

int64_t gojo_chan_len(gojo_chan *c) {
    spin_lock(&c->lock);
    int64_t n = c->len;
    spin_unlock(&c->lock);
    return n;
}

// Returns the channel case taken (0..n-1), a finished task (n + i), -1 for
// the default arm, -3 when the deadline passed, -4 when the task was
// cancelled (with `want_cancel`), or -2 for nothing. cases[i].ok reports the
// outcome: a received value, or a sent one. False on a receive means closed or
// cancelled; false on a send means cancelled, and the value was not sent.
int gojo_select(gojo_sel_case *cases, int n, gojo_task **tasks, int nt, int64_t deadline, bool want_cancel, bool has_default, const char *loc) {
    sim_point();
    gojo_g *g = cur_g();
    if (want_cancel && scope_cancelled(g->scope)) return -4;
    // a finished task, or a deadline already passed, is ready now
    for (int i = 0; i < nt; i++) {
        spin_lock(&tasks[i]->lock);
        bool done = tasks[i]->done;
        spin_unlock(&tasks[i]->lock);
        if (done) return n + i;
    }
    if (deadline >= 0 && gojo_time_now() >= deadline) return -3;
    int order[n > 0 ? n : 1], locks[n > 0 ? n : 1];
    gojo_worker *w = cur_worker();
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
#define SEL_LOCK_ALL() do { gojo_chan *prev_ = NULL; for (int k_ = 0; k_ < nlocks; k_++) { gojo_chan *c_ = cases[locks[k_]].c; if (c_ != prev_) spin_lock(&c_->lock); prev_ = c_; } } while (0)
#define SEL_UNLOCK_ALL() do { gojo_chan *prev_ = NULL; for (int k_ = nlocks - 1; k_ >= 0; k_--) { gojo_chan *c_ = cases[locks[k_]].c; if (c_ != prev_) spin_unlock(&c_->lock); prev_ = c_; } } while (0)
    SEL_LOCK_ALL();
    for (int i = 0; i < n; i++) {
        gojo_sel_case *cs = &cases[order[i]];
        gojo_chan *c = cs->c;
        if (cs->send) {
            // nobody can receive: this arm is never taken
            if (c->deaf) continue;
            gojo_sudog *r = sq_pop(&c->recv_head, &c->recv_tail);
            if (r) {
                g_touch_begin(r->g);
                memcpy(r->elem, cs->elem, c->esize);
                g_touch_end(r->g);
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
            gojo_sudog *s = NULL;
            if (c->len > 0) {
                memcpy(cs->elem, chan_slot(c, 0), c->esize);
                c->head = (c->head + 1) % c->cap;
                c->len--;
                s = sq_pop(&c->send_head, &c->send_tail);
                if (s) {
                    g_touch_begin(s->g);
                    memcpy(chan_slot(c, c->len), s->elem, c->esize);
                    g_touch_end(s->g);
                    c->len++;
                    s->ok = true;
                }
            } else {
                s = sq_pop(&c->send_head, &c->send_tail);
                if (s) {
                    g_touch_begin(s->g);
                    memcpy(cs->elem, s->elem, c->esize);
                    g_touch_end(s->g);
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
    // nothing ready: wait on every channel, task and the deadline
    uint64_t token = wait_begin(g);
    gojo_sudog *suds = n > 0 ? gojo_alloc((size_t)n * sizeof *suds) : NULL;
    for (int i = 0; i < n; i++) {
        gojo_sudog *s = &suds[i];
        memset(s, 0, sizeof *s);
        s->g = g;
        s->c = cases[i].c;
        s->elem = cases[i].elem;
        s->token = token;
        s->idx = i;
        if (cases[i].send) sq_push(&s->c->send_head, &s->c->send_tail, s);
        else sq_push(&s->c->recv_head, &s->c->recv_tail, s);
    }
    SEL_UNLOCK_ALL();
    gojo_task *ready = NULL;
    for (int i = 0; i < nt; i++) {
        spin_lock(&tasks[i]->lock);
        if (tasks[i]->done) ready = tasks[i];
        else {
            tasks[i]->sel_g = g;
            tasks[i]->sel_token = token;
        }
        spin_unlock(&tasks[i]->lock);
    }
    gojo_timer *tm = &g->timer;
    memset(tm, 0, sizeof *tm);
    tm->idx = -1;
    if (deadline >= 0) {
        tm->when = deadline;
        tm->g = g;
        tm->token = token;
        timer_add(tm);
    }
    if (ready && wait_claim(g, token)) {
        // finished between the first look and registering
        g->wake = WAKE_TASK;
        g->wake_data = ready;
    } else {
        blk(g, BLK_SELECT, n > 0 ? cases[0].c : NULL);
        wait_park(g, token);
        unblk(g);
    }
    gojo_sudog *fired = g->wake == WAKE_CHAN ? g->wake_data : NULL;
    SEL_LOCK_ALL();
    for (int i = 0; i < n; i++) {
        gojo_sudog *s = &suds[i];
        if (cases[i].send) sq_remove(&s->c->send_head, &s->c->send_tail, s);
        else sq_remove(&s->c->recv_head, &s->c->recv_tail, s);
    }
    SEL_UNLOCK_ALL();
    int fired_idx = fired ? fired->idx : -1;
    bool fired_ok = fired ? fired->ok : false;
    free(suds);
    for (int i = 0; i < nt; i++) {
        spin_lock(&tasks[i]->lock);
        if (tasks[i]->sel_g == g && tasks[i]->sel_token == token) tasks[i]->sel_g = NULL;
        spin_unlock(&tasks[i]->lock);
    }
    if (deadline >= 0 && g->wake != WAKE_TIMER) timer_remove(tm);
    if (g->wake == WAKE_TASK) {
        for (int i = 0; i < nt; i++)
            if (tasks[i] == g->wake_data) return n + i;
    }
    if (g->wake == WAKE_TIMER) return -3;
    if (g->wake == WAKE_CANCEL && want_cancel) return -4;
#undef SEL_LOCK_ALL
#undef SEL_UNLOCK_ALL
    if (fired_idx >= 0) {
        int i = fired_idx;
        if (cases[i].send && !fired_ok) {
            // the receivers went away while this send waited: wait on the rest
            return gojo_select(cases, n, tasks, nt, deadline, want_cancel, has_default, loc);
        }
        cases[i].ok = fired_ok;
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
struct gojo_mutex {
    int state;              // 0 free, 1 locked, 2 locked with waiters
    gojo_spin lock;
    gojo_g *head, *tail;
    gojo_g *owner;          // for deadlock reports
    const char *loc;        // where it was made: its class for lock-order checks
};

gojo_mutex *gojo_mutex_new(const char *loc) {
    gojo_mutex *m = gojo_alloc(sizeof *m);
    memset(m, 0, sizeof *m);
    m->loc = loc;
    return m;
}

static void lockdep_acquire(gojo_mutex *m);
static void lockdep_release(gojo_mutex *m);
static void mutex_lock_slow(gojo_mutex *m);


void gojo_mutex_free(gojo_mutex *m) { free(m); }

void gojo_mutex_lock(gojo_mutex *m) {
    sim_point();
    if (gojo_debug_checks) lockdep_acquire(m);
    mutex_lock_slow(m);
    gojo_g *g = cur_g();
    if (g) {
        m->owner = g;
        if (gojo_debug_checks && m->loc) {
            if (g->nheld < 8) g->held[g->nheld] = m->loc;
            g->nheld++;
        }
    }
}

static void mutex_lock_slow(gojo_mutex *m) {
    int expected = 0;
    if (__atomic_compare_exchange_n(&m->state, &expected, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return;
    for (int i = 0; i < 64 && !gojo_sim_on; i++) {
        cpu_relax();
        if (GOJO_RELAXED(&m->state) == 0) {
            expected = 0;
            if (__atomic_compare_exchange_n(&m->state, &expected, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return;
        }
    }
    gojo_g *g = cur_g();
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
    blk(g, BLK_LOCK, m);
    g_park();
    unblk(g);
    // the unlocker handed the lock over
}

static void mutex_unlock_slow(gojo_mutex *m);

void gojo_mutex_lock_all(gojo_mutex **ms, int n, const char *loc) {
    gojo_mutex *sorted[n > 0 ? n : 1];
    for (int i = 0; i < n; i++) {
        int k = i;
        while (k > 0 && sorted[k - 1] > ms[i]) {
            sorted[k] = sorted[k - 1];
            k--;
        }
        sorted[k] = ms[i];
    }
    for (int i = 1; i < n; i++)
        if (sorted[i] == sorted[i - 1]) gojo_panic("`lock(...)` was given the same mutex twice", loc);
    gojo_g *g = cur_g();
    int before = g ? g->nheld : 0;
    for (int i = 0; i < n; i++) {
        sim_point();
        // order against the locks held before; not among these, whose order is fixed
        if (gojo_debug_checks && g) {
            int saved = g->nheld;
            g->nheld = before;
            lockdep_acquire(sorted[i]);
            g->nheld = saved;
        }
        mutex_lock_slow(sorted[i]);
        if (g) {
            sorted[i]->owner = g;
            if (gojo_debug_checks && sorted[i]->loc) {
                if (g->nheld < 8) g->held[g->nheld] = sorted[i]->loc;
                g->nheld++;
            }
        }
    }
}

void gojo_mutex_unlock(gojo_mutex *m) {
    m->owner = NULL;
    if (gojo_debug_checks) lockdep_release(m);
    mutex_unlock_slow(m);
    sim_point();
}

static void mutex_unlock_slow(gojo_mutex *m) {
    int expected = 1;
    if (__atomic_compare_exchange_n(&m->state, &expected, 0, false, __ATOMIC_RELEASE, __ATOMIC_RELAXED)) return;
    spin_lock(&m->lock);
    gojo_g *g = m->head;
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

// ------------------------------------------------------------------ rwlock
//
// Readers share, a writer is alone. Waiters queue in order; a writer at the
// front keeps later readers waiting, so writers aren't starved; readers at the
// front all go in together.

typedef struct {
    gojo_spin lock;
    int readers;
    bool writer;
    gojo_g *head, *tail;    // waiters; g->rw_write says which kind
} gojo_rwlock;

uint8_t *gojo_prelude___rwNew(void) {
    gojo_rwlock *l = gojo_alloc(sizeof *l);
    memset(l, 0, sizeof *l);
    return (uint8_t *)l;
}

void gojo_prelude___rwFree(uint8_t *l) { free(l); }

static void rw_wait(gojo_rwlock *l, bool write) {
    gojo_g *g = cur_g();
    g->rw_write = write;
    g->qnext = NULL;
    if (l->tail) l->tail->qnext = g;
    else l->head = g;
    l->tail = g;
    spin_unlock(&l->lock);
    blk(g, BLK_RWLOCK, l);
    g_park();
    unblk(g);
    // the unlocker handed it over
}

// With l->lock held: let in whoever is first in line. Returns the list to wake.
static gojo_g *rw_grant(gojo_rwlock *l) {
    gojo_g *wake = NULL;
    while (l->head) {
        gojo_g *g = l->head;
        if (g->rw_write) {
            if (l->readers > 0 || l->writer || wake) break;
            l->writer = true;
        } else {
            if (l->writer) break;
            l->readers++;
        }
        l->head = g->qnext;
        if (!l->head) l->tail = NULL;
        g->qnext = wake;
        wake = g;
        if (g->rw_write) break;
    }
    return wake;
}

void gojo_rw_rlock(uint8_t *p) {
    sim_point();
    gojo_rwlock *l = (gojo_rwlock *)p;
    spin_lock(&l->lock);
    if (!l->writer && !l->head) {
        l->readers++;
        spin_unlock(&l->lock);
        return;
    }
    rw_wait(l, false);
}

void gojo_rw_wlock(uint8_t *p) {
    sim_point();
    gojo_rwlock *l = (gojo_rwlock *)p;
    spin_lock(&l->lock);
    if (!l->writer && l->readers == 0 && !l->head) {
        l->writer = true;
        spin_unlock(&l->lock);
        return;
    }
    rw_wait(l, true);
}

static void rw_wake(gojo_g *w) {
    while (w) {
        gojo_g *n = w->qnext;
        w->qnext = NULL;
        g_ready(w);
        w = n;
    }
}

void gojo_rw_runlock(uint8_t *p) {
    gojo_rwlock *l = (gojo_rwlock *)p;
    spin_lock(&l->lock);
    gojo_g *w = --l->readers == 0 ? rw_grant(l) : NULL;
    spin_unlock(&l->lock);
    rw_wake(w);
    sim_point();
}

void gojo_rw_wunlock(uint8_t *p) {
    gojo_rwlock *l = (gojo_rwlock *)p;
    spin_lock(&l->lock);
    l->writer = false;
    gojo_g *w = rw_grant(l);
    spin_unlock(&l->lock);
    rw_wake(w);
    sim_point();
}

// ------------------------------------------------------------------ task groups

// Tasks started and finished are counted per group of workers (each count on
// a cache line of its own), so spawning and finishing on different threads
// don't fight over one counter. The body waits until finished == started.
typedef struct {
    int64_t started, finished, exited;
    char pad[64 - 3 * sizeof(int64_t)];
} __attribute__((aligned(64))) group_count;

struct gojo_group {
    gojo_scope scope;
    gojo_spin lock;
    group_count count[SCOPE_SHARDS];
    gojo_g *waiter;
    bool failed;
    const char *loc;        // the `scope` block
    gojo_g *owner;          // the goroutine that opened it: its tasks may borrow from its stack
    struct gojo_group *next_open; // the owner's other open scopes
};

static inline int count_shard(void) {
    gojo_worker *w = cur_worker();
    return w ? (int)(w->id % SCOPE_SHARDS) : 0;
}

// Tasks of the group still running. Finished first, then started: a task
// starts only from a running one, so an equal pair means none run.
static int64_t group_live(gojo_group *grp) {
    int64_t fin = 0, st = 0;
    for (int k = 0; k < SCOPE_SHARDS; k++) fin += GOJO_SC_LOAD(&grp->count[k].finished);
    for (int k = 0; k < SCOPE_SHARDS; k++) st += GOJO_SC_LOAD(&grp->count[k].started);
    return st - fin;
}

static void group_started(gojo_group *grp) { GOJO_ADD(&grp->count[count_shard()].started, 1); }

// Whether tasks of scopes g opened are running: they may borrow from g's stack.
static bool g_borrowed(gojo_g *g) {
    for (gojo_group *grp = GOJO_LOAD(&g->open_groups); grp; grp = grp->next_open)
        if (group_live(grp) > 0) return true;
    return false;
}

// `.ptr()`: a raw pointer to Gojo memory, which C may keep after the call it
// is passed to, and use while the goroutine owning that memory is parked.
// When the memory is on a goroutine's stack, that goroutine is pinned: its
// stack is never hibernated, so the pointer keeps reaching the live bytes.
// Gojo code can only point into its own stack and the stacks of the
// goroutines that opened the scopes it runs in (what it may borrow from), so
// the search follows that chain: the current goroutine, the owner of its
// group, that one's group's owner... (all running or waiting for their
// scopes while this one runs, so none of them is going away).
void *gojo_ptr_escape(const void *p) {
    if (!s_hib_on || !p) return (void *)p;
    gojo_worker *w = cur_worker();
    for (gojo_g *g = w ? w->cur : NULL; g; g = g->group ? g->group->owner : NULL) {
        if ((const uint8_t *)p >= g->stack && (const uint8_t *)p < g->stack + g->stack_size) {
            if (!GOJO_RELAXED(&g->pinned)) GOJO_SC_STORE(&g->pinned, 1);
            break;
        }
    }
    return (void *)p;
}

gojo_group *gojo_group_new(const char *loc) {
    // (the poller and monitor start with the first task, timer or network wait)
    gojo_group *g = gojo_alloc(sizeof *g);
    memset(g, 0, sizeof *g);
    g->loc = loc;
    // the body runs in the group: a failing task cancels it too
    gojo_g *cur = cur_g();
    g->owner = cur;
    g->next_open = cur->open_groups;
    GOJO_STORE(&cur->open_groups, g);
    bool root = cur->scope == NULL && cur->stack_size == s_main_stack_size;
    scope_enter(&g->scope, cur);
    // `main`'s own scope: a shutdown request (Ctrl-C) cancels it
    if (root) {
        spin_lock(&s_root_lock);
        if (!s_root_scope) s_root_scope = &g->scope;
        spin_unlock(&s_root_lock);
    }
    return g;
}

void gojo_group_go_env(gojo_group *grp, void (*fn)(void *), void *env, void (*drop)(void *), const char *loc);

void gojo_group_go(gojo_group *grp, void (*fn)(void *), void *arg, const char *loc) {
    gojo_group_go_env(grp, fn, arg, NULL, loc);
}

// A task that runs fn(env), then drop(env): a closure with nothing to report.
void gojo_group_go_env(gojo_group *grp, void (*fn)(void *), void *arg, void (*drop)(void *), const char *loc) {
    ensure_poller();
    gojo_g *g = g_alloc(cur_worker(), s_stack_size);
    g->fn = fn;
    g->arg = arg;
    g->arg_drop = drop;
    g->spawn_loc = loc;
    g->group = grp;
    GOJO_ADD(&s_ngoroutines, 1);
    group_started(grp);
    scope_join(&grp->scope, g);
    g_ready(g);
    sim_point();
}

// A task of the group is done. If the body waits and this was the last one,
// wake it. `exited` tells the body that this task no longer touches the group,
// which the body frees.
void gojo_group_task_done(gojo_group *grp) {
    int k = count_shard();
    GOJO_ADD(&grp->count[k].finished, 1);
    if (GOJO_SC_LOAD(&grp->waiter) && group_live(grp) == 0) {
        gojo_g *w = __atomic_exchange_n(&grp->waiter, NULL, __ATOMIC_SEQ_CST);
        if (w) g_ready(w);
    }
    __atomic_add_fetch(&grp->count[k].exited, 1, __ATOMIC_RELEASE);
}

// True for the first failure: that task records its error. Also cancels the group.
bool gojo_group_fail(gojo_group *grp) {
    spin_lock(&grp->lock);
    bool first = !grp->failed;
    grp->failed = true;
    spin_unlock(&grp->lock);
    scope_cancel_r(&grp->scope, CANCEL_FAILED);
    return first;
}

void gojo_group_cancel(gojo_group *grp) { scope_cancel(&grp->scope); }

// A task with a handle failed: its scope is cancelled, the error goes to `join`.
void gojo_group_cancel_failed(gojo_group *grp) { scope_cancel_r(&grp->scope, CANCEL_FAILED); }

// ------------------------------------------------------------------ task handles
//
// `let t = go f()`. The task runs in a cancel scope of its own, under its
// group's, so `t.cancel()` (or dropping `t`) stops just that task. The result
// is stored after the header. Exactly one side frees the record: the joiner;
// or, when the handle is dropped, whichever of the handle and the task is
// last (the other one is gone, so the result has nobody to go to: the last one
// drops it).


#define TASK_HDR ((sizeof(gojo_task) + 15) & ~(size_t)15)

gojo_task *gojo_task_new(size_t result_size) {
    gojo_task *t = gojo_alloc(TASK_HDR + result_size);
    memset(t, 0, sizeof *t);
    return t;
}

void *gojo_task_result(gojo_task *t) { return (char *)t + TASK_HDR; }

void gojo_group_go_task(gojo_group *grp, gojo_task *t, void (*fn)(void *), void *arg, const char *loc) {
    ensure_poller();
    gojo_g *g = g_alloc(cur_worker(), s_stack_size);
    g->fn = fn;
    g->arg = arg;
    g->spawn_loc = loc;
    t->loc = loc;
    g->group = grp;
    GOJO_ADD(&s_ngoroutines, 1);
    group_started(grp);
    // the task's scope, a child of the group's
    gojo_scope *s = &t->scope, *p = &grp->scope;
    s->parent = p;
    spin_lock(&p->lock);
    s->cancelled = p->cancelled;
    s->reason = p->reason;
    s->next_sib = p->first_child;
    if (p->first_child) p->first_child->prev_sib = s;
    p->first_child = s;
    spin_unlock(&p->lock);
    scope_join(s, g);
    g_ready(g);
    sim_point();
}

// On the task, after it stored its result. True when the handle is gone: the
// caller drops the result and frees the record.
bool gojo_task_finish(gojo_task *t) {
    gojo_g *g = cur_g();
    // back into the group's scope: the record may be freed once `done` is set
    scope_leave(g);
    scope_join(t->scope.parent, g);
    scope_unlink(&t->scope);
    spin_lock(&t->lock);
    t->done = true;
    bool gone = t->handle_gone;
    gojo_g *w = t->waiter;
    t->waiter = NULL;
    gojo_g *sg = t->sel_g;
    uint64_t stok = t->sel_token;
    t->sel_g = NULL;
    spin_unlock(&t->lock);
    if (w) g_ready(w);
    if (sg && wait_claim(sg, stok)) {
        sg->wake = WAKE_TASK;
        sg->wake_data = t;
        g_ready(sg);
    }
    return gone;
}

// Waits until the task has stored its result. Not a cancellation point: the
// task belongs to a scope that is cancelled along with the waiter.
void gojo_task_join(gojo_task *t) {
    sim_point();
    spin_lock(&t->lock);
    if (!t->done) {
        gojo_g *g = cur_g();
        t->waiter = g;
        spin_unlock(&t->lock);
        blk(g, BLK_JOIN, t);
        g_park();
        unblk(g);
    } else {
        spin_unlock(&t->lock);
    }
    // the task may still be releasing the lock
    spin_lock(&t->lock);
    spin_unlock(&t->lock);
}

void gojo_task_cancel(gojo_task *t) {
    spin_lock(&t->lock);
    bool done = t->done;
    spin_unlock(&t->lock);
    if (!done) scope_cancel(&t->scope);
}

// The handle is dropped without a join: cancel the task. True when it already
// finished: the caller drops the result and frees the record.
bool gojo_task_abandon(gojo_task *t) {
    gojo_task_cancel(t);
    spin_lock(&t->lock);
    t->handle_gone = true;
    bool done = t->done;
    spin_unlock(&t->lock);
    return done;
}

void gojo_task_free(gojo_task *t) { free(t); }

// ------------------------------------------------------------------ par for
//
// Indices are handed out in chunks from a shared counter, so tasks that get
// cheap elements take more chunks. The calling task works too; the helpers
// belong to a group of their own, so they are cancelled along with it.

typedef struct {
    void (*body)(void *, int64_t);
    void *ctx;
    int64_t n, grain, next;
} par_job;

static void par_run(par_job *j) {
    for (;;) {
        int64_t i = __atomic_fetch_add(&j->next, j->grain, __ATOMIC_RELAXED);
        if (i >= j->n) return;
        int64_t e = i + j->grain < j->n ? i + j->grain : j->n;
        for (int64_t k = i; k < e; k++) j->body(j->ctx, k);
    }
}

static void par_helper(void *p) { par_run(p); }

void gojo_par_for(int64_t n, void (*body)(void *, int64_t), void *ctx) {
    if (n <= 0) return;
    int64_t procs = GOJO_RELAXED(&s_nprocs);
    int64_t grain = n / (procs * 8);
    if (grain < 1) grain = 1;
    int64_t chunks = (n + grain - 1) / grain;
    int64_t helpers = procs - 1 < chunks - 1 ? procs - 1 : chunks - 1;
    if (helpers <= 0) {
        for (int64_t i = 0; i < n; i++) body(ctx, i);
        return;
    }
    par_job j = { body, ctx, n, grain, 0 };
    gojo_group *grp = gojo_group_new("<par for>");
    for (int64_t h = 0; h < helpers; h++) gojo_group_go(grp, par_helper, &j, "<par for helper>");
    par_run(&j);
    gojo_group_wait(grp);
}

// Always waits for every task, cancelled or not: tasks may borrow from this frame.
void gojo_group_wait(gojo_group *grp) {
    gojo_g *g = cur_g();
    // the body is done; its tasks stay reachable by cancellation from above
    scope_step_out(&grp->scope, g);
    if (group_live(grp) > 0) {
        GOJO_SC_STORE(&grp->waiter, g);
        // the last task may have finished before it could see the waiter
        if (group_live(grp) == 0 && __atomic_exchange_n(&grp->waiter, NULL, __ATOMIC_SEQ_CST) == g) {
            // nobody will wake us
        } else {
            blk(g, BLK_SCOPE, grp);
            g_park();
            unblk(g);
        }
    }
    // the last tasks may still be leaving gojo_group_task_done
    for (;;) {
        int64_t ex = 0, st = 0;
        for (int k = 0; k < SCOPE_SHARDS; k++) ex += __atomic_load_n(&grp->count[k].exited, __ATOMIC_ACQUIRE);
        for (int k = 0; k < SCOPE_SHARDS; k++) st += GOJO_RELAXED(&grp->count[k].started);
        if (ex == st) break;
        cpu_relax();
    }
    scope_unlink(&grp->scope);
    spin_lock(&s_root_lock);
    if (s_root_scope == &grp->scope) s_root_scope = NULL;
    spin_unlock(&s_root_lock);
    // scopes close in the order they opened, innermost first
    if (g->open_groups == grp) GOJO_STORE(&g->open_groups, grp->next_open);
    free(grp);
}

// ------------------------------------------------------------------ timers


static pthread_mutex_t s_timer_lock = PTHREAD_MUTEX_INITIALIZER;
static gojo_timer **s_heap;
static int s_heap_len, s_heap_cap;

static void heap_set(int i, gojo_timer *t) {
    s_heap[i] = t;
    t->idx = i;
}

static void heap_up(int i) {
    gojo_timer *t = s_heap[i];
    while (i > 0) {
        int p = (i - 1) / 2;
        if (s_heap[p]->when <= t->when) break;
        heap_set(i, s_heap[p]);
        i = p;
    }
    heap_set(i, t);
}

static void heap_down(int i) {
    gojo_timer *t = s_heap[i];
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

static void heap_remove(gojo_timer *t) {
    int i = t->idx;
    if (i < 0) return;
    t->idx = -1;
    GOJO_ADD(&s_timers_pending, -1);
    gojo_timer *last = s_heap[--s_heap_len];
    if (last != t) {
        heap_set(i, last);
        heap_up(i);
        heap_down(last->idx);
    }
}

static void timer_add(gojo_timer *t) {
    ensure_poller();
    pthread_mutex_lock(&s_timer_lock);
    if (s_heap_len == s_heap_cap) {
        s_heap_cap = s_heap_cap ? s_heap_cap * 2 : 64;
        s_heap = gojo_realloc(s_heap, (size_t)s_heap_cap * sizeof *s_heap);
    }
    GOJO_ADD(&s_timers_pending, 1);
    s_heap[s_heap_len++] = t;
    t->idx = s_heap_len - 1;
    heap_up(t->idx);
    bool earliest = t->idx == 0;
    pthread_mutex_unlock(&s_timer_lock);
    if (earliest) poller_wake();
}

static void timer_remove(gojo_timer *t) {
    pthread_mutex_lock(&s_timer_lock);
    heap_remove(t);
    pthread_mutex_unlock(&s_timer_lock);
}

// False: the task was cancelled before the time was up.
bool gojo_time___sleep(int64_t ns) {
    gojo_g *g = cur_g();
    if (scope_cancelled(g->scope)) return false;
    if (ns <= 0) {
        gojo_task_yieldNow();
        return !scope_cancelled(cur_g()->scope);
    }
    gojo_timer *t = &g->timer;
    memset(t, 0, sizeof *t);
    t->when = gojo_time_now() + ns;
    t->g = g;
    t->token = wait_begin(g);
    t->idx = -1;
    timer_add(t);
    blk(g, BLK_SLEEP, NULL);
    wait_park(g, t->token);
    unblk(g);
    if (g->wake == WAKE_TIMER) return true;
    timer_remove(t);
    return false;
}

// A cancel scope with a deadline, for task.timeout.
uint8_t *gojo_task___scopeEnter(int64_t ns) {
    gojo_g *g = cur_g();
    struct { gojo_scope s; gojo_timer t; } *sc = gojo_alloc(sizeof *sc);
    scope_enter(&sc->s, g);
    memset(&sc->t, 0, sizeof sc->t);
    sc->t.idx = -1;
    sc->t.scope = &sc->s;
    if (ns <= 0) {
        scope_cancel_r(&sc->s, CANCEL_DEADLINE);
    } else {
        sc->t.when = gojo_time_now() + ns;
        sc->s.deadline = sc->t.when;
        timer_add(&sc->t);
    }
    return (uint8_t *)sc;
}

// Why the current task was cancelled (CANCEL_*), or 0.
int64_t gojo_task___reason(void) {
    gojo_scope *s = cur_g()->scope;
    if (!scope_cancelled(s)) return 0;
    return s->reason ? s->reason : CANCEL_EXPLICIT;
}

// The nearest deadline of the current task, or -1.
int64_t gojo_task___deadline(void) {
    int64_t best = -1;
    for (gojo_scope *s = cur_g()->scope; s; s = s->parent)
        if (s->deadline && (best < 0 || s->deadline < best)) best = s->deadline;
    return best;
}

// `task.shield(d, f)`: the task leaves its scope for a fresh one that only
// its own deadline cancels, then comes back.
typedef struct {
    gojo_scope s;
    gojo_timer t;
    gojo_scope *saved;
} gojo_shield;

uint8_t *gojo_task___shieldEnter(int64_t ns) {
    gojo_g *g = cur_g();
    gojo_shield *sh = gojo_alloc(sizeof *sh);
    memset(sh, 0, sizeof *sh);
    sh->saved = g->scope;
    sh->s.shielded = g->scope;
    sh->t.idx = -1;
    sh->t.scope = &sh->s;
    scope_leave(g);
    scope_join(&sh->s, g);
    if (ns <= 0) {
        scope_cancel_r(&sh->s, CANCEL_DEADLINE);
    } else {
        sh->t.when = gojo_time_now() + ns;
        sh->s.deadline = sh->t.when;
        timer_add(&sh->t);
    }
    return (uint8_t *)sh;
}

void gojo_task___shieldExit(uint8_t *p) {
    gojo_shield *sh = (gojo_shield *)p;
    gojo_g *g = cur_g();
    timer_remove(&sh->t);
    scope_leave(g);
    scope_join(sh->saved, g);
    free(sh);
}

// Around a type's `drop` method: in a cancelled task, cleanup still gets to
// block, for a grace period (GOJO_DROP_GRACE ms, default 1000). NULL: the
// task isn't cancelled, nothing to do.
uint8_t *gojo_drop_shield(void) {
    gojo_worker *w = cur_worker();
    if (!w || !w->cur || !scope_cancelled(w->cur->scope)) return NULL;
    return gojo_task___shieldEnter(s_drop_grace);
}

void gojo_drop_unshield(uint8_t *sh) {
    if (sh) gojo_task___shieldExit(sh);
}

// True: the scope was cancelled, by its deadline or from outside.
bool gojo_task___scopeExit(uint8_t *p) {
    struct { gojo_scope s; gojo_timer t; } *sc = (void *)p;
    timer_remove(&sc->t);
    bool cancelled = GOJO_SC_LOAD(&sc->s.cancelled) != 0;
    scope_exit(&sc->s, cur_g());
    free(sc);
    return cancelled;
}

// ------------------------------------------------------------------ task-local values

static int64_t s_next_local;
int64_t gojo_task___newLocal(void) { return GOJO_ADD(&s_next_local, 1); }

// `task.with(key, value, body)`: a scope under the current one, binding key.
uint8_t *gojo_task_bind_enter(int64_t id, void *val) {
    gojo_g *g = cur_g();
    gojo_scope *s = gojo_alloc(sizeof *s);
    scope_enter(s, g);
    s->local_id = id;
    s->local_val = val;
    return (uint8_t *)s;
}

void gojo_task_bind_exit(uint8_t *p) {
    gojo_scope *s = (gojo_scope *)p;
    scope_exit(s, cur_g());
    free(s);
}

// The value bound to key `id` for the current task, or NULL.
void *gojo_task_lookup(int64_t id) {
    for (gojo_scope *s = cur_g()->scope; s; s = s->parent ? s->parent : s->shielded)
        if (s->local_id == id) return s->local_val;
    return NULL;
}

void gojo_task___groupCancel(uint8_t *state) { gojo_group_cancel(*(gojo_group **)state); }

// ------------------------------------------------------------------ poller and monitor

static int s_poll_fd = -1;
static int s_wake_fd = -1;
static int s_timer_fd = -1;     // Linux: epoll timeouts are in milliseconds; timerfd isn't

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

static void dump_task(FILE *f, gojo_g *g, int64_t now);

// Debug builds: a scope that has waited for its tasks for 10 s says what they
// are waiting for (once per wait).
static void note_slow_scopes(int64_t now) {
    static int64_t last;
    if (now - last < 1000000000) return;
    last = now;
    uint32_t n = GOJO_LOAD(&s_g_count);
    for (uint32_t i = 0; i < n; i++) {
        gojo_g *g = g_slot(i);
        if (!GOJO_RELAXED(&g->alive) || GOJO_RELAXED(&g->blocked) != BLK_SCOPE || GOJO_RELAXED(&g->slow_noted)) continue;
        int64_t since = blocked_since(g, now);
        if (now - since < 10 * (int64_t)1000000000) continue;
        GOJO_SET_RELAXED(&g->slow_noted, true);
        gojo_group *grp = (gojo_group *)GOJO_RELAXED(&g->blocked_on);
        fprintf(stderr, "note: the scope at %s has waited %.0f s for its tasks:\n", grp->loc ? grp->loc : "?", (double)(now - since) / 1e9);
        for (uint32_t j = 0; j < n; j++) {
            gojo_g *t = g_slot(j);
            if (GOJO_RELAXED(&t->alive) && t->group == grp) dump_task(stderr, t, now);
        }
    }
}

static void monitor_tick(void) {
    int64_t now = gojo_time_now();
    if (gojo_debug_checks) note_slow_scopes(now);
    hibernate_parked(now);
    int started = GOJO_LOAD(&s_nstarted);
    bool queued = GOJO_RELAXED(&s_gq_size) > 0;
    for (int i = 0; i < started && !queued; i++) {
        gojo_worker *w = GOJO_LOAD(&s_workers[i]);
        if (w && !runq_empty(w)) queued = true;
    }
    for (int i = 0; i < started; i++) {
        gojo_worker *w = GOJO_LOAD(&s_workers[i]);
        if (!w) continue;
        uint64_t tick = GOJO_RELAXED(&w->tick);
        uint64_t seq = GOJO_RELAXED(&w->sysseq);
        if (tick != w->seen_tick || !GOJO_RELAXED(&w->cur)) {
            // a new goroutine since the last look
            w->seen_tick = tick;
            w->seen_since = now;
            w->seen_sysseq = seq;
            w->preempt_since = 0;
            cancel_preempt(w);
            continue;
        }
        if (GOJO_SC_LOAD(&w->insys)) {
            // one C call has been running since the last look
            if (seq == w->seen_sysseq && now - w->seen_since >= SYSCALL_AFTER) {
                worker_release(w);
                // it may have returned in the meantime
                if (!GOJO_SC_LOAD(&w->insys)) worker_reclaim(w);
            } else if (seq != w->seen_sysseq) {
                w->seen_sysseq = seq;
                w->seen_since = now;
            }
            continue;
        }
        if (GOJO_RELAXED(&w->released)) continue;
        if (!queued || now - w->seen_since < PREEMPT_AFTER) continue;
        if (!w->preempt_since) {
            w->preempt_since = now;
            w->preempt_cpu = worker_cpu_ns(w);
            if (!__atomic_exchange_n(&w->preempt, 1, __ATOMIC_ACQ_REL)) GOJO_ADD(&gojo_preempt_flag, 1);
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
    if (queued && can_wake() && GOJO_SC_LOAD(&s_nspinning) == 0) wake_one();
}

// Wakes the sleepers whose time has come, and cancels expired deadlines.
static void timers_fire(int64_t now) {
    pthread_mutex_lock(&s_timer_lock);
    while (s_heap_len > 0 && s_heap[0]->when <= now) {
        gojo_timer *t = s_heap[0];
        if (t->scope) {
            heap_remove(t);
            scope_cancel_r(t->scope, CANCEL_DEADLINE);
        } else {
            // keep it counted until the goroutine is queued, for deadlock detection
            GOJO_ADD(&s_timers_pending, 1);
            heap_remove(t);
            if (wait_claim(t->g, t->token)) {
                t->g->wake = WAKE_TIMER;
                g_ready(t->g);
            }
            GOJO_ADD(&s_timers_pending, -1);
        }
    }
    pthread_mutex_unlock(&s_timer_lock);
}

static void deliver_token(uint64_t data) {
    uint32_t idx = (uint32_t)(data >> 34);
    uint64_t low = data & (((uint64_t)1 << 34) - 1);
    if (idx >= GOJO_LOAD(&s_g_count)) return;
    gojo_g *g = g_slot(idx);
    uint64_t w = GOJO_SC_LOAD(&g->wait);
    if ((w & (((uint64_t)1 << 34) - 1)) != low) return; // a stale event
    if (wait_claim(g, w)) {
        g->wake = WAKE_IO;
        g_ready(g);
        GOJO_ADD(&s_io_waiters, -1);
    }
}

#if defined(__linux__)
static int s_ur_efd = -1;
static void ur_reap(gojo_g *self);
#endif

static void *poller_main(void *arg) {
    (void)arg;
    sigset_t all;
    sigfillset(&all);
    pthread_sigmask(SIG_BLOCK, &all, NULL);
    for (;;) {
        int64_t next = -1;
        check_shutdown();
        if (!gojo_sim_on) {
            int64_t now = gojo_time_now();
            timers_fire(now);
            pthread_mutex_lock(&s_timer_lock);
            if (s_heap_len > 0) next = s_heap[0]->when - now;
            pthread_mutex_unlock(&s_timer_lock);
            monitor_tick();
        }
        // while anything runs, tick for the monitor; when all is idle, only events matter
        bool idle = GOJO_SC_LOAD(&s_nidle) == GOJO_SC_LOAD(&s_nstarted);
        if (!gojo_sim_on) {
            if (!idle && (next < 0 || next > 2000000)) next = 2000000;
            if (next < 0 || next > 100000000) next = 100000000;
        }
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
            int64_t at = gojo_time_now() + next;
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
            if (evs[i].data.u64 == 2) {
                uint64_t v;
                ssize_t r = read(s_ur_efd, &v, sizeof v);
                (void)r;
                ur_reap(NULL);
                continue;
            }
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
    if (GOJO_LOAD(&s_poller_started)) return;
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
        if (s_poll_fd < 0) gojo_panic("cannot create the event queue", "<runtime>");
        if (!start_thread(poller_main, NULL)) gojo_panic("cannot start the poller thread", "<runtime>");
        GOJO_STORE(&s_poller_started, 1);
    }
    pthread_mutex_unlock(&s_lock);
}

// Parks until fd is ready: 1, or 0 when the task was cancelled. With `soft`,
// -1 when the poller can't watch fd (a regular file, /dev/null) instead of a
// panic.
static int wait_fd_r(int fd, bool for_write, bool soft) {
    ensure_poller();
    gojo_g *g = cur_g();
    if (scope_cancelled(g->scope)) return 0;
    uint64_t token = wait_begin(g);
    uint64_t data = ((uint64_t)g->idx << 34) | (token & (((uint64_t)1 << 34) - 1));
    GOJO_ADD(&s_io_waiters, 1);
#if defined(__APPLE__)
    struct kevent ev;
    EV_SET(&ev, fd, for_write ? EVFILT_WRITE : EVFILT_READ, EV_ADD | EV_ONESHOT, 0, 0, (void *)data);
    if (kevent(s_poll_fd, &ev, 1, NULL, 0, NULL) != 0) {
        if (!soft) gojo_panic("cannot register a file descriptor with the poller", "<runtime>");
        wait_claim(g, token);
        GOJO_ADD(&s_io_waiters, -1);
        return -1;
    }
#else
    struct epoll_event ev;
    memset(&ev, 0, sizeof ev);
    ev.events = (for_write ? EPOLLOUT : EPOLLIN) | EPOLLONESHOT | EPOLLRDHUP;
    ev.data.u64 = data;
    if (epoll_ctl(s_poll_fd, EPOLL_CTL_MOD, fd, &ev) != 0) {
        if (errno != ENOENT || epoll_ctl(s_poll_fd, EPOLL_CTL_ADD, fd, &ev) != 0) {
            if (!soft) gojo_panic("cannot register a file descriptor with the poller", "<runtime>");
            wait_claim(g, token);
            GOJO_ADD(&s_io_waiters, -1);
            return -1;
        }
    }
#endif
    blk(g, BLK_IO, NULL);
    wait_park(g, token);
    unblk(g);
    if (g->wake == WAKE_IO) return 1;
    // cancelled: the registration is stale now, and a late event won't match the token
    GOJO_ADD(&s_io_waiters, -1);
#if defined(__APPLE__)
    EV_SET(&ev, fd, for_write ? EVFILT_WRITE : EVFILT_READ, EV_DELETE, 0, 0, NULL);
    kevent(s_poll_fd, &ev, 1, NULL, 0, NULL);
#else
    ev.events = 0;
    epoll_ctl(s_poll_fd, EPOLL_CTL_MOD, fd, &ev);
#endif
    return 0;
}

static bool wait_fd(int fd, bool for_write) { return wait_fd_r(fd, for_write, false) == 1; }

#if defined(__linux__)
// ------------------------------------------------------------------ io_uring
//
// File reads and writes that would block (a pipe or terminal with nothing to
// read, a full pipe, a file not in the page cache) go to an io_uring instead
// of blocking a worker thread: the task parks, other tasks keep the thread,
// and cancelling the task cancels the operation. Each read or write first
// tries the plain syscall with RWF_NOWAIT, so data that is already there costs
// one syscall, as before. The poller watches an eventfd the ring signals on
// completion; a submitter also takes the completions that are ready when its
// submit returns. Without io_uring (old kernels, seccomp filters such as
// Docker's default, GOJO_IO_URING=0) file I/O blocks its thread as before.
//
// The kernel ABI is declared here rather than taken from <linux/io_uring.h>,
// which older systems don't have.

#ifndef __NR_io_uring_setup
#define __NR_io_uring_setup 425
#define __NR_io_uring_enter 426
#define __NR_io_uring_register 427
#endif
#ifndef RWF_NOWAIT
#define RWF_NOWAIT 0x00000008
#endif
#if !defined(__NR_preadv2) && defined(__x86_64__)
#define __NR_preadv2 327
#define __NR_pwritev2 328
#elif !defined(__NR_preadv2) && defined(__aarch64__)
#define __NR_preadv2 286
#define __NR_pwritev2 287
#endif

typedef struct { uint32_t head, tail, ring_mask, ring_entries, flags, dropped, array, resv1; uint64_t user_addr; } ur_sq_off;
typedef struct { uint32_t head, tail, ring_mask, ring_entries, overflow, cqes, flags, resv1; uint64_t user_addr; } ur_cq_off;
typedef struct { uint32_t sq_entries, cq_entries, flags, sq_thread_cpu, sq_thread_idle, features, wq_fd, resv[3]; ur_sq_off sq_off; ur_cq_off cq_off; } ur_params;
typedef struct { uint8_t opcode, flags; uint16_t ioprio; int32_t fd; uint64_t off, addr; uint32_t len, op_flags; uint64_t user_data; uint16_t buf_index, personality; int32_t splice_fd_in; uint64_t addr3, pad; } ur_sqe;
typedef struct { uint64_t user_data; int32_t res; uint32_t flags; } ur_cqe;
_Static_assert(sizeof(ur_params) == 120, "io_uring_params");
_Static_assert(sizeof(ur_sqe) == 64, "io_uring_sqe");

enum { UR_OP_ASYNC_CANCEL = 14, UR_OP_READ = 22, UR_OP_WRITE = 23 };
#define UR_SETUP_CQSIZE (1u << 3)
#define UR_SETUP_CLAMP (1u << 4)
#define UR_FEAT_SINGLE_MMAP (1u << 0)
#define UR_FEAT_NODROP (1u << 1)
#define UR_FEAT_RW_CUR_POS (1u << 3)
#define UR_ENTER_GETEVENTS 1u
#define UR_SQ_CQ_OVERFLOW (1u << 1)
#define UR_REGISTER_EVENTFD 4
#define UR_OFF_CQ_RING 0x8000000ULL
#define UR_OFF_SQES 0x10000000ULL
#define UR_FALLBACK INT64_MIN       // io_uring can't take it: block instead

static int s_ur_state;              // 0: not tried, 1: on, -1: off
static bool s_ur_always;            // GOJO_IO_URING=always: every file read and write (tests)
static pthread_mutex_t s_ur_init_lock = PTHREAD_MUTEX_INITIALIZER;
static int s_ur_fd = -1;
static uint32_t *s_sq_head, *s_sq_tail, *s_sq_array, *s_sq_flags, s_sq_mask, s_sq_entries;
static ur_sqe *s_sqes;
static uint32_t *s_cq_head, *s_cq_tail, s_cq_mask;
static ur_cqe *s_cqes;
static pthread_mutex_t s_sq_lock = PTHREAD_MUTEX_INITIALIZER;
static gojo_spin s_cq_lock;

static int ur_enter(unsigned submit, unsigned min, unsigned flags) {
    return (int)syscall(__NR_io_uring_enter, s_ur_fd, submit, min, flags, NULL, 0);
}

static bool ur_setup(void) {
    const char *e = getenv("GOJO_IO_URING");
    if (e && e[0] == '0') return false;
    s_ur_always = e && strcmp(e, "always") == 0;
    if (gojo_sim_on) return false;  // simulated tests: one thread, real blocking
    ur_params p;
    memset(&p, 0, sizeof p);
    p.flags = UR_SETUP_CQSIZE | UR_SETUP_CLAMP;
    p.cq_entries = 4096;
    int fd = (int)syscall(__NR_io_uring_setup, 256, &p);
    if (fd < 0) return false;
    uint32_t need = UR_FEAT_NODROP | UR_FEAT_RW_CUR_POS;
    if ((p.features & need) != need) {
        close(fd);
        return false;
    }
    size_t sqsz = p.sq_off.array + p.sq_entries * sizeof(uint32_t);
    size_t cqsz = p.cq_off.cqes + p.cq_entries * sizeof(ur_cqe);
    bool single = p.features & UR_FEAT_SINGLE_MMAP;
    if (single && cqsz > sqsz) sqsz = cqsz;
    uint8_t *sq = mmap(NULL, sqsz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, fd, 0);
    if (sq == MAP_FAILED) {
        close(fd);
        return false;
    }
    uint8_t *cq = single ? sq : mmap(NULL, cqsz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, fd, UR_OFF_CQ_RING);
    ur_sqe *sqes = mmap(NULL, p.sq_entries * sizeof(ur_sqe), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, fd, UR_OFF_SQES);
    int efd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (cq == MAP_FAILED || sqes == MAP_FAILED || efd < 0 || syscall(__NR_io_uring_register, fd, UR_REGISTER_EVENTFD, &efd, 1) != 0) {
        // (the mappings go with the process; this happens at most once)
        if (efd >= 0) close(efd);
        close(fd);
        return false;
    }
    s_ur_fd = fd;
    s_sq_head = (uint32_t *)(sq + p.sq_off.head);
    s_sq_tail = (uint32_t *)(sq + p.sq_off.tail);
    s_sq_array = (uint32_t *)(sq + p.sq_off.array);
    s_sq_flags = (uint32_t *)(sq + p.sq_off.flags);
    s_sq_mask = *(uint32_t *)(sq + p.sq_off.ring_mask);
    s_sq_entries = p.sq_entries;
    s_sqes = sqes;
    s_cq_head = (uint32_t *)(cq + p.cq_off.head);
    s_cq_tail = (uint32_t *)(cq + p.cq_off.tail);
    s_cq_mask = *(uint32_t *)(cq + p.cq_off.ring_mask);
    s_cqes = (ur_cqe *)(cq + p.cq_off.cqes);
    ensure_poller();
    struct epoll_event ev;
    memset(&ev, 0, sizeof ev);
    ev.events = EPOLLIN;
    ev.data.u64 = 2;
    s_ur_efd = efd;
    if (epoll_ctl(s_poll_fd, EPOLL_CTL_ADD, efd, &ev) != 0) gojo_panic("cannot watch the io_uring", "<runtime>");
    return true;
}

static inline bool ur_on(void) {
#ifndef __NR_preadv2
    return false;
#endif
    int st = GOJO_LOAD(&s_ur_state);
    if (__builtin_expect(st != 0, 1)) return st > 0;
    pthread_mutex_lock(&s_ur_init_lock);
    if (!s_ur_state) GOJO_STORE(&s_ur_state, ur_setup() ? 1 : -1);
    pthread_mutex_unlock(&s_ur_init_lock);
    return GOJO_LOAD(&s_ur_state) > 0;
}

// Queues one operation and hands it to the kernel. False: it couldn't take it.
static bool ur_submit(uint8_t op, int fd, uint64_t addr, uint32_t len, uint64_t ud) {
    pthread_mutex_lock(&s_sq_lock);
    uint32_t tail = *s_sq_tail;
    if (tail - __atomic_load_n(s_sq_head, __ATOMIC_ACQUIRE) >= s_sq_entries) {
        pthread_mutex_unlock(&s_sq_lock);
        return false;
    }
    uint32_t i = tail & s_sq_mask;
    ur_sqe *e = &s_sqes[i];
    memset(e, 0, sizeof *e);
    e->opcode = op;
    e->fd = fd;
    // reads and writes at the file's position, as read(2) and write(2); a
    // cancel takes no offset
    e->off = op == UR_OP_ASYNC_CANCEL ? 0 : (uint64_t)-1;
    e->addr = addr;
    e->len = len;
    e->user_data = ud;
    s_sq_array[i] = i;
    __atomic_store_n(s_sq_tail, tail + 1, __ATOMIC_RELEASE);
    int r, tries = 0;
    for (;;) {
        r = ur_enter(1, 0, 0);
        if (r >= 0 || (errno != EINTR && errno != EAGAIN && errno != EBUSY) || ++tries > 1000) break;
        // the completion queue is full: let the kernel move what overflowed
        if (errno != EINTR) {
            ur_enter(0, 0, UR_ENTER_GETEVENTS);
            sched_yield();
        }
    }
    bool ok = r == 1;
    if (!ok) {
        // still in the queue: take it back
        __atomic_store_n(s_sq_tail, tail, __ATOMIC_RELEASE);
    }
    pthread_mutex_unlock(&s_sq_lock);
    return ok;
}

// A finished operation: the result goes to its task, which is woken if it is
// waiting. `self` is the calling task: it looks for itself before it waits.
static void ur_complete(uint64_t ud, int32_t res, gojo_g *self) {
    if (!ud) return;    // a cancel request
    uint32_t idx = (uint32_t)(ud >> 32), seq = (uint32_t)ud;
    if (idx >= GOJO_LOAD(&s_g_count)) return;
    gojo_g *g = g_slot(idx);
    if (GOJO_LOAD(&g->io_seq) != seq) return;
    g->io_res = res;
    GOJO_SC_STORE(&g->io_done, seq);
    if (g == self) return;
    uint64_t w = GOJO_SC_LOAD(&g->wait);
    if ((w & 3) != WAIT_NONE && wait_claim(g, w)) {
        g->wake = WAKE_IO;
        g_ready(g);
    }
}

static void ur_reap(gojo_g *self) {
    if (GOJO_LOAD(&s_ur_state) <= 0) return;
    for (;;) {
        spin_lock(&s_cq_lock);
        uint32_t head = *s_cq_head;
        uint32_t tail = __atomic_load_n(s_cq_tail, __ATOMIC_ACQUIRE);
        while (head != tail) {
            ur_cqe c = s_cqes[head & s_cq_mask];
            head++;
            __atomic_store_n(s_cq_head, head, __ATOMIC_RELEASE);
            ur_complete(c.user_data, c.res, self);
        }
        spin_unlock(&s_cq_lock);
        if (!(__atomic_load_n(s_sq_flags, __ATOMIC_ACQUIRE) & UR_SQ_CQ_OVERFLOW)) break;
        ur_enter(0, 0, UR_ENTER_GETEVENTS);
    }
}

// Reads or writes through the ring; parks until it is done. The result of the
// syscall (or -errno), -ECANCELED, or UR_FALLBACK.
static int64_t ur_rw(bool write, int fd, void *p, size_t len) {
    gojo_g *g = cur_g();
    if (scope_cancelled(g->scope)) return -ECANCELED;
    if (len > 0x7ffff000) len = 0x7ffff000;
    uint32_t seq = g->io_seq + 1;
    if (!seq) seq = 1;
    GOJO_SC_STORE(&g->io_inflight, 1);
    GOJO_SC_STORE(&g->io_done, 0);
    GOJO_STORE(&g->io_seq, seq);
    uint64_t ud = ((uint64_t)g->idx << 32) | seq;
    if (!ur_submit(write ? UR_OP_WRITE : UR_OP_READ, fd, (uint64_t)(uintptr_t)p, (uint32_t)len, ud)) {
        GOJO_SC_STORE(&g->io_inflight, 0);
        return UR_FALLBACK;
    }
    ur_reap(g);
    if (GOJO_SC_LOAD(&g->io_done) != seq) {
        GOJO_ADD(&s_io_waiters, 1);
        uint64_t token = wait_begin(g);
        if (GOJO_SC_LOAD(&g->io_done) == seq) {
            // it finished meanwhile; if the poller claimed the wait, take its wake-up
            if (!wait_claim(g, token)) g_park();
        } else {
            blk(g, BLK_FILE, NULL);
            wait_park(g, token);
            unblk(g);
            if (GOJO_SC_LOAD(&g->io_done) != seq) {
                // cancelled: the kernel may still use the buffer, so ask it to
                // stop and wait until it has
                ur_submit(UR_OP_ASYNC_CANCEL, -1, ud, 0, 0);
                while (GOJO_SC_LOAD(&g->io_done) != seq) {
                    g->epoch++;
                    uint64_t t = ((uint64_t)g->epoch << 2) | WAIT_DRAIN;
                    g->wake = WAKE_NONE;
                    GOJO_SC_STORE(&g->wait, t);
                    if (GOJO_SC_LOAD(&g->io_done) == seq) {
                        if (!wait_claim(g, t)) g_park();
                        break;
                    }
                    blk(g, BLK_FILE, NULL);
                    g_park();
                    unblk(g);
                }
            }
        }
        GOJO_ADD(&s_io_waiters, -1);
    }
    GOJO_SC_STORE(&g->io_inflight, 0);
    return g->io_res;
}

#endif

// ------------------------------------------------------------------ file reads and writes
//
// os.File reads and writes don't block a worker thread when they don't have
// to. A pipe, terminal or socket that has nothing to read (or no room to
// write) is waited for like a network connection: the task parks on the
// poller and can be cancelled. On Linux, a regular file whose data isn't in
// the page cache goes through the io_uring, if there is one. Anything else
// (a regular file without RWF_NOWAIT, a macOS file) blocks its thread as
// before, giving its CPU slot away meanwhile.

#define FILE_BLOCK INT64_MIN
// fds known to need the blocking path: regular files (macOS), regular files
// whose filesystem can't do RWF_NOWAIT (Linux). Cleared when opened or closed.
static uint8_t s_fd_block[65536 / 8];

static bool fd_blocks(int fd) { return fd >= 0 && fd < 65536 && (GOJO_RELAXED(&s_fd_block[fd >> 3]) >> (fd & 7) & 1); }

static void fd_set_blocks(int fd, bool on) {
    if (fd < 0 || fd >= 65536) return;
    uint8_t bit = (uint8_t)(1u << (fd & 7));
    if (on) __atomic_fetch_or(&s_fd_block[fd >> 3], bit, __ATOMIC_RELAXED);
    else __atomic_fetch_and(&s_fd_block[fd >> 3], (uint8_t)~bit, __ATOMIC_RELAXED);
}

// Reads or writes `fd` without blocking the thread where it can: the result
// (or -errno), -ECANCELED, or FILE_BLOCK: do it the blocking way (when this
// waited for the fd to be ready, that won't wait).
static int64_t file_io(bool write, int fd, void *p, size_t len) {
    if (fd_blocks(fd) || cur_worker() == NULL || gojo_sim_on) return FILE_BLOCK;
#if defined(__linux__) && defined(__NR_preadv2)
    if (ur_on() && s_ur_always) {
        for (;;) {
            int64_t r = ur_rw(write, fd, p, len);
            if (r != -EAGAIN) return r == UR_FALLBACK ? FILE_BLOCK : r;
            int w = wait_fd_r(fd, write, true);
            if (w == 0) return -ECANCELED;
            if (w < 0) return FILE_BLOCK;
        }
    }
    struct iovec iov = { p, len };
    for (;;) {
        // (offset -1: the file's position; as two longs, low half first)
        ssize_t n = syscall(write ? __NR_pwritev2 : __NR_preadv2, fd, &iov, 1, -1L, 0L, RWF_NOWAIT);
        if (n >= 0) return n;
        int e = errno;
        if (e == EINTR) continue;
        if (e != EAGAIN && e != EOPNOTSUPP && e != EINVAL) return -(int64_t)e;
        struct stat st;
        if (fstat(fd, &st) == 0 && S_ISREG(st.st_mode)) {
            if (e != EAGAIN) {
                // no RWF_NOWAIT on this filesystem
                fd_set_blocks(fd, true);
                return FILE_BLOCK;
            }
            // not in the page cache
            if (!ur_on()) return FILE_BLOCK;
            int64_t r = ur_rw(write, fd, p, len);
            return r == UR_FALLBACK || r == -EAGAIN ? FILE_BLOCK : r;
        }
        // a pipe, terminal or socket: wait until it's ready
        int w = wait_fd_r(fd, write, true);
        if (w == 0) return -ECANCELED;
        if (w < 0 || e != EAGAIN) return FILE_BLOCK;
    }
#else
    (void)p;
    (void)len;
    struct stat st;
    if (fstat(fd, &st) != 0) return FILE_BLOCK;
    if (!S_ISFIFO(st.st_mode) && !S_ISSOCK(st.st_mode)) {
        if (S_ISREG(st.st_mode)) fd_set_blocks(fd, true);
        return FILE_BLOCK;
    }
    struct pollfd pf = { fd, write ? POLLOUT : POLLIN, 0 };
    if (poll(&pf, 1, 0) == 0 && wait_fd_r(fd, write, true) == 0) return -ECANCELED;
    return FILE_BLOCK;
#endif
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

// Splits "host:port", "[v6]:port" or ":port" into C strings.
static bool split_hostport(gojo_str addr, char *host, size_t hcap, char *port, size_t pcap) {
    if (addr.len && memchr(addr.ptr, 0, (size_t)addr.len)) return false;
    int64_t colon = -1;
    for (int64_t i = addr.len - 1; i >= 0; i--) {
        if (addr.ptr[i] == ':') {
            colon = i;
            break;
        }
    }
    if (colon < 0) return false;
    int64_t h0 = 0, h1 = colon;
    if (colon >= 2 && addr.ptr[0] == '[' && addr.ptr[colon - 1] == ']') {
        h0 = 1;
        h1 = colon - 1;
    }
    if (h1 - h0 >= (int64_t)hcap || addr.len - colon - 1 >= (int64_t)pcap) return false;
    memcpy(host, addr.ptr + h0, (size_t)(h1 - h0));
    host[h1 - h0] = 0;
    memcpy(port, addr.ptr + colon + 1, (size_t)(addr.len - colon - 1));
    port[addr.len - colon - 1] = 0;
    return true;
}

// "host:port" or ":port"; an empty host means all interfaces when listening.
static int resolve_as(gojo_str addr, int socktype, int family, bool passive, struct addrinfo **out) {
    char host[256], port[32];
    if (!split_hostport(addr, host, sizeof host, port, sizeof port)) return EINVAL;
    struct addrinfo hints;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = family;
    hints.ai_socktype = socktype;
    if (passive) hints.ai_flags = AI_PASSIVE;
#if defined(AI_V4MAPPED)
    if (family == AF_INET6) hints.ai_flags |= AI_V4MAPPED;
#endif
    // name lookups can take seconds: give the CPU slot away meanwhile
    gojo_blocking_enter();
    int r = getaddrinfo(host[0] ? host : NULL, port, &hints, out);
    gojo_ffi_exit();
    return r == 0 ? 0 : EHOSTUNREACH;
}

static int resolve(gojo_str addr, bool passive, struct addrinfo **out) {
    return resolve_as(addr, SOCK_STREAM, AF_UNSPEC, passive, out);
}

int64_t gojo_net___listen(gojo_str addr) {
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

int64_t gojo_net___connect(gojo_str addr) {
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

int64_t gojo_net___accept(int64_t lfd) {
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

int64_t gojo_net___read(int64_t fd, gojo_vec *buf, int64_t max) {
    if (max <= 0) return 0;
    gojo_vec_reserve(buf, max, 1);
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

int64_t gojo_net___write(int64_t fd, gojo_slice data) {
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

void gojo_net___close(int64_t fd) { close((int)fd); }

void gojo_net___shutdownWrite(int64_t fd) { shutdown((int)fd, SHUT_WR); }

int64_t gojo_net___port(int64_t fd) {
    struct sockaddr_storage ss;
    socklen_t len = sizeof ss;
    if (getsockname((int)fd, (struct sockaddr *)&ss, &len) != 0) return -errno;
    if (ss.ss_family == AF_INET) return ntohs(((struct sockaddr_in *)&ss)->sin_port);
    if (ss.ss_family == AF_INET6) return ntohs(((struct sockaddr_in6 *)&ss)->sin6_port);
    return -EINVAL;
}


// ------------------------------------------------------------------ UDP and DNS

static int udp_open(int family) {
    int fd = socket(family, SOCK_DGRAM, 0);
    if (fd < 0) return -errno;
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

// Binds a datagram socket. ":port" is dual-stack where IPv6 is available.
int64_t gojo_net___udpListen(gojo_str addr) {
    char host[256], port[32];
    if (!split_hostport(addr, host, sizeof host, port, sizeof port)) return -EINVAL;
    struct addrinfo *ai;
    int r = resolve_as(addr, SOCK_DGRAM, host[0] ? AF_UNSPEC : AF_INET6, true, &ai);
    if (r && !host[0]) r = resolve_as(addr, SOCK_DGRAM, AF_INET, true, &ai);
    if (r) return -r;
    int err = EADDRNOTAVAIL;
    for (struct addrinfo *a = ai; a; a = a->ai_next) {
        int fd = udp_open(a->ai_family);
        if (fd < 0) {
            err = -fd;
            continue;
        }
        if (a->ai_family == AF_INET6 && !host[0]) {
            int zero = 0;
            setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &zero, sizeof zero);
        }
        if (bind(fd, a->ai_addr, a->ai_addrlen) == 0) {
            freeaddrinfo(ai);
            return fd;
        }
        err = errno;
        close(fd);
    }
    freeaddrinfo(ai);
    // no IPv6 here: fall back to IPv4 for ":port"
    if (!host[0] && err == EAFNOSUPPORT && resolve_as(addr, SOCK_DGRAM, AF_INET, true, &ai) == 0) {
        for (struct addrinfo *a = ai; a; a = a->ai_next) {
            int fd = udp_open(a->ai_family);
            if (fd >= 0 && bind(fd, a->ai_addr, a->ai_addrlen) == 0) {
                freeaddrinfo(ai);
                return fd;
            }
            err = fd < 0 ? -fd : errno;
            if (fd >= 0) close(fd);
        }
        freeaddrinfo(ai);
    }
    return -err;
}

// A datagram socket whose peer is fixed: `send` and `recv` talk to it alone.
int64_t gojo_net___udpDial(gojo_str addr) {
    struct addrinfo *ai;
    int r = resolve_as(addr, SOCK_DGRAM, AF_UNSPEC, false, &ai);
    if (r) return -r;
    int err = EHOSTUNREACH;
    for (struct addrinfo *a = ai; a; a = a->ai_next) {
        int fd = udp_open(a->ai_family);
        if (fd < 0) {
            err = -fd;
            continue;
        }
        if (connect(fd, a->ai_addr, a->ai_addrlen) == 0) {
            freeaddrinfo(ai);
            return fd;
        }
        err = errno;
        close(fd);
    }
    freeaddrinfo(ai);
    return -err;
}

static int sock_family(int fd) {
    struct sockaddr_storage ss;
    socklen_t len = sizeof ss;
    if (getsockname(fd, (struct sockaddr *)&ss, &len) != 0) return AF_INET;
    return ss.ss_family;
}

// The destination for a datagram in the socket's own family. Literal
// addresses skip the resolver, so a hot `sendTo` loop costs no lookups.
static int dest_addr(int fd, gojo_str addr, struct sockaddr_storage *ss, socklen_t *len) {
    char host[256], port[32];
    if (!split_hostport(addr, host, sizeof host, port, sizeof port) || !host[0]) return EINVAL;
    int family = sock_family(fd);
    char *end;
    long pn = strtol(port, &end, 10);
    if (*port && !*end && pn >= 0 && pn <= 65535) {
        memset(ss, 0, sizeof *ss);
        struct in_addr v4;
        if (family == AF_INET6) {
            struct sockaddr_in6 *s6 = (struct sockaddr_in6 *)ss;
            s6->sin6_family = AF_INET6;
            s6->sin6_port = htons((uint16_t)pn);
            *len = sizeof *s6;
            if (inet_pton(AF_INET6, host, &s6->sin6_addr) == 1) return 0;
            if (inet_pton(AF_INET, host, &v4) == 1) {
                s6->sin6_addr.s6_addr[10] = s6->sin6_addr.s6_addr[11] = 0xff;
                memcpy(&s6->sin6_addr.s6_addr[12], &v4, 4);
                return 0;
            }
        } else if (inet_pton(AF_INET, host, &v4) == 1) {
            struct sockaddr_in *s4 = (struct sockaddr_in *)ss;
            s4->sin_family = AF_INET;
            s4->sin_port = htons((uint16_t)pn);
            s4->sin_addr = v4;
            *len = sizeof *s4;
            return 0;
        }
    }
    struct addrinfo *ai;
    int r = resolve_as(addr, SOCK_DGRAM, family, false, &ai);
    if (r) return r == EHOSTUNREACH && family == AF_INET ? EAFNOSUPPORT : r;
    memcpy(ss, ai->ai_addr, ai->ai_addrlen);
    *len = ai->ai_addrlen;
    freeaddrinfo(ai);
    return 0;
}

#if defined(MSG_NOSIGNAL)
#define GOJO_SEND_FLAGS MSG_NOSIGNAL
#else
#define GOJO_SEND_FLAGS 0
#endif

// Sends one datagram; `addr` empty means the socket's fixed peer.
int64_t gojo_net___sendTo(int64_t fd, gojo_slice data, gojo_str addr) {
    struct sockaddr_storage ss;
    socklen_t len = 0;
    if (addr.len) {
        int r = dest_addr((int)fd, addr, &ss, &len);
        if (r) return -r;
    }
    for (;;) {
        ssize_t n = addr.len ? sendto((int)fd, data.ptr, (size_t)data.len, GOJO_SEND_FLAGS, (struct sockaddr *)&ss, len)
                             : send((int)fd, data.ptr, (size_t)data.len, GOJO_SEND_FLAGS);
        if (n >= 0) return n;
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) return -errno;
        if (!wait_fd((int)fd, true)) return -ECANCELED;
    }
}

// "1.2.3.4:53" or "[::1]:53"; IPv4 peers of a dual-stack socket read as IPv4.
static void format_addr(const struct sockaddr_storage *ss, gojo_string *out) {
    char buf[INET6_ADDRSTRLEN + 16], ip[INET6_ADDRSTRLEN];
    int n = 0;
    if (ss->ss_family == AF_INET6) {
        const struct sockaddr_in6 *s6 = (const struct sockaddr_in6 *)ss;
        int port = ntohs(s6->sin6_port);
        if (IN6_IS_ADDR_V4MAPPED(&s6->sin6_addr)) {
            inet_ntop(AF_INET, &s6->sin6_addr.s6_addr[12], ip, sizeof ip);
            n = snprintf(buf, sizeof buf, "%s:%d", ip, port);
        } else {
            inet_ntop(AF_INET6, &s6->sin6_addr, ip, sizeof ip);
            n = snprintf(buf, sizeof buf, "[%s]:%d", ip, port);
        }
    } else if (ss->ss_family == AF_INET) {
        const struct sockaddr_in *s4 = (const struct sockaddr_in *)ss;
        inet_ntop(AF_INET, &s4->sin_addr, ip, sizeof ip);
        n = snprintf(buf, sizeof buf, "%s:%d", ip, ntohs(s4->sin_port));
    }
    out->len = 0;
    gojo_str s = { (const uint8_t *)buf, n };
    gojo_string_drop(out);
    *out = gojo_string_from(s);
}

// Receives one datagram into `buf` (at most `max` bytes; the rest of a longer
// one is dropped) and its sender into `from` when non-null.
int64_t gojo_net___recvFrom(int64_t fd, gojo_vec *buf, int64_t max, gojo_string *from) {
    if (max <= 0) max = 1;
    gojo_vec_reserve(buf, max, 1);
    for (;;) {
        struct sockaddr_storage ss;
        socklen_t len = sizeof ss;
        ssize_t n = recvfrom((int)fd, (uint8_t *)buf->ptr + buf->len, (size_t)max, 0, (struct sockaddr *)&ss, &len);
        if (n >= 0) {
            buf->len += n;
            if (from) format_addr(&ss, from);
            return n;
        }
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) return -errno;
        if (!wait_fd((int)fd, false)) return -ECANCELED;
    }
}

// The addresses of `host`, IPv4 first. Returns 0, or 1 when there is no such
// host, 2 when the resolver failed for now, 3 for anything else.
int64_t gojo_net___lookup(gojo_str host, gojo_vec *out) {
    char h[256];
    if (host.len == 0 || host.len >= (int64_t)sizeof h || memchr(host.ptr, 0, (size_t)host.len)) return 1;
    memcpy(h, host.ptr, (size_t)host.len);
    h[host.len] = 0;
    struct addrinfo hints, *ai;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    gojo_blocking_enter();
    int r = getaddrinfo(h, NULL, &hints, &ai);
    gojo_ffi_exit();
    if (r) return r == EAI_AGAIN ? 2 : (r == EAI_NONAME || r == EAI_FAIL
#if defined(EAI_NODATA)
                                        || r == EAI_NODATA
#endif
                                        ) ? 1 : 3;
    for (int pass = 0; pass < 2; pass++) {
        for (struct addrinfo *a = ai; a; a = a->ai_next) {
            if (a->ai_family != (pass ? AF_INET6 : AF_INET)) continue;
            char ip[INET6_ADDRSTRLEN];
            const void *src = pass ? (const void *)&((struct sockaddr_in6 *)a->ai_addr)->sin6_addr
                                   : (const void *)&((struct sockaddr_in *)a->ai_addr)->sin_addr;
            inet_ntop(a->ai_family, src, ip, sizeof ip);
            gojo_str s = { (const uint8_t *)ip, (int64_t)strlen(ip) };
            bool dup = false;
            for (int64_t i = 0; i < out->len && !dup; i++) {
                gojo_string *e = &((gojo_string *)out->ptr)[i];
                dup = e->len == s.len && memcmp(e->ptr, s.ptr, (size_t)s.len) == 0;
            }
            if (dup) continue;
            gojo_vec_reserve(out, 1, sizeof(gojo_string));
            ((gojo_string *)out->ptr)[out->len++] = gojo_string_from(s);
        }
    }
    freeaddrinfo(ai);
    return 0;
}


// ------------------------------------------------------------------ task dumps
//
// Every live goroutine with where it was started and what it waits for. The
// reads race with running goroutines: this is for people (and agents)
// reading a stuck program, not for the program itself.

static const char *loc_or(const char *l, const char *d) { return l ? l : d; }

static void dump_task(FILE *f, gojo_g *g, int64_t now) {
    const char *who = g->spawn_loc ? g->spawn_loc : (g->stack_size == s_main_stack_size ? "main" : "<runtime>");
    fprintf(f, "  task %s: ", who);
    double secs = (double)(now - blocked_since(g, now)) / 1e9;
    switch (GOJO_RELAXED(&g->blocked)) {
    case BLK_RECV:
        fprintf(f, "receiving from the channel made at %s", loc_or(((gojo_chan *)g->blocked_on)->loc, "?"));
        break;
    case BLK_SEND:
        fprintf(f, "sending on the channel made at %s", loc_or(((gojo_chan *)g->blocked_on)->loc, "?"));
        break;
    case BLK_SELECT:
        if (g->blocked_on) fprintf(f, "in a select (one channel made at %s)", loc_or(((gojo_chan *)g->blocked_on)->loc, "?"));
        else fprintf(f, "in a select");
        break;
    case BLK_LOCK: {
        gojo_mutex *m = (gojo_mutex *)g->blocked_on;
        gojo_g *o = m->owner;
        fprintf(f, "locking the mutex made at %s", loc_or(m->loc, "?"));
        if (o) fprintf(f, ", held by task %s", o->spawn_loc ? o->spawn_loc : "main");
        break;
    }
    case BLK_JOIN:
        fprintf(f, "joining the task started at %s", loc_or(((gojo_task *)g->blocked_on)->loc, "?"));
        break;
    case BLK_SCOPE: {
        gojo_group *grp = (gojo_group *)g->blocked_on;
        fprintf(f, "waiting for %lld task(s) of the scope at %s", (long long)group_live(grp), loc_or(grp->loc, "?"));
        break;
    }
    case BLK_RWLOCK: fprintf(f, "waiting for an RwLock"); break;
    case BLK_SLEEP: fprintf(f, "sleeping"); break;
    case BLK_IO: fprintf(f, "waiting for the network"); break;
    case BLK_FILE: fprintf(f, "waiting for a file read or write"); break;
    default: fprintf(f, "running or ready"); secs = -1; break;
    }
    if (secs >= 0) fprintf(f, " (%.3f s)", secs);
    if (g->nheld > 0) {
        fprintf(f, "; holds the mutex made at %s", g->held[0]);
        for (int i = 1; i < g->nheld && i < 8; i++) fprintf(f, ", %s", g->held[i]);
    }
    fprintf(f, "\n");
}

static void dump_tasks(FILE *f) {
    int64_t now = gojo_time_now();
    uint32_t n = GOJO_LOAD(&s_g_count);
    for (uint32_t i = 0; i < n; i++) {
        gojo_g *g = g_slot(i);
        if (GOJO_RELAXED(&g->alive)) dump_task(f, g, now);
    }
}

void gojo_task_dump(void) {
    fflush(stdout);
    fprintf(stderr, "tasks:\n");
    dump_tasks(stderr);
    fflush(stderr);
}


// Every goroutine is parked and nothing (timer, network) can wake one.
static _Noreturn void deadlock(void) {
    fflush(stdout);
    fprintf(stderr, "panic: all tasks are blocked: deadlock\n");
    dump_tasks(stderr);
    // a cycle of tasks waiting for each other's locks
    uint32_t n = GOJO_LOAD(&s_g_count);
    for (uint32_t i = 0; i < n; i++) {
        gojo_g *g = g_slot(i);
        if (!g->alive || g->blocked != BLK_LOCK) continue;
        gojo_g *cur = g;
        for (uint32_t steps = 0; steps <= n && cur && cur->blocked == BLK_LOCK; steps++) {
            cur = ((gojo_mutex *)cur->blocked_on)->owner;
            if (cur == g) {
                fprintf(stderr, "  the tasks above wait for each other's locks: take them in one order everywhere, or together with `lock(a, b)`\n");
                i = n;
                break;
            }
        }
    }
    fflush(stderr);
    _exit(2);
}

static void on_quit(int sig) {
    (void)sig;
    gojo_task_dump();
    _exit(2);
}

// ------------------------------------------------------------------ lock order (debug builds)
//
// Each mutex is classed by where it was made. Taking B while holding A
// records A -> B; finding the reverse path means two tasks could deadlock,
// even if this run didn't. Reported once per pair.

typedef struct { const char *from, *to; } lock_edge;
static lock_edge *s_edges;
static int s_nedges, s_cap_edges;
static gojo_spin s_edge_lock;

static bool edge_path(const char *from, const char *to, int depth) {
    if (from == to) return true;
    if (depth > 32) return false;
    for (int i = 0; i < s_nedges; i++)
        if (s_edges[i].from == from && edge_path(s_edges[i].to, to, depth + 1)) return true;
    return false;
}

static void lockdep_acquire(gojo_mutex *m) {
    gojo_g *g = cur_g();
    if (!g || !m->loc) return;
    spin_lock(&s_edge_lock);
    for (int h = 0; h < g->nheld && h < 8; h++) {
        const char *a = g->held[h];
        if (a == m->loc) continue;
        bool known = false;
        for (int i = 0; i < s_nedges && !known; i++) known = s_edges[i].from == a && s_edges[i].to == m->loc;
        if (known) continue;
        if (edge_path(m->loc, a, 0)) {
            fprintf(stderr, "warning: possible deadlock: a task takes the mutex made at %s while holding the one made at %s, and somewhere they are taken the other way round\n  take them in one order everywhere, or together with `lock(a, b)`\n", m->loc, a);
        }
        if (s_nedges == s_cap_edges) {
            s_cap_edges = s_cap_edges ? s_cap_edges * 2 : 64;
            s_edges = gojo_realloc(s_edges, (size_t)s_cap_edges * sizeof *s_edges);
        }
        s_edges[s_nedges++] = (lock_edge){ a, m->loc };
    }
    spin_unlock(&s_edge_lock);
}

static void lockdep_release(gojo_mutex *m) {
    gojo_g *g = cur_g();
    if (!g || !m->loc || g->nheld == 0) return;
    int n = g->nheld < 8 ? g->nheld : 8;
    for (int i = n - 1; i >= 0; i--) {
        if (g->held[i] == m->loc) {
            for (int j = i; j + 1 < n; j++) g->held[j] = g->held[j + 1];
            break;
        }
    }
    g->nheld--;
}

// ------------------------------------------------------------------ simulation
//
// `gojo test` (and GOJO_SEED=n for any program) runs on one thread with a
// seeded scheduler: which runnable task goes next, and whether a task gives
// way at a channel operation, lock, spawn or join, comes from the seed. The
// clock is virtual: when every task waits, it jumps to the next timer. Since
// tasks only affect each other through these operations (no data races),
// the seed decides everything that can vary -- except the network, which is
// waited for in real time.


static uint64_t sim_rand(void) {
    s_sim_rng ^= s_sim_rng << 13;
    s_sim_rng ^= s_sim_rng >> 7;
    s_sim_rng ^= s_sim_rng << 17;
    return s_sim_rng;
}

void gojo_sim_setup(uint64_t seed) {
    gojo_sim_on = 1;
    s_sim_rng = seed * 0x9E3779B97F4A7C15ULL + 0x632BE59BD9B4E019ULL;
    if (!s_sim_rng) s_sim_rng = 1;
    gojo_sim_clock = 0;
    // every checkpoint counts down to a random preemption
    __atomic_store_n(&gojo_preempt_flag, 1, __ATOMIC_RELAXED);
}

// Called with s_lock not held, from any thread (the poller delivers network events).
static void sim_push(gojo_g *g) {
    pthread_mutex_lock(&s_lock);
    if (s_simq_len == s_simq_cap) {
        s_simq_cap = s_simq_cap ? s_simq_cap * 2 : 256;
        s_simq = gojo_realloc(s_simq, (size_t)s_simq_cap * sizeof *s_simq);
    }
    s_simq[s_simq_len++] = g;
    if (s_sim_sleeping) pthread_cond_signal(&s_workers[0]->cv);
    pthread_mutex_unlock(&s_lock);
}


static void timers_fire(int64_t now);

// Nothing runnable: advance the clock to the next timer, or wait for the
// network. False: nothing can ever wake anyone.
static bool sim_idle(gojo_worker *w) {
    pthread_mutex_lock(&s_timer_lock);
    int64_t next = s_heap_len > 0 ? s_heap[0]->when : -1;
    pthread_mutex_unlock(&s_timer_lock);
    int64_t io = GOJO_SC_LOAD(&s_io_waiters);
    if (next >= 0 && io == 0) {
        if (next > gojo_sim_clock) __atomic_store_n(&gojo_sim_clock, next, __ATOMIC_RELAXED);
        timers_fire(gojo_sim_clock);
        return true;
    }
    if (io > 0) {
        // the network runs in real time; so does the clock meanwhile
        struct timespec t0, t1;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        pthread_mutex_lock(&s_lock);
        s_sim_sleeping = true;
        while (s_simq_len == 0) {
            if (next >= 0) {
                int64_t wait = next - gojo_sim_clock;
                if (wait <= 0) break;
                struct timespec ts;
                clock_gettime(CLOCK_REALTIME, &ts);
                int64_t at = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec + wait;
                ts.tv_sec = (time_t)(at / 1000000000);
                ts.tv_nsec = (long)(at % 1000000000);
                if (pthread_cond_timedwait(&w->cv, &s_lock, &ts) == ETIMEDOUT) break;
            } else {
                pthread_cond_wait(&w->cv, &s_lock);
            }
        }
        s_sim_sleeping = false;
        pthread_mutex_unlock(&s_lock);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        int64_t dt = ((int64_t)t1.tv_sec - t0.tv_sec) * 1000000000 + (t1.tv_nsec - t0.tv_nsec);
        __atomic_store_n(&gojo_sim_clock, gojo_sim_clock + (dt > 0 ? dt : 0), __ATOMIC_RELAXED);
        if (next >= 0 && gojo_sim_clock < next && s_simq_len == 0) __atomic_store_n(&gojo_sim_clock, next, __ATOMIC_RELAXED);
        timers_fire(gojo_sim_clock);
        return true;
    }
    return false;
}

void gojo_sim_point(void) { sim_point(); }

// state: 0 not computed, 1 being computed (by `owner`), 2 done. Others wait.
void gojo_once_run(gojo_once *o, void (*init)(void), const char *name) {
    gojo_worker *w = cur_worker();
    void *me = w && w->cur ? (void *)w->cur : (void *)&s_lock;
    for (;;) {
        int st = __atomic_load_n(&o->state, __ATOMIC_ACQUIRE);
        if (st == 2) return;
        int zero = 0;
        if (st == 0 && __atomic_compare_exchange_n(&o->state, &zero, 1, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
            __atomic_store_n(&o->owner, me, __ATOMIC_RELEASE);
            init();
            __atomic_store_n(&o->state, 2, __ATOMIC_RELEASE);
            return;
        }
        if (__atomic_load_n(&o->owner, __ATOMIC_ACQUIRE) == me) {
            char msg[256];
            snprintf(msg, sizeof msg, "the value of `%s` depends on itself", name);
            gojo_panic(msg, "<runtime>");
        }
        if (w && w->cur) gojo_task_yieldNow();
        else sched_yield();
    }
}

static gojo_g *sim_find_work(gojo_worker *w) {
    // time passes while tasks compute too: a microsecond per scheduling step,
    // so a task that computes until a deadline gets there
    __atomic_store_n(&gojo_sim_clock, gojo_sim_clock + 1000, __ATOMIC_RELAXED);
    if (GOJO_RELAXED(&s_heap_len) > 0) timers_fire(gojo_sim_clock);
    for (;;) {
        pthread_mutex_lock(&s_lock);
        if (s_simq_len > 0) {
            int i = (int)(sim_rand() % (uint64_t)s_simq_len);
            gojo_g *g = s_simq[i];
            s_simq[i] = s_simq[--s_simq_len];
            pthread_mutex_unlock(&s_lock);
            return g;
        }
        pthread_mutex_unlock(&s_lock);
        if (!sim_idle(w)) deadlock();
    }
}

// ------------------------------------------------------------------ tests

// One run of a test in a process of its own; seed 0: the real scheduler.
static bool run_test_once(void (*fn)(void), uint64_t seed) {
    fflush(stdout);
    fflush(stderr);
    pid_t pid = fork();
    if (pid == 0) {
        if (seed) gojo_sim_setup(seed);
        gojo_sched_main(fn);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

// Each test gets a process, and a scheduler, of its own. The scheduler is
// simulated (see "simulation") with a seed taken from the test's name, so a
// test does the same thing every run; GOJO_SEED picks another seed,
// GOJO_EXPLORE=n tries n seeds, GOJO_SCHED=real uses the real scheduler.
int gojo_run_test(void (*fn)(void), const char *name) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    int64_t t0 = (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
    const char *sched = getenv("GOJO_SCHED");
    bool real = sched && strcmp(sched, "real") == 0;
    uint64_t seed = 14695981039346656037ULL;
    for (const char *p = name; *p; p++) seed = (seed ^ (uint8_t)*p) * 1099511628211ULL;
    seed = seed % 1000000 + 1;
    const char *env = getenv("GOJO_SEED");
    if (env && *env) seed = strtoull(env, NULL, 10);
    long runs = 1;
    env = getenv("GOJO_EXPLORE");
    if (env && atol(env) > 1) runs = atol(env);
    bool ok = true;
    uint64_t failed_seed = 0;
    long done = 0;
    for (long r = 0; r < runs && ok; r++) {
        uint64_t s = real ? 0 : seed + (uint64_t)r;
        ok = run_test_once(fn, s);
        done++;
        if (!ok) failed_seed = s;
    }
    clock_gettime(CLOCK_MONOTONIC, &ts);
    double ms = (double)((int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec - t0) / 1e6;
    if (ok) {
        if (runs > 1) fprintf(stderr, "  ok   %s (%ld schedules, %.1f ms)\n", name, done, ms);
        else fprintf(stderr, "  ok   %s (%.1f ms)\n", name, ms);
    } else if (failed_seed) {
        fprintf(stderr, "  FAIL %s (seed %llu; rerun it with `gojo test --seed %llu`)\n", name, (unsigned long long)failed_seed, (unsigned long long)failed_seed);
    } else {
        fprintf(stderr, "  FAIL %s (%.1f ms)\n", name, ms);
    }
    return ok ? 0 : 1;
}

// ------------------------------------------------------------------ task stats

int64_t gojo_task___numGoroutines(void) { return GOJO_SC_LOAD(&s_ngoroutines) + 1; }
int64_t gojo_task___numThreads(void) { return GOJO_SC_LOAD(&s_nstarted); }
int64_t gojo_task___maxProcs(void) { return GOJO_RELAXED(&s_nprocs); }
int64_t gojo_task___numHibernated(void) { return GOJO_RELAXED(&s_nhib); }

int64_t gojo_task___setMaxProcs(int64_t n) {
    if (n < 1) n = 1;
    if (n > MAX_WORKERS) n = MAX_WORKERS;
    int old = __atomic_exchange_n(&s_nprocs, (int)n, __ATOMIC_SEQ_CST);
    // more room: start using it; less: workers park as they reach the scheduler
    if (n > old) maybe_wake();
    return old;
}
