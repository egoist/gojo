#!/usr/bin/env python3
"""Deterministic fixtures for the Gojo vs Rust real-application suite.

Not timed. Both languages read the same files. Seed and sizes are documented
in stdout and copied into the results.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

REGIONS = ("ap-east", "eu-west", "us-east", "us-west")
SKUS = tuple(f"SKU-{i:04d}" for i in range(40))
METHODS = ("GET", "POST")
PATHS = ("/api/orders/", "/api/checkout", "/health", "/api/search")
LEVELS = ("INFO", "INFO", "INFO", "INFO", "WARN", "ERROR")  # ~1/6 ERROR
TAGS = ("vip", "repeat", "new", "bulk")


def lcg(seed: int):
    x = seed & 0x7FFFFFFF
    while True:
        x = (x * 1103515245 + 12345) & 0x7FFFFFFF
        yield x


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--dir", type=Path, default=Path(__file__).resolve().parent / "fixtures")
    p.add_argument("--seed", type=int, default=20261109)
    p.add_argument("--events", type=int, default=300_000)
    p.add_argument("--logs", type=int, default=600_000)
    p.add_argument("--products", type=int, default=10_000)
    args = p.parse_args()
    args.dir.mkdir(parents=True, exist_ok=True)
    rng = lcg(args.seed)

    events_path = args.dir / "events.ndjson"
    with events_path.open("w", encoding="utf-8", newline="\n") as f:
        for i in range(args.events):
            x = next(rng)
            region = REGIONS[x % 4]
            sku = SKUS[(x // 4) % 40]
            qty = 1 + (x % 5)
            cents = 50 + (x % 20_000)
            ok = (x % 10) != 0
            tags = [TAGS[0]] if x % 5 == 0 else [TAGS[1 + (x % 3)]]
            if x % 17 == 0:
                tags.append(TAGS[2])
            rec = {
                "id": i + 1,
                "user": f"u{(x % 50_000):06d}",
                "region": region,
                "sku": sku,
                "qty": qty,
                "cents": cents,
                "ok": ok,
                "tags": tags,
            }
            f.write(json.dumps(rec, separators=(",", ":"), ensure_ascii=True))
            f.write("\n")

    logs_path = args.dir / "app.log"
    with logs_path.open("w", encoding="utf-8", newline="\n") as f:
        for i in range(args.logs):
            x = next(rng)
            level = LEVELS[x % 6]
            method = METHODS[(x // 6) % 2]
            kind = PATHS[(x // 12) % 4]
            oid = x % 80_000
            path = f"{kind}{oid}" if kind == "/api/orders/" else kind
            status = (200, 200, 200, 201, 400, 404, 500)[x % 7]
            lat = 1 + (x % 250)
            ip = f"10.{(x >> 8) & 255}.{x & 255}.{(i >> 8) & 255}"
            mm = (i // 60000) % 60
            ss = (i // 1000) % 60
            ms = i % 1000
            ts = f"2024-06-15T12:{mm:02d}:{ss:02d}.{ms:03d}Z"
            user = f"u{(x % 50_000):06d}"
            extra = " café 订单" if x % 5 == 0 else ""
            line = (
                f"{ts} {level} {ip} {method} {path} {status} {lat}ms {user}{extra}\n"
            )
            f.write(line)

    products_path = args.dir / "products.json"
    products = []
    for i in range(args.products):
        x = next(rng)
        products.append(
            {
                "id": i + 1,
                "sku": f"SKU-{i+1:06d}",
                "region": REGIONS[x % 4],
                "cents": 100 + (x % 50_000),
                "tag": TAGS[x % 4],
            }
        )
    products_path.write_text(
        json.dumps(products, separators=(",", ":"), ensure_ascii=True),
        encoding="utf-8",
    )

    meta = {
        "seed": args.seed,
        "events": args.events,
        "logs": args.logs,
        "products": args.products,
        "json_filter": "ok == true AND cents >= 500 AND tags contains vip",
        "log_needle_ascii": "GET /api/orders/",
        "log_needle_utf8": "订单",
        "log_semantics": "UTF-8 substring search (Gojo str.contains / Rust str::contains); valid UTF-8 only",
        "files": {
            "events": str(events_path),
            "logs": str(logs_path),
            "products": str(products_path),
        },
        "bytes": {
            "events": events_path.stat().st_size,
            "logs": logs_path.stat().st_size,
            "products": products_path.stat().st_size,
        },
    }
    (args.dir / "manifest.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(json.dumps(meta, indent=2))


if __name__ == "__main__":
    main()
