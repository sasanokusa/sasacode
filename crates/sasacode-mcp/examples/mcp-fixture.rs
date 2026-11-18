//! The test MCP server over stdio (`bun packages/mcp/test/fixture-server.ts` in the TS tests).
//! Built by `cargo test`; tests/mcp.rs starts it as a child process.
//!
//! FIXTURE_LOG: a line for every process started and ended, to count them (#101).
//! FIXTURE_SLOW_EXIT: takes this many ms to shut down once the client goes away, as a server that
//! closes a browser or writes out a database does.

#[path = "../tests/fixture/mod.rs"]
mod fixture;

use std::io::Write;

use rmcp::ServiceExt;

fn log(what: &str) {
    if let Some(path) = std::env::var_os("FIXTURE_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{what} {}", std::process::id());
    }
}

#[tokio::main]
async fn main() {
    log("start");
    match fixture::FixtureServer::new().serve(rmcp::transport::stdio()).await {
        // Runs until stdin ends.
        Ok(service) => {
            let _ = service.waiting().await;
        }
        Err(e) => eprintln!("fixture: {e}"),
    }
    let slow: u64 = std::env::var("FIXTURE_SLOW_EXIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    tokio::time::sleep(std::time::Duration::from_millis(slow)).await;
    log("exit");
    std::process::exit(0);
}
