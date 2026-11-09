# Issue #4: profiling and optimizing the real-world workloads

Before = Gojo `0c6e3f904d` (main). After = this branch. The fixtures, the Rust build (the control), the host and the commands are the same for both.

## Environment and commands

- Host: 4-vCPU Intel Xeon @ 2.60GHz, KVM guest (Amp orb, E2B), 7.8 GiB RAM, Linux 6.1.189+ x86_64
- C compiler: Debian clang 18.1.8 (`cc` on PATH → `clang-18`), Gojo `--release` = `-O2`, overflow and bounds checks on
- rustc / cargo 1.99.1, `cargo build --release` (default profile)
- hyperfine 2.0.0, Python 3.11.6
- Fixtures: seed `20261109`, 300,000 NDJSON events (34.4 MB), 600,000 log lines (48.9 MB), 10,000 products
- Benchmark sources: `bench/realworld` from `4b42da3` (unchanged apart from the harness below)

Each of the four directories here came from one command (`--gojo` picks the compiler: main's build for *before*, this branch's for *after*):

```sh
python3 bench/realworld/run_realworld.py --gojo GOJO --results bench/realworld/results/issue-4/LABEL --bin bench/realworld/bin/before-or-after \
  --runs 15 --warmup 3 --http-seconds 10 --http-warmup 1 --http-trials 5 [--server-cpus 0-2 --client-cpus 3]
python3 bench/realworld/compare.py before=bench/realworld/results/issue-4/before-pinned after=bench/realworld/results/issue-4/after-pinned
```

They ran in the order before-pinned, after-pinned, after-unpinned, before-unpinned (ABBA), so slow drift of the host can't favour either side. In every run the two HTTP servers run at the same time, and the 5 windows alternate between them.

Harness changes (`run_realworld.py`, plus the new `compare.py`) address the issue's measurement caveats:

- HTTP: 5 windows of 10 s per server and concurrency (before: 1 × 5 s), reported as the median with min–max. Every window is kept in `results.json` (`http.<lang>.raw`) and in `results.csv`.
- Each window records the server's CPU, the load generator's CPU, and the host's idle share. This shows which side is saturated.
- Optional CPU pinning with `taskset`. In the pinned runs the servers get CPUs 0–2 and the load generator gets CPU 3. The Gojo server then gets `GOJO_MAXPROCS=3`, because tokio sizes its pool from the affinity mask and Gojo sizes its own from the online CPUs.
- `--gojo`, `--results` and `--bin` options, so that two compilers can be compared on one machine.

Note: a supervisor restarted the script after the last run and started a fifth run, which overwrote `before-pinned/hyperfine/*.json`. That run was stopped and its partial files deleted. The 15 raw times per batch job of the real before-pinned run are still in `before-pinned/results.json` (`batch.*.*.times_s`).

## Results (pinned: servers on CPUs 0–2, load generator on CPU 3)

| Workload | Gojo before | Gojo after | change | Rust (control) before / after | Gojo/Rust before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| jsonpipe, median ± MAD | 234.5 ± 9.0 ms | 111.0 ± 2.8 ms | −52.6% | 143.6 / 136.7 ms | 1.63× → 0.81× |
| logscan, median ± MAD | 402.5 ± 7.6 ms | 174.8 ± 2.6 ms | −56.6% | 129.7 / 129.1 ms | 3.10× → 1.35× |

| HTTP, conc. | Gojo req/s before (min–max) | after (min–max) | change | Gojo p50 / p99 before → after | Gojo server CPU µs/req before → after | Rust req/s before / after |
| ---: | ---: | ---: | ---: | --- | ---: | ---: |
| 1 | 6,997 (6,819–7,418) | 7,976 (7,553–8,040) | +14.0% | 0.123 / 0.318 → 0.110 / 0.241 ms | 163 → 115 | 11,993 / 12,338 |
| 4 | 25,324 (24,289–27,363) | 32,787 (31,216–33,920) | +29.5% | 0.122 / 0.810 → 0.112 / 0.317 ms | 62 → 53 | 37,379 / 37,947 |
| 8 | 36,732 (36,624–37,043) | 47,083 (44,054–49,395) | +28.2% | 0.169 / 1.272 → 0.141 / 0.763 ms | 48 → 40 | 66,573 / 63,892 |
| 16 | 51,966 (44,314–53,239) | 66,320 (63,212–69,759) | +27.6% | 0.244 / 1.806 → 0.197 / 0.930 ms | 40 → 34 | 90,971 / 91,194 |

HTTP errors: 0 in every window of every run (Gojo and Rust). The route checks (health, stats, product, 404, query) passed on both servers in all four runs. The batch outputs matched Rust byte for byte in all four runs (jsonpipe SHA-256 `6a63e717…`; logscan `{"lines":600000,"needle":74720,"errors":100093,"han":119996,"s2":342363,"s4":171659,"s5":85978}`).

## Results (unpinned, as in the original issue)

| | before | after | change |
| --- | ---: | ---: | ---: |
| jsonpipe Gojo | 248.3 ± 14.5 ms | 115.1 ± 4.8 ms | −53.6% |
| logscan Gojo | 440.7 ± 23.1 ms | 181.9 ± 8.2 ms | −58.7% |
| HTTP Gojo c=1 / 4 / 8 / 16 req/s | 6,929 / 24,642 / 35,901 / 48,197 | 7,588 / 27,704 / 40,299 / 53,018 | +9.5 / +12.4 / +12.3 / +10.0% |
| HTTP Rust c=1 / 4 / 8 / 16 req/s (control) | 12,390 / 33,707 / 53,906 / 74,716 | 11,768 / 32,008 / 52,035 / 69,234 | −5.0 / −5.0 / −3.5 / −7.3% |

Unpinned, the load generator takes about 1 core and competes with the server's workers; the host is still 26–40% idle. The unpinned HTTP numbers measure that contention as much as either server. They also vary more: Rust, unchanged, drifted 4–7% between the unpinned runs. The Gojo HTTP gains are clear in the pinned runs; unpinned they are smaller (+10–12%) but still larger than the control's drift. Use the pinned tables for HTTP comparisons.

## Where the time went (profiles)

`perf record --call-graph dwarf` on the before binaries; an `LD_PRELOAD` malloc counter for allocations; `strace -f -c` for HTTP syscalls.

### logscan (before: 402 ms)

- **About 55% was the allocator under `bufio.Reader.readLine`.** `readUntil` pushed the line into a fresh `Vec[byte]` one byte at a time (`gojo_vec_grow` → `realloc` 4–5 times per line). `readLine` then copied it into a `String`. `fill` compacted the buffer by pushing the leftover bytes one at a time into a new Vec. In the profile, `_int_free` was 18%, `realloc` 14%, `_int_malloc` 9% and `malloc` 5%; `gojo_entry` self time (the inlined byte loop) was 20%.
- **`splitN` grew its result by realloc.** Each call allocated the 8-piece `Vec[String]` through 4 → 8 doublings.
- The substring search itself (memchr + memcmp) was about 10%; `parseInt` was 3%.
- Allocation calls fell from 10.2 M (5.4 M malloc + 4.8 M realloc) to 6.0 M (malloc only, about 10 per line: 1 line String, 1 piece Vec, 8 piece Strings). Bytes requested fell from 469 MB to 212 MB.

### jsonpipe (before: 235 ms)

- **The same `readLine` path** was more than half of the time: `gojo_vec_grow` + realloc + free.
- The compiler-generated decoder was not the bottleneck at the start. Afterwards it is the largest part (`jd_string` 19%, `gojo_jdec_key` 13%, `gojo_jdec_i64` 8%, plus malloc/free for the decoded Strings).
- Allocation calls fell from 3.65 M to 1.85 M. Rust makes 1.82 M, about the same.

### HTTP server

- **Server CPU per request** was 163 µs at c=1, against 56 µs for Rust. At c=16 it was 40 µs, against 26 µs.
- **Poller wake-ups.** `strace` at c=1 counted 20 syscalls per request, among them 2 `write`s to the poller's eventfd, 2 `epoll_wait`s, 2 `timerfd_settime`s and 9 `futex`es. Every request arms two `task.timeout` deadlines (the 60 s idle timeout of the connection and the request deadline). Each became the earliest timer and woke the poller thread, which re-armed the timerfd. Rust makes about 2.5 syscalls per request.
- **A task per handler call.** `handleOne` spawned two tasks per request (the hang-up watcher and the handler) in a `scope`, which meant extra goroutine switches and futex wakes.
- **Handler CPU** (c=16, user time) was dominated by the `POST /query` scan: `gojo_str_eq` was 41% including its `memcmp` calls. Each comparison was an out-of-line call to `gojo_str_eq`, which then called `memcmp`.

## Changes

1. **`std/bufio`** (`Reader.readLine`, `readUntil`, `read`, `fill`; `Writer.write`): a new `scan` finds the delimiter with `memchr` across the buffer. A refill resumes the search where the last one stopped, so a long line is scanned once. The line becomes a `String` with a single copy from the buffer. `fill` compacts in place with `memmove`, and bulk copies use one reserve and one `memcpy`. These are three small runtime helpers (`gojo_bufio___indexByte/append/dropFront`); they take slices, so bounds are still checked on the Gojo side. If a read fails partway through a line, the reader now drops the bytes of that line read so far (`errdefer`). This matches what the old `readUntil` did, whose local `out` was dropped with the error. Reading continues after the error, as before (tested).
2. **`str.split` / `splitN`** (`runtime`): these count the pieces first and allocate the `Vec[String]` once at its exact size. The first 32 cut positions are kept from the counting pass, so short strings are searched once. Output is the same; this is tested past 32 pieces, with multi-byte and overlapping separators, and against `splitN` limits.
3. **Substring search** (`str_find`): after `memchr` has matched the first byte, `memcmp` compares only the remaining bytes.
4. **`==` on strings** (`gojo_str_eq`): now `static inline` in `gojo_rt.h`. A length mismatch returns without a call, and 1–16 bytes compare with two overlapping loads instead of calling `memcmp`. A scratch catalog scan (2,000 × 10,000 item comparisons) went from 108.2 ± 5.8 to 81.2 ± 6.2 ms. Tested on every length 0–17, with a byte changed at the start, middle and end.
5. **Scheduler timers** (`gojo_sched.c`): `timer_add` wakes the poller only for a timer due before the poller's next wake-up (`s_poller_at`, kept under `s_timer_lock`), not for every new earliest timer. The poller never sleeps more than 100 ms (2 ms while tasks run) and re-reads the heap before sleeping, so later timers still fire on time. A scratch check of sleep lateness (1 ms – 250 ms sleeps, idle and with 4 pending 10 s deadlines, plus a 120 ms `task.timeout`) gave 144–409 µs worst case after vs 144–369 µs before, the same within noise. The simulated scheduler (`--seed`) keeps the old rule.
6. **`std/http` `handleOne`**: the handler runs in the scope's body instead of a second task. `s.cancel()` cancels the body too, so the deadline, a client hang-up and server shutdown still cancel it the same way (`examples/http` checks 504, hung up, 503 at shutdown and the stats; `tests/run/http` checks pipelining).

At c=1 syscalls per request fell from 20.1 to 9.7 (eventfd `write` 2.0 → 0, `epoll_wait` 2.0 → 0.24, futex 9.1 → 5.9). At c=16 they fell from 4.6 to 3.2. Allocations per request are unchanged (about 70: 30 malloc, 40 realloc).

## Safety model

No check was removed. Overflow and bounds checks, ownership, drops and cancellation are unchanged. The runtime helpers take `[]byte` slices built (and bounds-checked) by Gojo code; `dropFront` also checks `n <= len` itself. The test suite (`cargo test --release`, 21 groups including `run_programs` with and without `--verify-bce`, `simulated_scheduler`, `shutdown_signal`, `stdin_pipe_cancel`) passes, and so do the `address_sanitizer` and `thread_sanitizer` suites and `gojo fmt --check std examples`.

Regression tests: `tests/run/buffered.gojo` covers lines and `\r\n` split across 1- and 3-byte reads, a trailing `\r`, a read error in the middle of a line, a 200 KB line, `readUntil` across reads, and `read` after `readLine`. `tests/run/text.gojo` covers split past 32 pieces, multi-byte and overlapping separators, edge cases, and `==` on every length 0–17. Main's compiler gives the same output for both files, so the behaviour is unchanged.

## Tradeoffs

- `split` searches the string twice when it has more than 32 pieces (about one extra `memchr` pass), in exchange for one allocation instead of log₂(n) reallocations.
- A new timer that isn't due before the poller's next wake-up no longer wakes it. If the poller is late by more than its own backstop, such a timer can fire up to that late; this is the same bound the epoll timeout already had.
- Peak RSS: batch jobs stay at the ~3 MiB measurement floor. The HTTP server's VmHWM is 9.9–10.7 MiB before and after, so no change.

## What remains (measured, not addressed here)

- **logscan (1.35× Rust)**: about 50% of the time is now in `splitN`, mostly allocating 8 `String`s per line (`malloc` + `free` ≈ 40%). Rust's `splitn` yields borrowed `&str`s and allocates nothing. Closing this gap needs an API that returns views (an iterator, or `str` pieces), which is a language/library design question, not a runtime fix.
- **jsonpipe (0.81× Rust)**: now ahead of serde_json on this machine. The rest is the generated decoder (`jd_string`, key matching) and one allocation per decoded String.
- **HTTP (73% of Rust's throughput at c=16, 65% at c=1)**: the remaining gap is server CPU. Each request still makes about 70 allocations, mostly the request head parsed into separate `String`s (`split("\r\n")`, `split(" ")`, header names and values, `toLower` per header lookup) and responses built with `fmt.format`. It also makes 5–6 futex calls per request at c=1, from workers sleeping and waking on the global scheduler lock (`s_lock`). Both are larger changes to `std/http` and the scheduler, with risks of their own.
