# Concurrency (design notes)

Status: implemented (Nov 2026). This file keeps the reasons; the rules are in the language reference, [spec.md](spec.md) section 10. It started as a proposal to redesign Gojo's goroutines; the last section lists where the result differs from the proposal, and what was left out.

## Where Gojo was

Before this, Gojo had stackful goroutines on an M:N work-stealing scheduler, Go's channels and `select`, `Mutex[T]` whose `lock` is a projection, `task.group` whose children could borrow from outside the group, cancellation down the task tree, `task.timeout`, preemption through compiler-inserted checkpoints, and data races ruled out by `send` and ownership. The problems:

| Problem | Example | Why it mattered |
| --- | --- | --- |
| **A soundness hole** | `task.Group` was an `unsafe send copy` struct: sending `g` out of its group, then calling `g.cancel()`, was a heap use-after-free in safe code (confirmed under AddressSanitizer) | A handle to a running scope must be a view |
| Tasks couldn't return values | Results went through a `Mutex[Vec[T]]` or a channel, then were sorted back into order | The most common concurrent code (fetch N things, combine) was the most awkward |
| Tasks couldn't write anything | Not even a variable no other task touched | Parallel work on disjoint data needed locks |
| Bare `go` was unstructured | Detached, never cancelled, never joined | Goroutine leaks (Go needs a GC-based leak profiler, since 1.26, to find them) |
| Go's channel edges | `close` twice and send after close panicked; nobody closed when the last sender left; with no nil, no way to disable a `select` arm | Panics in production, and a missing idiom |
| `select` saw only channels | No timers, task results or cancellation | Timeouts in a loop needed extra goroutines |
| Blunt cancellation | Cleanup that had to block failed silently in a cancelled task; no reason, no deadline to pass on | Lost data on shutdown |
| All-or-nothing sharing | A finished `rc` tree could never be sent; no interface or function value was `send`; no atomics | `arc` everywhere, or copies |
| Memory per goroutine | 100,000 parked goroutines: 1.67 GB on macOS (one 16 KB page each), 293 MB in Go | Servers with many idle connections |
| Irreproducible bugs | Deadlocks and logic races appeared only sometimes | Agents, Gojo's main users, can't fix what they can't rerun |

## What newer systems taught

| System | What it got right | What we skip |
| --- | --- | --- |
| Trio / AnyIO (Python) | Nurseries: no task outlives its parent. Cancel scopes with deadlines, level-triggered cancellation, and *shielding* for cleanup | Exceptions as the error channel |
| Swift 6 | `async let` and task groups; an `async let` that is never awaited is cancelled and awaited at scope exit. `Sendable`; region-based isolation (SE-0414) and `sending` (SE-0430) let a non-Sendable value cross threads when nothing else can reach it. Task-local values | Actors with reentrancy as the default model; function colouring. Even Swift 6.2 had to make code run on the caller's actor by default to make it approachable |
| Java 25–28 | Virtual threads (no colouring). `StructuredTaskScope` with *Joiner* policies (all must succeed, first success wins), final in JDK 28. `ScopedValue` (final in 25): immutable values inherited by child tasks | Seven previews to settle the API: policies need to be few and simple |
| Zig 0.16 `std.Io` | `async` (may run inline) vs `concurrent` (must run in parallel) are different requests. `Future.cancel` is always allowed and idempotent, so `defer f.cancel()` is the cleanup idiom. `Group` and `Select` | Explicit `io` parameter everywhere |
| Kotlin coroutines | `coroutineScope` / `supervisorScope`; structured concurrency as the default | Colouring (`suspend`) |
| OCaml 5 Eio | Effects give direct-style code; a `Switch` scopes fibers *and* resources; capabilities passed explicitly | User-visible effect handlers: too much language for Gojo |
| Rust | `Send`/`Sync`; `std::thread::scope` lets threads borrow; rayon's fork-join with work stealing and `split_at_mut` | Async: cancelling a future at an `.await` silently drops state, and an io_uring buffer can be freed while the kernel still writes to it. Stackful tasks don't have this problem |
| Pony, Inko | `iso` and `recover`: build a value with any aliasing inside a block that only sees sendable values, and the result is safe to send | A capability for every reference; actors only |
| Verona (BoC, OOPSLA 2023) | `when (a, b) { }` acquires several resources together: atomic, deadlock-free, race-free | Asynchronous-only behaviours |
| Concurrent ML | `select` over first-class events: channel operations, timers, results; guards; negative acknowledgements | The full event algebra: most of its power is covered by guards plus "losing tasks are cancelled" |
| Go 1.25–1.27 | `testing/synctest`: a bubble with a virtual clock that advances only when every goroutine in it is blocked. Leak profile via GC reachability | Detached goroutines in the first place |
| Shuttle, Loom, PCT, FoundationDB | Run the program under a controlled scheduler; replay any interleaving from a seed; PCT finds a bug of depth *d* with probability at least 1/(n·k^(d-1)) per run | Instrumenting every memory access: Gojo doesn't need to (see "Deterministic testing") |
| libco (Tencent) | Copy a coroutine's used stack out when it switches away, so millions of coroutines fit in memory | Copying on every switch, and the dangling pointers into swapped-out stacks |

The common thread: **every task has a parent that outlives it**, results come back as values, cancellation is part of the scope, and tests can control the scheduler. Gojo's borrow checker lets it go further than any of them, because a scope is exactly the lifetime the checker already reasons about.

## The design in one page

```zig
use std/{task, time}

// `scope` waits for the tasks started in it with `go`; they borrow what
// outlives it, and `let t = go f()` gives the result back with `t.join()`.
fn dashboard(id int) Page ! Error {
    return scope {
        let user = go fetchUser(id)            // Task[User ! Error]
        let orders = go fetchOrders(id)
        Page{user: try user.join(), orders: try orders.join()}
    }   // a failing task cancels the other; leaving early cancels both
}

fn stats(words []String) {
    var longest = 0
    let total = Atomic.new(0)
    scope {
        go () => {                             // writes `longest`: nothing else may touch it
            for w in words { longest = max(longest, w.len()) }
        }
        par for w in words {                   // one task per CPU
            _ = total.add(w.len())
        }
    }
    println(longest, total.load())
}

// Channels have two ends; the last sender gone closes the channel.
fn pipeline(jobs []Job) {
    let ch = chan[Job](16)
    go produce(jobs, ch.tx)                     // the task takes the sender
    for j in ch.rx {                            // ends when `produce` returns
        run(j)
    }
}

// `select` over channels, timers and cancellation, with guards.
fn worker(jobs Receiver[Job], busy bool) {
    for {
        select {
            let j = <-jobs if !busy => run(j.unwrap())
            time.after(5 * time.second) => println("idle")
            cancelled => return
        }
    }
}

// Cleanup survives cancellation, for a while; Ctrl-C cancels `main`.
struct Conn {
    fn drop(self inout) { _ = self.writeText("bye\n") }   // runs shielded (1 s)
}

// A graph of `rc` nodes, built from nothing shared, crosses tasks as a whole.
let tree = isolate { buildTree(src) }         // Iso[Tree]
go index(tree)                                  // index(t sink Iso[Tree]) { let tr = t.take() ... }
```

Tests run on a simulated scheduler: `gojo test --explore 1000` tries a thousand interleavings and prints the seed of a failing one, which `--seed` replays exactly.

## 1. Scopes replace groups and bare `go`

- `scope { }` is a block, not a closure (`task.group((g) => { g.go(() => ...) })` had two levels of closures), so `try`, `return` and `break` inside mean what they mean everywhere else. Leaving early cancels the tasks, then waits.
- `go` finds its scope lexically: the innermost one in the same function. A library function that starts long-lived tasks takes a `Scope` parameter, so a signature says when work outlives the call. `main` and tests are scopes, so `go` there works as in Go, except that `main` waits for its tasks (it doesn't kill them).
- **Every task has a parent that waits for it**, so leaks can't happen and tasks can borrow their parent's data: a scope is exactly the kind of block the borrow checker already reasons about. A borrow by a task lasts until the scope ends; one task may write a variable nothing else uses. The scope waits before dropping what its body declared, so tasks may borrow the body's own variables too; values declared in blocks inside the body move into the task.
- **Handles are views** (`Task[T]`, `Scope`): they can't be stored or sent, which also closes the old `Group` hole. A handle must be joined or cancelled on every normal path (`task t is never joined`), the explicit form of Swift's rule that an `async let` nobody awaits is cancelled.
- **Failure**: every failing task cancels its scope. The error of a plain `go` task is the scope's (`try scope { }`); the error of a task with a handle goes to `join`. A task that may fail without stopping the others converts its error itself (`f().ok()`, `catch`).
- `go f(a, b)` evaluates arguments that aren't places before the task starts, as Go does; places are borrowed (or moved, for `sink` parameters), which is what makes `go worker(jobs.rx)` move one end of a channel into a task.
- Tasks borrow **the fields they use**: `ch.rx` leaves `ch.tx` free to drop. Closures in general still capture whole variables.

## 2. Data parallelism

`par for x in &v { }` gives each iteration its element, mutably, and splits the index range between one helper task per CPU (chunks taken from a shared counter). Its body is a `send fn`: it may only read what it captures. The proposal's recursive `go psort(&v[..mid])` example is not possible: Gojo has no mutable slice views at all, which would be a separate language feature. `par for` and `task.map` cover the common cases.

**`send fn`** was not in the proposal. Helpers like `task.map(xs, limit, f)` call `f` from several tasks at once, so `f` must capture only `send` values and only read them. A closure gets that type by itself when it qualifies (the parameter type asks for it); function items always do. Without it, every helper would have had to be a compiler intrinsic taking a closure literal.

## 3. Channels

`chan[T](n)` returns `Chan[T] { tx, rx }`. Ownership knows when the last `Sender` is gone, so that closes the channel: no `close`, nothing panics. The last `Receiver` gone makes sends fail (`tx.send(v)` returns `false`; the proposal's `! Unsent[T]` giving the value back seemed more than this needs) instead of blocking forever, which was a classic Go leak. This is Rust's and crossbeam's model; Go can't do it, having no destructors.

## 4. `select`

Arms on task handles (`let r = <-t`), `time.after(d)`, `time.at(t)`, `cancelled`, and `if` guards, which replace Go's nil-channel trick. The other half of Concurrent ML's negative acknowledgements, cancelling what lost, comes from scopes: run the other operation as a task and select on its handle.

## 5. Cancellation

Reasons (`task.reason()`: failed, deadline, explicit, shutdown), `task.deadline()` to pass a budget on, `task.shield(d, f)` (Trio's shielding, always bounded), and `drop` methods that run shielded for up to a second when their task is cancelled, so a `bufio` writer still flushes. New since the proposal: SIGINT and SIGTERM cancel `main`'s scope, so every program shuts down cleanly by default and exits with 130; a second signal kills it.

## 6. Sharing

`send interface`, `Atomic[T]` for integers (sequentially consistent only: one memory order to learn), `with a, b = lock(m1, m2)` (several locks in one global order, the synchronous half of Verona's `when`), and a compile error for waiting directly inside a lock block (the indirect cases are caught by the lock-order checks, section 8). `isolate { }` is Pony's `recover` without capabilities: the body may only use `send` values from outside, so nothing outside points into what it builds, and the result `Iso[T]` may be sent even if it holds `rc` or `Cell`. All of this is checked within one function body.

## 7. Stack hibernation

Stacks can't move, but they can be put away and brought back at the same address, so every pointer into them stays valid. A task parked for more than 200 ms, among more than 256, whose scopes have no running tasks (they might be reading its stack), has its used bytes copied out and its pages released. For this to be safe, nothing may read a parked stack: wait records (channel waiters, timers) moved from the stack into the task's descriptor or the heap, and the one remaining access, a channel copying a value into or out of a parked task's stack, first brings the stack back, under a per-task lock. The scheduler brings it back before running the task (a Dekker-style handshake on `on_cpu` and `hib`). It is libco's copying stacks made safe: the compiler and runtime know every way another task can point into a stack.

Measured on an Apple M5 (16 KB pages): 100,000 tasks parked on a channel use 1.72 GB without hibernation and 0.20 GB with it. Putting a stack away costs about 1.3 µs and bringing it back 0.8 µs.

## 8. Tools

- **Simulated scheduler** for `gojo test` (and `gojo run --seed n`): one thread, a seeded choice of the next task and of yields at every channel operation, lock, spawn, join and atomic, a virtual clock (1 µs per step; jumps to the next timer when all wait). Because Gojo has no data races, tasks affect each other only through these operations, so the seed fixes the interleaving: no need to instrument memory accesses as Loom and Shuttle do. `--explore n` runs n seeds. Programs that use the network are not fully repeatable: network events happen in real time.
- **Deadlock reports** list every task, where it was started, what it waits for (channel, mutex, scope or task, and where that was made), which task holds the mutex, and say when tasks wait for each other's locks. `task.dump()` and SIGQUIT print the same.
- **Debug builds** warn when two mutexes (classed by where they were made) are taken in opposite orders anywhere (Linux's lockdep), and note a scope that has waited 10 s for its tasks.

## Differences from the proposal, and what was left out

- Task-local values (`task.Local[T]`) were left out: Gojo has no package-level variables to declare the keys in, and adding them is a language change of its own. Values that a call chain needs still travel as parameters.
- `RwLock[T]` and `Once` / `Lazy[T]` were left out: `Once` and `Lazy` need package-level variables too, and `RwLock` is a library type that can come when there is a use for it.
- io_uring is used for what readiness can't do: on Linux, a regular-file read or write that would wait for the disk goes through the ring and parks its task (found with `RWF_NOWAIT` first, so cached data still costs one syscall). Pipes, terminals and sockets opened as files wait on the poller like the network (epoll or kqueue), so they are cancellable everywhere, even where seccomp blocks io_uring. Sockets stay on epoll: an optimistic syscall plus readiness is already one syscall when data is there, and a buffer the kernel owns can't be freed by cancellation. Tested in a container (`scripts/linux-test.sh`), with the ring on, off, and taking every file operation.
- Spawning got slower: 1,000,000 short tasks take 0.50 s (0.38 s before; Go: 0.26 s). Each task joins its scope's counter, which a million tasks in one scope share. Joining the member list that cancellation walks only when a task first waits, and counting channel ends with atomics, took it down from 0.72 s.
- Mutable sub-slices for tasks (`go psort(&v[..mid])`) need mutable slice views, which Gojo doesn't have; `par for` covers element-wise work.
- Added beyond the proposal: `send fn`, field-precise task captures, Ctrl-C cancelling `main`, `task.stats().hibernated`.

## Sources

- Swift: [SE-0414 Region-based isolation](https://github.com/apple/swift-evolution/blob/main/proposals/0414-region-based-isolation.md); [Swift 6.2 approachable concurrency](https://www.infoq.com/news/2025/08/swift62-approachable-concurrency)
- Java: [JEP 543, structured concurrency final in JDK 28](https://www.infoq.com/news/2026/10/jep-543-jdk-28/); [JEP 506 Scoped Values](https://openjdk.org/jeps/506); [Structured concurrency guide (JDK 25)](https://docs.oracle.com/en/java/javase/25/core/structured-concurrency.html)
- Zig: [LWN on Zig's new I/O interface](https://lwn.net/Articles/1068409/); [Zig cookbook: Io tasks](https://cookbook.ziglang.cc/16-07-io-tasks/)
- Go: [Go 1.26 release notes (goroutine leak profile)](https://go.googlesource.com/website/+show/HEAD/_content/doc/go1.26.md); [testing/synctest](https://pkg.go.dev/testing/synctest)
- Trio/AnyIO: [Cancellation and timeouts](https://anyio.readthedocs.io/en/stable/cancellation.html)
- OCaml: [Eio](https://github.com/ocaml-multicore/eio)
- Verona: [Behaviour-Oriented Concurrency artifacts](https://github.com/microsoft/verona-artifacts)
- Pony / Inko: [Pony reference capabilities](https://tamnd.github.io/brain/research/memory-management/ownership/06_pony_refcaps/); [Inko](https://inko-lang.org)
- Concurrent ML: [Transactional events (reproduces the CML signatures)](https://www.cs.cornell.edu/people/fluet/research/tx-events/ICFP06/icfp06.pdf)
- Testing: [PCT paper (ASPLOS 2010)](https://people.cs.rutgers.edu/%7Esn349/papers/asplos2010.pdf); [Shuttle](https://docs.rs/crate/shuttle/0.9.1/source/README.md)
- Rust async and io_uring cancellation: [Rust async traps](https://qouteall.fun/qouteall-blog/2026/Rust%20async%20traps)
- Copying stacks: [libco](https://github.com/hhy5277/libco/blob/master/README.md)
