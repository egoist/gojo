# Gojo vs Rust: real-application workloads

Same algorithms, same generated fixtures, same machine. Gojo `--release` (`cc -O2`, overflow and bounds checks on). Rust `cargo build --release` (default profile: `opt-level=3`, no extra LTO, no `-C target-cpu=native`).

This is **not** a compiler-only comparison on the HTTP side: Gojo uses `std/http` + compiler-generated `json`; Rust uses **axum 0.8 + tokio + serde_json**. The NDJSON and log jobs compare generated JSON/line I/O vs serde_json/`BufRead`.

## Environment

- Host: Intel(R) Xeon(R) Processor, 4 CPUs, 15.6 GiB RAM, KVM
- OS: `Linux cursor 6.12.113+ #1 SMP PREEMPT_DYNAMIC Thu Nov  5 16:04:51 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux`
- Gojo: `gojo 0.0.1 (0c6e3f904d, x86_64-unknown-linux-gnu)` (git `0c6e3f904d6e`)
- Rustc: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- Cargo: `cargo 1.99.1 (5f94df478 2026-08-27)`
- C compiler: `Ubuntu clang version 18.1.3 (1ubuntu1)`
- Hyperfine: `hyperfine 2.0.0`
- Fixture seed `20261109`: 300,000 NDJSON events (34.4 MB), 600,000 log lines (48.9 MB), 10,000 products (0.77 MB)
- Compile (not timed in the table): Gojo ~0.1s for these three files; Rust 14s first `cargo build --release` of axum/tokio/serde, ~0.1s on rebuild

## Correctness

Stdout of jsonpipe and logscan is hashed before timing. HTTP routes are checked (health, stats, product by id, 404, POST /query counts) against the fixture.

- jsonpipe SHA-256: `6a63e7173414a3225ec3ad84a2bf1c384e295db6795244dff2099cd70c942b1e` (Gojo == Rust: **yes**)
- logscan SHA-256: `3e1244e2f61f21471113cf3d65597b82514320b194c50297d2d7db63c78492f5` (Gojo == Rust: **yes**)
- HTTP JSON bodies compared by parsed value: **yes**

## Batch jobs (median wall-clock, hyperfine)

| Workload | What it measures | Gojo | Rust | Gojo / Rust | Gojo RSS | Rust RSS |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| jsonpipe | NDJSON decode → filter → aggregate → encode (300,000 records) | 143.0 ms | 92.5 ms | 1.55× | 3.5 MiB | 3.4 MiB |
| logscan | UTF-8 line scan + field aggregate (600,000 lines) | 275.4 ms | 97.8 ms | 2.81× | 3.5 MiB | 3.5 MiB |

Hyperfine: 7 runs, 2 warmup. Dispersion is median ± MAD: jsonpipe Gojo 143.0 ms ± 0.6 ms, Rust 92.5 ms ± 1.0 ms; logscan Gojo 275.4 ms ± 1.1 ms, Rust 97.8 ms ± 0.6 ms.

Throughput (input bytes / median seconds):

| Workload | Gojo | Rust |
| --- | ---: | ---: |
| jsonpipe | 240.5 MB/s | 371.6 MB/s |
| logscan | 177.6 MB/s | 499.8 MB/s |

## HTTP JSON API

Read-mostly in-memory catalog. Mix: 70% `GET /products/:id`, 20% `POST /query` (full scan, first 50 ids), 8% `GET /stats`, 2% `GET /health`. HTTP/1.1 keep-alive. Same loadgen for both servers. Warmup is discarded; the table is the measuring window only. Latency is client-side.

| Lang | conc. | req/s | p50 | p95 | p99 | ok | err |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Gojo | 1 | 21,243 | 0.039 ms | 0.082 ms | 0.092 ms | 106,236 | 0 |
| Gojo | 4 | 49,110 | 0.066 ms | 0.164 ms | 0.452 ms | 245,564 | 0 |
| Gojo | 8 | 51,162 | 0.132 ms | 0.312 ms | 0.629 ms | 255,863 | 0 |
| Gojo | 16 | 77,349 | 0.170 ms | 0.444 ms | 0.879 ms | 386,822 | 0 |
| Rust | 1 | 30,592 | 0.025 ms | 0.060 ms | 0.069 ms | 152,985 | 0 |
| Rust | 4 | 70,988 | 0.048 ms | 0.112 ms | 0.146 ms | 355,010 | 0 |
| Rust | 8 | 88,648 | 0.080 ms | 0.177 ms | 0.246 ms | 443,317 | 0 |
| Rust | 16 | 108,528 | 0.131 ms | 0.278 ms | 0.397 ms | 542,756 | 0 |

Server peak RSS while serving (VmHWM): Gojo 9.9 MiB, Rust 7.2 MiB.

## Caveats

- **Startup / I/O.** Batch times include process start, opening the file, and page-cache I/O after warmup. They are not “CPU minus I/O” numbers.
- **Hyperfine RSS.** Peak RSS of a short process can sit on a measurement floor of a few MiB. Treat RSS as approximate; HTTP RSS is from `/proc/<pid>/status` VmHWM of the long-lived server.
- **JSON.** Gojo’s encoder/decoder is compiler-generated for the struct. Rust uses serde_json. A faster or slower JSON library on either side would move the jsonpipe number without saying anything about the language runtime alone.
- **HTTP stack.** axum/tokio/hyper vs Gojo `std/http` is an ecosystem comparison. Both are HTTP/1.1 (no HTTP/2). The loadgen is a hyper client; it is not the system under test.
- **Sharing.** Rust holds the catalog in `Arc<Vec<Item>>` with no lock. Gojo’s `http.serve` borrows the handler for the life of the server, so the catalog is also read-only with no mutex. That is the idiomatic form on each side for a read-mostly map.
- **No strawman, no shortcuts.** Overflow/bounds checks stay on in Gojo. Rust is not a debug build, and was not given LTO/`target-cpu=native` that Gojo does not get. Integers for money; UTF-8 substring search, not Unicode case-folding.
- **4-CPU KVM guest.** Absolute req/s will not match a metal box. Ratios on this machine are the result.
- **PR #2** (MoonBit / Go / Gojo microbenchmarks) is untouched.

## How to rerun

```sh
python3 bench/realworld/run_realworld.py
```

Artifacts: `results/RESULTS.md`, `results/results.json`, `results/results.csv`, `results/chart.png`.

