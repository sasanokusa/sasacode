//! Stands in for rustxng (packages/bundled/test/fake-rustxng.ts): `--bind A --port P` serves /healthz
//! and a SearXNG-style /search, and exits after RUSTXNG_IDLE_TIMEOUT seconds without a request, as
//! the real one does. Built by `cargo test -p sasacode-bundled`; the net_web tests start it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Query, Request};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde_json::json;

fn arg(args: &[String], name: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default()
}

/// encodeURIComponent.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bind = arg(&args, "--bind");
    let port = arg(&args, "--port");
    let idle: u64 = std::env::var("RUSTXNG_IDLE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let last = Arc::new(Mutex::new(Instant::now()));

    let touched = last.clone();
    let app = Router::new()
        .route("/healthz", get(|| async { "OK" }))
        .route(
            "/search",
            get(move |Query(q): Query<HashMap<String, String>>| async move {
                let query = q.get("q").cloned().unwrap_or_default();
                let max = q.get("max_results").cloned().unwrap_or_else(|| "null".into());
                Json(json!({ "results": [{
                    "title": format!("About {query}"),
                    "url": format!("https://example.com/{}", encode_uri_component(&query)),
                    "content": format!("idle={idle} max={max}"),
                }] }))
                .into_response()
            }),
        )
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let touched = touched.clone();
            async move {
                *touched.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                let res: Response = next.run(req).await;
                res
            }
        }));

    let listener = match tokio::net::TcpListener::bind(format!("{bind}:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake-rustxng: {e}");
            std::process::exit(1);
        }
    };
    if idle > 0 {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if last.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= Duration::from_secs(idle) {
                    std::process::exit(0);
                }
            }
        });
    }
    let _ = axum::serve(listener, app).await;
}
