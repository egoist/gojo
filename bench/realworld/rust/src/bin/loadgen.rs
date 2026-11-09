//! HTTP/1.1 mixed-workload client. Not the system under test.
//! Reports JSON: ok, err, rps, p50/p95/p99, bytes.

use std::env;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use tokio::sync::Mutex;
use tokio::time::sleep;

fn lcg_next(x: &mut u64) -> u64 {
    *x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
    *x
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[tokio::main]
async fn main() {
    let mut base = String::from("http://127.0.0.1:8080");
    let mut seconds = 5.0f64;
    let mut warmup = 1.0f64;
    let mut concurrency = 8usize;
    let mut n_products = 10_000i64;
    let mut seed = 1u64;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => base = args.next().expect("--url"),
            "--seconds" => seconds = args.next().expect("--seconds").parse().unwrap(),
            "--warmup" => warmup = args.next().expect("--warmup").parse().unwrap(),
            "--concurrency" => concurrency = args.next().expect("--concurrency").parse().unwrap(),
            "--products" => n_products = args.next().expect("--products").parse().unwrap(),
            "--seed" => seed = args.next().expect("--seed").parse().unwrap(),
            _ => {
                eprintln!(
                    "usage: loadgen --url URL --seconds 5 --warmup 1 --concurrency 8 --products 10000 --seed 1"
                );
                process::exit(2);
            }
        }
    }
    while base.ends_with('/') {
        base.pop();
    }
    let uri: http::Uri = base.parse().unwrap_or_else(|e| {
        eprintln!("bad --url {base}: {e}");
        process::exit(2);
    });
    let host = uri
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    connector.set_keepalive(Some(Duration::from_secs(30)));
    let client = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(concurrency.max(4) * 2)
        .build::<_, Full<Bytes>>(connector);

    let stop = Arc::new(AtomicBool::new(false));
    let measuring = Arc::new(AtomicBool::new(false));
    let ok = Arc::new(AtomicU64::new(0));
    let err = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let latencies = Arc::new(Mutex::new(Vec::<f64>::with_capacity(64 * 1024)));

    let regions = ["ap-east", "eu-west", "us-east", "us-west"];
    let tags = ["vip", "repeat", "new", "bulk"];

    let mut handles = Vec::new();
    for w in 0..concurrency {
        let client = client.clone();
        let base = base.clone();
        let host = host.clone();
        let stop = stop.clone();
        let measuring = measuring.clone();
        let ok = ok.clone();
        let err = err.clone();
        let bytes = bytes.clone();
        let latencies = latencies.clone();
        let mut rng = seed.wrapping_add(w as u64).wrapping_mul(0x9E3779B97F4A7C15);
        handles.push(tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let x = lcg_next(&mut rng);
                let kind = x % 100;
                let t0 = Instant::now();
                let result: Result<(u16, usize), String> = async {
                    let req = if kind < 70 {
                        let id = 1 + ((x >> 8) as i64).rem_euclid(n_products);
                        Request::builder()
                            .method("GET")
                            .uri(format!("{base}/products/{id}"))
                            .header("host", host.as_str())
                            .header("connection", "keep-alive")
                            .body(Full::new(Bytes::new()))
                            .map_err(|e| e.to_string())?
                    } else if kind < 90 {
                        let region = regions[(x as usize / 7) % 4];
                        let tag = tags[(x as usize / 13) % 4];
                        let body = format!(
                            r#"{{"region":"{region}","min_cents":500,"tag":"{tag}"}}"#
                        );
                        Request::builder()
                            .method("POST")
                            .uri(format!("{base}/query"))
                            .header("host", host.as_str())
                            .header("content-type", "application/json")
                            .header("connection", "keep-alive")
                            .body(Full::new(Bytes::from(body)))
                            .map_err(|e| e.to_string())?
                    } else if kind < 98 {
                        Request::builder()
                            .method("GET")
                            .uri(format!("{base}/stats"))
                            .header("host", host.as_str())
                            .header("connection", "keep-alive")
                            .body(Full::new(Bytes::new()))
                            .map_err(|e| e.to_string())?
                    } else {
                        Request::builder()
                            .method("GET")
                            .uri(format!("{base}/health"))
                            .header("host", host.as_str())
                            .header("connection", "keep-alive")
                            .body(Full::new(Bytes::new()))
                            .map_err(|e| e.to_string())?
                    };
                    let resp = client.request(req).await.map_err(|e| e.to_string())?;
                    let status = resp.status().as_u16();
                    let body = resp
                        .into_body()
                        .collect()
                        .await
                        .map_err(|e| e.to_string())?
                        .to_bytes();
                    Ok((status, body.len()))
                }
                .await;
                let dt = t0.elapsed().as_secs_f64() * 1000.0;
                match result {
                    Ok((status, n)) if (200..500).contains(&status) => {
                        if measuring.load(Ordering::Relaxed) {
                            ok.fetch_add(1, Ordering::Relaxed);
                            bytes.fetch_add(n as u64, Ordering::Relaxed);
                            latencies.lock().await.push(dt);
                        }
                    }
                    _ => {
                        if measuring.load(Ordering::Relaxed) {
                            err.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }

    sleep(Duration::from_secs_f64(warmup)).await;
    measuring.store(true, Ordering::SeqCst);
    let t_meas = Instant::now();
    sleep(Duration::from_secs_f64(seconds)).await;
    let elapsed = t_meas.elapsed().as_secs_f64();
    stop.store(true, Ordering::SeqCst);
    for h in handles {
        let _ = h.await;
    }
    let ok_n = ok.load(Ordering::SeqCst);
    let err_n = err.load(Ordering::SeqCst);
    let mut samples = latencies.lock().await.clone();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rps = ok_n as f64 / elapsed.max(1e-9);
    let out = serde_json::json!({
        "concurrency": concurrency,
        "warmup_s": warmup,
        "seconds": elapsed,
        "ok": ok_n,
        "err": err_n,
        "rps": rps,
        "p50_ms": percentile(&samples, 50.0),
        "p95_ms": percentile(&samples, 95.0),
        "p99_ms": percentile(&samples, 99.0),
        "bytes": bytes.load(Ordering::SeqCst),
        "samples": samples.len(),
        "url": base,
        "http": "1.1",
        "mix": "70% GET /products/:id, 20% POST /query, 8% GET /stats, 2% GET /health",
    });
    println!("{}", serde_json::to_string(&out).unwrap());
}
