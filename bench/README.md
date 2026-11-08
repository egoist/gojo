# Performance comparison: Gojo vs Go vs Zig

8 common benchmarks (most from the Benchmarks Game). Each one is written in all three languages with exactly the same algorithm and input. `run.sh` first checks that the output of every version is byte-for-byte identical, then measures with hyperfine.

A separate **MoonBit / Go / Gojo** native comparison (same eight workloads, medians, raw CSV/JSON) is in [`compare-moonbit/`](compare-moonbit/README.md).

```sh
./run.sh            # RUNS=10, results are written to results.md
```

## Environment

Apple M5 (10 cores), 32 GB of memory, macOS. Gojo prototype (C back end, Apple clang 21, `--release` = `-O2`), Go 1.27.3, Zig 0.17.0. The programs themselves are all single-threaded; Go's GC runs in parallel on other cores. Zig uses the system `malloc` (`std.heap.c_allocator`).

- **Gojo `--release`**: integer overflow checks and index bounds checks are on (Gojo's default).
- **Go**: index bounds checks; integers wrap silently.
- **Zig ReleaseFast**: no checks at all. **Zig ReleaseSafe**: overflow and bounds checks are on; its safety level is the closest to Gojo.

## Run time

Mean of 10 hyperfine runs. Lower is better.

| Benchmark | What it tests | Gojo | Go | Zig ReleaseFast | Zig ReleaseSafe |
| --- | --- | --- | --- | --- | --- |
| fib(40) | Function calls | 247 ms | 237 ms | **152 ms** | 248 ms |
| binary-trees(18) | Many small allocations | **245 ms** | 669 ms | 957 ms | 963 ms |
| nbody(5M steps) | Floating point, structs | 150 ms | 155 ms | **98 ms** | **98 ms** |
| spectral-norm(2000) | Floating point, nested loops | 106 ms | 134 ms | **101 ms** | 102 ms |
| fannkuch-redux(10) | Integer arrays | 129 ms | 123 ms | 116 ms | **110 ms** |
| mandelbrot(2000) | Pure floating-point loops | **102 ms** | 161 ms | 141 ms | 138 ms |
| hashmap | Integer keys / string keys | **80 ms** | 92 ms | 93 ms | 102 ms |
| sort(5M integers) | Sorting | **260 ms** | 309 ms | 380 ms | 429 ms |

Geometric mean: Gojo takes **0.78×** the time of Go, **0.80×** of Zig ReleaseSafe and **0.87×** of Zig ReleaseFast. The binary-trees gap comes mainly from the allocator (see below); without it the figures are 0.87×, 0.94× and 1.03×.

These programs are all single-threaded, but Gojo programs already run on the M:N scheduler (`main` is the first goroutine).

Cost of the preemption checkpoints, compared with commit `d1f7d13`, which has no checkpoints (CPU cycle counts, minimum of 9 runs; the wall-clock times in the table were measured at that commit, and the machine had other load later, so cycle counts are used here):

| Benchmark | Change in cycles |
| --- | --- |
| fib | −11.6% |
| binary-trees | −0.5% |
| spectral-norm | −2.7% |
| hashmap | +1.0% |
| sort | +2.7% |
| mandelbrot | +3.1% |
| nbody | +3.6% |
| fannkuch | +5.2% |

Geometric mean −0.2%. fib getting faster is not due to the checkpoints (fib gains one load and one branch, and executes +35% instructions); it is most likely a code layout effect. Do not read it as an optimization.

Where the checkpoints go was decided by measuring each case:

- **Entries of possibly recursive functions, and loops that contain nested loops, calls through function values, or runtime operations (Map, allocation, formatting)**: checked every time. Compared with the work in the loop body, one load and one branch are negligible.
- **Pure computational counting loops**: the loop body is generated twice. One copy runs in chunks of 65536 iterations with a check between chunks; the other is a plain loop for the remainder. A short loop pays only one extra comparison. Chunking alone should not be applied to every loop: with only one copy of the body, the C compiler cannot prove that indexes are in bounds, and spectral-norm gets 15% slower.
- **Other pure computational loops** (`while cond {}`): not checked; the enclosing loop takes care of it. If such a loop does not stop, the scheduler treats it as a C call after 20 ms and gives up its CPU slot.
- **The slow path is an assembly trampoline**: the C compiler sees an `asm` statement that changes nothing, so it does not need to move values out of registers. The trampoline saves all registers itself (on arm64 including v0–v31; on x86-64 with XSAVE). This is valid because a preemption point is not a synchronization point: Gojo code can only see writes from other goroutines through locks and channels, and those are real function calls.

`--cc "cc -DGOJO_NO_POLL"` removes the checkpoints for comparison.

## Optimizations done

In the first round of measurements, Gojo took 1.10× the time of Go and 1.09× of Zig ReleaseSafe. Controlled experiments (removing checks or rewriting code in the generated C and measuring again) found the causes below. All of them are fixed:

| Optimization | Benchmark affected | Before | After |
| --- | --- | --- | --- |
| `inline` hint on small functions | spectral-norm | 152 ms | 100 ms |
| Sorting generated per element type | sort | 415 ms | 259 ms |
| Map lookup generated per key type; faster string hashing and integer formatting | hashmap | 107 ms | 79 ms |
| none represented as a null pointer for `?Box[T]` etc. | binary-trees | 938 ms | 887 ms |
| `Box`/`Rc`/`Arc`/interface boxes allocated from size-class free lists | binary-trees | 887 ms | 268 ms |
| Free lists located by reading the thread register directly (no longer through the macOS thread-local variable function) | binary-trees | 268 ms | 245 ms |

- **Overflow checks are cheap; failing to inline is expensive.** The spectral-norm hot spot `a(i, j)` is a single line, but it carries 5 panic branches for overflow checks. clang judged it too large and did not inline it into the inner loop. With an `inline` hint, Gojo with all checks is as fast as Zig ReleaseSafe (100 vs 99 ms). Removing the checks saves only another 4 ms. The compiler now adds this hint to every function with at most 60 typed IR nodes.
- **The fib gap is the overflow checks themselves.** The recursive `fib(n-1) + fib(n-2)` cannot be proven not to overflow: Gojo at 247 ms matches Zig ReleaseSafe at 246 ms, and without checks it is 159 ms, close to Zig ReleaseFast. This is the cost of "release builds check overflow by default". Options are turning checks off per package, or removing provably non-overflowing checks in the back end.
- **Sorting**: this used to be a generic merge sort that compared through a function pointer and moved elements with `memcpy`. Code is now generated per element type: integers, floats and strings use introsort (comparisons are inlined; the order of equal elements is not observable for these types). Other types use a stable merge sort, also generated per type, with direct calls for comparison.
- **Map**: lookup, insertion and deletion are generated per key type, with hashing and equality inlined instead of going through function pointers. Growing and reindexing depend only on the stored hashes and remain generic code in the runtime. The integer-key part is already faster than Go (53 vs 61 ms). String hashing now processes 8 bytes at a time, and `fmt.sprint(int)` no longer uses `snprintf` (45 ns → 20 ns per call).
- **`?Box[T]`, `?Rc[T]`, `?Arc[T]` and `?Weak[T]` take only one pointer**, and none is NULL (as the design notes promise). Binary tree nodes shrink from 32 bytes to 16 bytes.
- **Small-object allocator**: Gojo's ownership rules let the compiler know the exact size of an object at every point where it is freed. So `Box`, `Rc`/`Arc` and the heap boxes of interface values (≤ 256 bytes) now use thread-local free lists in 16-byte size classes, carved from 64 KB blocks, instead of calling `malloc`/`free` every time. `Vec`/`String` buffers still use `malloc`/`realloc`. Builds with AddressSanitizer fall back to `malloc`/`free` automatically, so the memory safety tests remain valid.
- **Thread-local variables and goroutine migration**: a goroutine may resume on another thread, but the C compiler caches the address of a thread-local variable within a function and keeps using it across a call that may switch threads. So the generated code re-reads the thread register every time with volatile inline assembly (plus a fixed offset on Linux; reading a pthread key slot on macOS), the same as the Go runtime does. This is faster than the old access through `tlv_get_addr` on macOS.

## Gaps not yet explained

- **nbody**: Gojo is on par with Go (151 vs 150 ms); Zig takes 98 ms. Part of the reason is that the Zig version uses compile-time fixed-length arrays, so the loops can be fully unrolled; with Zig changed to run-time-length slices it takes 122 ms. For the remaining ~20%, none of these changes helped: removing overflow / index checks, compiling the same C with `zig cc`, `-O3`, removing `-fwrapv`, reading the Vec header into local variables, disabling FMA contraction.
- **The binary-trees comparison is about the allocator**: Zig also speeds up a lot with a memory pool or arena allocator. What this benchmark shows is that Gojo gets such an allocator without changing the program, not that the language itself is faster.
- On **mandelbrot** Gojo is fastest only because clang happens to generate better code for this loop. It does not show an advantage of the language itself.

## goroutine

Concurrent programs are measured separately (Zig has no equivalent):

```sh
gojo build --release gojo/goroutines.gojo -o /tmp/k && /usr/bin/time -l /tmp/k   # on Linux use time -v
(cd go && go build -o /tmp/g ./goroutines) && /usr/bin/time -l /tmp/g
```

| Test | Platform | Gojo | Go |
| --- | --- | --- | --- |
| 100,000 simultaneously suspended goroutines passing a value along a chain (`goroutines`) | Linux arm64 (container) | 0.38 s, 441 MB | 0.11 s, 284 MB |
| Same as above | macOS (Apple M5) | 0.63 s, 1.70 GB | 0.39 s, 293 MB |
| 100,000 tasks parked for 3 s on a channel (an idle server) | macOS | 0.20 GB after hibernation (1.72 GB without) | |
| 1,000,000 short-lived goroutines, in batches of 1,000 (`spawn`) | macOS | 0.43 s | 0.26 s |

- **Memory**: Gojo goroutine stacks cannot move (a task can borrow data on another goroutine's stack), so they cannot start at 2 KB and grow by copying as in Go. Each stack is a range of virtual addresses whose pages are committed on demand. A suspended small goroutine uses one page: 4 KB on Linux, 16 KB on Apple Silicon. That is where the 1.7 GB on macOS comes from. A task parked for more than 200 ms (among more than 256) is **hibernated**: the bytes it uses (465 on average in `goroutines`, 0.6 to 1.5 KB in the HTTP test) are copied out, its pages released, and both come back at the same address before it runs again (about 1.3 µs to put away, 0.8 µs to bring back). In `goroutines` the tasks don't stay parked long enough for it to matter; in the idle-server measurement above they do. `GOJO_HIBERNATE=0` turns it off.
- **Spawning**: the same as before tasks became structured (0.43 s then, measured on the same machine). A task joins its scope, but the scope's counts are split across cache lines, one share per worker; the member list that cancellation walks is per worker and joined only by tasks that wait; a task with no result runs its closure directly; and a scope that starts no task costs nothing. The first structured version took 0.50 s.
- **Mapping count**: since Linux 6.13, guard pages are set with `MADV_GUARD_INSTALL`, and adjacent stacks merge into one mapping. On older kernels each goroutine uses two mappings and is limited by `vm.max_map_count` (default 262144); at about 130,000 goroutines you get an error suggesting you raise this value.
- **Stack reuse**: a finished goroutine leaves its stack on the worker thread's free list; extra stacks are handed to other threads. Memory is returned to the system only when the shared pool exceeds 4096 stacks. Originally every exit called `madvise`, and `spawn` took 1.16 s.

## Compile time

Using nbody as the example. Before each measurement the source file is changed so the compile cache is invalidated (for Gojo the binary cache is cleared; the compiled runtime library is kept):

| Command | Time |
| --- | --- |
| `gojo check` (parsing, type checking, borrow checking) | 3 ms |
| `gojo build` (debug, C generation + `cc -O0`) | 51 ms |
| `gojo build --release` (`cc -O2`) | 65 ms |
| `go build` | 97 ms |
| `zig build-exe` (Debug) | 924 ms |
| `zig build-exe -O ReleaseFast` | 3.8 s |

The Gojo front end takes only about 3 ms; the rest is the C compiler. When the source is unchanged, a rebuild uses the cached binary directly.
