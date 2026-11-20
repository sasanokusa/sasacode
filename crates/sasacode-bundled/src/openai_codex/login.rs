//! `sasacode login` (`openai-codex/login.ts`): sign in with a ChatGPT plan the way OpenAI's Codex
//! CLI does — PKCE (S256), Codex's public client, and a callback on localhost:1455 (the redirect
//! registered for that client, so no other port works). When the browser cannot reach this machine
//! (SSH) or the port is taken, the URL the browser ends up on can be pasted instead.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use futures::future::BoxFuture;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use sasacode_ai::Fetch;

use super::auth::{
    CODEX_AUTHORIZE_URL, CODEX_CLIENT_ID, CODEX_SCOPES, clear_codex_auth, codex_auth_path, plan_type,
    read_codex_auth_at, token_request, tokens_from, write_codex_auth,
};
use crate::net_util::{js_round, js_trim, utf16_prefix};

const PORT: u16 = 1455;
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

/// Asks the user for the pasted URL or code; cancelled once the browser has come back.
pub type PromptFn = Arc<dyn Fn(String, CancellationToken) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;

/// Takes a line of output, or the URL to open.
pub type LineFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What [`codex_login_with`] uses instead of the terminal, the browser and the network (tests).
#[derive(Clone, Default)]
pub struct LoginDeps {
    pub fetch: Option<Arc<dyn Fetch>>,
    pub prompt: Option<PromptFn>,
    pub out: Option<LineFn>,
    pub open_browser: Option<LineFn>,
    /// Where credentials go (tests).
    pub path: Option<PathBuf>,
    /// Skip the callback server (tests).
    pub no_server: bool,
}

pub fn authorize_url(challenge: &str, state: &str) -> String {
    let mut url = match url::Url::parse(CODEX_AUTHORIZE_URL) {
        Ok(url) => url,
        // INFALLIBLE: a fixed, valid URL.
        Err(_) => return CODEX_AUTHORIZE_URL.to_string(),
    };
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", CODEX_CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", CODEX_SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "sasacode"),
    ]);
    url.to_string()
}

fn query(url: &url::Url, key: &str) -> Option<String> {
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

/// A bare code, or the whole URL the browser landed on (then its state must match).
pub fn parse_callback(input: &str, state: &str) -> anyhow::Result<Option<String>> {
    let text = js_trim(input);
    if text.is_empty() {
        return Ok(None);
    }
    if !(text.starts_with("http://") || text.starts_with("https://")) {
        let code = text.split('#').next().unwrap_or("");
        return Ok((!code.is_empty()).then(|| code.to_string()));
    }
    let url = url::Url::parse(text).map_err(|_| anyhow::anyhow!("Invalid URL"))?;
    if let Some(got) = query(&url, "state")
        && !got.is_empty()
        && got != state
    {
        anyhow::bail!("login failed: the pasted URL belongs to another login attempt (state mismatch)");
    }
    Ok(query(&url, "code"))
}

/// The callback server: each code (or failure) the browser brings, and the task to stop.
struct CallbackServer {
    codes: tokio::sync::mpsc::UnboundedReceiver<Result<String, String>>,
    task: tokio::task::JoinHandle<()>,
}

/// Write a whole HTTP/1.1 response and close.
async fn respond(stream: &mut tokio::net::TcpStream, status: u16, page: Option<&str>) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = page
        .map(|text| format!("<p style=\"font:16px system-ui;margin:3em\">{text}</p>"))
        .unwrap_or_default();
    let kind = if page.is_some() {
        "content-type: text/html; charset=utf-8\r\n"
    } else {
        ""
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n{kind}content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Answer one request: the code when it is the browser coming back for this login.
async fn serve(mut stream: tokio::net::TcpStream, state: &str) -> Option<Result<String, String>> {
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 64 * 1024 {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/");
    let base = url::Url::parse(REDIRECT_URI).ok()?;
    let Ok(url) = base.join(target) else {
        respond(&mut stream, 404, None).await;
        return None;
    };
    if url.path() != "/auth/callback" {
        respond(&mut stream, 404, None).await;
        return None;
    }
    // Whatever the page says, it counts only for this login: its state is checked first.
    if query(&url, "state").as_deref() != Some(state) {
        respond(&mut stream, 400, Some("sasacode: このログインの応答ではありません。")).await;
        return None;
    }
    if let Some(error) = query(&url, "error").filter(|e| !e.is_empty()) {
        respond(
            &mut stream,
            400,
            Some("sasacode: ログインできませんでした。ターミナルを確認してください。"),
        )
        .await;
        let description = query(&url, "error_description").unwrap_or_default();
        return Some(Err(js_trim(&format!("login failed: {error} {description}")).to_string()));
    }
    match query(&url, "code").filter(|c| !c.is_empty()) {
        Some(code) => {
            respond(
                &mut stream,
                200,
                Some("sasacode: ログインしました。このタブは閉じてかまいません。"),
            )
            .await;
            Some(Ok(code))
        }
        None => {
            respond(&mut stream, 400, Some("sasacode: このログインの応答ではありません。")).await;
            None
        }
    }
}

/// Listen for the browser on 127.0.0.1:1455; None when the port is taken.
async fn callback_server(state: String) -> Option<CallbackServer> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await.ok()?;
    let (tx, codes) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(state);
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            let state = state.clone();
            tokio::spawn(async move {
                if let Some(code) = serve(stream, &state).await {
                    let _ = tx.send(code);
                }
            });
        }
    });
    Some(CallbackServer { codes, task })
}

/// Read a line from the terminal after printing `question`; cancelled, it stops waiting (the read
/// itself goes on in its thread until the process ends).
fn ask(question: String, signal: CancellationToken) -> BoxFuture<'static, anyhow::Result<String>> {
    Box::pin(async move {
        print!("{question}");
        let _ = std::io::stdout().flush();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = std::io::stdin()
                .read_line(&mut line)
                .map(|_| line.trim_end_matches(['\r', '\n']).to_string());
            let _ = tx.send(read);
        });
        tokio::select! {
            _ = signal.cancelled() => anyhow::bail!("The operation was aborted."),
            line = rx => Ok(line.map_err(|_| anyhow::anyhow!("The operation was aborted."))??),
        }
    })
}

fn open_in_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = tokio::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// `sasacode login`, on the terminal and in the browser.
pub async fn codex_login() -> anyhow::Result<()> {
    codex_login_with(LoginDeps::default()).await
}

/// [`codex_login`] with its terminal, browser, network and credentials file replaced.
pub async fn codex_login_with(deps: LoginDeps) -> anyhow::Result<()> {
    let out: LineFn = deps.out.clone().unwrap_or_else(|| Arc::new(|s: &str| println!("{s}")));
    let prompt: PromptFn = deps.prompt.clone().unwrap_or_else(|| Arc::new(ask));
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let verifier = b64.encode(random_bytes::<32>());
    let challenge = b64.encode(Sha256::digest(verifier.as_bytes()));
    let state: String = random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect();
    let url = authorize_url(&challenge, &state);

    let cb = if deps.no_server {
        None
    } else {
        callback_server(state.clone()).await
    };
    out("Sign in with your ChatGPT account in the browser:");
    out(&format!("  {url}"));
    match &deps.open_browser {
        Some(open) => open(&url),
        None => open_in_browser(&url),
    }
    let stop_asking = CancellationToken::new();
    let code: anyhow::Result<Option<String>> = match cb {
        Some(mut cb) => {
            out(&format!("Waiting for the browser ({REDIRECT_URI})."));
            out("If it cannot reach this machine (e.g. over SSH), paste the URL it ends up on here and press enter.");
            // Whichever comes first: the callback, or a pasted URL. An empty line keeps waiting.
            let pasted = {
                let state = state.clone();
                let answer = prompt("> ".into(), stop_asking.clone());
                async move { parse_callback(&answer.await?, &state) }
            };
            let from_browser = |c: Option<Result<String, String>>| match c {
                Some(Ok(code)) => Ok(Some(code)),
                Some(Err(e)) => Err(anyhow::Error::msg(e)),
                None => Ok(None),
            };
            let code = tokio::select! {
                c = cb.codes.recv() => from_browser(c),
                p = pasted => match p {
                    Ok(Some(code)) => Ok(Some(code)),
                    Ok(None) => from_browser(cb.codes.recv().await),
                    Err(e) => Err(e),
                },
            };
            stop_asking.cancel();
            cb.task.abort();
            code
        }
        None => {
            out(&format!(
                "Port {PORT} is in use, so the browser cannot come back here: after signing in, paste the URL it ends up on."
            ));
            let answer = prompt("> ".into(), stop_asking.clone()).await;
            stop_asking.cancel();
            answer.and_then(|a| parse_callback(&a, &state))
        }
    };
    let Some(code) = code?.filter(|c| !c.is_empty()) else {
        anyhow::bail!("login cancelled: no authorization code");
    };

    let req = token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CODEX_CLIENT_ID),
        ("code", &code),
        ("code_verifier", &verifier),
        ("redirect_uri", REDIRECT_URI),
    ])?;
    let res = sasacode_ai::send(deps.fetch.as_ref(), req).await?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let text = res.text().await?;
        anyhow::bail!(
            "login failed: the token exchange returned {status}: {}",
            utf16_prefix(&text, 200)
        );
    }
    let data: Value = res.json().await?;
    let tokens = tokens_from(&data, None)?;
    let path = deps.path.clone().unwrap_or_else(codex_auth_path);
    write_codex_auth(&tokens, &path)?;
    let plan = plan_type(tokens.id_token.as_deref())
        .map(|p| format!(" (ChatGPT {p})"))
        .unwrap_or_default();
    out(&format!("Signed in{plan}. Credentials: {}", path.display()));
    out("Use it with: sasacode -m openai-codex/gpt-5.5   (or pick one in /model)");
    Ok(())
}

/// `sasacode logout`: remove the stored login.
pub fn codex_logout() -> String {
    codex_logout_at(&codex_auth_path())
}

/// [`codex_logout`] of another credentials file.
pub fn codex_logout_at(path: &std::path::Path) -> String {
    if clear_codex_auth(path) {
        "Signed out (local credentials removed)."
    } else {
        "Not signed in."
    }
    .to_string()
}

/// What `sasacode login status` says, and whether there is a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexStatus {
    pub ok: bool,
    pub text: String,
}

pub fn codex_status() -> CodexStatus {
    codex_status_at(&codex_auth_path(), sasacode_ai::now_ms() as f64)
}

/// [`codex_status`] of another credentials file, at `now` (epoch ms).
pub fn codex_status_at(path: &std::path::Path, now: f64) -> CodexStatus {
    let Some(t) = read_codex_auth_at(path) else {
        return CodexStatus {
            ok: false,
            text: "Not signed in to a ChatGPT plan. Run: sasacode login".into(),
        };
    };
    let plan = plan_type(t.id_token.as_deref())
        .map(|p| format!(" (ChatGPT {p})"))
        .unwrap_or_default();
    let mins = t
        .expires_at
        .filter(|at| *at != 0.0 && !at.is_nan())
        .map(|at| js_round((at - now) / 60_000.0));
    let left = match mins {
        Some(m) if m > 0.0 => format!(
            " Access token: {} min left (refreshed automatically).",
            crate::net_util::js_num_str(m)
        ),
        Some(_) => " Access token: expired (refreshed automatically).".to_string(),
        None => String::new(),
    };
    CodexStatus {
        ok: true,
        text: format!("Signed in{plan}.{left}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `serve` makes of one request for `target`.
    async fn answer(target: &str) -> Option<Result<String, String>> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let request = format!("GET {target} HTTP/1.1\r\nhost: localhost\r\n\r\n");
        let client = tokio::spawn(async move {
            let mut c = tokio::net::TcpStream::connect(addr).await.expect("connect");
            c.write_all(request.as_bytes()).await.expect("write");
            let mut reply = Vec::new();
            let _ = c.read_to_end(&mut reply).await;
        });
        let (stream, _) = listener.accept().await.expect("accept");
        let out = serve(stream, "mine").await;
        let _ = client.await;
        out
    }

    #[tokio::test]
    async fn only_a_response_carrying_this_logins_state_counts_error_or_code() {
        assert_eq!(
            answer("/auth/callback?code=c1&state=mine").await,
            Some(Ok("c1".to_string()))
        );
        assert_eq!(answer("/auth/callback?code=c1&state=other").await, None);
        assert_eq!(answer("/auth/callback?code=c1").await, None);
        // An error ends the login only when it is this login's.
        assert_eq!(answer("/auth/callback?error=access_denied&state=other").await, None);
        assert_eq!(answer("/auth/callback?error=access_denied").await, None);
        let failed = answer("/auth/callback?error=access_denied&error_description=no&state=mine").await;
        assert_eq!(failed, Some(Err("login failed: access_denied no".to_string())));
    }
}
