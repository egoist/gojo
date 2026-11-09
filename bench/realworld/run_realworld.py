#!/usr/bin/env python3
"""Build and run the Gojo vs idiomatic-Rust real-application suite.

Batch jobs (jsonpipe, logscan) use hyperfine wall-clock + peak RSS.
HTTP uses a mixed-workload HTTP/1.1 loadgen against each server: both servers
run at once, and the trials alternate between them (Gojo, Rust, then Rust,
Gojo, ...) so host noise falls on both. Each trial records the CPU time the
server and the load generator used, and the host's idle share, to show which
side was saturated. `--server-cpus` / `--client-cpus` pin them with taskset.

Does not modify the Gojo compiler. Does not enable extra Rust LTO / native CPU
flags. Fixtures are generated, not committed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import re
import resource
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
RESULTS = ROOT / "results"
BIN = ROOT / "bin"


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
    r = subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, **kw)
    return r.stdout.strip()


def env_info(gojo: str) -> dict:
    uname = capture(["uname", "-a"])
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
    nproc = os.cpu_count()
    rustc = which_or("rustc")
    cargo = which_or("cargo")
    cc = which_or("cc")
    return {
        "uname": uname,
        "cpu": cpu,
        "nproc": nproc,
        "mem_kb": mem_kb,
        "gojo_bin": gojo,
        "gojo_version": capture([gojo, "--version"]),
        "rustc": capture([rustc, "--version"]),
        "cargo": capture([cargo, "--version"]),
        "cc": capture([cc, "--version"]).splitlines()[0],
        "hyperfine": capture(["hyperfine", "--version"]),
        "python": sys.version.split()[0],
        "cwd": str(ROOT),
        "git_head": capture(["git", "rev-parse", "HEAD"], cwd=str(ROOT.parents[1])),
        "git_dirty": capture(["git", "status", "--porcelain", "--untracked-files=no"], cwd=str(ROOT.parents[1])) != "",
        "notes": {
            "gojo_release": "gojo build --release → cc -O2; overflow and bounds checks on (compiler default)",
            "rust_release": "cargo build --release; profile defaults (opt-level=3, no extra LTO, no target-cpu=native)",
            "json_gojo": "compiler-generated json.decode[T] / json.encode[T]",
            "json_rust": "serde_json 1.x per NDJSON line / serde_json of the catalog",
            "http_gojo": "std/http HTTP/1.1, keep-alive, Handler.serve; catalog borrowed read-only (no Mutex)",
            "http_rust": "axum 0.8 + tokio + hyper HTTP/1.1; catalog in Arc<Vec<Item>> (no mutex)",
            "http_client": "custom mixed-workload hyper HTTP/1.1 client (not oha; oha is single-URL)",
            "log_semantics": "UTF-8 substring (Gojo str.contains / Rust str::contains); valid UTF-8 only",
            "money": "integer cents; no floating-point JSON amounts",
            "dce": "stdout is hashed; results are serialized so the work cannot be deleted",
        },
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


def build(gojo: str, cargo: str) -> dict:
    BIN.mkdir(parents=True, exist_ok=True)
    bins = {}
    t0 = time.perf_counter()
    for name in ("jsonpipe", "logscan", "httpserver"):
        out = BIN / f"gojo-{name}"
        run([gojo, "build", "--release", str(GOJO_SRC / f"{name}.gojo"), "-o", str(out)])
        bins[f"gojo-{name}"] = str(out)
    gojo_build_s = time.perf_counter() - t0
    t0 = time.perf_counter()
    run(
        [cargo, "build", "--release", "--bins"],
        cwd=str(RUST_DIR),
        env={**os.environ, "CARGO_TERM_COLOR": "always"},
    )
    rust_build_s = time.perf_counter() - t0
    rust_target = RUST_DIR / "target" / "release"
    for name in ("jsonpipe", "logscan", "httpserver", "loadgen"):
        src = rust_target / name
        dst = BIN / f"rust-{name}"
        shutil.copy2(src, dst)
        bins[f"rust-{name}"] = str(dst)
    bins["gojo_build_s"] = gojo_build_s
    bins["rust_build_s"] = rust_build_s
    return bins


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def run_stdout(cmd) -> bytes:
    r = subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if r.stderr:
        sys.stderr.buffer.write(r.stderr)
    return r.stdout


def median(xs: list[float]) -> float:
    if not xs:
        return float("nan")
    return statistics.median(xs)


def mad(xs: list[float], med: float) -> float:
    if not xs:
        return float("nan")
    return statistics.median([abs(x - med) for x in xs])


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
        "raw": data,
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
    return parse_hyperfine(json_out)


def read_vmhwm(pid: int) -> int | None:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) * 1024
    except OSError:
        return None
    return None


def pinned(cmd: list[str], cpus: str | None) -> list[str]:
    return ["taskset", "-c", cpus, *cmd] if cpus else cmd


def cpu_count(cpus: str) -> int:
    n = 0
    for part in cpus.split(","):
        lo, _, hi = part.partition("-")
        n += int(hi or lo) - int(lo) + 1
    return n


def start_server(bin_path: str, products: Path, cpus: str | None = None) -> tuple[subprocess.Popen, str, int]:
    env = dict(os.environ)
    if cpus and Path(bin_path).name.startswith("gojo-"):
        # the Gojo scheduler sizes itself from the online CPUs, tokio from the
        # affinity mask: give both the same number of workers
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


def json_loads(b: bytes):
    return json.loads(b.decode())


def check_http(url: str, products: list[dict]) -> dict:
    st, body = http_get(url + "/health")
    health = json_loads(body)
    assert st == 200 and health == {"ok": True}, (st, body)

    st, body = http_get(url + "/stats")
    stats = json_loads(body)
    assert st == 200
    assert stats["n"] == len(products)
    assert stats["cents"] == sum(p["cents"] for p in products)

    first = products[0]
    st, body = http_get(url + f"/products/{first['id']}")
    got = json_loads(body)
    assert st == 200 and got == first, (st, got, first)

    st, body = http_get(url + "/products/0")
    assert st == 404, (st, body)

    q = {"region": first["region"], "min_cents": first["cents"], "tag": first["tag"]}
    st, body = http_post_json(url + "/query", q)
    out = json_loads(body)
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
    return {"health": health, "stats": stats, "product": got, "query": out}


def proc_cpu_s(pid: int) -> float:
    """utime + stime of a live process, in seconds."""
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def host_cpu() -> tuple[int, int]:
    """(idle, total) jiffies of all CPUs."""
    v = [int(x) for x in Path("/proc/stat").read_text().splitlines()[0].split()[1:]]
    return v[3] + v[4], sum(v[:8])


def loadgen_run(loadgen: str, url: str, concurrency: int, seconds: float, warmup: float, products: int, seed: int, server_pid: int, cpus: str | None) -> dict:
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
    # over the whole loadgen run (warmup included); cores = CPU seconds / wall seconds
    row["server_cpu_cores"] = (s1 - s0) / wall
    row["loadgen_cpu_cores"] = (u1.ru_utime + u1.ru_stime - u0.ru_utime - u0.ru_stime) / wall
    row["host_idle_frac"] = (i1 - i0) / max(1, t1 - t0)
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
        "ok": sum(t["ok"] for t in trials),
        "err": sum(t["err"] for t in trials),
    }
    for k in ("p50_ms", "p95_ms", "p99_ms", "server_cpu_cores", "loadgen_cpu_cores", "host_idle_frac", "server_rss_bytes"):
        out[k] = median([t[k] for t in trials])
    out["p99_ms_max"] = max(t["p99_ms"] for t in trials)
    return out


def measure_http(servers: dict[str, str], loadgen: str, products_path: Path, products: list[dict], concs: list[int], seconds: float, warmup: float, seed: int, trials: int, server_cpus: str | None, client_cpus: str | None) -> dict:
    started = {}
    try:
        for lang, bin_path in servers.items():
            proc, url, port = start_server(bin_path, products_path, server_cpus)
            started[lang] = (proc, url, port)
        time.sleep(0.2)
        res = {lang: {"url": url, "port": port, "correctness": check_http(url, products), "raw": []} for lang, (proc, url, port) in started.items()}
        langs = list(servers)
        for t in range(trials):
            for c in concs:
                # alternate which server goes first
                for lang in langs if t % 2 == 0 else langs[::-1]:
                    proc, url, _ = started[lang]
                    row = loadgen_run(loadgen, url, c, seconds, warmup, len(products), seed, proc.pid, client_cpus)
                    row["trial"] = t
                    row["server_rss_bytes"] = read_vmhwm(proc.pid) or 0
                    res[lang]["raw"].append(row)
                    print(
                        f"  trial {t + 1}/{trials} {lang} c={c}: rps={row['rps']:.1f} p50={row['p50_ms']:.3f}ms "
                        f"p99={row['p99_ms']:.3f}ms err={row['err']} server={row['server_cpu_cores']:.2f} "
                        f"loadgen={row['loadgen_cpu_cores']:.2f} idle={row['host_idle_frac']:.0%}",
                        flush=True,
                    )
        for lang, (proc, _, _) in started.items():
            r = res[lang]
            r["concurrency"] = [summarize_trials([x for x in r["raw"] if x["concurrency"] == c]) for c in concs]
            r["server_rss_peak_bytes"] = read_vmhwm(proc.pid) or 0
        return res
    finally:
        for proc, _, _ in started.values():
            stop_server(proc)


def write_chart(doc: dict, path: Path) -> None:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(11.2, 4.4))
    ax = axes[0]
    labels = ["jsonpipe", "logscan"]
    gojo = [
        doc["batch"]["jsonpipe"]["gojo"]["median_s"] * 1000,
        doc["batch"]["logscan"]["gojo"]["median_s"] * 1000,
    ]
    rust = [
        doc["batch"]["jsonpipe"]["rust"]["median_s"] * 1000,
        doc["batch"]["logscan"]["rust"]["median_s"] * 1000,
    ]
    x = range(len(labels))
    w = 0.36
    ax.bar([i - w / 2 for i in x], gojo, w, label="Gojo --release", color="#3b6d9a")
    ax.bar([i + w / 2 for i in x], rust, w, label="Rust cargo --release", color="#c45c26")
    ax.set_xticks(list(x), labels)
    ax.set_ylabel("median wall-clock (ms)")
    ax.set_title("Parse / scan / aggregate")
    ax.legend(frameon=False)
    ax.grid(axis="y", linestyle=":", alpha=0.5)

    ax = axes[1]
    for lang, color, key in (
        ("Gojo std/http", "#3b6d9a", "gojo"),
        ("Rust axum/tokio", "#c45c26", "rust"),
    ):
        rows = doc["http"][key]["concurrency"]
        xs = [r["concurrency"] for r in rows]
        ys = [r["rps"] for r in rows]
        ax.plot(xs, ys, marker="o", label=lang, color=color)
    ax.set_xlabel("loadgen concurrency")
    ax.set_ylabel("successful req/s")
    ax.set_title("HTTP JSON API (HTTP/1.1 keep-alive)")
    ax.legend(frameon=False)
    ax.grid(True, linestyle=":", alpha=0.5)

    fig.suptitle("Gojo vs idiomatic Rust — real-application workloads", fontsize=11)
    fig.tight_layout()
    fig.savefig(path, dpi=140)
    plt.close(fig)


def fmt_ms(s: float) -> str:
    return f"{s * 1000:.1f} ms"


def fmt_rps(x: float) -> str:
    return f"{x:,.0f}"


def fmt_rss(b: float) -> str:
    return f"{b / (1024 * 1024):.1f} MiB"


def ratio(a: float, b: float) -> str:
    if b == 0:
        return "n/a"
    return f"{a / b:.2f}×"


def write_markdown(doc: dict, path: Path) -> None:
    jp_g = doc["batch"]["jsonpipe"]["gojo"]
    jp_r = doc["batch"]["jsonpipe"]["rust"]
    ls_g = doc["batch"]["logscan"]["gojo"]
    ls_r = doc["batch"]["logscan"]["rust"]
    env = doc["env"]
    fx = doc["fixtures"]
    lines = []
    a = lines.append
    a("# Gojo vs Rust: real-application workloads")
    a("")
    a("Same algorithms, same generated fixtures, same machine. Gojo `--release` (`cc -O2`, overflow and bounds checks on). Rust `cargo build --release` (default profile: `opt-level=3`, no extra LTO, no `-C target-cpu=native`).")
    a("")
    a("This is **not** a compiler-only comparison on the HTTP side: Gojo uses `std/http` + compiler-generated `json`; Rust uses **axum 0.8 + tokio + serde_json**. The NDJSON and log jobs compare generated JSON/line I/O vs serde_json/`BufRead`.")
    a("")
    a("## Environment")
    a("")
    a(f"- Host: {env['cpu']}, {env['nproc']} CPUs, {env['mem_kb'] / 1024 / 1024:.1f} GiB RAM, KVM")
    a(f"- OS: `{env['uname']}`")
    a(f"- Gojo: `{env['gojo_version']}` (git `{env['git_head'][:12]}`)")
    a(f"- Rustc: `{env['rustc']}`")
    a(f"- Cargo: `{env['cargo']}`")
    a(f"- C compiler: `{env['cc']}`")
    a(f"- Hyperfine: `{env['hyperfine']}`")
    a(f"- Fixture seed `{fx['seed']}`: {fx['events']:,} NDJSON events ({fx['bytes']['events'] / 1e6:.1f} MB), {fx['logs']:,} log lines ({fx['bytes']['logs'] / 1e6:.1f} MB), {fx['products']:,} products ({fx['bytes']['products'] / 1e6:.2f} MB)")
    a(f"- Compile (not timed in the table): Gojo {doc['bins']['gojo_build_s']:.1f}s, Rust {doc['bins']['rust_build_s']:.1f}s")
    a("")
    a("## Correctness")
    a("")
    a("Stdout of jsonpipe and logscan is hashed before timing. HTTP routes are checked (health, stats, product by id, 404, POST /query counts) against the fixture.")
    a("")
    def yn(v) -> str:
        return "yes" if v else "no"

    a(f"- jsonpipe SHA-256: `{doc['correctness']['jsonpipe']['sha256']}` (Gojo == Rust: **{yn(doc['correctness']['jsonpipe']['match'])}**)")
    a(f"- logscan SHA-256: `{doc['correctness']['logscan']['sha256']}` (Gojo == Rust: **{yn(doc['correctness']['logscan']['match'])}**)")
    a(f"- HTTP JSON bodies compared by parsed value: **{yn(doc['correctness']['http'])}**")
    a("")
    a("## Batch jobs (median wall-clock, hyperfine)")
    a("")
    a("| Workload | What it measures | Gojo | Rust | Gojo / Rust | Gojo RSS | Rust RSS |")
    a("| --- | --- | ---: | ---: | ---: | ---: | ---: |")
    a(
        f"| jsonpipe | NDJSON decode → filter → aggregate → encode ({fx['events']:,} records) | "
        f"{fmt_ms(jp_g['median_s'])} | {fmt_ms(jp_r['median_s'])} | {ratio(jp_g['median_s'], jp_r['median_s'])} | "
        f"{fmt_rss(jp_g['rss_median_bytes'])} | {fmt_rss(jp_r['rss_median_bytes'])} |"
    )
    a(
        f"| logscan | UTF-8 line scan + field aggregate ({fx['logs']:,} lines) | "
        f"{fmt_ms(ls_g['median_s'])} | {fmt_ms(ls_r['median_s'])} | {ratio(ls_g['median_s'], ls_r['median_s'])} | "
        f"{fmt_rss(ls_g['rss_median_bytes'])} | {fmt_rss(ls_r['rss_median_bytes'])} |"
    )
    a("")
    a(f"Hyperfine: {jp_g['n']} runs, {doc['hyperfine_warmup']} warmup. Dispersion is median ± MAD: jsonpipe Gojo {fmt_ms(jp_g['median_s'])} ± {fmt_ms(jp_g['mad_s'])}, Rust {fmt_ms(jp_r['median_s'])} ± {fmt_ms(jp_r['mad_s'])}; logscan Gojo {fmt_ms(ls_g['median_s'])} ± {fmt_ms(ls_g['mad_s'])}, Rust {fmt_ms(ls_r['median_s'])} ± {fmt_ms(ls_r['mad_s'])}.")
    a("")
    a("Throughput (input bytes / median seconds):")
    a("")
    a("| Workload | Gojo | Rust |")
    a("| --- | ---: | ---: |")
    a(f"| jsonpipe | {fx['bytes']['events'] / jp_g['median_s'] / 1e6:.1f} MB/s | {fx['bytes']['events'] / jp_r['median_s'] / 1e6:.1f} MB/s |")
    a(f"| logscan | {fx['bytes']['logs'] / ls_g['median_s'] / 1e6:.1f} MB/s | {fx['bytes']['logs'] / ls_r['median_s'] / 1e6:.1f} MB/s |")
    a("")
    a("## HTTP JSON API")
    a("")
    a("Read-mostly in-memory catalog. Mix: 70% `GET /products/:id`, 20% `POST /query` (full scan, first 50 ids), 8% `GET /stats`, 2% `GET /health`. HTTP/1.1 keep-alive. Same loadgen for both servers. Warmup is discarded; the table is the measuring window only. Latency is client-side.")
    a("")
    pin = f"servers pinned to CPUs `{doc['server_cpus']}`, load generator to `{doc['client_cpus']}`" if doc.get("server_cpus") else "no CPU pinning"
    a(
        f"{doc['http_trials']} trials of {doc['http_seconds']:g} s (after {doc['http_warmup']:g} s warmup) per server and concurrency, "
        f"interleaved between the servers; {pin}. req/s is the median of the trials, with min–max; latencies are medians of the per-trial "
        "percentiles. CPU columns are cores busy (CPU seconds / wall seconds) over each loadgen run; idle is the host's idle share."
    )
    a("")
    a("| Lang | conc. | req/s (median) | min–max | p50 | p95 | p99 | server CPU | loadgen CPU | host idle | ok | err |")
    a("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for lang, key in (("Gojo", "gojo"), ("Rust", "rust")):
        for row in doc["http"][key]["concurrency"]:
            a(
                f"| {lang} | {row['concurrency']} | {fmt_rps(row['rps'])} | {fmt_rps(row['rps_min'])}–{fmt_rps(row['rps_max'])} | "
                f"{row['p50_ms']:.3f} ms | {row['p95_ms']:.3f} ms | {row['p99_ms']:.3f} ms | "
                f"{row['server_cpu_cores']:.2f} | {row['loadgen_cpu_cores']:.2f} | {row['host_idle_frac']:.0%} | "
                f"{row['ok']:,} | {row['err']:,} |"
            )
    a("")
    a(
        f"Server peak RSS while serving (VmHWM): Gojo {fmt_rss(doc['http']['gojo']['server_rss_peak_bytes'])}, "
        f"Rust {fmt_rss(doc['http']['rust']['server_rss_peak_bytes'])}."
    )
    a("")
    a("## Caveats")
    a("")
    a("- **Startup / I/O.** Batch times include process start, opening the file, and page-cache I/O after warmup. They are not “CPU minus I/O” numbers.")
    a("- **Hyperfine RSS.** Peak RSS of a short process can sit on a measurement floor of a few MiB. Treat RSS as approximate; HTTP RSS is from `/proc/<pid>/status` VmHWM of the long-lived server.")
    a("- **JSON.** Gojo’s encoder/decoder is compiler-generated for the struct. Rust uses serde_json. A faster or slower JSON library on either side would move the jsonpipe number without saying anything about the language runtime alone.")
    a("- **HTTP stack.** axum/tokio/hyper vs Gojo `std/http` is an ecosystem comparison. Both are HTTP/1.1 (no HTTP/2). The loadgen is a hyper client; it is not the system under test.")
    a("- **Sharing.** Rust holds the catalog in `Arc<Vec<Item>>` with no lock. Gojo’s `http.serve` borrows the handler for the life of the server, so the catalog is also read-only with no mutex. That is the idiomatic form on each side for a read-mostly map.")
    a("- **No strawman, no shortcuts.** Overflow/bounds checks stay on in Gojo. Rust is not a debug build, and was not given LTO/`target-cpu=native` that Gojo does not get. Integers for money; UTF-8 substring search, not Unicode case-folding.")
    a("- **4-CPU KVM guest.** Absolute req/s will not match a metal box. Ratios on this machine are the result.")
    a("- **Shared host.** Load generator and servers share the machine. When neither server CPU nor loadgen CPU is near its CPU budget and the host still has idle time, throughput is bound by round-trip latency, not by server capacity.")
    a("- **PR #2** (MoonBit / Go / Gojo microbenchmarks) is untouched.")
    a("")
    a("## How to rerun")
    a("")
    a("```sh")
    a("python3 bench/realworld/run_realworld.py")
    a("```")
    a("")
    a(f"Arguments of this run: `{' '.join(env.get('argv', [])) or '(defaults)'}`.")
    a("")
    a("Artifacts: `RESULTS.md`, `results.json` (with every HTTP trial under `http.<lang>.raw`), `results.csv`, `chart.png`, `hyperfine/`.")
    a("")
    path.write_text("\n".join(lines) + "\n")


def write_csv(doc: dict, path: Path) -> None:
    rows = ["kind,lang,name,concurrency,trial,median_s,mad_s,rps,p50_ms,p95_ms,p99_ms,ok,err,rss_bytes,server_cpu_cores,loadgen_cpu_cores,host_idle_frac"]
    for name in ("jsonpipe", "logscan"):
        for lang in ("gojo", "rust"):
            b = doc["batch"][name][lang]
            rows.append(
                f"batch,{lang},{name},,,{b['median_s']:.6f},{b['mad_s']:.6f},,,,,,,"
                f"{int(b['rss_median_bytes'])},,,"
            )
    for lang in ("gojo", "rust"):
        for r in doc["http"][lang]["raw"]:
            rows.append(
                f"http,{lang},httpserver,{r['concurrency']},{r['trial']},,,{r['rps']:.3f},{r['p50_ms']:.4f},"
                f"{r['p95_ms']:.4f},{r['p99_ms']:.4f},{r['ok']},{r['err']},{r['server_rss_bytes']},"
                f"{r['server_cpu_cores']:.3f},{r['loadgen_cpu_cores']:.3f},{r['host_idle_frac']:.3f}"
            )
    path.write_text("\n".join(rows) + "\n")


def main() -> None:
    global RESULTS, BIN
    p = argparse.ArgumentParser()
    p.add_argument("--events", type=int, default=300_000)
    p.add_argument("--logs", type=int, default=600_000)
    p.add_argument("--products", type=int, default=10_000)
    p.add_argument("--seed", type=int, default=20261109)
    p.add_argument("--runs", type=int, default=7)
    p.add_argument("--warmup", type=int, default=2)
    p.add_argument("--http-seconds", type=float, default=10.0)
    p.add_argument("--http-warmup", type=float, default=1.0)
    p.add_argument("--http-trials", type=int, default=5, help="measuring windows per server and concurrency")
    p.add_argument("--concurrency", default="1,4,8,16")
    p.add_argument("--server-cpus", help="taskset CPU list for the servers, e.g. 0-2")
    p.add_argument("--client-cpus", help="taskset CPU list for the load generator, e.g. 3")
    p.add_argument("--gojo", help="the gojo compiler (default: gojo on PATH)")
    p.add_argument("--results", type=Path, default=RESULTS, help="where to write the results")
    p.add_argument("--bin", type=Path, default=BIN, help="where to put the built binaries")
    p.add_argument("--skip-build", action="store_true")
    args = p.parse_args()
    concs = [int(x) for x in args.concurrency.split(",") if x.strip()]
    RESULTS, BIN = args.results.resolve(), args.bin.resolve()

    RESULTS.mkdir(parents=True, exist_ok=True)
    env = env_info(str(Path(args.gojo).resolve()) if args.gojo else which_or("gojo", "/workspace/target/release/gojo"))
    env["argv"] = sys.argv[1:]
    print(json.dumps({k: env[k] for k in ("gojo_version", "rustc", "cpu", "nproc")}, indent=2), flush=True)

    fx = generate_fixtures(args.events, args.logs, args.products, args.seed)
    gojo = env["gojo_bin"]
    cargo = which_or("cargo")
    if args.skip_build and (BIN / "gojo-jsonpipe").exists() and (BIN / "rust-jsonpipe").exists():
        bins = {
            "gojo-jsonpipe": str(BIN / "gojo-jsonpipe"),
            "gojo-logscan": str(BIN / "gojo-logscan"),
            "gojo-httpserver": str(BIN / "gojo-httpserver"),
            "rust-jsonpipe": str(BIN / "rust-jsonpipe"),
            "rust-logscan": str(BIN / "rust-logscan"),
            "rust-httpserver": str(BIN / "rust-httpserver"),
            "rust-loadgen": str(BIN / "rust-loadgen"),
            "gojo_build_s": float("nan"),
            "rust_build_s": float("nan"),
        }
    else:
        bins = build(gojo, cargo)

    events = FIXTURES / "events.ndjson"
    logs = FIXTURES / "app.log"
    products_path = FIXTURES / "products.json"
    products = json.loads(products_path.read_text())

    out_g = run_stdout([bins["gojo-jsonpipe"], str(events)])
    out_r = run_stdout([bins["rust-jsonpipe"], str(events)])
    jp_match = out_g == out_r
    if not jp_match:
        print("jsonpipe stdout differs; Gojo", out_g[:200], "Rust", out_r[:200], file=sys.stderr)
        g_obj, r_obj = json.loads(out_g), json.loads(out_r)
        if g_obj != r_obj:
            raise SystemExit("jsonpipe JSON values differ")
        print("jsonpipe: bytes differ, parsed JSON matches (encoder spacing/order)", file=sys.stderr)
    log_g = run_stdout([bins["gojo-logscan"], str(logs)])
    log_r = run_stdout([bins["rust-logscan"], str(logs)])
    ls_match = log_g == log_r
    if not ls_match:
        print("logscan stdout differs; Gojo", log_g, "Rust", log_r, file=sys.stderr)
        if json.loads(log_g) != json.loads(log_r):
            raise SystemExit("logscan JSON values differ")
        print("logscan: bytes differ, parsed JSON matches", file=sys.stderr)

    correctness = {
        "jsonpipe": {
            "sha256": sha256_bytes(out_g),
            "sha256_rust": sha256_bytes(out_r),
            "match": jp_match or json.loads(out_g) == json.loads(out_r),
            "stdout": json.loads(out_g),
        },
        "logscan": {
            "sha256": sha256_bytes(log_g),
            "sha256_rust": sha256_bytes(log_r),
            "match": ls_match or json.loads(log_g) == json.loads(log_r),
            "stdout": json.loads(log_g),
        },
        "http": True,
    }
    print("jsonpipe", correctness["jsonpipe"]["stdout"], "match", correctness["jsonpipe"]["match"], flush=True)
    print("logscan", correctness["logscan"]["stdout"], "match", correctness["logscan"]["match"], flush=True)

    hf_dir = RESULTS / "hyperfine"
    hf_dir.mkdir(exist_ok=True)
    batch = {"jsonpipe": {}, "logscan": {}}
    for name, path in (("jsonpipe", events), ("logscan", logs)):
        for lang, key in (("gojo", f"gojo-{name}"), ("rust", f"rust-{name}")):
            print(f"--- {lang} {name} ---", flush=True)
            batch[name][lang] = hyperfine_cmd(
                bins[key],
                [str(path)],
                args.runs,
                args.warmup,
                hf_dir / f"{lang}-{name}.json",
            )
            # drop bulky raw nested copy from the summary file later
            raw_path = hf_dir / f"{lang}-{name}.json"
            batch[name][lang]["hyperfine_json"] = str(raw_path.relative_to(RESULTS))
            batch[name][lang].pop("raw", None)

    print("--- HTTP (Gojo and Rust, interleaved) ---", flush=True)
    http = measure_http(
        {"gojo": bins["gojo-httpserver"], "rust": bins["rust-httpserver"]},
        bins["rust-loadgen"],
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
    # correctness payloads are large (stats cents); keep the checked fields
    for r in http.values():
        r["correctness"] = {
            "stats_n": r["correctness"]["stats"]["n"],
            "stats_cents": r["correctness"]["stats"]["cents"],
            "query_n": r["correctness"]["query"]["n"],
            "query_ids": r["correctness"]["query"]["ids"][:5],
        }

    doc = {
        "env": env,
        "fixtures": fx,
        "bins": {k: bins[k] for k in bins},
        "hyperfine_runs": args.runs,
        "hyperfine_warmup": args.warmup,
        "correctness": correctness,
        "batch": batch,
        "http": http,
        "http_seconds": args.http_seconds,
        "http_warmup": args.http_warmup,
        "http_trials": args.http_trials,
        "server_cpus": args.server_cpus,
        "client_cpus": args.client_cpus,
    }
    (RESULTS / "results.json").write_text(json.dumps(doc, indent=2) + "\n")
    write_csv(doc, RESULTS / "results.csv")
    write_markdown(doc, RESULTS / "RESULTS.md")
    write_chart(doc, RESULTS / "chart.png")
    print("wrote", RESULTS / "RESULTS.md", RESULTS / "chart.png", flush=True)


if __name__ == "__main__":
    main()
