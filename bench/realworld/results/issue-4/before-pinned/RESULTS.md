# Gojo vs Rust: real-application workloads

Same algorithms, same generated fixtures, same machine. Gojo `--release` (`cc -O2`, overflow and bounds checks on). Rust `cargo build --release` (default profile: `opt-level=3`, no extra LTO, no `-C target-cpu=native`).

This is **not** a compiler-only comparison on the HTTP side: Gojo uses `std/http` + compiler-generated `json`; Rust uses **axum 0.8 + tokio + serde_json**. The NDJSON and log jobs compare generated JSON/line I/O vs serde_json/`BufRead`.

## Environment

- Host: Intel(R) Xeon(R) Processor @ 2.60GHz, 4 CPUs, 7.8 GiB RAM, KVM
- OS: `Linux e2b.local 6.1.189+ #1 SMP PREEMPT_DYNAMIC Mon Nov  2 21:08:28 UTC 2026 x86_64 GNU/Linux`
- Gojo: `gojo 0.0.1 (0c6e3f904d, x86_64-unknown-linux-gnu)` (git `0c6e3f904d6e`)
- Rustc: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- Cargo: `cargo 1.99.1 (5f94df478 2026-08-27)`
- C compiler: `Debian clang version 18.1.8 (++20240731024826+3b5b5c1ec4a3-1~exp1~20240731144843.145)`
- Hyperfine: `hyperfine 2.0.0`
- Fixture seed `20261109`: 300,000 NDJSON events (34.4 MB), 600,000 log lines (48.9 MB), 10,000 products (0.77 MB)
- Compile (not timed in the table): Gojo 1.7s, Rust 0.1s

## Correctness

Stdout of jsonpipe and logscan is hashed before timing. HTTP routes are checked (health, stats, product by id, 404, POST /query counts) against the fixture.

- jsonpipe SHA-256: `6a63e7173414a3225ec3ad84a2bf1c384e295db6795244dff2099cd70c942b1e` (Gojo == Rust: **yes**)
- logscan SHA-256: `3e1244e2f61f21471113cf3d65597b82514320b194c50297d2d7db63c78492f5` (Gojo == Rust: **yes**)
- HTTP JSON bodies compared by parsed value: **yes**

## Batch jobs (median wall-clock, hyperfine)

| Workload | What it measures | Gojo | Rust | Gojo / Rust | Gojo RSS | Rust RSS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| jsonpipe | NDJSON decode → filter → aggregate → encode (300,000 records) | 234.5 ms | 143.6 ms | 1.63× | 3.1 MiB | 3.0 MiB |
| logscan | UTF-8 line scan + field aggregate (600,000 lines) | 402.5 ms | 129.7 ms | 3.10× | 3.0 MiB | 3.0 MiB |

Hyperfine: 15 runs, 3 warmup. Dispersion is median ± MAD: jsonpipe Gojo 234.5 ms ± 9.0 ms, Rust 143.6 ms ± 7.9 ms; logscan Gojo 402.5 ms ± 7.6 ms, Rust 129.7 ms ± 1.2 ms.

Throughput (input bytes / median seconds):

| Workload | Gojo | Rust |
| --- | ---: | ---: |
| jsonpipe | 146.6 MB/s | 239.5 MB/s |
| logscan | 121.5 MB/s | 377.0 MB/s |

## HTTP JSON API

Read-mostly in-memory catalog. Mix: 70% `GET /products/:id`, 20% `POST /query` (full scan, first 50 ids), 8% `GET /stats`, 2% `GET /health`. HTTP/1.1 keep-alive. Same loadgen for both servers. Warmup is discarded; the table is the measuring window only. Latency is client-side.

5 trials of 10 s (after 1 s warmup) per server and concurrency, interleaved between the servers; servers pinned to CPUs `0-2`, load generator to `3`. req/s is the median of the trials, with min–max; latencies are medians of the per-trial percentiles. CPU columns are cores busy (CPU seconds / wall seconds) over each loadgen run; idle is the host's idle share.

| Lang | conc. | req/s (median) | min–max | p50 | p95 | p99 | server CPU | loadgen CPU | host idle | ok | err |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Gojo | 1 | 6,997 | 6,819–7,418 | 0.123 ms | 0.240 ms | 0.318 ms | 1.14 | 0.23 | 68% | 354,129 | 0 |
| Gojo | 4 | 25,324 | 24,289–27,363 | 0.122 ms | 0.340 ms | 0.810 ms | 1.57 | 0.55 | 49% | 1,292,393 | 0 |
| Gojo | 8 | 36,732 | 36,624–37,043 | 0.169 ms | 0.465 ms | 1.272 ms | 1.75 | 0.68 | 44% | 1,839,915 | 0 |
| Gojo | 16 | 51,966 | 44,314–53,239 | 0.244 ms | 0.633 ms | 1.806 ms | 2.08 | 0.79 | 32% | 2,516,608 | 0 |
| Rust | 1 | 11,993 | 11,522–12,270 | 0.066 ms | 0.163 ms | 0.214 ms | 0.67 | 0.40 | 73% | 596,517 | 0 |
| Rust | 4 | 37,379 | 36,387–38,614 | 0.095 ms | 0.215 ms | 0.274 ms | 1.50 | 0.68 | 44% | 1,868,264 | 0 |
| Rust | 8 | 66,573 | 65,817–67,310 | 0.110 ms | 0.233 ms | 0.301 ms | 2.09 | 0.85 | 25% | 3,325,941 | 0 |
| Rust | 16 | 90,971 | 81,625–93,399 | 0.169 ms | 0.296 ms | 0.400 ms | 2.40 | 0.97 | 15% | 4,460,442 | 0 |

Server peak RSS while serving (VmHWM): Gojo 10.1 MiB, Rust 6.6 MiB.

## Caveats

- **Startup / I/O.** Batch times include process start, opening the file, and page-cache I/O after warmup. They are not “CPU minus I/O” numbers.
- **Hyperfine RSS.** Peak RSS of a short process can sit on a measurement floor of a few MiB. Treat RSS as approximate; HTTP RSS is from `/proc/<pid>/status` VmHWM of the long-lived server.
- **JSON.** Gojo’s encoder/decoder is compiler-generated for the struct. Rust uses serde_json. A faster or slower JSON library on either side would move the jsonpipe number without saying anything about the language runtime alone.
- **HTTP stack.** axum/tokio/hyper vs Gojo `std/http` is an ecosystem comparison. Both are HTTP/1.1 (no HTTP/2). The loadgen is a hyper client; it is not the system under test.
- **Sharing.** Rust holds the catalog in `Arc<Vec<Item>>` with no lock. Gojo’s `http.serve` borrows the handler for the life of the server, so the catalog is also read-only with no mutex. That is the idiomatic form on each side for a read-mostly map.
- **No strawman, no shortcuts.** Overflow/bounds checks stay on in Gojo. Rust is not a debug build, and was not given LTO/`target-cpu=native` that Gojo does not get. Integers for money; UTF-8 substring search, not Unicode case-folding.
- **4-CPU KVM guest.** Absolute req/s will not match a metal box. Ratios on this machine are the result.
- **Shared host.** Load generator and servers share the machine. When neither server CPU nor loadgen CPU is near its CPU budget and the host still has idle time, throughput is bound by round-trip latency, not by server capacity.
- **PR #2** (MoonBit / Go / Gojo microbenchmarks) is untouched.

## How to rerun

```sh
python3 bench/realworld/run_realworld.py
```

Arguments of this run: `--gojo /tmp/tc/base/gojo --results bench/realworld/results/issue-4/before-pinned --bin bench/realworld/bin/before --runs 15 --warmup 3 --http-seconds 10 --http-warmup 1 --http-trials 5 --server-cpus 0-2 --client-cpus 3`.

Artifacts: `RESULTS.md`, `results.json` (with every HTTP trial under `http.<lang>.raw`), `results.csv`, `chart.png`, `hyperfine/`.

