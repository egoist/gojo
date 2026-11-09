//! NDJSON parse → filter → aggregate → serialize.
//! serde_json per line (the usual NDJSON approach), matching Gojo json.decode.

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process;

use gojo_realworld_bench::{Event, RegionRow, Summary};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: jsonpipe FILE.ndjson");
        process::exit(2);
    });
    let f = File::open(&path).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    let reader = BufReader::with_capacity(256 * 1024, f);
    let mut matched: i64 = 0;
    let mut total_cents: i64 = 0;
    let mut regions: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for line in reader.lines() {
        let row = line.unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(1);
        });
        if row.is_empty() {
            continue;
        }
        let ev: Event = serde_json::from_str(&row).unwrap_or_else(|e| {
            eprintln!("json: {e}");
            process::exit(1);
        });
        if !ev.ok || ev.cents < 500 || !ev.tags.iter().any(|t| t == "vip") {
            continue;
        }
        matched += 1;
        total_cents += ev.cents;
        let e = regions.entry(ev.region).or_insert((0, 0));
        e.0 += 1;
        e.1 += ev.cents;
    }
    let out = Summary {
        matched,
        cents: total_cents,
        regions: regions
            .into_iter()
            .map(|(k, (n, cents))| RegionRow { k, n, cents })
            .collect(),
    };
    println!("{}", serde_json::to_string(&out).unwrap());
}
