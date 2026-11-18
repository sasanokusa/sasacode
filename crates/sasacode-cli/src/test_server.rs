//! A tiny HTTP/1.1 server on 127.0.0.1 for the tests (the TS tests' `Bun.serve` with a fetch handler):
//! each request goes to a closure that answers it. It runs on its own threads, so it serves the same
//! from any test runtime, and stops when dropped.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

pub struct Req {
    pub method: String,
    /// The path, without the query.
    pub path: String,
    pub query: String,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == &name.to_ascii_lowercase())
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

pub struct Res {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Res {
    /// `Response.json(v)`.
    pub fn json(v: Value) -> Res {
        Res {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: v.to_string().into_bytes(),
        }
    }

    /// `new Response(body, { status })`.
    pub fn text(status: u16, body: &str) -> Res {
        Res {
            status,
            headers: vec![("content-type".into(), "text/plain;charset=utf-8".into())],
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn redirect(location: &str) -> Res {
        Res {
            status: 302,
            headers: vec![("location".into(), location.into())],
            body: Vec::new(),
        }
    }
}

type Handler = Arc<dyn Fn(&Req) -> Res + Send + Sync>;

pub struct TestServer {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl TestServer {
    pub fn start(handler: impl Fn(&Req) -> Res + Send + Sync + 'static) -> TestServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Handler = Arc::new(handler);
        let stopping = stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let handler = handler.clone();
                std::thread::spawn(move || serve(stream, &handler));
            }
        });
        TestServer { port, stop }
    }

    /// `http://127.0.0.1:<port>`
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(stream: TcpStream, handler: &Handler) {
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() {
            return;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((n, v)) = h.split_once(':') {
            headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let req = Req {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let res = handler(&req);
    let mut out = format!("HTTP/1.1 {} X\r\n", res.status);
    for (n, v) in &res.headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    let body: &[u8] = if req.method == "HEAD" { &[] } else { &res.body };
    out.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", body.len()));
    let mut stream = stream;
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}
