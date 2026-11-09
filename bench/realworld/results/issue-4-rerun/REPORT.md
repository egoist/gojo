# Issue #4 independent rerun: Gojo baseline vs PR #5 vs Rust

Fresh measurements on this VM. Not a restatement of PR #5's tables. Absolute times from earlier sessions or other machines are not compared here.

## What was compared

| Role | Pin | Notes |
| --- | --- | --- |
| Gojo **baseline** | `0c6e3f904d6e5cbaa0643cfce8084a8598863ac8` | `gojo --version` `gojo 0.0.1 (0c6e3f904d, x86_64-unknown-linux-gnu)`; compiler SHA-256 `0670b0b1226b3282…` |
| Gojo **PR #5 HEAD** | `a8a7643e30d71cfcf31855e793718dad1616b62c` (`perf/realworld-issue-4`) | `gojo --version` `gojo 0.0.1 (a8a7643e30, x86_64-unknown-linux-gnu)`; compiler SHA-256 `92c5f85559e6b056…` |
| Rust **control** | `rustc 1.99.1 (58cf8f9c3 2026-10-13)` | default `cargo build --release` (`opt-level=3`, no extra LTO, no `target-cpu=native`) |

Workload sources are a single tree (`jsonpipe` / `logscan` / `httpserver` / `loadgen` as of `4b42da3d570bc06e2b46274d86370514c7cc93ac`). Only the Gojo **compiler/runtime** differs between the two Gojo columns. Overflow and bounds checks stay on (`gojo build --release` → `cc -O2`).

## Environment

- Host: Intel(R) Xeon(R) Processor, 4 CPUs, affinity [0, 1, 2, 3], 15.6 GiB RAM, KVM
- OS: `Linux cursor 6.12.113+ #1 SMP PREEMPT_DYNAMIC Thu Nov  5 16:04:51 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux`
- C compiler: `Ubuntu clang version 18.1.3 (1ubuntu1)`
- Cargo: `cargo 1.99.1 (5f94df478 2026-08-27)`
- Hyperfine: `hyperfine 2.0.0`
- Pinning: servers `taskset -c 0-2` (Gojo `GOJO_MAXPROCS=3`), loadgen `taskset -c 3`
- Fixture seed `20261109`: 300,000 NDJSON events (34.4 MB), 600,000 log lines (48.9 MB), 10,000 products (0.77 MB)
- Batch: hyperfine warmup 3, measured runs 11
- HTTP: 5 interleaved windows × 10 s measure + 1 s warmup; mix 70% `GET /products/:id`, 20% `POST /query`, 8% `GET /stats`, 2% `GET /health`; HTTP/1.1 keep-alive; concurrencies 1, 4, 8, 16
- Commands: `python3 bench/realworld/rerun_issue4.py --gojo-base /tmp/build-gojo-base/release/gojo --gojo-opt /tmp/build-gojo-opt/release/gojo --runs 11 --warmup 3 --http-seconds 10 --http-warmup 1 --http-trials 5 --concurrency 1,4,8,16 --server-cpus 0-2 --client-cpus 3 --results bench/realworld/results/issue-4-rerun --bin bench/realworld/bin/issue-4-rerun`

This protocol **pins** the servers to CPUs 0–2 and the load generator to CPU 3. The first recording of these workloads on this agent (branch `cursor/rust-gojo-realworld-bench-435b`) was a shorter unpinned 5 s single window. Those absolute req/s are not compared here; a small unpinned sanity block is included only to show that pinning changes the bottleneck.

## Correctness

Stdout of jsonpipe and logscan is hashed **before** timing. All three binaries must match. HTTP routes (health, stats, product by id, 404, POST `/query` counts) are checked against the fixture for every server.

- jsonpipe SHA-256: `6a63e7173414a3225ec3ad84a2bf1c384e295db6795244dff2099cd70c942b1e` — base/opt/rust match: **yes**
- logscan SHA-256: `3e1244e2f61f21471113cf3d65597b82514320b194c50297d2d7db63c78492f5` — base/opt/rust match: **yes**
- jsonpipe value: `{"matched":29385,"cents":301009335,"regions":[{"k":"eu-west","n":14667,"cents":151108585},{"k":"us-west","n":14718,"cents":149900750}]}`
- logscan value: `{"lines":600000,"needle":74720,"errors":100093,"han":119996,"s2":342363,"s4":171659,"s5":85978}`
- HTTP route checks: **yes**

## Batch jobs (median wall-clock ± MAD)

Lower is better. Times include process start and page-cache I/O after warmup.

| Workload | Gojo baseline | Gojo PR #5 | Rust | base / Rust | opt / Rust | opt / base |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| jsonpipe | 142.3 ms ± 0.6 ms | 78.0 ms ± 0.3 ms | 91.8 ms ± 0.2 ms | 1.55× | 0.85× | 0.55× |
| logscan | 279.0 ms ± 3.2 ms | 128.9 ms ± 1.0 ms | 99.5 ms ± 0.4 ms | 2.80× | 1.30× | 0.46× |

Throughput (input bytes / median seconds) and hyperfine peak RSS (treat as approximate; short processes sit near a few-MiB floor):

| Workload | impl | MB/s | RSS median | min–max wall |
| --- | --- | ---: | ---: | ---: |
| jsonpipe | Gojo base | 241.6 | 3.5 MiB | 141.2 ms–163.8 ms |
| jsonpipe | Gojo opt | 440.9 | 3.5 MiB | 77.2 ms–78.7 ms |
| jsonpipe | Rust | 374.5 | 3.5 MiB | 91.3 ms–92.4 ms |
| logscan | Gojo base | 175.3 | 3.5 MiB | 273.4 ms–306.8 ms |
| logscan | Gojo opt | 379.3 | 3.5 MiB | 127.2 ms–157.1 ms |
| logscan | Rust | 491.5 | 3.5 MiB | 98.7 ms–102.8 ms |

Raw sample lists are in `results.json` under `batch.<job>.<impl>.times_s` and in `hyperfine/`.

## HTTP JSON API (pinned)

Higher req/s is better. req/s is the **median of the five windows**, with min–max. Latencies are medians of per-window percentiles (client-side). `ok`/`err` are summed over windows. CPU is cores busy (CPU seconds / wall seconds) over the loadgen process including warmup.

| impl | conc. | req/s median | min–max | MAD | p50 | p95 | p99 | p99 range | server CPU | loadgen CPU | host idle | ok | err | RSS (VmRSS) | VmHWM |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Gojo base | 1 | 21,509 | 20,719–22,096 | 391 | 0.038 ms | 0.082 ms | 0.095 ms | 0.089–0.098 | 1.15 | 0.30 | 65% | 1,070,412 | 0 | 10.7 MiB | 10.7 MiB |
| Gojo base | 4 | 52,805 | 50,308–54,152 | 1306 | 0.056 ms | 0.198 ms | 0.548 ms | 0.521–0.563 | 1.37 | 0.58 | 51% | 2,626,527 | 0 | 10.8 MiB | 10.8 MiB |
| Gojo base | 8 | 69,114 | 63,934–70,738 | 1065 | 0.078 ms | 0.260 ms | 0.908 ms | 0.763–0.991 | 1.89 | 0.71 | 34% | 3,413,956 | 0 | 10.9 MiB | 10.9 MiB |
| Gojo base | 16 | 83,090 | 80,584–87,298 | 550 | 0.134 ms | 0.428 ms | 1.501 ms | 1.104–1.559 | 2.28 | 0.81 | 22% | 4,171,442 | 0 | 10.9 MiB | 10.9 MiB |
| Gojo opt | 1 | 25,170 | 23,182–25,497 | 327 | 0.034 ms | 0.062 ms | 0.072 ms | 0.072–0.087 | 0.86 | 0.35 | 70% | 1,239,122 | 0 | 9.3 MiB | 9.3 MiB |
| Gojo opt | 4 | 67,013 | 61,855–70,611 | 1161 | 0.046 ms | 0.131 ms | 0.303 ms | 0.276–0.324 | 1.38 | 0.75 | 45% | 3,326,251 | 0 | 9.3 MiB | 9.3 MiB |
| Gojo opt | 8 | 78,328 | 76,083–78,819 | 491 | 0.062 ms | 0.217 ms | 1.054 ms | 0.995–1.151 | 1.56 | 0.80 | 41% | 3,887,997 | 0 | 9.3 MiB | 9.3 MiB |
| Gojo opt | 16 | 87,533 | 84,725–90,711 | 1143 | 0.116 ms | 0.393 ms | 2.398 ms | 1.847–2.648 | 1.85 | 0.88 | 32% | 4,381,187 | 0 | 9.6 MiB | 9.6 MiB |
| Rust | 1 | 30,500 | 28,788–31,518 | 652 | 0.026 ms | 0.059 ms | 0.076 ms | 0.067–0.092 | 0.60 | 0.43 | 72% | 1,518,140 | 0 | 7.2 MiB | 7.2 MiB |
| Rust | 4 | 90,226 | 84,373–91,984 | 1576 | 0.042 ms | 0.078 ms | 0.100 ms | 0.100–0.108 | 1.61 | 0.86 | 37% | 4,460,046 | 0 | 7.2 MiB | 7.2 MiB |
| Rust | 8 | 112,608 | 108,567–125,055 | 2692 | 0.073 ms | 0.113 ms | 0.142 ms | 0.128–0.149 | 2.00 | 0.99 | 24% | 5,690,883 | 0 | 7.2 MiB | 7.2 MiB |
| Rust | 16 | 119,386 | 106,992–129,811 | 3494 | 0.136 ms | 0.187 ms | 0.221 ms | 0.190–0.265 | 2.10 | 1.00 | 22% | 5,930,658 | 0 | 7.3 MiB | 7.3 MiB |

| conc. | opt / base req/s | base / Rust | opt / Rust |
| ---: | ---: | ---: | ---: |
| 1 | 1.17× | 0.71× | 0.83× |
| 4 | 1.27× | 0.59× | 0.74× |
| 8 | 1.13× | 0.61× | 0.70× |
| 16 | 1.05× | 0.70× | 0.73× |

Server peak RSS (VmHWM of the long-lived process): Gojo base 11.3 MiB, Gojo opt 10.7 MiB, Rust 7.3 MiB.

### Load-generator bottleneck

Servers are limited to 3 CPUs; the client to 1. If `loadgen CPU` approaches 1.0 while `host idle` is near 0 and `server CPU` is well below 3, the client is saturating its core and the req/s figure is not a pure server capacity. If `server CPU` is high and the loadgen is not, the server is the bound.

On this run: Rust at concurrency 8 and 16 has loadgen CPU 0.99–1.00 on the single client core, so those windows are partly client-limited. Gojo opt at concurrency 16 still has 32% host idle (server 1.85 / 3, loadgen 0.88 / 1). The unpinned sanity lets the loadgen use more than one core (Rust c=16 loadgen 1.27); that is why pinning is the primary protocol, and why unpinned req/s must not be mixed with the pinned table.

## HTTP unpinned sanity (not the primary result)

Same mix, one window per impl and concurrency, no `taskset`. Shown only to record that pinning changes sharing of the 4 CPUs. Do not mix these req/s with the pinned table.

| impl | conc. | req/s | p50 / p99 | server CPU | loadgen CPU | host idle | err |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Gojo base | 1 | 20,604 | 0.040 / 0.094 ms | 1.08 | 0.34 | 65% | 0 |
| Gojo base | 16 | 80,031 | 0.171 / 0.767 ms | 2.05 | 1.01 | 22% | 0 |
| Gojo opt | 1 | 23,549 | 0.036 / 0.087 ms | 0.84 | 0.36 | 69% | 0 |
| Gojo opt | 16 | 88,626 | 0.131 / 1.033 ms | 1.86 | 1.09 | 25% | 0 |
| Rust | 1 | 31,709 | 0.025 / 0.064 ms | 0.59 | 0.42 | 72% | 0 |
| Rust | 16 | 112,781 | 0.128 / 0.369 ms | 1.69 | 1.27 | 25% | 0 |

## Caveats

- **Not a language ranking.** jsonpipe is compiler-generated JSON vs serde_json. HTTP is `std/http` vs axum+tokio+hyper (ecosystem). logscan is closer to a runtime + std comparison.
- **No causal claims** beyond “this compiler/runtime at this commit, these flags, this machine, these programs.” Profiles that motivated PR #5 are in that PR; this rerun does not re-profile.
- **Checks stay on.** Gojo overflow and bounds checks were not disabled. Rust was not given LTO or `target-cpu=native`.
- **Hyperfine RSS** of short batch jobs can sit on a measurement floor of a few MiB. HTTP RSS is `/proc/<pid>/status` of the server.
- **4-CPU KVM guest.** Absolute req/s will not match a metal box.
- **Idle sibling servers.** All three HTTP servers listen at once (same as PR #5 keeping both SUTs up) and share the server CPU set while idle.
- **PR #2** (MoonBit / Go / Gojo microbenchmarks) is not part of this run.

## Artifacts

Primary copies (this rerun only; not the MoonBit suite):

- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/REPORT.md`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/results.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/results.csv`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/chart.png`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/gojo_base-jsonpipe.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/gojo_base-logscan.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/gojo_opt-jsonpipe.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/gojo_opt-logscan.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/rust-jsonpipe.json`
- `/opt/cursor/artifacts/rust-gojo-rerun-issue4/hyperfine/rust-logscan.json`

Same files in the repo tree: `/workspace/bench/realworld/results/issue-4-rerun/`.

