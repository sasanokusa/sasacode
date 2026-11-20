//! web_search (`packages/bundled/src/web-search.ts`): search the web through RustXNG
//! (https://github.com/sahenjp/RustXNG), a keyless SearXNG-compatible metasearch server, so the agent
//! finds URLs instead of guessing them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use regex::Regex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use sasacode_host::t;
use sasacode_plugin_api::{
    Command, CommandContext, NotifyLevel, PluginApi, SelectOption, ToolDef, ToolKind, ToolRef, ToolResult,
    apply_child_env, error_result, hook,
};

use crate::net_util::{js_num_str, js_trim, js_words, setting_number, setting_str, utf16_len, utf16_prefix};
use crate::rustxng_install::{InstallOptions, RUSTXNG_RELEASES, install_rustxng, managed_rustxng, no_redirects};
use crate::web_fetch::{USER_AGENT, content_type, html_to_text, read_capped, readable, send_within};

/// An IPv6 address as its eight 16-bit words, `::` filled in with zeros; nothing for anything else.
/// A zone (`fe80::1%eth0`) is accepted as Node's `isIP` accepts it, and does not count.
fn ipv6_words(ip: &str) -> Option<[u16; 8]> {
    let v = ip.to_lowercase();
    let addr = v.split_once('%').map_or(v.as_str(), |(a, _)| a);
    addr.parse::<Ipv6Addr>().ok().map(|a| a.segments())
}

/// The IPv4 an address means: itself for an IPv4, and for an IPv6 that carries one its last 32 bits —
/// written in dotted decimal (`::ffff:127.0.0.1`) or in hexadecimal (`::ffff:7f00:1`, which the URL
/// parser keeps as written and so used to be judged as an unknown, public IPv6). Nothing for an
/// address that really is IPv6.
fn carried_ipv4(ip: &str) -> Option<[u8; 4]> {
    if let Ok(v4) = ip.parse::<Ipv4Addr>() {
        return Some(v4.octets());
    }
    let w = ipv6_words(ip)?;
    let v4 = |hi: u16, lo: u16| [(hi >> 8) as u8, (hi & 0xff) as u8, (lo >> 8) as u8, (lo & 0xff) as u8];
    // An IPv4-mapped (`::ffff:`) address, or the older IPv4-compatible one (all zero): a fetch to
    // either goes to the IPv4 they carry, so both are judged as that address.
    if w[..5].iter().all(|x| *x == 0) && (w[5] == 0xffff || w[5] == 0) {
        return Some(v4(w[6], w[7]));
    }
    // NAT64's well-known prefix (64:ff9b::/96, RFC 6052): a NAT64 gateway connects to the IPv4 in the
    // last 32 bits, and DNS64 answers a name with such an address (#83).
    if w[0] == 0x64 && w[1] == 0xff9b && w[2..6].iter().all(|x| *x == 0) {
        return Some(v4(w[6], w[7]));
    }
    // 6to4 (2002::/16) carries its IPv4 in bits 16-47.
    if w[0] == 0x2002 {
        return Some(v4(w[1], w[2]));
    }
    None
}

/// Loopback, private, link-local (cloud metadata), shared, unspecified and multicast addresses.
pub fn private_address(ip: &str) -> bool {
    if let Some([a, b, _, _]) = carried_ipv4(ip) {
        return a == 0
            || a == 10
            || a == 127
            || a >= 224
            || (a == 169 && b == 254)
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 100 && (64..=127).contains(&b));
    }
    let v = ip.to_lowercase();
    let w = ipv6_words(ip);
    // A real IPv6, then: unspecified and loopback, unique local (fc/fd), link-local (fe80..), multicast (ff),
    // and NAT64's local-use prefix (64:ff9b:1::/48, RFC 8215), whose IPv4 are the site's own.
    v == "::"
        || v == "::1"
        || ["fc", "fd", "fe8", "fe9", "fea", "feb", "ff"]
            .iter()
            .any(|p| v.starts_with(p))
        || w.is_some_and(|w| w[0] == 0x64 && w[1] == 0xff9b && w[2] == 1)
}

const DEFAULT_PORT: f64 = 47888.0;
/// Seconds without a request before the sidecar exits (RUSTXNG_IDLE_TIMEOUT).
const DEFAULT_IDLE: f64 = 600.0;
const DEFAULT_RESULTS: f64 = 8.0;
/// Page text kept per result when include_text is on.
const TEXT_CHARS: usize = 4_000;
/// Results whose page include_text reads, at most: the first ones that match the query (setting textPages).
const DEFAULT_TEXT_PAGES: f64 = 3.0;
/// Snippet kept per result: some engines send whole paragraphs.
const SNIPPET_CHARS: usize = 300;

/// One search result (SearXNG's JSON), and what include_text made of its page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearxResult {
    pub title: Option<String>,
    pub url: Option<String>,
    pub content: Option<String>,
    /// The page's main text, read for include_text; or why it was not.
    pub text: Option<String>,
    pub text_note: Option<String>,
}

/// A SearXNG `/search?format=json` answer. Values that are not strings are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearxResponse {
    pub results: Vec<SearxResult>,
    /// Each answer as a string (`answers: (string | { answer?: string })[]`), None when it had none.
    pub answers: Vec<Option<String>>,
    pub corrections: Vec<String>,
    pub suggestions: Vec<String>,
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

impl SearxResponse {
    pub fn from_value(v: &Value) -> SearxResponse {
        let results = v
            .get("results")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|r| SearxResult {
                        title: str_of(r, "title"),
                        url: str_of(r, "url"),
                        content: str_of(r, "content"),
                        ..SearxResult::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let answers = v
            .get("answers")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| x.as_str().map(str::to_string).or_else(|| str_of(x, "answer")))
                    .collect()
            })
            .unwrap_or_default();
        SearxResponse {
            results,
            answers,
            corrections: strings(v, "corrections"),
            suggestions: strings(v, "suggestions"),
        }
    }
}

/// Whether `base` answers on /healthz within a second.
async fn healthy(base: &str) -> bool {
    let req = sasacode_ai::http::client().get(format!("{base}/healthz"));
    match tokio::time::timeout(Duration::from_secs(1), req.send()).await {
        Ok(Ok(res)) => res.status().is_success(),
        _ => false,
    }
}

static SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"[\s\-_・.]+").expect("fixed pattern")
});

fn words(s: &str) -> String {
    let normalized: String = s.nfkc().collect::<String>().to_lowercase();
    SEPARATORS.replace_all(&normalized, "").into_owned()
}

/// Whether a result looks like it is about the query: enough of the query's words appear in its title
/// and snippet, compared without case, width, spaces or hyphens. Enough is two (or all of a shorter
/// query), and one for the engine's first result.
pub fn matches_query(query: &str, r: &SearxResult, first: bool) -> bool {
    let terms: Vec<String> = js_words(query)
        .into_iter()
        .map(words)
        .filter(|w| !w.is_empty())
        .collect();
    let hay = words(&format!(
        "{} {}",
        r.title.as_deref().unwrap_or(""),
        r.content.as_deref().unwrap_or("")
    ));
    let found = terms.iter().filter(|t| hay.contains(t.as_str())).count();
    found >= if first { 1 } else { terms.len().min(2) }
}

/// Whether a URL's host is on the public internet (what a page an engine returned may be read from).
async fn public_host(url: &url::Url) -> Result<bool, String> {
    let host = url.host_str().unwrap_or("");
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    if host == "localhost" || host.ends_with(".localhost") {
        return Ok(false);
    }
    let addresses: Vec<String> = if host.parse::<IpAddr>().is_ok() {
        vec![host.to_string()]
    } else {
        tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| e.to_string())?
            .map(|a| a.ip().to_string())
            .collect()
    };
    Ok(!addresses.is_empty() && !addresses.iter().any(|a| private_address(a)))
}

static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"\[([^\]]*)\]\[\d+\]").expect("fixed pattern")
});

/// A result page's main text (furniture and menus left out), at most TEXT_CHARS + 1 characters.
/// The URL comes from the engine, not the user: unless `any_host`, it and every redirect must be public.
async fn page_text(url: &str, signal: &CancellationToken, any_host: bool) -> Result<String, String> {
    let mut next = url::Url::parse(url).map_err(|_| "Invalid URL".to_string())?;
    let mut hops = 0;
    // The 15 seconds run from the last request's start over its body too.
    let mut started;
    let res = loop {
        if next.scheme() != "http" && next.scheme() != "https" {
            return Err(format!("not http(s): {}:", next.scheme()));
        }
        if !any_host && !public_host(&next).await? {
            return Err("not a public address".into());
        }
        let req = no_redirects()
            .get(next.clone())
            .header("user-agent", USER_AGENT)
            .header("accept", "text/html,text/plain;q=0.9");
        started = tokio::time::Instant::now();
        let res = send_within(req, signal, Duration::from_secs(15)).await?;
        let location = if res.status().is_redirection() {
            res.headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        } else {
            None
        };
        let Some(location) = location else { break res };
        drop(res);
        if hops >= 5 {
            return Err("too many redirects".into());
        }
        hops += 1;
        next = next.join(&location).map_err(|_| "Invalid URL".to_string())?;
    };
    let type_ = content_type(&res);
    if !res.status().is_success() {
        return Err(res.status().as_u16().to_string());
    }
    if !(type_.contains("html") || type_.contains("text/plain")) {
        return Err(format!(
            "not a text page ({})",
            if type_.is_empty() { "no type" } else { &type_ }
        ));
    }
    let raw = tokio::select! {
        _ = signal.cancelled() => return Err("The operation was aborted.".into()),
        _ = tokio::time::sleep_until(started + Duration::from_secs(15)) => return Err("The operation timed out.".into()),
        r = read_capped(res, 5_000_000) => r.map_err(|e| e.to_string())?.text,
    };
    // The links' targets are left out here: the agent opens a page with web_fetch to follow them.
    let text = if type_.contains("html") {
        REFERENCE
            .replace_all(&html_to_text(&readable(&raw, false), Some(next.as_str())).text, "$1")
            .into_owned()
    } else {
        raw
    };
    Ok(utf16_prefix(js_trim(&text), TEXT_CHARS + 1).to_string())
}

pub fn format_results(query: &str, data: &SearxResponse) -> String {
    let results: Vec<&SearxResult> = data
        .results
        .iter()
        .filter(|r| r.url.as_deref().is_some_and(|u| !u.is_empty()))
        .collect();
    if results.is_empty() {
        let other: Vec<&str> = data
            .corrections
            .iter()
            .chain(&data.suggestions)
            .take(3)
            .map(String::as_str)
            .collect();
        let perhaps = if other.is_empty() {
            String::new()
        } else {
            format!(" (perhaps: {})", other.join(", "))
        };
        return format!("No results for \"{query}\". Try different words{perhaps}.");
    }
    let mut lines: Vec<String> = Vec::new();
    let answers: Vec<&str> = data
        .answers
        .iter()
        .flatten()
        .filter(|a| !a.is_empty())
        .map(String::as_str)
        .collect();
    if !answers.is_empty() {
        lines.push(format!("Answer: {}", answers.join(" / ")));
        lines.push(String::new());
    }
    for (i, r) in results.iter().enumerate() {
        let url = r.url.as_deref().unwrap_or("");
        let title = js_trim(r.title.as_deref().unwrap_or(""));
        lines.push(format!("{}. {}", i + 1, if title.is_empty() { url } else { title }));
        lines.push(format!("   {url}"));
        let snippet = crate::net_util::collapse_space(r.content.as_deref().unwrap_or(""));
        let snippet = js_trim(&snippet);
        if !snippet.is_empty() {
            if utf16_len(snippet) > SNIPPET_CHARS {
                lines.push(format!("   {}…", utf16_prefix(snippet, SNIPPET_CHARS)));
            } else {
                lines.push(format!("   {snippet}"));
            }
        }
        match (&r.text, &r.text_note) {
            (Some(text), _) if !text.is_empty() => {
                let first = if utf16_len(text) > TEXT_CHARS {
                    format!(", first {TEXT_CHARS} characters")
                } else {
                    String::new()
                };
                lines.push(format!("   --- page text (read{first}) ---"));
                lines.push(utf16_prefix(text, TEXT_CHARS).to_string());
                lines.push("   ---".into());
            }
            (_, Some(note)) if !note.is_empty() => lines.push(format!("   ({note})")),
            _ => {}
        }
    }
    lines.push(String::new());
    lines.push("A snippet is the search engine's excerpt, not the page: read a page (page text above, or web_fetch) before citing it.".into());
    lines.join("\n")
}

/// The sidecar being started, shared by every search that waits for it.
type Starting = Shared<BoxFuture<'static, Result<(), String>>>;

/// The plugin's settings and what it found, shared by its tool, command and hook.
struct WebSearch {
    api: PluginApi,
    url: Option<String>,
    configured: Option<String>,
    command: Mutex<Option<String>>,
    base: String,
    idle: f64,
    count: f64,
    private_pages: bool,
    text_pages: f64,
    registered: Mutex<bool>,
    /// One start at a time: concurrent searches wait for the same sidecar.
    starting: Mutex<Option<Starting>>,
    tool: Mutex<Option<ToolRef>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl WebSearch {
    fn find(&self) -> Option<String> {
        if self.url.is_some() {
            return None;
        }
        if let Some(configured) = &self.configured {
            return which::which(configured)
                .ok()
                .map(|p| p.display().to_string())
                .or_else(|| Path::new(configured).exists().then(|| configured.clone()));
        }
        // PATH as it is now: `which` reads it on every call.
        which::which("rustxng")
            .ok()
            .map(|p| p.display().to_string())
            .or_else(|| {
                let managed = managed_rustxng();
                managed.exists().then(|| managed.display().to_string())
            })
    }

    fn command(&self) -> Option<String> {
        lock(&self.command).clone()
    }

    async fn ensure(self: &Arc<Self>, signal: &CancellationToken) -> Result<(), String> {
        if self.url.is_some() || healthy(&self.base).await {
            return Ok(());
        }
        let starting = {
            let mut starting = lock(&self.starting);
            match &*starting {
                Some(started) => started.clone(),
                None => {
                    let me = self.clone();
                    let signal = signal.clone();
                    let start = async move {
                        let done = me.start(&signal).await.map_err(|e| e.to_string());
                        *lock(&me.starting) = None;
                        done
                    }
                    .boxed()
                    .shared();
                    *starting = Some(start.clone());
                    start
                }
            }
        };
        starting.await
    }

    async fn start(&self, signal: &CancellationToken) -> anyhow::Result<()> {
        let Some(command) = self.command() else {
            anyhow::bail!("rustxng did not answer on {}", self.base);
        };
        let port = url::Url::parse(&self.base)
            .ok()
            .and_then(|u| u.port())
            .map(|p| p.to_string())
            .unwrap_or_default();
        // Detached: it outlives this session and ends on its own idle timeout. (Not on Windows, where
        // there is no process group to leave: there it is per session.)
        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(["--bind", "127.0.0.1", "--port", &port])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into());
        apply_child_env(
            &mut cmd,
            &[
                ("RUSTXNG_IDLE_TIMEOUT".into(), js_num_str(self.idle)),
                ("RUST_LOG".into(), rust_log),
            ],
        );
        #[cfg(unix)]
        cmd.process_group(0);
        let mut proc = cmd.spawn()?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline && !signal.is_cancelled() {
            if healthy(&self.base).await {
                return Ok(());
            }
            // Exited without answering: another session's sidecar may have taken the port first.
            if let Ok(Some(status)) = proc.try_wait()
                && let Some(code) = status.code()
                && !healthy(&self.base).await
            {
                anyhow::bail!(
                    "rustxng exited ({code}) without answering on {}; is the port in use? Set plugins.settings.web-search.port",
                    self.base
                );
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        if !healthy(&self.base).await {
            anyhow::bail!("rustxng did not answer on {}", self.base);
        }
        Ok(())
    }

    async fn search(self: Arc<Self>, args: sasacode_ai::JsonObject, signal: CancellationToken) -> ToolResult {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
        if js_trim(&query).is_empty() {
            return error_result("The query is empty.");
        }
        if let Err(e) = self.ensure(&signal).await {
            return error_result(format!("web_search is unavailable: {e}"));
        }
        let n = args.get("max_results").and_then(Value::as_f64).unwrap_or(self.count);
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &query)
            .append_pair("format", "json")
            .append_pair("max_results", &js_num_str(n))
            .finish();
        let req = sasacode_ai::http::client().get(format!("{}/search?{params}", self.base));
        let res = match send_within(req, &signal, Duration::from_secs(30)).await {
            Ok(res) => res,
            Err(e) => return error_result(format!("Search failed: {e}")),
        };
        if !res.status().is_success() {
            let status = res.status();
            return error_result(format!(
                "Search failed: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }
        let mut data = match res.json::<Value>().await {
            Ok(v) => SearxResponse::from_value(&v),
            Err(e) => return error_result(format!("Search failed: {e}")),
        };
        data.results.truncate(n.max(0.0) as usize); // SearXNG itself has no max_results
        if args.get("include_text").and_then(Value::as_bool) == Some(true) {
            // Read here rather than by the server: its page text keeps menus, inline CSS and scripts.
            let mut chosen: Vec<usize> = Vec::new();
            for i in 0..data.results.len() {
                if chosen.len() as f64 >= self.text_pages || data.results[i].url.as_deref().is_none_or(str::is_empty) {
                    break;
                }
                if matches_query(&query, &data.results[i], i == 0) {
                    chosen.push(i);
                } else {
                    data.results[i].text_note =
                        Some("page not read: its title and snippet share little with the query".into());
                }
            }
            let reads = chosen.iter().map(|&i| {
                let url = data.results[i].url.clone().unwrap_or_default();
                let signal = signal.clone();
                let any_host = self.private_pages;
                async move { (i, page_text(&url, &signal, any_host).await) }
            });
            for (i, read) in futures::future::join_all(reads).await {
                let r = &mut data.results[i];
                match read {
                    Ok(text) if text.is_empty() => r.text_note = Some("page read, but it has no text".into()),
                    Ok(text) => r.text = Some(text),
                    Err(e) => r.text_note = Some(format!("page could not be read: {e}")),
                }
            }
        }
        ToolResult::text(format_results(&query, &data))
    }

    /// Offered when there is something to search with and browsr's search is not connected.
    fn sync(&self) {
        {
            let mut command = lock(&self.command);
            if command.is_none() {
                *command = self.find(); // installed meanwhile (sasacode web-search install from a shell)
            }
        }
        let browsr = self
            .api
            .agent()
            .tools()
            .iter()
            .any(|t| t.name() == "search" || t.name() == "web");
        let offer = (self.url.is_some() || self.command().is_some()) && !browsr;
        let mut registered = lock(&self.registered);
        if offer && !*registered {
            if let Some(tool) = lock(&self.tool).clone() {
                self.api.register_tool(tool);
            }
        } else if !offer && *registered {
            self.api.unregister_tool("web_search");
        }
        *registered = offer;
    }

    async fn run_command(&self, args: &str) {
        let words = js_words(args);
        let sub = words.first().copied().unwrap_or("");
        let ui = self.api.ui();
        if sub == "install" {
            let log_api = self.api.clone();
            let opts = InstallOptions {
                log: Some(Arc::new(move |m: &str| log_api.ui().notify(m, NotifyLevel::Info))),
                ..InstallOptions::default()
            };
            match install_rustxng(opts).await {
                Ok(done) => {
                    *lock(&self.command) = self.find();
                    self.sync();
                    let command = self.command();
                    let message = if command.as_deref() == Some(done.path.as_str()) {
                        t!(
                            "{version} を {path} に入れました。web_search が使えます。",
                            version = done.version,
                            path = done.path
                        )
                    } else {
                        t!(
                            "{version} を {path} に入れました（ただし web_search は {command} を使います）。",
                            version = done.version,
                            path = done.path,
                            command = command.unwrap_or_default()
                        )
                    };
                    ui.notify(&message, NotifyLevel::Info);
                }
                Err(e) => ui.notify(
                    &t!("RustXNG を入れられませんでした: {error}", error = e),
                    NotifyLevel::Error,
                ),
            }
            return;
        }
        let command = self.command();
        if self.url.is_none() && command.is_none() {
            let message = match &self.configured {
                Some(c) => t!("web-search: {command} が見つかりません", command = c),
                None => t!(
                    "rustxng が見つかりません。/web-search install（または sasacode web-search install）で入れるか、{releases} から入れて PATH に置いてください。",
                    releases = RUSTXNG_RELEASES
                ),
            };
            return ui.notify(&message, NotifyLevel::Info);
        }
        let up = healthy(&self.base).await;
        let message = match &self.url {
            Some(url) => format!(
                "web_search: {url} · {}",
                if up { t!("応答あり") } else { t!("応答なし") }
            ),
            None => format!(
                "web_search: {} · {} · {}",
                command.unwrap_or_default(),
                self.base,
                if up {
                    t!("起動中")
                } else {
                    t!(
                        "停止中（次の検索で起動、{idle} 秒使わなければ終了）",
                        idle = js_num_str(self.idle)
                    )
                }
            ),
        };
        ui.notify(&message, NotifyLevel::Info);
    }
}

/// web_search: nothing stays resident: the first search starts `rustxng` as a sidecar on 127.0.0.1
/// that exits by itself after RUSTXNG_IDLE_TIMEOUT seconds without a request, and sessions that run
/// meanwhile share it. With `url` set, that server (RustXNG or SearXNG) is used and nothing is
/// started. While browsr's search tools are connected, web_search steps aside so the model is not
/// given two ways to search. Inactive until rustxng is installed (on PATH, or by `/web-search
/// install` / `sasacode web-search install` into ~/.sasacode/bin) or a url is set; an install while
/// running takes effect at once.
///
/// include_text reads result pages on public addresses only; `privatePages: true` lifts that (an
/// intranet search server whose results are internal pages).
///
/// settings: { command?: string, url?: string, port?: number, idleTimeout?: number, results?: number, textPages?: number, privatePages?: boolean }
pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let url = setting_str(&settings, "url")
            .map(|u| u.trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        let configured = setting_str(&settings, "command").filter(|c| !c.is_empty());
        let base = url.clone().unwrap_or_else(|| {
            format!(
                "http://127.0.0.1:{}",
                js_num_str(setting_number(&settings, "port", DEFAULT_PORT))
            )
        });
        let text_pages = setting_number(&settings, "textPages", DEFAULT_TEXT_PAGES);
        let text_pages = if text_pages.is_nan() || text_pages == 0.0 {
            DEFAULT_TEXT_PAGES
        } else {
            text_pages
        };
        let ws = Arc::new(WebSearch {
            api: api.clone(),
            url,
            configured,
            command: Mutex::new(None),
            base,
            idle: setting_number(&settings, "idleTimeout", DEFAULT_IDLE),
            count: setting_number(&settings, "results", DEFAULT_RESULTS),
            private_pages: settings.get("privatePages").and_then(Value::as_bool) == Some(true),
            text_pages: text_pages.max(1.0),
            registered: Mutex::new(false),
            starting: Mutex::new(None),
            tool: Mutex::new(None),
        });
        *lock(&ws.command) = ws.find();

        let run = ws.clone();
        let command = Command::new(
            "web-search",
            &t!("web_search（RustXNG）の状態。install で RustXNG を入れる"),
            move |CommandContext { args }| {
                let ws = run.clone();
                async move {
                    ws.run_command(&args).await;
                    Ok(())
                }
            },
        )
        .hint("[install]")
        .complete(|prefix: String| async move {
            if "install".starts_with(&prefix) {
                vec![SelectOption::new("install", t!("RustXNG を入れる・更新する"))]
            } else {
                Vec::new()
            }
        });
        api.register_command(command);
        if ws.url.is_none()
            && ws.command().is_none()
            && let Some(c) = &ws.configured
        {
            api.ui().notify(
                &t!("web-search: {command} が見つかりません", command = c),
                NotifyLevel::Warning,
            );
        }

        let exec = ws.clone();
        let tool = ToolDef::new(
            "web_search",
            "Search the web. Use it to find the right URL instead of guessing one, then read the page with web_fetch. Returns titles, URLs and snippets; include_text also reads the main text of the first results that match the query (slower).",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search words" },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 20, "description": format!("Number of results (default {})", js_num_str(ws.count)) },
                    "include_text": { "type": "boolean", "description": format!("Also read the main text of the first {} results that match the query", js_num_str(ws.text_pages)) },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
            ToolKind::Other,
            move |args, ctx| {
                let ws = exec.clone();
                async move { Ok(ws.search(args, ctx.signal).await) }
            },
        )
        .concurrent(true)
        .match_target(|a| a.get("query").and_then(Value::as_str).unwrap_or("").to_string())
        .summary(|a| a.get("query").and_then(Value::as_str).unwrap_or("").to_string());
        *lock(&ws.tool) = Some(tool.into_ref());
        ws.sync();
        // Checked before each request, as browsr connects in the background and can drop.
        let hooked = ws.clone();
        api.on::<hook::SystemPrompt, _, _>(move |_event, _ctx| {
            hooked.sync();
            async { Ok(None) }
        });
        Ok(())
    })
}

/// [`format_results`] of a plain JSON answer, for callers holding the server's JSON.
pub fn format_json(query: &str, data: &Value) -> String {
    format_results(query, &SearxResponse::from_value(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    // bundled.test.ts: "privateAddress: loopback, private, link-local and the like, IPv4 and IPv6"
    #[test]
    fn private_address_loopback_private_link_local_and_the_like_ipv4_and_ipv6() {
        let internal = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            // The same addresses written in hexadecimal: the URL parser keeps them as written, so they
            // must be judged by the IPv4 they mean (#55).
            "::ffff:7f00:1",
            "::ffff:7f00:0001",
            "0:0:0:0:0:ffff:7f00:1",
            "::ffff:a00:1",
            "::ffff:a9fe:a9fe",
            "::ffff:6440:0",
            "::ffff:c0a8:1",
            "::ffff:0:0",
        ];
        for ip in internal {
            assert_eq!((ip, private_address(ip)), (ip, true));
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "1.1.1.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "::ffff:100:63",
            "::ffff:a9fd:1",
            "::ffff:c0a7:1",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, false));
        }
        // NAT64 (#83): the well-known prefix is judged by the IPv4 it carries, the local-use one is the
        // site's own; 6to4 likewise.
        for ip in [
            "64:ff9b::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::c0a8:101",
            "64:ff9b:1::8.8.8.8",
            "2002:7f00:1::1",
            "2002:a00:1::",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, true));
        }
        for ip in [
            "64:ff9b::8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "64:ff9b:2::7f00:1",
        ] {
            assert_eq!((ip, private_address(ip)), (ip, false));
        }
    }

    #[test]
    fn private_address_accepts_a_zone_and_judges_upper_case() {
        assert!(private_address("fe80::1%eth0"));
        assert!(private_address("::FFFF:7F00:1"));
        assert!(!private_address("2606:4700::1111%en0"));
    }
}
