# Real-application: Gojo vs idiomatic Rust

Workloads that look like services, not `fib` / `nbody`:

1. **jsonpipe** — NDJSON parse → filter → aggregate → serialize (`json.decode` / `serde_json`)
2. **logscan** — stream a UTF-8 log, substring search + field aggregate
3. **httpserver** — local HTTP/1.1 JSON API, concurrency sweep (Gojo `std/http` vs axum + tokio)

Same algorithms, same generated fixtures, same machine. Release builds only. Gojo overflow/bounds checks stay on. Rust uses the default `cargo --release` profile (no extra LTO, no `target-cpu=native`).

```sh
python3 bench/realworld/run_realworld.py
```

Results: [`results/RESULTS.md`](results/RESULTS.md), [`results/results.json`](results/results.json), [`results/results.csv`](results/results.csv), [`results/chart.png`](results/chart.png).

Independent issue #4 rerun (Gojo baseline `0c6e3f9` vs PR #5 `a8a7643` vs Rust, pinned CPUs): [`results/issue-4-rerun/REPORT.md`](results/issue-4-rerun/REPORT.md).

The MoonBit / Go / Gojo microbenchmarks (PR #2) live under `bench/compare-moonbit/` on that branch and are not part of this suite.
