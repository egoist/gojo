# Gojo vs Go vs MoonBit (native)

This directory holds the **measured** comparison of three native backends on
the same eight workloads already used by [`../README.md`](../README.md)
(plus a `hello` startup floor).

| Language | Timed target | Build |
| --- | --- | --- |
| Gojo | native ELF (C backend, `cc -O2`, overflow and bounds checks on) | `gojo build --release` |
| Go | native ELF (`gc`) | `go build` |
| MoonBit | native ELF (`--target native --release`, generated C, same `cc`) | `moon build --target native --release` |

MoonBit also has `wasm` / `js` / experimental `llvm` backends. Those are **not**
in the timing table: Gojo has no implemented WASM target, and mixing WASM with
native Go would not be the same experiment. The LLVM backend on this toolchain
fails with `missing prelude.mi`.

## Run

From the repository root, with the Gojo compiler, Go, MoonBit, and hyperfine
on `PATH` (this environment uses **mise** for Go/hyperfine/rust/bun, and the
official MoonBit unix installer because MoonBit is not in the mise registry):

```sh
cargo build --release
cd bench
./run-gojo-go-moonbit.sh          # default: 3 warmup + 11 timed runs
./run-gojo-go-moonbit.sh --runs 11 --warmup 3
```

The script:

1. Records CPU/OS/compiler versions into `env.json`.
2. Builds each program (`--release` / Go default / MoonBit native release).
3. Runs each binary once and requires **byte-identical stdout** across the three.
4. Times with hyperfine (`-N`, interleaved languages), then writes median and MAD.
5. Copies peak RSS from the same hyperfine measurements.

Outputs (committed from the run that produced this PR):

- [`RESULTS.md`](RESULTS.md) — human-readable report
- [`results.csv`](results.csv) — one row per workload × language
- [`results.json`](results.json) — structured medians, raw times, ratios
- [`env.json`](env.json) — machine and toolchain
- [`build.json`](build.json) — compile commands and wall times
- [`raw/`](raw/) — hyperfine JSON
- [`outputs/`](outputs/) — verified stdout

The Gojo compiler itself is not changed. Zig remains in `../run.sh` and is
outside this comparison.
