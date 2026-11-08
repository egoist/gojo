# Gojo vs Go vs MoonBit — native benchmark results

Measured on one machine, same algorithms and inputs, release/optimized builds.
Numbers below are **from this run**, not copied from elsewhere. Failures are listed as such.

## How to reproduce

```sh
# toolchain: mise for go/hyperfine/rust/bun; MoonBit via official installer (not in mise)
eval "$(mise activate bash)"
export PATH="$HOME/.moon/bin:$PATH"
cargo build --release
cd bench && ./run-gojo-go-moonbit.sh
```

Default: 3 warmup runs discarded, 11 timed runs, **median** and MAD (median absolute deviation).
Hyperfine `-N` runs the binaries without a shell. Commands interleave the three languages per workload.

## Machine

- Host: `cursor`
- OS: Linux cursor 6.12.113+ #1 SMP PREEMPT_DYNAMIC Thu Nov  5 16:04:51 UTC 2026 x86_64 x86_64 x86_64 GNU/Linux
- CPU: Intel(R) Xeon(R) Processor (4 logical CPUs)
- Captured: 2026-11-08T20:31:37.972657+00:00

## Toolchain

- Gojo: `gojo 0.0.1 (01031d6139, x86_64-unknown-linux-gnu)` — git `01031d6` `01031d6139c8cfbd02e0864d57efedbd1ddbbd0f Rewrite links inside tables on docs pages`
- Go: `go version go1.27.3 linux/amd64` GOARCH=amd64 GOAMD64=v1
- rustc (to build the Gojo compiler): `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- C compiler (`cc`, used by Gojo and MoonBit native): `Ubuntu clang version 18.1.3 (1ubuntu1)` (/usr/bin/cc)
- MoonBit: `moon 0.1.20261104 (940aa18 2026-11-04)` / moonc `v0.10.20+65efd5b93 (2026-11-05)`
- hyperfine: `hyperfine 2.0.0`
- mise: `2026.11.4 linux-x64 (2026-11-07)`; bun `1.4.4` (npm backend, not used by these native builds)

## Flags and targets

- **gojo**: `gojo build --release  (C backend, overflow/bounds checks on, cc -O2)`
- **go**: `go build  (default optimizing compile; GOMAXPROCS left at default)`
- **moonbit**: `moon build --target native --release  (native C backend, MOON_CC=cc)`
- **MOON_CC**: `cc`
- **GOMAXPROCS**: `unset (Go default)`
- **hyperfine**: `hyperfine -N --warmup 3 --runs 11`
- **warmup**: `3`
- **runs**: `11`

All three timed columns are **native executables** on this x86_64 Linux host:

- Gojo: compile to C, then `cc -O2` (checks on).
- Go: `gc` compiler native binary.
- MoonBit: `--target native --release` (release uses generated C, then the same `cc`).

MoonBit `--target llvm` is experimental here (`missing prelude.mi`) and was **not** measured.
MoonBit `--target wasm` builds, but WASM is a different runtime than native Go/Gojo; it is **not** in the table.
Gojo has no implemented WASM target (`docs/wasm.md` is a proposal).

## Workloads

| name | what it measures | size |
| --- | --- | --- |
| `fib` | recursive calls: fib(40); Int64 / Go int | n=40 |
| `binarytrees` | many small allocations: binary-trees n=18 | n=18, minDepth=4 |
| `nbody` | floating-point structs: 5 bodies, 5e6 steps | 5 bodies, 5_000_000 steps, dt=0.01 |
| `spectralnorm` | nested float loops: spectral-norm n=2000 | n=2000, 10 power iterations |
| `fannkuch` | integer arrays: fannkuch-redux n=10 | n=10 |
| `mandelbrot` | pure float loops: mandelbrot n=2000, 50 iters | n=2000, max 50 iterations |
| `hashmap` | Map/map: 1e6 int keys + 2e5 string keys | 1_000_000 int inserts/lookups; 200_000 string keys, 400_000 lookups |
| `sort` | sort 5e6 Int64/int values from a 64-bit LCG | 5_000_000 Int64/int, LCG seed=42, (seed*1103515245+12345)%2^31 |
| `hello` | print one line; process startup floor | print 'ok' |

Integer payload types: Go and Gojo use 64-bit `int` on this host. MoonBit uses `Int64` wherever that width is required for matching arithmetic (hashmap keys, sort LCG, checksums, fib). Loop indices use MoonBit `Int32` because `Array` is indexed by `Int`.

## Output verification

Every workload: Gojo, Go, and MoonBit printed **byte-for-byte identical** stdout (after a single untimed run each).

Golden outputs are stored under `outputs/` in this directory.

## Steady run time (native)

Lower is better. Median of timed runs after warmup, milliseconds. Dispersion is MAD.
These times include process startup; for all eight workloads except `hello`, the body dominates (see startup section).

| benchmark | Gojo median (MAD) | Go median (MAD) | MoonBit native median (MAD) | Gojo / Go | MoonBit / Go | MoonBit / Gojo |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `fib` | 401.92 ms (2.78) | 391.64 ms (2.39) | 233.03 ms (2.48) | 1.026× | 0.595× | 0.580× |
| `binarytrees` | 547.49 ms (4.49) | 1061.33 ms (35.28) | 1258.55 ms (48.26) | 0.516× | 1.186× | 2.299× |
| `nbody` | 219.65 ms (3.73) | 374.41 ms (8.02) | 280.89 ms (9.85) | 0.587× | 0.750× | 1.279× |
| `spectralnorm` | 236.66 ms (10.65) | 178.35 ms (2.29) | 176.31 ms (3.61) | 1.327× | 0.989× | 0.745× |
| `fannkuch` | 221.57 ms (4.07) | 187.06 ms (2.51) | 201.42 ms (3.00) | 1.185× | 1.077× | 0.909× |
| `mandelbrot` | 231.02 ms (0.27) | 233.11 ms (0.84) | 220.02 ms (1.12) | 0.991× | 0.944× | 0.952× |
| `hashmap` | 166.09 ms (1.85) | 181.19 ms (2.05) | 330.53 ms (5.66) | 0.917× | 1.824× | 1.990× |
| `sort` | 349.13 ms (2.83) | 482.01 ms (28.73) | 517.76 ms (13.98) | 0.724× | 1.074× | 1.483× |

Geometric mean of median ratios across the eight workloads: Gojo/Go = **0.868×**, MoonBit/Go = **1.005×**, MoonBit/Gojo = **1.158×** (<1 means faster than the denominator).

Do not rank the languages from `fib` alone: it is overflow-check / call heavy and is only one row.

**On this run (medians):** Gojo was fastest on binary-trees, nbody, hashmap, and sort. MoonBit was fastest on fib (no overflow checks on `Int64` add) and slightly ahead on mandelbrot; it matched Go on spectral-norm. Go was fastest on fannkuch and on spectral-norm vs Gojo. Geometric means vs Go: Gojo **0.87×**, MoonBit **1.00×**. vs Gojo, MoonBit is **1.16×** (slower overall), driven by binary-trees, hashmap, and sort — not by fib.

## Memory (max RSS)

Peak RSS from the same hyperfine timed runs (`memory_peak_resident`, converted to KiB). Median of those samples. This is process peak RSS, not a precise allocator profile.
On this host, hyperfine reports **3312 KiB** for every tiny process (including `hello`); treat that as a measurement floor, not equal heaps. Differences above that floor (binary-trees, hashmap, sort) are meaningful.

| benchmark | Gojo KiB | Go KiB | MoonBit KiB |
| --- | ---: | ---: | ---: |
| `fib` | 3312 | 3312 | 3312 |
| `binarytrees` | 18776 | 37072 | 34676 |
| `nbody` | 3312 | 3312 | 3312 |
| `spectralnorm` | 3308 | 3308 | 3308 |
| `fannkuch` | 3312 | 3312 | 3312 |
| `mandelbrot` | 3312 | 3312 | 3312 |
| `hashmap` | 71428 | 63308 | 80592 |
| `sort` | 41132 | 41296 | 40784 |
| `hello` | 3308 | 3308 | 3308 |

## Build time (produce the timed binary)

Wall time of one `build` that writes the executable used above. Gojo and MoonBit may reuse a cached C runtime from earlier packages in the same run. This is **compile/link**, not steady execution.

| benchmark | Gojo s | Go s | MoonBit s | binary bytes (Gojo / Go / MoonBit) |
| --- | ---: | ---: | ---: | --- |
| `fib` | 0.028 | 0.106 | 0.600 | 241040 / 2350794 / 301168 |
| `binarytrees` | 0.029 | 0.109 | 0.244 | 241136 / 2355847 / 307408 |
| `nbody` | 0.134 | 0.112 | 0.181 | 241096 / 2360526 / 301216 |
| `spectralnorm` | 0.140 | 0.114 | 0.248 | 241152 / 2356300 / 307400 |
| `fannkuch` | 0.029 | 0.112 | 0.248 | 241016 / 2355409 / 307336 |
| `mandelbrot` | 0.109 | 0.108 | 0.136 | 241016 / 2351381 / 301120 |
| `hashmap` | 0.141 | 0.112 | 0.307 | 241016 / 2355649 / 311728 |
| `sort` | 0.133 | 0.114 | 0.312 | 241040 / 2365704 / 311632 |
| `hello` | 0.031 | 0.116 | 0.130 | 236912 / 2350889 / 297024 |

Gojo **compiler** `cargo build --release` (already cached if re-run): 0.023 s.

## Startup floor (`hello`)

Same harness, program only prints `ok`. This is not a throughput benchmark; it bounds how much of a short run is process bring-up.

- Gojo median 0.61 ms (MAD 0.06)
- Go median 0.95 ms (MAD 0.04)
- MoonBit median 0.45 ms (MAD 0.02)
- stdout matched: `ok`

## Limitations

- Primary comparison is native machine code on this host for all three languages.
- MoonBit LLVM backend is experimental on this toolchain (missing prelude.mi) and was not used.
- MoonBit wasm/js backends exist but are not comparable to native Go/Gojo; they are not in the timing table.
- Gojo WASM is a design proposal only (docs/wasm.md); Gojo programs here are native.
- Programs print checksums so the compiler cannot delete the work.
- All programs are single-threaded; Go's GC may still use other cores.
- Gojo --release keeps integer overflow and index checks; Go wraps integers; MoonBit native wrapping is used for Int/Int64 overflow.
- Default ISAs: go env GOAMD64=v1; clang -O2 on this host is SSE2/x86-64 (not -march=native). AVX in binaries is mostly runtime, not a claimed nbody win.
- One cloud VM, 4 logical CPUs; not an Apple M5 (the in-tree `bench/results.md` Zig table was measured elsewhere).
- No CPU pinning; other processes on the VM can add noise. MAD is the spread we observed, not a confidence interval.
- Sorting algorithms differ at the library level (Go `slices.Sort`, Gojo `Vec.sort`, MoonBit `Array.sort`) on the same LCG input; that is the language default sort, not a shared introsort implementation.
- Hash maps are each language's default map (Gojo insertion-ordered `Map`, Go `map`, MoonBit insertion-ordered `Map`).
- MoonBit native release currently goes through C, like Gojo; that is comparable codegen, not a claim about the LLVM backend.
- Memory figures are peak RSS and include the runtime/GC heap; they are not allocation counts.

## Raw data

- `results.json` — full structured result
- `results.csv` — one row per workload × language
- `env.json` — machine and toolchain dump
- `raw/<bench>.hyperfine.json` — hyperfine times
- `outputs/<bench>.txt` — verified stdout
- `build.json` — compile commands and wall times

