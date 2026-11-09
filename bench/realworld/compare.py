#!/usr/bin/env python3
"""Before/after tables from two or more run_realworld.py results.json files.

    python3 bench/realworld/compare.py before=results/x/before after=results/x/after

Each argument is LABEL=DIR (a --results directory). The first is the
reference. Rust runs in every results file, unchanged, as a control for drift
of the host between the runs.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


def load(arg: str) -> tuple[str, dict]:
    label, _, d = arg.partition("=")
    return label, json.loads((Path(d) / "results.json").read_text())


def pct(a: float, b: float) -> str:
    return f"{(b / a - 1) * 100:+.1f}%"


def main() -> None:
    runs = [load(a) for a in sys.argv[1:]]
    (ref_label, ref) = runs[0]
    out = []
    a = out.append
    a("### Batch jobs (median wall-clock ± MAD, hyperfine)")
    a("")
    head = "| Workload | Lang | " + " | ".join(label for label, _ in runs) + " | change |"
    a(head)
    a("| --- | --- | " + " | ".join("---:" for _ in runs) + " | ---: |")
    for name in ("jsonpipe", "logscan"):
        for lang in ("gojo", "rust"):
            cells = []
            for _, doc in runs:
                b = doc["batch"][name][lang]
                cells.append(f"{b['median_s'] * 1000:.1f} ± {b['mad_s'] * 1000:.1f} ms")
            first = ref["batch"][name][lang]["median_s"]
            last = runs[-1][1]["batch"][name][lang]["median_s"]
            a(f"| {name} | {lang} | " + " | ".join(cells) + f" | {pct(first, last)} |")
    a("")
    a("Gojo / Rust time ratio (median):")
    a("")
    a("| Workload | " + " | ".join(label for label, _ in runs) + " |")
    a("| --- | " + " | ".join("---:" for _ in runs) + " |")
    for name in ("jsonpipe", "logscan"):
        cells = [f"{doc['batch'][name]['gojo']['median_s'] / doc['batch'][name]['rust']['median_s']:.2f}×" for _, doc in runs]
        a(f"| {name} | " + " | ".join(cells) + " |")
    a("")
    a("### HTTP JSON API (median of trials, min–max)")
    a("")
    a("| Lang | conc. | " + " | ".join(f"{label} req/s" for label, _ in runs) + " | change | " + " | ".join(f"{label} p50 / p99" for label, _ in runs) + " | " + " | ".join(f"{label} server CPU" for label, _ in runs) + " | err |")
    a("| --- | ---: | " + " | ".join("---:" for _ in runs) + " | ---: | " + " | ".join("---:" for _ in runs) + " | " + " | ".join("---:" for _ in runs) + " | ---: |")
    for lang in ("gojo", "rust"):
        for i, row0 in enumerate(ref["http"][lang]["concurrency"]):
            rows = [doc["http"][lang]["concurrency"][i] for _, doc in runs]
            rps = [f"{r['rps']:,.0f} ({r['rps_min']:,.0f}–{r['rps_max']:,.0f})" for r in rows]
            lat = [f"{r['p50_ms']:.3f} / {r['p99_ms']:.3f} ms" for r in rows]
            cpu = [f"{r['server_cpu_cores']:.2f}" for r in rows]
            err = sum(r["err"] for r in rows)
            a(f"| {lang} | {row0['concurrency']} | " + " | ".join(rps) + f" | {pct(rows[0]['rps'], rows[-1]['rps'])} | " + " | ".join(lat) + " | " + " | ".join(cpu) + f" | {err} |")
    a("")
    a("Server CPU per request (server CPU cores / req/s, µs):")
    a("")
    a("| Lang | conc. | " + " | ".join(label for label, _ in runs) + " |")
    a("| --- | ---: | " + " | ".join("---:" for _ in runs) + " |")
    for lang in ("gojo", "rust"):
        for i, row0 in enumerate(ref["http"][lang]["concurrency"]):
            rows = [doc["http"][lang]["concurrency"][i] for _, doc in runs]
            a(f"| {lang} | {row0['concurrency']} | " + " | ".join(f"{r['server_cpu_cores'] / r['rps'] * 1e6:.1f}" for r in rows) + " |")
    a("")
    a("Server peak RSS (VmHWM): " + ", ".join(f"{label}: Gojo {doc['http']['gojo']['server_rss_peak_bytes'] / 2**20:.1f} MiB, Rust {doc['http']['rust']['server_rss_peak_bytes'] / 2**20:.1f} MiB" for label, doc in runs) + ".")
    print("\n".join(out))


if __name__ == "__main__":
    main()
