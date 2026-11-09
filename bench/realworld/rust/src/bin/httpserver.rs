//! Read-mostly in-memory JSON catalog. axum 0.8 + tokio + serde_json, HTTP/1.1.
//! Same routes and JSON as the Gojo std/http server.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use gojo_realworld_bench::{Health, Item, Query, QueryOut, StatsOut};

struct App {
    items: Vec<Item>,
    n: i64,
    cents: i64,
}

async fn health() -> Json<Health> {
    Json(Health { ok: true })
}

async fn stats(State(app): State<Arc<App>>) -> Json<StatsOut> {
    Json(StatsOut {
        n: app.n,
        cents: app.cents,
    })
}

async fn product(State(app): State<Arc<App>>, Path(id): Path<i64>) -> impl IntoResponse {
    if id < 1 || id as usize > app.items.len() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not found"})))
            .into_response();
    }
    Json(app.items[(id as usize) - 1].clone()).into_response()
}

async fn query(State(app): State<Arc<App>>, Json(q): Json<Query>) -> Json<QueryOut> {
    let mut n: i64 = 0;
    let mut ids = Vec::new();
    for it in &app.items {
        if it.region == q.region && it.cents >= q.min_cents && it.tag == q.tag {
            n += 1;
            if ids.len() < 50 {
                ids.push(it.id);
            }
        }
    }
    Json(QueryOut { n, ids })
}

#[tokio::main]
async fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: httpserver products.json [host:port]");
        process::exit(2);
    });
    let bind = args.next().unwrap_or_else(|| "127.0.0.1:0".to_string());
    let text = fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    let items: Vec<Item> = serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("products json: {e}");
        process::exit(1);
    });
    let n = items.len() as i64;
    let cents: i64 = items.iter().map(|i| i.cents).sum();
    let app = Arc::new(App { items, n, cents });
    let router = Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/products/{id}", get(product))
        .route("/query", post(query))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    let addr = listener.local_addr().unwrap();
    println!("listening {addr}");
    io::stdout().flush().unwrap();
    axum::serve(listener, router).await.unwrap();
}
