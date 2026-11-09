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
- Compile (not timed in the table): Gojo 0.1s, Rust 0.1s

## Correctness

Stdout of jsonpipe and logscan is hashed before timing. HTTP routes are checked (health, stats, product by id, 404, POST /query counts) against the fixture.

- jsonpipe SHA-256: `6a63e7173414a3225ec3ad84a2bf1c384e295db6795244dff2099cd70c942b1e` (Gojo == Rust: **yes**)
- logscan SHA-256: `3e1244e2f61f21471113cf3d65597b82514320b194c50297d2d7db63c78492f5` (Gojo == Rust: **yes**)
- HTTP JSON bodies compared by parsed value: **yes**

## Batch jobs (median wall-clock, hyperfine)

| Workload | What it measures | Gojo | Rust | Gojo / Rust | Gojo RSS | Rust RSS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| jsonpipe | NDJSON decode → filter → aggregate → encode (300,000 records) | 111.0 ms | 136.7 ms | 0.81× | 3.1 MiB | 3.0 MiB |
| logscan | UTF-8 line scan + field aggregate (600,000 lines) | 174.8 ms | 129.1 ms | 1.35× | 3.0 MiB | 3.1 MiB |

Hyperfine: 15 runs, 3 warmup. Dispersion is median ± MAD: jsonpipe Gojo 111.0 ms ± 2.8 ms, Rust 136.7 ms ± 0.6 ms; logscan Gojo 174.8 ms ± 2.6 ms, Rust 129.1 ms ± 1.2 ms.

Throughput (input bytes / median seconds):

| Workload | Gojo | Rust |
| --- | ---: | ---: |
| jsonpipe | 309.6 MB/s | 251.5 MB/s |
| logscan | 279.7 MB/s | 378.7 MB/s |

## HTTP JSON API

Read-mostly in-memory catalog. Mix: 70% `GET /products/:id`, 20% `POST /query` (full scan, first 50 ids), 8% `GET /stats`, 2% `GET /health`. HTTP/1.1 keep-alive. Same loadgen for both servers. Warmup is discarded; the table is the measuring window only. Latency is client-side.

5 trials of 10 s (after 1 s warmup) per server and concurrency, interleaved between the servers; servers pinned to CPUs `0-2`, load generator to `3`. req/s is the median of the trials, with min–max; latencies are medians of the per-trial percentiles. CPU columns are cores busy (CPU seconds / wall seconds) over each loadgen run; idle is the host's idle share.

| Lang | conc. | req/s (median) | min–max | p50 | p95 | p99 | server CPU | loadgen CPU | host idle | ok | err |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Gojo | 1 | 7,976 | 7,553–8,040 | 0.110 ms | 0.198 ms | 0.241 ms | 0.91 | 0.28 | 71% | 394,881 | 0 |
| Gojo | 4 | 32,787 | 31,216–33,920 | 0.112 ms | 0.228 ms | 0.317 ms | 1.73 | 0.69 | 39% | 1,634,283 | 0 |
| Gojo | 8 | 47,083 | 44,054–49,395 | 0.141 ms | 0.344 ms | 0.763 ms | 1.87 | 0.79 | 33% | 2,339,967 | 0 |
| Gojo | 16 | 66,320 | 63,212–69,759 | 0.197 ms | 0.491 ms | 0.930 ms | 2.27 | 0.88 | 21% | 3,313,794 | 0 |
| Rust | 1 | 12,338 | 12,148–12,510 | 0.064 ms | 0.154 ms | 0.205 ms | 0.67 | 0.41 | 73% | 615,716 | 0 |
| Rust | 4 | 37,947 | 36,104–39,104 | 0.093 ms | 0.213 ms | 0.269 ms | 1.51 | 0.68 | 45% | 1,889,687 | 0 |
| Rust | 8 | 63,892 | 62,053–67,627 | 0.113 ms | 0.240 ms | 0.316 ms | 2.07 | 0.86 | 26% | 3,240,886 | 0 |
| Rust | 16 | 91,194 | 79,170–95,276 | 0.169 ms | 0.295 ms | 0.405 ms | 2.41 | 0.97 | 15% | 4,450,442 | 0 |

Server peak RSS while serving (VmHWM): Gojo 9.9 MiB, Rust 6.7 MiB.

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

Arguments of this run: `--gojo target/release/gojo --results bench/realworld/results/issue-4/after-pinned --bin bench/realworld/bin/after --runs 15 --warmup 3 --http-seconds 10 --http-warmup 1 --http-trials 5 --server-cpus 0-2 --client-cpus 3`.

Artifacts: `RESULTS.md`, `results.json` (with every HTTP trial under `http.<lang>.raw`), `results.csv`, `chart.png`, `hyperfine/`.

