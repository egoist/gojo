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

HTTP runs both servers at once and alternates the measuring windows between them: `--http-trials` windows (default 5) of `--http-seconds` (default 10) per server and concurrency. Each window records the CPU the server and the load generator used and the host's idle share. `--server-cpus 0-2 --client-cpus 3` pins them with `taskset` (the Gojo server then gets `GOJO_MAXPROCS` = its CPU count, as tokio sizes itself from the affinity mask). `--gojo PATH` picks the compiler and `--results DIR` / `--bin DIR` where the run goes, so two compilers can be compared on one machine:

```sh
python3 bench/realworld/run_realworld.py --gojo /path/to/old/gojo --results bench/realworld/results/x/before --bin bench/realworld/bin/before
python3 bench/realworld/run_realworld.py --gojo target/release/gojo --results bench/realworld/results/x/after --bin bench/realworld/bin/after
python3 bench/realworld/compare.py before=bench/realworld/results/x/before after=bench/realworld/results/x/after
```

[`results/issue-4/`](results/issue-4/REPORT.md) has the profiles and before/after runs of the optimizations for issue #4.

The MoonBit / Go / Gojo microbenchmarks (PR #2) live under `bench/compare-moonbit/` on that branch and are not part of this suite.
