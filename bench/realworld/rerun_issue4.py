#!/usr/bin/env python3
"""Independent 3-way rerun of the real-world suite for issue #4 / PR #5.

Compares, on one VM, with one copy of the workload sources:

  - Gojo baseline compiler  0c6e3f904d6e5cbaa0643cfce8084a8598863ac8
  - Gojo optimized compiler a8a7643e30d71cfcf31855e793718dad1616b62c  (PR #5 HEAD)
  - Idiomatic Rust          cargo --release, unchanged control

Harness protocol follows PR #5's improved run_realworld.py (repeated
interleaved HTTP windows, CPU accounting, optional taskset) but measures all
three implementations in one session. Workload sources are the original
bench/realworld programs (seed 20261109, same mix and routes). This script
does not modify compiler sources.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import resource
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent
GOJO_SRC = ROOT / "gojo"
RUST_DIR = ROOT / "rust"
FIXTURES = ROOT / "fixtures"

BASE_SHA = "0c6e3f904d6e5cbaa0643cfce8084a8598863ac8"
OPT_SHA = "a8a7643e30d71cfcf31855e793718dad1616b62c"
WORKLOAD_SHA = "4b42da3d570bc06e2b46274d86370514c7cc93ac"

IMPLS = ("gojo_base", "gojo_opt", "rust")
IMPL_LABEL = {
    "gojo_base": "Gojo baseline (0c6e3f9)",
    "gojo_opt": "Gojo PR #5 (a8a7643)",
    "rust": "Rust control",
}
IMPL_SHORT = {"gojo_base": "Gojo base", "gojo_opt": "Gojo opt", "rust": "Rust"}
COLORS = {"gojo_base": "#8a9aab", "gojo_opt": "#2b6ca3", "rust": "#c45c26"}

ARTIFACT_DIR = Path("/opt/cursor/artifacts/rust-gojo-rerun-issue4")


def run(cmd, **kw):
    kw.setdefault("check", True)
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run(cmd, **kw)


def which_or(name: str, fallback: str | None = None) -> str:
    p = shutil.which(name)
    if p:
        return p
    if fallback and Path(fallback).exists():
        return fallback
    raise SystemExit(f"missing {name}")


def capture(cmd, **kw) -> str:
    r = subprocess.run(
        cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, **kw
    )
    return r.stdout.strip()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def median(xs: list[float]) -> float:
    if not xs:
        return float("nan")
    return statistics.median(xs)


def mad(xs: list[float], med: float) -> float:
    if not xs:
        return float("nan")
    return statistics.median([abs(x - med) for x in xs])


def pinned(cmd: list[str], cpus: str | None) -> list[str]:
    return ["taskset", "-c", cpus, *cmd] if cpus else cmd


def cpu_count(cpus: str) -> int:
    n = 0
    for part in cpus.split(","):
        lo, _, hi = part.partition("-")
        n += int(hi or lo) - int(lo) + 1
    return n


def host_info() -> dict:
    cpu = ""
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.lower().startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    mem_kb = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_kb = int(line.split()[1])
                break
    except OSError:
        pass
    affinity = sorted(os.sched_getaffinity(0))
    return {
        "uname": capture(["uname", "-a"]),
        "cpu": cpu,
        "nproc": os.cpu_count(),
        "mem_kb": mem_kb,
        "affinity": affinity,
        "taskset": shutil.which("taskset"),
        "rustc": capture([which_or("rustc"), "--version"]),
        "cargo": capture([which_or("cargo"), "--version"]),
        "cc": capture([which_or("cc"), "--version"]).splitlines()[0],
        "hyperfine": capture(["hyperfine", "--version"]),
        "python": sys.version.split()[0],
        "cwd": str(ROOT),
    }


def generate_fixtures(events: int, logs: int, products: int, seed: int) -> dict:
    run(
        [
            sys.executable,
            str(ROOT / "gen_fixtures.py"),
            "--dir",
            str(FIXTURES),
            "--events",
            str(events),
            "--logs",
            str(logs),
            "--products",
            str(products),
            "--seed",
            str(seed),
        ]
    )
    return json.loads((FIXTURES / "manifest.json").read_text())


def build_gojo_progs(gojo: str, dest: Path, tag: str) -> dict:
    dest.mkdir(parents=True, exist_ok=True)
    bins = {}
    t0 = time.perf_counter()
    for name in ("jsonpipe", "logscan", "httpserver"):
        out = dest / f"{tag}-{name}"
        run([gojo, "build", "--release", str(GOJO_SRC / f"{name}.gojo"), "-o", str(out)])
        bins[name] = str(out)
        bins[f"{name}_sha256"] = sha256_file(out)
    bins["compile_s"] = time.perf_counter() - t0
    return bins


def build_rust(dest: Path) -> dict:
    dest.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    run(
        [which_or("cargo"), "build", "--release", "--bins"],
        cwd=str(RUST_DIR),
        env={**os.environ, "CARGO_TERM_COLOR": "always"},
    )
    rust_build_s = time.perf_counter() - t0
    bins = {"compile_s": rust_build_s}
    for name in ("jsonpipe", "logscan", "httpserver", "loadgen"):
        src = RUST_DIR / "target" / "release" / name
        dst = dest / f"rust-{name}"
        shutil.copy2(src, dst)
        bins[name] = str(dst)
        bins[f"{name}_sha256"] = sha256_file(dst)
    return bins


def run_stdout(cmd) -> bytes:
    r = subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.stderr:
        sys.stderr.buffer.write(r.stderr)
    return r.stdout


def parse_hyperfine(path: Path) -> dict:
    data = json.loads(path.read_text())
    result = data["results"][0]
    walls, rss = [], []
    for m in result["measurements"]:
        walls.append(float(m["time_wall_clock"]["value"]))
        rss.append(float(m["memory_peak_resident"]["value"]))
    med = median(walls)
    rss_med = median(rss)
    return {
        "command": result["command"],
        "n": len(walls),
        "times_s": walls,
        "rss_bytes": rss,
        "median_s": med,
        "mean_s": statistics.mean(walls),
        "stdev_s": statistics.stdev(walls) if len(walls) > 1 else 0.0,
        "mad_s": mad(walls, med),
        "min_s": min(walls),
        "max_s": max(walls),
        "rss_median_bytes": rss_med,
        "rss_min_bytes": min(rss),
        "rss_max_bytes": max(rss),
    }


def hyperfine_cmd(bin_path: str, args: list[str], runs: int, warmup: int, json_out: Path) -> dict:
    command = shlex.join([bin_path, *args])
    cmd = [
        "hyperfine",
        "--style",
        "none",
        "--shell",
        "bash",
        "--warmup",
        str(warmup),
        "--runs",
        str(runs),
        "--metrics",
        "time_wall_clock,memory_peak_resident",
        "--export-json",
        str(json_out),
        "--command-name",
        Path(bin_path).name,
        command,
    ]
    run(cmd)
    rec = parse_hyperfine(json_out)
    rec["hyperfine_json"] = str(json_out)
    rec["argv"] = cmd
    return rec


def read_status_bytes(pid: int, key: str) -> int | None:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(key + ":"):
                return int(line.split()[1]) * 1024
    except OSError:
        return None
    return None


def start_server(bin_path: str, products: Path, cpus: str | None = None) -> tuple[subprocess.Popen, str, int]:
    env = dict(os.environ)
    if cpus and "gojo" in Path(bin_path).name:
        env["GOJO_MAXPROCS"] = str(cpu_count(cpus))
    proc = subprocess.Popen(
        pinned([bin_path, str(products), "127.0.0.1:0"], cpus),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert proc.stdout is not None
    line = ""
    deadline = time.time() + 30
    while time.time() < deadline:
        got = proc.stdout.readline()
        if not got:
            if proc.poll() is not None:
                rest = proc.stdout.read()
                raise SystemExit(f"server exited {proc.returncode}: {line}{rest}")
            continue
        line += got
        m = re.search(r"listening\s+(\S+)", got)
        if m:
            addr = m.group(1).strip()
            url = f"http://{addr}"
            port = int(addr.rsplit(":", 1)[1])
            return proc, url, port
    raise SystemExit(f"server did not print listening line; got {line!r}")


def stop_server(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def http_get(url: str, timeout: float = 5.0) -> tuple[int, bytes]:
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def http_post_json(url: str, obj: dict, timeout: float = 5.0) -> tuple[int, bytes]:
    data = json.dumps(obj, separators=(",", ":")).encode()
    req = urllib.request.Request(
        url, data=data, method="POST", headers={"Content-Type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def check_http(url: str, products: list[dict]) -> dict:
    st, body = http_get(url + "/health")
    health = json.loads(body.decode())
    assert st == 200 and health == {"ok": True}, (st, body)

    st, body = http_get(url + "/stats")
    stats = json.loads(body.decode())
    assert st == 200
    assert stats["n"] == len(products)
    assert stats["cents"] == sum(p["cents"] for p in products)

    first = products[0]
    st, body = http_get(url + f"/products/{first['id']}")
    got = json.loads(body.decode())
    assert st == 200 and got == first, (st, got, first)

    st, body = http_get(url + "/products/0")
    assert st == 404, (st, body)

    q = {"region": first["region"], "min_cents": first["cents"], "tag": first["tag"]}
    st, body = http_post_json(url + "/query", q)
    out = json.loads(body.decode())
    assert st == 200
    expect_n = sum(
        1
        for p in products
        if p["region"] == q["region"] and p["cents"] >= q["min_cents"] and p["tag"] == q["tag"]
    )
    expect_ids = [
        p["id"]
        for p in products
        if p["region"] == q["region"] and p["cents"] >= q["min_cents"] and p["tag"] == q["tag"]
    ][:50]
    assert out["n"] == expect_n, (out["n"], expect_n)
    assert out["ids"] == expect_ids, (out["ids"][:5], expect_ids[:5])
    return {
        "stats_n": stats["n"],
        "stats_cents": stats["cents"],
        "query_n": out["n"],
        "query_ids": out["ids"][:5],
        "product_id": first["id"],
    }


def proc_cpu_s(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def host_cpu() -> tuple[int, int]:
    v = [int(x) for x in Path("/proc/stat").read_text().splitlines()[0].split()[1:]]
    return v[3] + v[4], sum(v[:8])


def loadgen_run(
    loadgen: str,
    url: str,
    concurrency: int,
    seconds: float,
    warmup: float,
    products: int,
    seed: int,
    server_pid: int,
    cpus: str | None,
) -> dict:
    cmd = [
        loadgen,
        "--url",
        url,
        "--seconds",
        str(seconds),
        "--warmup",
        str(warmup),
        "--concurrency",
        str(concurrency),
        "--products",
        str(products),
        "--seed",
        str(seed),
    ]
    u0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    s0, (i0, t0), w0 = proc_cpu_s(server_pid), host_cpu(), time.perf_counter()
    p = subprocess.Popen(pinned(cmd, cpus), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    out, errout = p.communicate()
    wall = time.perf_counter() - w0
    s1, (i1, t1) = proc_cpu_s(server_pid), host_cpu()
    u1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if p.returncode != 0:
        raise SystemExit(f"loadgen failed ({p.returncode}): {errout}")
    if errout.strip():
        print(errout, file=sys.stderr)
    row = json.loads(out.strip().splitlines()[-1])
    row["server_cpu_cores"] = (s1 - s0) / wall
    row["loadgen_cpu_cores"] = (u1.ru_utime + u1.ru_stime - u0.ru_utime - u0.ru_stime) / wall
    row["host_idle_frac"] = (i1 - i0) / max(1, t1 - t0)
    row["wall_s"] = wall
    row["loadgen_argv"] = pinned(cmd, cpus)
    return row


def summarize_trials(trials: list[dict]) -> dict:
    rps = [t["rps"] for t in trials]
    med = median(rps)
    out = {
        "concurrency": trials[0]["concurrency"],
        "trials": len(trials),
        "rps": med,
        "rps_min": min(rps),
        "rps_max": max(rps),
        "rps_mad": mad(rps, med),
        "rps_samples": rps,
        "ok": sum(t["ok"] for t in trials),
        "err": sum(t["err"] for t in trials),
    }
    for k in (
        "p50_ms",
        "p95_ms",
        "p99_ms",
        "server_cpu_cores",
        "loadgen_cpu_cores",
        "host_idle_frac",
        "server_rss_hwm_bytes",
        "server_rss_bytes",
    ):
        out[k] = median([t[k] for t in trials])
    out["p50_ms_min"] = min(t["p50_ms"] for t in trials)
    out["p50_ms_max"] = max(t["p50_ms"] for t in trials)
    out["p99_ms_min"] = min(t["p99_ms"] for t in trials)
    out["p99_ms_max"] = max(t["p99_ms"] for t in trials)
    return out


def measure_http(
    servers: dict[str, str],
    loadgen: str,
    products_path: Path,
    products: list[dict],
    concs: list[int],
    seconds: float,
    warmup: float,
    seed: int,
    trials: int,
    server_cpus: str | None,
    client_cpus: str | None,
) -> dict:
    started = {}
    try:
        for impl, bin_path in servers.items():
            proc, url, port = start_server(bin_path, products_path, server_cpus)
            started[impl] = (proc, url, port)
        time.sleep(0.25)
        res = {}
        for impl, (proc, url, port) in started.items():
            res[impl] = {
                "url": url,
                "port": port,
                "correctness": check_http(url, products),
                "raw": [],
            }
        langs = list(servers)
        for t in range(trials):
            order = langs if t % 2 == 0 else list(reversed(langs))
            for c in concs:
                for impl in order:
                    proc, url, _ = started[impl]
                    row = loadgen_run(
                        loadgen, url, c, seconds, warmup, len(products), seed, proc.pid, client_cpus
                    )
                    row["trial"] = t
                    row["impl"] = impl
                    row["server_rss_hwm_bytes"] = read_status_bytes(proc.pid, "VmHWM") or 0
                    row["server_rss_bytes"] = read_status_bytes(proc.pid, "VmRSS") or 0
                    res[impl]["raw"].append(row)
                    print(
                        f"  trial {t + 1}/{trials} {impl} c={c}: rps={row['rps']:.1f} "
                        f"p50={row['p50_ms']:.3f}ms p99={row['p99_ms']:.3f}ms err={row['err']} "
                        f"ok={row['ok']} server={row['server_cpu_cores']:.2f} "
                        f"loadgen={row['loadgen_cpu_cores']:.2f} idle={row['host_idle_frac']:.0%} "
                        f"rss={row['server_rss_bytes'] / 2**20:.1f}MiB",
                        flush=True,
                    )
        for impl, (proc, _, _) in started.items():
            r = res[impl]
            r["concurrency"] = [
                summarize_trials([x for x in r["raw"] if x["concurrency"] == c]) for c in concs
            ]
            r["server_rss_peak_bytes"] = read_status_bytes(proc.pid, "VmHWM") or 0
        return res
    finally:
        for proc, _, _ in started.values():
            stop_server(proc)


def fmt_ms(s: float) -> str:
    return f"{s * 1000:.1f} ms"


def fmt_rps(x: float) -> str:
    return f"{x:,.0f}"


def fmt_rss(b: float) -> str:
    return f"{b / (1024 * 1024):.1f} MiB"


def ratio(a: float, b: float) -> str:
    if not b:
        return "n/a"
    return f"{a / b:.2f}×"


def write_chart(doc: dict, path: Path) -> None:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.ticker import FuncFormatter

    fig, axes = plt.subplots(1, 2, figsize=(13.2, 5.2))
    ax = axes[0]
    labels = ["jsonpipe", "logscan"]
    x = list(range(len(labels)))
    n = len(IMPLS)
    w = 0.22
    for i, impl in enumerate(IMPLS):
        ys = [doc["batch"][name][impl]["median_s"] * 1000 for name in labels]
        yerr = [doc["batch"][name][impl]["mad_s"] * 1000 for name in labels]
        offs = [xi + (i - (n - 1) / 2) * w for xi in x]
        ax.bar(
            offs,
            ys,
            w,
            yerr=yerr,
            capsize=3,
            label=IMPL_LABEL[impl],
            color=COLORS[impl],
            error_kw={"elinewidth": 1},
        )
    ax.set_xticks(x, labels)
    ax.set_ylabel("median wall-clock (ms)  —  lower is better")
    ax.set_title("Batch: parse / scan / aggregate")
    ax.legend(frameon=False, fontsize=8)
    ax.grid(axis="y", linestyle=":", alpha=0.5)

    ax = axes[1]
    for impl in IMPLS:
        rows = doc["http"][impl]["concurrency"]
        xs = [r["concurrency"] for r in rows]
        ys = [r["rps"] for r in rows]
        lo = [r["rps"] - r["rps_min"] for r in rows]
        hi = [r["rps_max"] - r["rps"] for r in rows]
        ax.errorbar(
            xs,
            ys,
            yerr=[lo, hi],
            marker="o",
            label=IMPL_LABEL[impl],
            color=COLORS[impl],
            capsize=3,
        )
    ax.set_xlabel("loadgen concurrency")
    ax.set_ylabel("successful req/s  —  higher is better")
    ax.set_title("HTTP JSON API (HTTP/1.1 keep-alive)")
    ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _p: f"{v:,.0f}"))
    ax.set_xticks([1, 4, 8, 16])
    ax.legend(frameon=False, fontsize=8)
    ax.grid(True, linestyle=":", alpha=0.5)

    pin = doc.get("server_cpus") or "unpinned"
    fig.suptitle(
        "Issue #4 independent rerun — Gojo baseline vs PR #5 vs Rust\n"
        f"same fixtures (seed {doc['fixtures']['seed']}), same workload sources, "
        f"servers {pin} / loadgen {doc.get('client_cpus') or 'unpinned'}",
        fontsize=11,
    )
    fig.tight_layout()
    fig.savefig(path, dpi=150)
    plt.close(fig)


def write_csv(doc: dict, path: Path) -> None:
    rows = [
        "kind,impl,name,concurrency,trial,median_s,mad_s,rps,p50_ms,p95_ms,p99_ms,ok,err,"
        "rss_bytes,rss_hwm_bytes,server_cpu_cores,loadgen_cpu_cores,host_idle_frac"
    ]
    for name in ("jsonpipe", "logscan"):
        for impl in IMPLS:
            b = doc["batch"][name][impl]
            rows.append(
                f"batch,{impl},{name},,,{b['median_s']:.6f},{b['mad_s']:.6f},,,,,,,"
                f"{int(b['rss_median_bytes'])},,,,"
            )
    for impl in IMPLS:
        for r in doc["http"][impl]["raw"]:
            rows.append(
                f"http,{impl},httpserver,{r['concurrency']},{r['trial']},,,{r['rps']:.3f},"
                f"{r['p50_ms']:.4f},{r['p95_ms']:.4f},{r['p99_ms']:.4f},{r['ok']},{r['err']},"
                f"{r['server_rss_bytes']},{r['server_rss_hwm_bytes']},"
                f"{r['server_cpu_cores']:.3f},{r['loadgen_cpu_cores']:.3f},{r['host_idle_frac']:.3f}"
            )
    if doc.get("http_unpinned"):
        for impl in IMPLS:
            for r in doc["http_unpinned"].get(impl, {}).get("raw", []):
                rows.append(
                    f"http_unpinned,{impl},httpserver,{r['concurrency']},{r['trial']},,,"
                    f"{r['rps']:.3f},{r['p50_ms']:.4f},{r['p95_ms']:.4f},{r['p99_ms']:.4f},"
                    f"{r['ok']},{r['err']},{r['server_rss_bytes']},{r['server_rss_hwm_bytes']},"
                    f"{r['server_cpu_cores']:.3f},{r['loadgen_cpu_cores']:.3f},{r['host_idle_frac']:.3f}"
                )
    path.write_text("\n".join(rows) + "\n")


def write_report(doc: dict, path: Path) -> None:
    env = doc["env"]
    fx = doc["fixtures"]
    pin = (
        f"servers `taskset -c {doc['server_cpus']}` (Gojo `GOJO_MAXPROCS={cpu_count(doc['server_cpus'])}`), "
        f"loadgen `taskset -c {doc['client_cpus']}`"
        if doc.get("server_cpus")
        else "unpinned"
    )
    lines: list[str] = []
    a = lines.append
    a("# Issue #4 independent rerun: Gojo baseline vs PR #5 vs Rust")
    a("")
    a("Fresh measurements on this VM. Not a restatement of PR #5's tables. Absolute times from earlier sessions or other machines are not compared here.")
    a("")
    a("## What was compared")
    a("")
    a("| Role | Pin | Notes |")
    a("| --- | --- | --- |")
    a(f"| Gojo **baseline** | `{BASE_SHA}` | `gojo --version` `{env['gojo_base_version']}`; compiler SHA-256 `{env['gojo_base_sha256'][:16]}…` |")
    a(f"| Gojo **PR #5 HEAD** | `{OPT_SHA}` (`perf/realworld-issue-4`) | `gojo --version` `{env['gojo_opt_version']}`; compiler SHA-256 `{env['gojo_opt_sha256'][:16]}…` |")
    a(f"| Rust **control** | `{env['rustc']}` | default `cargo build --release` (`opt-level=3`, no extra LTO, no `target-cpu=native`) |")
    a("")
    a(f"Workload sources are a single tree (`jsonpipe` / `logscan` / `httpserver` / `loadgen` as of `{WORKLOAD_SHA}`). Only the Gojo **compiler/runtime** differs between the two Gojo columns. Overflow and bounds checks stay on (`gojo build --release` → `cc -O2`).")
    a("")
    a("## Environment")
    a("")
    a(f"- Host: {env['cpu']}, {env['nproc']} CPUs, affinity {env['affinity']}, {env['mem_kb'] / 1024 / 1024:.1f} GiB RAM, KVM")
    a(f"- OS: `{env['uname']}`")
    a(f"- C compiler: `{env['cc']}`")
    a(f"- Cargo: `{env['cargo']}`")
    a(f"- Hyperfine: `{env['hyperfine']}`")
    a(f"- Pinning: {pin}")
    a(f"- Fixture seed `{fx['seed']}`: {fx['events']:,} NDJSON events ({fx['bytes']['events'] / 1e6:.1f} MB), {fx['logs']:,} log lines ({fx['bytes']['logs'] / 1e6:.1f} MB), {fx['products']:,} products ({fx['bytes']['products'] / 1e6:.2f} MB)")
    a(f"- Batch: hyperfine warmup {doc['hyperfine_warmup']}, measured runs {doc['hyperfine_runs']}")
    a(f"- HTTP: {doc['http_trials']} interleaved windows × {doc['http_seconds']:g} s measure + {doc['http_warmup']:g} s warmup; mix 70% `GET /products/:id`, 20% `POST /query`, 8% `GET /stats`, 2% `GET /health`; HTTP/1.1 keep-alive; concurrencies {', '.join(str(c) for c in doc['concurrency'])}")
    a(f"- Commands: `{doc['env'].get('argv_text', '')}`")
    a("")
    a("This protocol **pins** the servers to CPUs 0–2 and the load generator to CPU 3. The first recording of these workloads on this agent (branch `cursor/rust-gojo-realworld-bench-435b`) was a shorter unpinned 5 s single window. Those absolute req/s are not compared here; a small unpinned sanity block is included only to show that pinning changes the bottleneck.")
    a("")
    a("## Correctness")
    a("")
    a("Stdout of jsonpipe and logscan is hashed **before** timing. All three binaries must match. HTTP routes (health, stats, product by id, 404, POST `/query` counts) are checked against the fixture for every server.")
    a("")
    jp = doc["correctness"]["jsonpipe"]
    ls = doc["correctness"]["logscan"]
    a(f"- jsonpipe SHA-256: `{jp['sha256']}` — base/opt/rust match: **{'yes' if jp['match'] else 'NO'}**")
    a(f"- logscan SHA-256: `{ls['sha256']}` — base/opt/rust match: **{'yes' if ls['match'] else 'NO'}**")
    a(f"- jsonpipe value: `{json.dumps(jp['stdout'], separators=(',', ':'))}`")
    a(f"- logscan value: `{json.dumps(ls['stdout'], separators=(',', ':'))}`")
    a(f"- HTTP route checks: **{'yes' if doc['correctness']['http'] else 'NO'}**")
    a("")
    a("## Batch jobs (median wall-clock ± MAD)")
    a("")
    a("Lower is better. Times include process start and page-cache I/O after warmup.")
    a("")
    a("| Workload | Gojo baseline | Gojo PR #5 | Rust | base / Rust | opt / Rust | opt / base |")
    a("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for name in ("jsonpipe", "logscan"):
        b = doc["batch"][name]
        a(
            f"| {name} | {fmt_ms(b['gojo_base']['median_s'])} ± {fmt_ms(b['gojo_base']['mad_s'])} | "
            f"{fmt_ms(b['gojo_opt']['median_s'])} ± {fmt_ms(b['gojo_opt']['mad_s'])} | "
            f"{fmt_ms(b['rust']['median_s'])} ± {fmt_ms(b['rust']['mad_s'])} | "
            f"{ratio(b['gojo_base']['median_s'], b['rust']['median_s'])} | "
            f"{ratio(b['gojo_opt']['median_s'], b['rust']['median_s'])} | "
            f"{ratio(b['gojo_opt']['median_s'], b['gojo_base']['median_s'])} |"
        )
    a("")
    a("Throughput (input bytes / median seconds) and hyperfine peak RSS (treat as approximate; short processes sit near a few-MiB floor):")
    a("")
    a("| Workload | impl | MB/s | RSS median | min–max wall |")
    a("| --- | --- | ---: | ---: | ---: |")
    for name, nbytes in (("jsonpipe", fx["bytes"]["events"]), ("logscan", fx["bytes"]["logs"])):
        for impl in IMPLS:
            b = doc["batch"][name][impl]
            a(
                f"| {name} | {IMPL_SHORT[impl]} | {nbytes / b['median_s'] / 1e6:.1f} | "
                f"{fmt_rss(b['rss_median_bytes'])} | {fmt_ms(b['min_s'])}–{fmt_ms(b['max_s'])} |"
            )
    a("")
    a("Raw sample lists are in `results.json` under `batch.<job>.<impl>.times_s` and in `hyperfine/`." )
    a("")
    a("## HTTP JSON API (pinned)")
    a("")
    a("Higher req/s is better. req/s is the **median of the five windows**, with min–max. Latencies are medians of per-window percentiles (client-side). `ok`/`err` are summed over windows. CPU is cores busy (CPU seconds / wall seconds) over the loadgen process including warmup.")
    a("")
    a("| impl | conc. | req/s median | min–max | MAD | p50 | p95 | p99 | p99 range | server CPU | loadgen CPU | host idle | ok | err | RSS (VmRSS) | VmHWM |")
    a("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for impl in IMPLS:
        for row in doc["http"][impl]["concurrency"]:
            a(
                f"| {IMPL_SHORT[impl]} | {row['concurrency']} | {fmt_rps(row['rps'])} | "
                f"{fmt_rps(row['rps_min'])}–{fmt_rps(row['rps_max'])} | {row['rps_mad']:.0f} | "
                f"{row['p50_ms']:.3f} ms | {row['p95_ms']:.3f} ms | {row['p99_ms']:.3f} ms | "
                f"{row['p99_ms_min']:.3f}–{row['p99_ms_max']:.3f} | "
                f"{row['server_cpu_cores']:.2f} | {row['loadgen_cpu_cores']:.2f} | {row['host_idle_frac']:.0%} | "
                f"{row['ok']:,} | {row['err']:,} | {fmt_rss(row['server_rss_bytes'])} | {fmt_rss(row['server_rss_hwm_bytes'])} |"
            )
    a("")
    a("| conc. | opt / base req/s | base / Rust | opt / Rust |")
    a("| ---: | ---: | ---: | ---: |")
    for i, row in enumerate(doc["http"]["gojo_base"]["concurrency"]):
        c = row["concurrency"]
        b = row["rps"]
        o = doc["http"]["gojo_opt"]["concurrency"][i]["rps"]
        r = doc["http"]["rust"]["concurrency"][i]["rps"]
        a(f"| {c} | {o / b:.2f}× | {b / r:.2f}× | {o / r:.2f}× |")
    a("")
    a(
        "Server peak RSS (VmHWM of the long-lived process): "
        + ", ".join(
            f"{IMPL_SHORT[impl]} {fmt_rss(doc['http'][impl]['server_rss_peak_bytes'])}" for impl in IMPLS
        )
        + "."
    )
    a("")
    a("### Load-generator bottleneck")
    a("")
    a("Servers are limited to 3 CPUs; the client to 1. If `loadgen CPU` approaches 1.0 while `host idle` is near 0 and `server CPU` is well below 3, the client is saturating its core and the req/s figure is not a pure server capacity. If `server CPU` is high and the loadgen is not, the server is the bound.")
    a("")
    if doc.get("http_unpinned"):
        a("## HTTP unpinned sanity (not the primary result)")
        a("")
        a("Same mix, one window per impl and concurrency, no `taskset`. Shown only to record that pinning changes sharing of the 4 CPUs. Do not mix these req/s with the pinned table.")
        a("")
        a("| impl | conc. | req/s | p50 / p99 | server CPU | loadgen CPU | host idle | err |")
        a("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
        for impl in IMPLS:
            for row in doc["http_unpinned"][impl]["concurrency"]:
                a(
                    f"| {IMPL_SHORT[impl]} | {row['concurrency']} | {fmt_rps(row['rps'])} | "
                    f"{row['p50_ms']:.3f} / {row['p99_ms']:.3f} ms | {row['server_cpu_cores']:.2f} | "
                    f"{row['loadgen_cpu_cores']:.2f} | {row['host_idle_frac']:.0%} | {row['err']} |"
                )
        a("")
    a("## Caveats")
    a("")
    a("- **Not a language ranking.** jsonpipe is compiler-generated JSON vs serde_json. HTTP is `std/http` vs axum+tokio+hyper (ecosystem). logscan is closer to a runtime + std comparison.")
    a("- **No causal claims** beyond “this compiler/runtime at this commit, these flags, this machine, these programs.” Profiles that motivated PR #5 are in that PR; this rerun does not re-profile.")
    a("- **Checks stay on.** Gojo overflow and bounds checks were not disabled. Rust was not given LTO or `target-cpu=native`.")
    a("- **Hyperfine RSS** of short batch jobs can sit on a measurement floor of a few MiB. HTTP RSS is `/proc/<pid>/status` of the server.")
    a("- **4-CPU KVM guest.** Absolute req/s will not match a metal box.")
    a("- **Idle sibling servers.** All three HTTP servers listen at once (same as PR #5 keeping both SUTs up) and share the server CPU set while idle.")
    a("- **PR #2** (MoonBit / Go / Gojo microbenchmarks) is not part of this run.")
    a("")
    a("## Artifacts")
    a("")
    for p in doc.get("artifact_paths", []):
        a(f"- `{p}`")
    a("")
    path.write_text("\n".join(lines) + "\n")


def copy_artifacts(src: Path, dest: Path) -> list[str]:
    dest.mkdir(parents=True, exist_ok=True)
    copied = []
    for name in ("REPORT.md", "results.json", "results.csv", "chart.png"):
        p = src / name
        if p.exists():
            shutil.copy2(p, dest / name)
            copied.append(str(dest / name))
    hf_src = src / "hyperfine"
    if hf_src.exists():
        hf_dest = dest / "hyperfine"
        hf_dest.mkdir(parents=True, exist_ok=True)
        for p in sorted(hf_src.glob("*.json")):
            shutil.copy2(p, hf_dest / p.name)
            copied.append(str(hf_dest / p.name))
    return copied


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--gojo-base", default="/tmp/build-gojo-base/release/gojo")
    p.add_argument("--gojo-opt", default="/tmp/build-gojo-opt/release/gojo")
    p.add_argument("--events", type=int, default=300_000)
    p.add_argument("--logs", type=int, default=600_000)
    p.add_argument("--products", type=int, default=10_000)
    p.add_argument("--seed", type=int, default=20261109)
    p.add_argument("--runs", type=int, default=11)
    p.add_argument("--warmup", type=int, default=3)
    p.add_argument("--http-seconds", type=float, default=10.0)
    p.add_argument("--http-warmup", type=float, default=1.0)
    p.add_argument("--http-trials", type=int, default=5)
    p.add_argument("--concurrency", default="1,4,8,16")
    p.add_argument("--server-cpus", default="0-2")
    p.add_argument("--client-cpus", default="3")
    p.add_argument("--results", type=Path, default=ROOT / "results" / "issue-4-rerun")
    p.add_argument("--bin", type=Path, default=ROOT / "bin" / "issue-4-rerun")
    p.add_argument("--skip-build", action="store_true")
    p.add_argument("--skip-unpinned-sanity", action="store_true")
    args = p.parse_args()
    concs = [int(x) for x in args.concurrency.split(",") if x.strip()]
    results = args.results.resolve()
    bindir = args.bin.resolve()
    results.mkdir(parents=True, exist_ok=True)
    bindir.mkdir(parents=True, exist_ok=True)

    gojo_base = str(Path(args.gojo_base).resolve())
    gojo_opt = str(Path(args.gojo_opt).resolve())
    env = host_info()
    env["gojo_base"] = gojo_base
    env["gojo_opt"] = gojo_opt
    env["gojo_base_version"] = capture([gojo_base, "--version"])
    env["gojo_opt_version"] = capture([gojo_opt, "--version"])
    env["gojo_base_sha256"] = sha256_file(Path(gojo_base))
    env["gojo_opt_sha256"] = sha256_file(Path(gojo_opt))
    env["pinned_commits"] = {"gojo_base": BASE_SHA, "gojo_opt": OPT_SHA, "workloads": WORKLOAD_SHA}
    env["argv"] = sys.argv[1:]
    env["argv_text"] = "python3 bench/realworld/rerun_issue4.py " + shlex.join(sys.argv[1:])
    print(json.dumps({k: env[k] for k in ("gojo_base_version", "gojo_opt_version", "rustc", "cpu", "nproc")}, indent=2), flush=True)
    if BASE_SHA[:10] not in env["gojo_base_version"]:
        raise SystemExit(f"baseline compiler version {env['gojo_base_version']!r} does not contain {BASE_SHA[:10]}")
    if OPT_SHA[:10] not in env["gojo_opt_version"]:
        raise SystemExit(f"optimized compiler version {env['gojo_opt_version']!r} does not contain {OPT_SHA[:10]}")

    fx = generate_fixtures(args.events, args.logs, args.products, args.seed)
    if args.skip_build and (bindir / "base-jsonpipe").exists() and (bindir / "rust-jsonpipe").exists():
        bins = {
            "gojo_base": {n: str(bindir / f"base-{n}") for n in ("jsonpipe", "logscan", "httpserver")},
            "gojo_opt": {n: str(bindir / f"opt-{n}") for n in ("jsonpipe", "logscan", "httpserver")},
            "rust": {n: str(bindir / f"rust-{n}") for n in ("jsonpipe", "logscan", "httpserver", "loadgen")},
        }
    else:
        bins = {
            "gojo_base": build_gojo_progs(gojo_base, bindir, "base"),
            "gojo_opt": build_gojo_progs(gojo_opt, bindir, "opt"),
            "rust": build_rust(bindir),
        }

    events = FIXTURES / "events.ndjson"
    logs = FIXTURES / "app.log"
    products_path = FIXTURES / "products.json"
    products = json.loads(products_path.read_text())

    jp_out = {impl: run_stdout([bins[impl]["jsonpipe"], str(events)]) for impl in IMPLS}
    ls_out = {impl: run_stdout([bins[impl]["logscan"], str(logs)]) for impl in IMPLS}
    jp_match = jp_out["gojo_base"] == jp_out["gojo_opt"] == jp_out["rust"]
    ls_match = ls_out["gojo_base"] == ls_out["gojo_opt"] == ls_out["rust"]
    if json.loads(jp_out["gojo_base"]) != json.loads(jp_out["rust"]) or json.loads(jp_out["gojo_opt"]) != json.loads(jp_out["rust"]):
        raise SystemExit("jsonpipe JSON values differ")
    if json.loads(ls_out["gojo_base"]) != json.loads(ls_out["rust"]) or json.loads(ls_out["gojo_opt"]) != json.loads(ls_out["rust"]):
        raise SystemExit("logscan JSON values differ")
    if not jp_match:
        print("jsonpipe: parsed JSON matches, bytes differ", file=sys.stderr)
    if not ls_match:
        print("logscan: parsed JSON matches, bytes differ", file=sys.stderr)

    correctness = {
        "jsonpipe": {
            "sha256": sha256_bytes(jp_out["gojo_base"]),
            "sha256s": {impl: sha256_bytes(jp_out[impl]) for impl in IMPLS},
            "match": jp_match or json.loads(jp_out["gojo_base"]) == json.loads(jp_out["rust"]),
            "stdout": json.loads(jp_out["gojo_base"]),
        },
        "logscan": {
            "sha256": sha256_bytes(ls_out["gojo_base"]),
            "sha256s": {impl: sha256_bytes(ls_out[impl]) for impl in IMPLS},
            "match": ls_match or json.loads(ls_out["gojo_base"]) == json.loads(ls_out["rust"]),
            "stdout": json.loads(ls_out["gojo_base"]),
        },
        "http": True,
    }
    print("jsonpipe", correctness["jsonpipe"]["stdout"], "match", correctness["jsonpipe"]["match"], flush=True)
    print("logscan", correctness["logscan"]["stdout"], "match", correctness["logscan"]["match"], flush=True)

    hf_dir = results / "hyperfine"
    hf_dir.mkdir(exist_ok=True)
    batch = {"jsonpipe": {}, "logscan": {}}
    # Interleave implementations per job so host noise is shared.
    for name, path in (("jsonpipe", events), ("logscan", logs)):
        for impl in IMPLS:
            print(f"--- batch {impl} {name} ---", flush=True)
            batch[name][impl] = hyperfine_cmd(
                bins[impl][name],
                [str(path)],
                args.runs,
                args.warmup,
                hf_dir / f"{impl}-{name}.json",
            )

    servers = {
        "gojo_base": bins["gojo_base"]["httpserver"],
        "gojo_opt": bins["gojo_opt"]["httpserver"],
        "rust": bins["rust"]["httpserver"],
    }
    print("--- HTTP pinned, 3-way interleaved ---", flush=True)
    http = measure_http(
        servers,
        bins["rust"]["loadgen"],
        products_path,
        products,
        concs,
        args.http_seconds,
        args.http_warmup,
        args.seed,
        args.http_trials,
        args.server_cpus,
        args.client_cpus,
    )

    http_unpinned = None
    if not args.skip_unpinned_sanity:
        print("--- HTTP unpinned sanity (c=1 and c=16, 1 window) ---", flush=True)
        http_unpinned = measure_http(
            servers,
            bins["rust"]["loadgen"],
            products_path,
            products,
            [1, 16],
            args.http_seconds,
            args.http_warmup,
            args.seed,
            1,
            None,
            None,
        )

    doc = {
        "env": env,
        "fixtures": fx,
        "bins": bins,
        "hyperfine_runs": args.runs,
        "hyperfine_warmup": args.warmup,
        "correctness": correctness,
        "batch": batch,
        "http": http,
        "http_unpinned": http_unpinned,
        "http_seconds": args.http_seconds,
        "http_warmup": args.http_warmup,
        "http_trials": args.http_trials,
        "server_cpus": args.server_cpus,
        "client_cpus": args.client_cpus,
        "concurrency": concs,
        "notes": {
            "gojo_release": "gojo build --release → cc -O2; overflow and bounds checks on",
            "rust_release": "cargo build --release; default profile, no extra LTO, no target-cpu=native",
            "same_sources": "all Gojo programs compiled from bench/realworld/gojo; Rust from bench/realworld/rust",
            "independent": "fresh run; not copied from PR #5 result tables",
        },
    }
    (results / "results.json").write_text(json.dumps(doc, indent=2) + "\n")
    write_csv(doc, results / "results.csv")
    write_chart(doc, results / "chart.png")
    copied = copy_artifacts(results, ARTIFACT_DIR)
    # REPORT after copy list is known — write, then copy again
    doc["artifact_paths"] = [
        str(results / "REPORT.md"),
        str(results / "results.json"),
        str(results / "results.csv"),
        str(results / "chart.png"),
        str(ARTIFACT_DIR / "REPORT.md"),
        str(ARTIFACT_DIR / "results.json"),
        str(ARTIFACT_DIR / "results.csv"),
        str(ARTIFACT_DIR / "chart.png"),
    ] + copied
    # unique preserve order
    seen = set()
    paths = []
    for x in doc["artifact_paths"]:
        if x not in seen:
            seen.add(x)
            paths.append(x)
    doc["artifact_paths"] = paths
    write_report(doc, results / "REPORT.md")
    (results / "results.json").write_text(json.dumps(doc, indent=2) + "\n")
    copied = copy_artifacts(results, ARTIFACT_DIR)
    print("wrote", results / "REPORT.md", results / "chart.png", flush=True)
    print("copied", copied, flush=True)


if __name__ == "__main__":
    main()
