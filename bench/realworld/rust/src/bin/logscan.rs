//! Stream a UTF-8 log: fixed substring search + field parse/aggregate.
//! str::contains on valid UTF-8 — same as Gojo str.contains for these needles.

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process;

use gojo_realworld_bench::{Report, HAN, NEEDLE};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: logscan FILE.log");
        process::exit(2);
    });
    let f = File::open(&path).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    let reader = BufReader::with_capacity(256 * 1024, f);
    let mut r = Report {
        lines: 0,
        needle: 0,
        errors: 0,
        han: 0,
        s2: 0,
        s4: 0,
        s5: 0,
    };
    for line in reader.lines() {
        let row = line.unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(1);
        });
        r.lines += 1;
        if row.contains(NEEDLE) {
            r.needle += 1;
        }
        if row.contains(HAN) {
            r.han += 1;
        }
        let mut parts = row.splitn(8, ' ');
        let Some(_) = parts.next() else { continue }; // ts
        let Some(level) = parts.next() else { continue };
        let Some(_) = parts.next() else { continue }; // ip
        let Some(_) = parts.next() else { continue }; // method
        let Some(_) = parts.next() else { continue }; // path
        let Some(status_s) = parts.next() else { continue };
        if level == "ERROR" {
            r.errors += 1;
        }
        let Ok(status) = status_s.parse::<i64>() else { continue };
        if (200..300).contains(&status) {
            r.s2 += 1;
        } else if (400..500).contains(&status) {
            r.s4 += 1;
        } else if (500..600).contains(&status) {
            r.s5 += 1;
        }
    }
    println!("{}", serde_json::to_string(&r).unwrap());
}
