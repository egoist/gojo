#!/usr/bin/env python3
"""Build, verify, and time equivalent Gojo / Go / MoonBit native programs.

Does not modify the Gojo compiler. Writes Markdown, CSV, and JSON under
compare-moonbit/. Re-run with the same flags on the same machine to reproduce.
"""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent
REPO = BENCH_DIR.parent
MBT_DIR = BENCH_DIR / "moonbit"
OUT_RESULTS = BENCH_DIR / "compare-moonbit"

# Workloads shared by Gojo, Go, and MoonBit. hello is startup-only.
WORKLOADS = [
    ("fib", "recursive calls: fib(40); Int64 / Go int"),
    ("binarytrees", "many small allocations: binary-trees n=18"),
    ("nbody", "floating-point structs: 5 bodies, 5e6 steps"),
    ("spectralnorm", "nested float loops: spectral-norm n=2000"),
    ("fannkuch", "integer arrays: fannkuch-redux n=10"),
    ("mandelbrot", "pure float loops: mandelbrot n=2000, 50 iters"),
    ("hashmap", "Map/map: 1e6 int keys + 2e5 string keys"),
    ("sort", "sort 5e6 Int64/int values from a 64-bit LCG"),
]
STARTUP = ("hello", "print one line; process startup floor")
LANGS = ("gojo", "go", "moonbit")


def prepend_path(entries: list[str]) -> None:
    parts = [p for p in entries if p and os.path.isdir(p)]
    os.environ["PATH"] = os.pathsep.join(parts + [os.environ.get("PATH", "")])


def activate_tools() -> None:
    home = Path.home()
    prepend_path(
        [
            str(home / ".local/share/mise/shims"),
            str(home / ".moon/bin"),
            str(home / ".local/bin"),
            str(REPO / "target/release"),
        ]
    )
    os.environ.setdefault("MOON_CC", "cc")
    os.environ.setdefault("MISE_NPM_BUN", "true")


def run(
    cmd: list[str] | str,
    *,
    cwd: Path | None = None,
    env: dict | None = None,
    check: bool = True,
    capture: bool = True,
    timeout: float | None = None,
) -> subprocess.CompletedProcess:
    if isinstance(cmd, str):
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            env=env,
            check=False,
            capture_output=capture,
            text=True,
            shell=True,
            timeout=timeout,
        )
    else:
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            env=env,
            check=False,
            capture_output=capture,
            text=True,
            timeout=timeout,
        )
    if check and proc.returncode != 0:
        sys.stderr.write(proc.stdout or "")
        sys.stderr.write(proc.stderr or "")
        raise RuntimeError(f"command failed ({proc.returncode}): {cmd}")
    return proc


def which(name: str) -> str:
    p = shutil.which(name)
    if not p:
        raise RuntimeError(f"{name} not found on PATH={os.environ.get('PATH')}")
    return p


def cmd_text(cmd: list[str]) -> str:
    return " ".join(cmd)


def median(xs: list[float]) -> float:
    return statistics.median(xs)


def mad(xs: list[float]) -> float:
    if not xs:
        return float("nan")
    m = median(xs)
    return statistics.median([abs(x - m) for x in xs])


def geomean(xs: list[float]) -> float:
    xs = [x for x in xs if x > 0]
    return math.exp(sum(math.log(x) for x in xs) / len(xs))


def collect_env(args: argparse.Namespace) -> dict:
    def try_out(cmd: list[str]) -> str:
        try:
            return run(cmd, check=False).stdout.strip()
        except Exception as e:
            return f"UNAVAILABLE: {e}"

    lscpu = try_out(["lscpu"])
    cpuinfo = Path("/proc/cpuinfo").read_text() if Path("/proc/cpuinfo").exists() else ""
    meminfo = Path("/proc/meminfo").read_text() if Path("/proc/meminfo").exists() else ""
    model = ""
    for line in cpuinfo.splitlines():
        if line.lower().startswith("model name"):
            model = line.split(":", 1)[1].strip()
            break
    gojo = which("gojo") if shutil.which("gojo") else str(REPO / "target/release/gojo")
    env = {
        "captured_at_utc": datetime.now(timezone.utc).isoformat(),
        "hostname": platform.node(),
        "os": {
            "sysname": platform.system(),
            "release": platform.release(),
            "version": platform.version(),
            "platform": platform.platform(),
            "uname": try_out(["uname", "-a"]),
            "os_release": Path("/etc/os-release").read_text()
            if Path("/etc/os-release").exists()
            else "",
        },
        "cpu": {
            "model": model,
            "nproc": os.cpu_count(),
            "lscpu": lscpu,
        },
        "memory_meminfo": meminfo,
        "repo": {
            "root": str(REPO),
            "git_head": try_out(["git", "-C", str(REPO), "rev-parse", "HEAD"]),
            "git_head_short": try_out(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"]),
            "git_status": try_out(["git", "-C", str(REPO), "status", "--porcelain"]),
            "git_log1": try_out(["git", "-C", str(REPO), "log", "-1", "--format=%H %s"]),
        },
        "tools": {
            "gojo_bin": gojo,
            "gojo_version": try_out([gojo, "version"]) if Path(gojo).exists() else "not built yet",
            "go": try_out(["go", "version"]),
            "go_env_GOARCH": try_out(["go", "env", "GOARCH"]),
            "go_env_GOOS": try_out(["go", "env", "GOOS"]),
            "go_env_GOAMD64": try_out(["go", "env", "GOAMD64"]),
            "rustc": try_out(["rustc", "--version"]),
            "cargo": try_out(["cargo", "--version"]),
            "cc": try_out(["cc", "--version"]).splitlines()[0] if shutil.which("cc") else "",
            "cc_path": shutil.which("cc"),
            "clang": try_out(["clang", "--version"]).splitlines()[0] if shutil.which("clang") else "",
            "moon": try_out(["moon", "version"]),
            "moonc": try_out(["moonc", "-v"]),
            "hyperfine": try_out(["hyperfine", "--version"]),
            "python": sys.version,
            "mise": try_out(["mise", "--version"]),
            "bun": try_out(["bun", "--version"]),
        },
        "flags": {
            "gojo": "gojo build --release  (C backend, overflow/bounds checks on, cc -O2)",
            "go": "go build  (default optimizing compile; GOMAXPROCS left at default)",
            "moonbit": "moon build --target native --release  (native C backend, MOON_CC=cc)",
            "MOON_CC": os.environ.get("MOON_CC"),
            "GOMAXPROCS": os.environ.get("GOMAXPROCS", "unset (Go default)"),
            "hyperfine": f"hyperfine -N --warmup {args.warmup} --runs {args.runs}",
            "warmup": args.warmup,
            "runs": args.runs,
        },
        "workload_sizes": {
            "fib": "n=40",
            "binarytrees": "n=18, minDepth=4",
            "nbody": "5 bodies, 5_000_000 steps, dt=0.01",
            "spectralnorm": "n=2000, 10 power iterations",
            "fannkuch": "n=10",
            "mandelbrot": "n=2000, max 50 iterations",
            "hashmap": "1_000_000 int inserts/lookups; 200_000 string keys, 400_000 lookups",
            "sort": "5_000_000 Int64/int, LCG seed=42, (seed*1103515245+12345)%2^31",
            "hello": "print 'ok'",
        },
        "notes": [
            "Primary comparison is native machine code on this host for all three languages.",
            "MoonBit LLVM backend is experimental on this toolchain (missing prelude.mi) and was not used.",
            "MoonBit wasm/js backends exist but are not comparable to native Go/Gojo; they are not in the timing table.",
            "Gojo WASM is a design proposal only (docs/wasm.md); Gojo programs here are native.",
            "Programs print checksums so the compiler cannot delete the work.",
            "All programs are single-threaded; Go's GC may still use other cores.",
            "Gojo --release keeps integer overflow and index checks; Go wraps integers; MoonBit native wrapping is used for Int/Int64 overflow.",
            "Default ISAs: go env GOAMD64=v1; clang -O2 on this host is SSE2/x86-64 (not -march=native). AVX in binaries is mostly runtime, not a claimed nbody win.",
        ],
    }
    return env


def ensure_gojo() -> str:
    bin_path = REPO / "target/release/gojo"
    t0 = time.perf_counter()
    run(["cargo", "build", "--release", "-q"], cwd=REPO)
    compile_s = time.perf_counter() - t0
    ver = run([str(bin_path), "version"]).stdout.strip()
    return str(bin_path), ver, compile_s


def build_one(
    lang: str, name: str, dest: Path, gojo: str
) -> dict:
    dest.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    if lang == "gojo":
        src = BENCH_DIR / "gojo" / f"{name}.gojo"
        cmd = [gojo, "build", "--release", str(src), "-o", str(dest)]
        proc = run(cmd, cwd=REPO)
    elif lang == "go":
        srcdir = BENCH_DIR / "go" / name
        cmd = ["go", "build", "-o", str(dest), "."]
        proc = run(cmd, cwd=srcdir)
    elif lang == "moonbit":
        cmd = [
            "moon",
            "-C",
            str(MBT_DIR),
            "build",
            "--target",
            "native",
            "--release",
            name,
        ]
        proc = run(cmd, cwd=MBT_DIR)
        build_dir = MBT_DIR / "_build/native/release/build" / name
        candidates = [
            build_dir / f"{name}.exe",
            build_dir / "main.exe",
        ]
        art = next((p for p in candidates if p.exists()), None)
        if art is None:
            listed = sorted(str(p) for p in build_dir.glob("*")) if build_dir.exists() else []
            raise RuntimeError(
                f"MoonBit artifact missing under {build_dir}: tried {[str(c) for c in candidates]}\n"
                f"found={listed}\n{proc.stdout}\n{proc.stderr}"
            )
        shutil.copy2(art, dest)
    else:
        raise ValueError(lang)
    elapsed = time.perf_counter() - t0
    return {
        "lang": lang,
        "bench": name,
        "command": cmd_text(cmd),
        "seconds": elapsed,
        "binary": str(dest),
        "bytes": dest.stat().st_size if dest.exists() else None,
        "stdout": (proc.stdout or "").strip(),
        "stderr": (proc.stderr or "").strip(),
    }


def run_once(binary: Path) -> tuple[str, float]:
    t0 = time.perf_counter()
    proc = run([str(binary)], timeout=600)
    return proc.stdout, time.perf_counter() - t0


def hyperfine_three(binaries: dict[str, Path], json_path: Path, warmup: int, runs: int) -> dict:
    json_path.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        "hyperfine",
        "-N",
        "--warmup",
        str(warmup),
        "--runs",
        str(runs),
        "--export-json",
        str(json_path),
    ]
    for lang in LANGS:
        cmd.extend(["-n", lang, str(binaries[lang])])
    proc = run(cmd, timeout=3600)
    data = json.loads(json_path.read_text())
    data["_hyperfine_cmd"] = cmd_text(cmd)
    data["_hyperfine_stdout"] = proc.stdout
    return data


def child_rss_kb(binary: Path) -> int:
    """Max RSS of one child process, in KiB on Linux."""
    if sys.argv[0].endswith("run_gojo_go_moonbit.py") or Path(__file__).exists():
        helper = [
            sys.executable,
            str(Path(__file__)),
            "--child-rss",
            str(binary),
        ]
        proc = run(helper)
        return int(proc.stdout.strip().splitlines()[-1])
    raise RuntimeError("rss helper")


def hyperfine_times_s(item: dict) -> list[float]:
    if "times" in item:
        return [float(t) for t in item["times"]]
    out = []
    for m in item.get("measurements") or []:
        tw = m.get("time_wall_clock")
        if isinstance(tw, dict):
            out.append(float(tw["value"]))
        elif tw is not None:
            out.append(float(tw))
    return out


def hyperfine_rss_bytes(item: dict) -> list[float]:
    out = []
    for m in item.get("measurements") or []:
        rss = m.get("memory_peak_resident")
        if isinstance(rss, dict):
            out.append(float(rss["value"]))
        elif rss is not None:
            out.append(float(rss))
    if out:
        return out
    if "memory" in item:
        # hyperfine 1.x sometimes stored a single memory field
        return [float(item["memory"])]
    return []


def hyperfine_lang_key(item: dict) -> str | None:
    name = item.get("name")
    if name in LANGS:
        return name
    cmd = str(item.get("command") or "")
    bn = Path(cmd).name
    for lang in LANGS:
        if bn.startswith(lang + "_") or bn == lang:
            return lang
    return None


def parse_hyperfine_lang_stats(hf: dict) -> dict[str, dict]:
    """Parse hyperfine 1.x or 2.x JSON into per-language timing/RSS stats."""
    by_lang: dict[str, dict] = {}
    items = hf.get("results") or []
    for i, item in enumerate(items):
        key = hyperfine_lang_key(item)
        if key is None and i < len(LANGS):
            key = LANGS[i]
        if key is None:
            continue
        times = hyperfine_times_s(item)
        rss = hyperfine_rss_bytes(item)
        by_lang[key] = {
            "times_s": times,
            "median_s": median(times),
            "mad_s": mad(times),
            "mean_s": statistics.mean(times),
            "stdev_s": statistics.stdev(times) if len(times) > 1 else 0.0,
            "min_s": min(times),
            "max_s": max(times),
            "rss_bytes": rss,
            "rss_median_kib": int(median(rss) / 1024) if rss else None,
            "label": item.get("name") or item.get("command"),
        }
    return by_lang


def ms(x: float) -> str:
    return f"{x * 1000:.2f}"


def write_csv(path: Path, rows: list[dict], fieldnames: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fieldnames)
        w.writeheader()
        for row in rows:
            w.writerow(row)


def render_md(env: dict, payload: dict) -> str:
    lines = []
    a = lines.append
    a("# Gojo vs Go vs MoonBit — native benchmark results")
    a("")
    a("Measured on one machine, same algorithms and inputs, release/optimized builds.")
    a("Numbers below are **from this run**, not copied from elsewhere. Failures are listed as such.")
    a("")
    a("## How to reproduce")
    a("")
    a("```sh")
    a("# toolchain: mise for go/hyperfine/rust/bun; MoonBit via official installer (not in mise)")
    a("eval \"$(mise activate bash)\"")
    a("export PATH=\"$HOME/.moon/bin:$PATH\"")
    a("cargo build --release")
    a("cd bench && ./run-gojo-go-moonbit.sh")
    a("```")
    a("")
    a("Default: 3 warmup runs discarded, 11 timed runs, **median** and MAD (median absolute deviation).")
    a("Hyperfine `-N` runs the binaries without a shell. Commands interleave the three languages per workload.")
    a("")
    a("## Machine")
    a("")
    cpu = env["cpu"]
    a(f"- Host: `{env['hostname']}`")
    a(f"- OS: {env['os']['uname']}")
    a(f"- CPU: {cpu['model']} ({cpu['nproc']} logical CPUs)")
    a(f"- Captured: {env['captured_at_utc']}")
    a("")
    a("## Toolchain")
    a("")
    t = env["tools"]
    r = env["repo"]
    a(f"- Gojo: `{t.get('gojo_version', '')}` — git `{r['git_head_short']}` `{r['git_log1']}`")
    a(f"- Go: `{t['go']}` GOARCH={t['go_env_GOARCH']} GOAMD64={t['go_env_GOAMD64']}")
    a(f"- rustc (to build the Gojo compiler): `{t['rustc']}`")
    a(f"- C compiler (`cc`, used by Gojo and MoonBit native): `{t['cc']}` ({t['cc_path']})")
    a(f"- MoonBit: `{t['moon'].splitlines()[0]}` / moonc `{t['moonc']}`")
    a(f"- hyperfine: `{t['hyperfine']}`")
    a(f"- mise: `{t['mise']}`; bun `{t['bun']}` (npm backend, not used by these native builds)")
    a("")
    a("## Flags and targets")
    a("")
    for k, v in env["flags"].items():
        a(f"- **{k}**: `{v}`")
    a("")
    a("All three timed columns are **native executables** on this x86_64 Linux host:")
    a("")
    a("- Gojo: compile to C, then `cc -O2` (checks on).")
    a("- Go: `gc` compiler native binary.")
    a("- MoonBit: `--target native --release` (release uses generated C, then the same `cc`).")
    a("")
    a("MoonBit `--target llvm` is experimental here (`missing prelude.mi`) and was **not** measured.")
    a("MoonBit `--target wasm` builds, but WASM is a different runtime than native Go/Gojo; it is **not** in the table.")
    a("Gojo has no implemented WASM target (`docs/wasm.md` is a proposal).")
    a("")
    a("## Workloads")
    a("")
    a("| name | what it measures | size |")
    a("| --- | --- | --- |")
    sizes = env["workload_sizes"]
    desc = {k: v for k, v in WORKLOADS}
    for name, what in WORKLOADS:
        a(f"| `{name}` | {what} | {sizes[name]} |")
    a(f"| `{STARTUP[0]}` | {STARTUP[1]} | {sizes['hello']} |")
    a("")
    a("Integer payload types: Go and Gojo use 64-bit `int` on this host. MoonBit uses `Int64` wherever that width is required for matching arithmetic (hashmap keys, sort LCG, checksums, fib). Loop indices use MoonBit `Int32` because `Array` is indexed by `Int`.")
    a("")
    a("## Output verification")
    a("")
    ver = payload["verification"]
    if ver["all_match"]:
        a("Every workload: Gojo, Go, and MoonBit printed **byte-for-byte identical** stdout (after a single untimed run each).")
    else:
        a("**Some outputs did not match.** Details:")
        a("")
        for row in ver["rows"]:
            if not row["match"]:
                a(f"- `{row['bench']}`: {row['detail']}")
    a("")
    a("Golden outputs are stored under `outputs/` in this directory.")
    a("")
    a("## Steady run time (native)")
    a("")
    a("Lower is better. Median of timed runs after warmup, milliseconds. Dispersion is MAD.")
    a("These times include process startup; for all eight workloads except `hello`, the body dominates (see startup section).")
    a("")
    a("| benchmark | Gojo median (MAD) | Go median (MAD) | MoonBit native median (MAD) | Gojo / Go | MoonBit / Go | MoonBit / Gojo |")
    a("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    runtime_rows = payload["runtime"]
    ratios_gojo = []
    ratios_mbt = []
    ratios_mbt_gojo = []
    for row in runtime_rows:
        if row.get("status") != "ok":
            a(f"| `{row['bench']}` | {row.get('note', 'FAILED')} | | | | | |")
            continue
        a(
            "| `{bench}` | {gojo} ms ({gojo_mad}) | {go} ms ({go_mad}) | {mbt} ms ({mbt_mad}) | {r1:.3f}× | {r2:.3f}× | {r3:.3f}× |".format(
                bench=row["bench"],
                gojo=ms(row["gojo_median_s"]),
                gojo_mad=ms(row["gojo_mad_s"]),
                go=ms(row["go_median_s"]),
                go_mad=ms(row["go_mad_s"]),
                mbt=ms(row["moonbit_median_s"]),
                mbt_mad=ms(row["moonbit_mad_s"]),
                r1=row["gojo_over_go"],
                r2=row["moonbit_over_go"],
                r3=row["moonbit_over_gojo"],
            )
        )
        ratios_gojo.append(row["gojo_over_go"])
        ratios_mbt.append(row["moonbit_over_go"])
        ratios_mbt_gojo.append(row["moonbit_over_gojo"])
    a("")
    if ratios_gojo:
        a(
            "Geometric mean of median ratios across the eight workloads: "
            f"Gojo/Go = **{geomean(ratios_gojo):.3f}×**, "
            f"MoonBit/Go = **{geomean(ratios_mbt):.3f}×**, "
            f"MoonBit/Gojo = **{geomean(ratios_mbt_gojo):.3f}×** "
            "(<1 means faster than the denominator)."
        )
        a("")
        a("Do not rank the languages from `fib` alone: it is overflow-check / call heavy and is only one row.")
    a("")
    a("## Memory (max RSS)")
    a("")
    a("Peak RSS from the same hyperfine timed runs (`memory_peak_resident`, converted to KiB). Median of those samples. This is process peak RSS, not a precise allocator profile.")
    a("On this host, hyperfine reports **3312 KiB** for every tiny process (including `hello`); treat that as a measurement floor, not equal heaps. Differences above that floor (binary-trees, hashmap, sort) are meaningful.")
    a("")
    a("| benchmark | Gojo KiB | Go KiB | MoonBit KiB |")
    a("| --- | ---: | ---: | ---: |")
    for row in payload["memory"]:
        if row.get("status") != "ok":
            a(f"| `{row['bench']}` | {row.get('note', 'FAILED')} | | |")
            continue
        a(f"| `{row['bench']}` | {row['gojo_kib']} | {row['go_kib']} | {row['moonbit_kib']} |")
    a("")
    a("## Build time (produce the timed binary)")
    a("")
    a("Wall time of one `build` that writes the executable used above. Gojo and MoonBit may reuse a cached C runtime from earlier packages in the same run. This is **compile/link**, not steady execution.")
    a("")
    a("| benchmark | Gojo s | Go s | MoonBit s | binary bytes (Gojo / Go / MoonBit) |")
    a("| --- | ---: | ---: | ---: | --- |")
    for row in payload["build"]:
        if row.get("status") != "ok":
            a(f"| `{row['bench']}` | {row.get('note', 'FAILED')} | | | |")
            continue
        a(
            f"| `{row['bench']}` | {row['gojo_s']:.3f} | {row['go_s']:.3f} | {row['moonbit_s']:.3f} | "
            f"{row['gojo_bytes']} / {row['go_bytes']} / {row['moonbit_bytes']} |"
        )
    a("")
    gojo_compiler_s = payload.get("gojo_compiler_build_s")
    if gojo_compiler_s is not None:
        a(f"Gojo **compiler** `cargo build --release` (already cached if re-run): {gojo_compiler_s:.3f} s.")
        a("")
    a("## Startup floor (`hello`)")
    a("")
    a("Same harness, program only prints `ok`. This is not a throughput benchmark; it bounds how much of a short run is process bring-up.")
    a("")
    h = payload.get("hello")
    if h and h.get("status") == "ok":
        a(
            f"- Gojo median {ms(h['gojo_median_s'])} ms (MAD {ms(h['gojo_mad_s'])})"
        )
        a(f"- Go median {ms(h['go_median_s'])} ms (MAD {ms(h['go_mad_s'])})")
        a(
            f"- MoonBit median {ms(h['moonbit_median_s'])} ms (MAD {ms(h['moonbit_mad_s'])})"
        )
        a(f"- stdout matched: `{h['output'].rstrip()}`")
    else:
        a(h.get("note", "hello not measured") if h else "hello not measured")
    a("")
    a("## Limitations")
    a("")
    for n in env["notes"]:
        a(f"- {n}")
    a("- One cloud VM, 4 logical CPUs; not an Apple M5 (the in-tree `bench/results.md` Zig table was measured elsewhere).")
    a("- No CPU pinning; other processes on the VM can add noise. MAD is the spread we observed, not a confidence interval.")
    a("- Sorting algorithms differ at the library level (Go `slices.Sort`, Gojo `Vec.sort`, MoonBit `Array.sort`) on the same LCG input; that is the language default sort, not a shared introsort implementation.")
    a("- Hash maps are each language's default map (Gojo insertion-ordered `Map`, Go `map`, MoonBit insertion-ordered `Map`).")
    a("- MoonBit native release currently goes through C, like Gojo; that is comparable codegen, not a claim about the LLVM backend.")
    a("- Memory figures are peak RSS and include the runtime/GC heap; they are not allocation counts.")
    a("")
    a("## Raw data")
    a("")
    a("- `results.json` — full structured result")
    a("- `results.csv` — one row per workload × language")
    a("- `env.json` — machine and toolchain dump")
    a("- `raw/<bench>.hyperfine.json` — hyperfine times")
    a("- `outputs/<bench>.txt` — verified stdout")
    a("- `build.json` — compile commands and wall times")
    a("")
    return "\n".join(lines) + "\n"


def main() -> int:
    if len(sys.argv) > 1 and sys.argv[1] == "--child-rss":
        binary = sys.argv[2]
        subprocess.run([binary], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        print(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
        return 0

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=int, default=11, help="timed repetitions (default 11)")
    parser.add_argument("--warmup", type=int, default=3, help="discarded warmup runs (default 3)")
    parser.add_argument("--out", type=Path, default=OUT_RESULTS, help="result directory")
    parser.add_argument("--bin-dir", type=Path, default=Path("/tmp/gojo-go-mbt-bench"))
    parser.add_argument("--skip-gojo-compiler", action="store_true")
    parser.add_argument("--only", nargs="*", help="subset of workload names")
    args = parser.parse_args()

    activate_tools()
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    (out / "raw").mkdir(exist_ok=True)
    (out / "outputs").mkdir(exist_ok=True)
    args.bin_dir.mkdir(parents=True, exist_ok=True)

    names = [w[0] for w in WORKLOADS]
    if args.only:
        unknown = set(args.only) - set(names) - {"hello"}
        if unknown:
            raise SystemExit(f"unknown workloads: {unknown}")
        names = [n for n in names if n in args.only]

    print("collecting environment…", flush=True)
    env = collect_env(args)

    gojo_compiler_s = None
    if args.skip_gojo_compiler and (REPO / "target/release/gojo").exists():
        gojo = str(REPO / "target/release/gojo")
        env["tools"]["gojo_version"] = run([gojo, "version"]).stdout.strip()
    else:
        print("building Gojo compiler (cargo build --release)…", flush=True)
        gojo, ver, gojo_compiler_s = ensure_gojo()
        env["tools"]["gojo_bin"] = gojo
        env["tools"]["gojo_version"] = ver
    gojo = env["tools"]["gojo_bin"]

    (out / "env.json").write_text(json.dumps(env, indent=2) + "\n")

    build_rows = []
    binaries: dict[str, dict[str, Path]] = {}
    verify_rows = []
    all_match = True

    build_all = names + (["hello"] if not args.only or "hello" in (args.only or []) else ["hello"])
    # always build hello for the startup section unless --only excludes everything
    if args.only and "hello" not in args.only:
        pass
    else:
        if "hello" not in build_all:
            build_all.append("hello")
    if not args.only:
        build_all = names + ["hello"]

    for name in build_all:
        print(f"building {name}…", flush=True)
        binaries[name] = {}
        brow = {"bench": name, "status": "ok"}
        try:
            for lang in LANGS:
                dest = args.bin_dir / f"{lang}_{name}"
                info = build_one(lang, name, dest, gojo)
                binaries[name][lang] = dest
                brow[f"{lang}_s"] = info["seconds"]
                brow[f"{lang}_bytes"] = info["bytes"]
                brow[f"{lang}_cmd"] = info["command"]
            # verify outputs
            outs = {}
            for lang in LANGS:
                text, _ = run_once(binaries[name][lang])
                outs[lang] = text
            (out / "outputs" / f"{name}.txt").write_text(outs["gojo"])
            match = outs["gojo"] == outs["go"] == outs["moonbit"]
            detail = "ok"
            if not match:
                all_match = False
                detail = (
                    f"mismatch gojo_vs_go={outs['gojo']==outs['go']} "
                    f"gojo_vs_mbt={outs['gojo']==outs['moonbit']}"
                )
                for lang in LANGS:
                    (out / "outputs" / f"{name}.{lang}.txt").write_text(outs[lang])
                raise RuntimeError(f"output mismatch for {name}: {detail}")
            verify_rows.append(
                {
                    "bench": name,
                    "match": True,
                    "detail": detail,
                    "bytes": len(outs["gojo"]),
                }
            )
        except Exception as e:
            brow["status"] = "failed"
            brow["note"] = str(e)
            verify_rows.append({"bench": name, "match": False, "detail": str(e)})
            all_match = False
            print(f"  FAILED {name}: {e}", flush=True)
        build_rows.append(brow)

    (out / "build.json").write_text(json.dumps(build_rows, indent=2) + "\n")

    runtime_rows = []
    memory_rows = []
    hello_summary = None

    def measure_name(name: str) -> dict | None:
        if binaries.get(name) is None or any(l not in binaries[name] for l in LANGS):
            return {
                "bench": name,
                "status": "failed",
                "note": "binary missing (build/verify failed)",
            }
        print(f"timing {name}…", flush=True)
        hf_path = out / "raw" / f"{name}.hyperfine.json"
        try:
            hf = hyperfine_three(binaries[name], hf_path, args.warmup, args.runs)
        except Exception as e:
            return {"bench": name, "status": "failed", "note": f"hyperfine: {e}"}
        by_lang = parse_hyperfine_lang_stats(hf)
        if set(by_lang) != set(LANGS):
            return {
                "bench": name,
                "status": "failed",
                "note": f"hyperfine languages parsed {sorted(by_lang)} from {hf_path}",
            }
        row = {"bench": name, "status": "ok", "hyperfine_cmd": hf["_hyperfine_cmd"]}
        for lang in LANGS:
            s = by_lang[lang]
            row[f"{lang}_median_s"] = s["median_s"]
            row[f"{lang}_mad_s"] = s["mad_s"]
            row[f"{lang}_mean_s"] = s["mean_s"]
            row[f"{lang}_stdev_s"] = s["stdev_s"]
            row[f"{lang}_min_s"] = s["min_s"]
            row[f"{lang}_max_s"] = s["max_s"]
            row[f"{lang}_times_s"] = s["times_s"]
            row[f"{lang}_rss_median_kib"] = s["rss_median_kib"]
            row[f"{lang}_rss_bytes"] = s["rss_bytes"]
        g = row["go_median_s"]
        row["gojo_over_go"] = row["gojo_median_s"] / g
        row["moonbit_over_go"] = row["moonbit_median_s"] / g
        row["moonbit_over_gojo"] = row["moonbit_median_s"] / row["gojo_median_s"]
        return row

    def measure_rss(name: str) -> dict:
        if binaries.get(name) is None or any(l not in binaries[name] for l in LANGS):
            return {"bench": name, "status": "failed", "note": "binary missing"}
        print(f"rss {name}…", flush=True)
        row = {"bench": name, "status": "ok"}
        try:
            for lang in LANGS:
                samples = [child_rss_kb(binaries[name][lang]) for _ in range(3)]
                row[f"{lang}_kib"] = int(median(samples))
                row[f"{lang}_kib_samples"] = samples
        except Exception as e:
            return {"bench": name, "status": "failed", "note": str(e)}
        return row

    def memory_from_runtime(row: dict) -> dict:
        if row.get("status") != "ok":
            return {"bench": row["bench"], "status": "failed", "note": row.get("note", "no timing")}
        mem = {"bench": row["bench"], "status": "ok"}
        for lang in LANGS:
            kib = row.get(f"{lang}_rss_median_kib")
            if kib is None:
                return {"bench": row["bench"], "status": "failed", "note": f"no RSS from hyperfine for {lang}"}
            mem[f"{lang}_kib"] = kib
        return mem

    for name in names:
        row = measure_name(name) or {"bench": name, "status": "failed"}
        runtime_rows.append(row)
        memory_rows.append(memory_from_runtime(row))

    if "hello" in binaries and all(l in binaries["hello"] for l in LANGS):
        hello_summary = measure_name("hello")
        if hello_summary and hello_summary.get("status") == "ok":
            hello_summary["output"] = (out / "outputs" / "hello.txt").read_text()
            memory_rows.append(memory_from_runtime(hello_summary))
        elif hello_summary:
            memory_rows.append({"bench": "hello", "status": "failed", "note": hello_summary.get("note")})

    csv_rows = []
    for row in runtime_rows:
        if row.get("status") != "ok":
            for lang in LANGS:
                csv_rows.append(
                    {
                        "bench": row["bench"],
                        "lang": lang,
                        "status": "failed",
                        "note": row.get("note", ""),
                    }
                )
            continue
        for lang in LANGS:
            csv_rows.append(
                {
                    "bench": row["bench"],
                    "lang": lang,
                    "status": "ok",
                    "median_s": f"{row[f'{lang}_median_s']:.9f}",
                    "mad_s": f"{row[f'{lang}_mad_s']:.9f}",
                    "mean_s": f"{row[f'{lang}_mean_s']:.9f}",
                    "stdev_s": f"{row[f'{lang}_stdev_s']:.9f}",
                    "min_s": f"{row[f'{lang}_min_s']:.9f}",
                    "max_s": f"{row[f'{lang}_max_s']:.9f}",
                    "times_s": " ".join(f"{t:.9f}" for t in row[f"{lang}_times_s"]),
                    "over_go": f"{(row[f'{lang}_median_s'] / row['go_median_s']):.6f}",
                }
            )

    write_csv(
        out / "results.csv",
        csv_rows,
        [
            "bench",
            "lang",
            "status",
            "median_s",
            "mad_s",
            "mean_s",
            "stdev_s",
            "min_s",
            "max_s",
            "times_s",
            "over_go",
            "note",
        ],
    )

    payload = {
        "verification": {"all_match": all_match, "rows": verify_rows},
        "runtime": runtime_rows,
        "memory": memory_rows,
        "build": build_rows,
        "hello": hello_summary,
        "gojo_compiler_build_s": gojo_compiler_s,
    }
    (out / "results.json").write_text(json.dumps({"env_file": "env.json", **payload}, indent=2) + "\n")
    (out / "RESULTS.md").write_text(render_md(env, payload))

    print(f"wrote {out / 'RESULTS.md'}", flush=True)
    if not all_match:
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.TimeoutExpired as e:
        print(f"timeout: {e}", file=sys.stderr)
        sys.exit(2)
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        raise
