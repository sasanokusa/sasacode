//! image-attach (`packages/bundled/src/image-attach.ts`): images in a prompt. `@path` (any path)
//! and a dropped absolute path become images after the text; ⌘V / ctrl+v of a screenshot and a drop
//! stand in the input as `[Image #n]`; `/image` queues one for the next message.
//!
//! Offsets in [`Ref`] are byte offsets into the string (UTF-16 units in the TS).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Value, json};

use sasacode_ai::{ImageContent, UserContent};
use sasacode_host::t;
use sasacode_plugin_api::{Command, CommandContext, NotifyLevel, PluginApi, UserPromptResult, home_dir, hook};

use crate::net_util::js_trim;

/// The providers' limit for one image (Anthropic's is 5 MB); a larger one would fail every request after it.
const MAX_BYTES: u64 = 5_000_000;

// `@path`, a quoted path, or a bare word; a bare word may carry `\ `-escaped spaces (what terminals paste on a drop).
// Only ASCII whitespace ends a word: macOS names screenshots "… 12.34.56\u202fPM.png". A '…' quote starts a word and
// ends one: between the apostrophes of "Here's … what's" a path was taken for quoted text (#123). Japanese may follow it (#132).
static TOKEN: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern. JS's \s (with U+FEFF) and its ASCII \w spelled out.
    fancy_regex::Regex::new(
        r#"(@)?(?:"([^"\n]+)"|(?:^|(?<=[\s\x{feff}@]))'([^'\n]+)'(?![A-Za-z0-9_'"])|((?:\\[^\n\r\x{2028}\x{2029}]|[^ \t\r\n"'\\])+))"#,
    )
    .expect("fixed pattern")
});
static QUOTED_IMAGE: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"(?i)\.(png|jpe?g|gif|webp)$").expect("fixed pattern")
});
static BARE_IMAGE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    fancy_regex::Regex::new(r"(?i)^([^\n\r\x{2028}\x{2029}]*?\.(?:png|jpe?g|gif|webp))(?![A-Za-z0-9_])")
        .expect("fixed pattern")
});
static ESCAPED: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"\\([^\n\r\x{2028}\x{2029}])").expect("fixed pattern")
});
/// A Windows absolute path (C:\x, C:/x, \\server\share): its `\` are separators, not escapes.
static WINDOWS_ABS: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"^(?:[A-Za-z]:[\\/]|\\\\[^\\])").expect("fixed pattern")
});
static WORDS: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r#""([^"]*)"|'([^']*)'|((?:\\[^\n\r\x{2028}\x{2029}]|[^ \t\r\n])+)"#).expect("fixed pattern")
});
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"\[Image #(\d+)\]").expect("fixed pattern")
});
static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern.
    Regex::new(r"\[Image \d+: [^\]\n]*\]").expect("fixed pattern")
});

const WINDOWS: bool = cfg!(windows);

/// What a dropped file pastes: an absolute path, `~/` or a file:// URL (on Windows also C:\… and ~\).
fn dropped(raw: &str) -> bool {
    if WINDOWS {
        raw.starts_with('/')
            || raw.starts_with("~/")
            || raw.starts_with("~\\")
            || raw.starts_with("file://")
            || raw.starts_with("\\\\")
            || WINDOWS_ABS.is_match(raw)
    } else {
        raw.starts_with('/') || raw.starts_with("~/") || raw.starts_with("file://")
    }
}

/// `\ ` and the like unescaped, except in a Windows path (terminals there quote a path with spaces instead).
fn unescape(w: &str) -> String {
    if WINDOWS && WINDOWS_ABS.is_match(w) {
        return w.to_string();
    }
    ESCAPED.replace_all(w, "$1").into_owned()
}

/// An image path in a prompt; `start`..`end` in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub start: usize,
    pub end: usize,
    pub path: String,
    pub explicit: bool,
}

/// Image paths in a prompt. `@path` (any path) is explicit; without the `@` only an absolute, `~/`
/// or `file://` path counts, which is what dropping a file into the terminal pastes.
pub fn find_refs(s: &str) -> Vec<Ref> {
    let mut refs = Vec::new();
    for m in TOKEN.captures_iter(s) {
        let Ok(m) = m else { break };
        let Some(whole) = m.get(0) else { continue };
        let explicit = m.get(1).is_some();
        let mut end = whole.end();
        let quoted = m.get(2).or_else(|| m.get(3)).map(|q| q.as_str());
        let raw = match quoted {
            Some(q) => QUOTED_IMAGE.is_match(q).then(|| q.to_string()),
            None => {
                // Japanese text often has no space after a path: stop at the extension.
                let word = m.get(4).map(|w| w.as_str()).unwrap_or("");
                match BARE_IMAGE.captures(word).ok().flatten().and_then(|c| c.get(1)) {
                    Some(e) => {
                        end = whole.start() + usize::from(explicit) + e.as_str().len();
                        Some(unescape(e.as_str()))
                    }
                    None => None,
                }
            }
        };
        let Some(raw) = raw else { continue };
        if !explicit && !dropped(&raw) {
            continue;
        }
        refs.push(Ref {
            start: whole.start(),
            end,
            path: raw,
            explicit,
        });
    }
    refs
}

/// `path.resolve(cwd, p)`: absolute, `.` and `..` worked out by name.
fn resolve(cwd: &Path, p: &Path) -> PathBuf {
    let joined = cwd.join(p);
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Where an image path points, from `cwd`: `file://` URLs and `~` worked out.
pub fn resolve_path(p: &str, cwd: &Path) -> PathBuf {
    resolve_path_in(p, cwd, &home_dir())
}

/// [`resolve_path`] with another home folder.
pub fn resolve_path_in(p: &str, cwd: &Path, home: &Path) -> PathBuf {
    let mut p = p.to_string();
    // A URL with a host (file://nas/a.png) cannot be a path here: kept as written, it is a file that is not found,
    // where the exception lost every other image of the message (#123).
    if p.starts_with("file://")
        && let Ok(url) = url::Url::parse(&p)
        && matches!(url.host_str(), None | Some("") | Some("localhost"))
        && let Ok(path) = url.to_file_path()
    {
        p = path.display().to_string();
    }
    if p == "~" || p.starts_with("~/") || (WINDOWS && p.starts_with("~\\")) {
        let rest = p[1..].trim_start_matches(['/', '\\']).to_string();
        return resolve(cwd, &home.join(rest));
    }
    resolve(cwd, Path::new(&p))
}

/// The type from the bytes, not the name: a wrong media type would fail every later request.
pub fn sniff(b: &[u8]) -> Option<&'static str> {
    let at = |i: usize, s: &[u8]| b.get(i..i + s.len()) == Some(s);
    if b.first() == Some(&0x89) && at(1, b"PNG") {
        return Some("image/png");
    }
    if b.len() >= 3 && b[0] == 0xff && b[1] == 0xd8 && b[2] == 0xff {
        return Some("image/jpeg");
    }
    if at(0, b"GIF8") {
        return Some("image/gif");
    }
    if at(0, b"RIFF") && at(8, b"WEBP") {
        return Some("image/webp");
    }
    None
}

fn too_big(size: u64) -> String {
    format!("{size} bytes, over the {MAX_BYTES} an image may have (make a smaller copy, e.g. sips -Z 1600)")
}

pub fn to_image(buf: &[u8]) -> anyhow::Result<ImageContent> {
    let Some(media_type) = sniff(buf) else {
        anyhow::bail!("not a PNG, JPEG, GIF or WebP image");
    };
    if buf.len() as u64 > MAX_BYTES {
        anyhow::bail!(too_big(buf.len() as u64));
    }
    Ok(ImageContent {
        media_type: media_type.to_string(),
        data: Arc::from(base64::engine::general_purpose::STANDARD.encode(buf)),
    })
}

pub async fn load_image(path: &Path) -> anyhow::Result<ImageContent> {
    let st = tokio::fs::metadata(path).await?;
    if !st.is_file() {
        anyhow::bail!("not a file");
    }
    if st.len() > MAX_BYTES {
        anyhow::bail!(too_big(st.len()));
    }
    to_image(&tokio::fs::read(path).await?)
}

/// Run a program and return its stdout (`execFile`, replaced in tests).
pub type Run = Arc<dyn Fn(String, Vec<String>) -> BoxFuture<'static, anyhow::Result<Vec<u8>>> + Send + Sync>;

/// `execFile` with a 10 s timeout: errors read as Node's ("Command failed: …", "spawn … ENOENT").
fn run_program(cmd: String, args: Vec<String>) -> BoxFuture<'static, anyhow::Result<Vec<u8>>> {
    Box::pin(async move {
        let child = tokio::process::Command::new(&cmd)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = match tokio::time::timeout(Duration::from_secs(10), child).await {
            Err(_) => anyhow::bail!("Command failed: {cmd} {}", args.join(" ")),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => anyhow::bail!("spawn {cmd} ENOENT"),
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(out)) => out,
        };
        if !out.status.success() {
            anyhow::bail!(
                "Command failed: {cmd} {}\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(out.stdout)
    })
}

/// A fresh private folder under the system temp folder (`mkdtemp`).
fn temp_dir(prefix: &str) -> anyhow::Result<PathBuf> {
    use rand::Rng;
    let tmp = std::env::temp_dir();
    for _ in 0..100 {
        let suffix: String = (0..6)
            .map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[rand::rng().random_range(0..36)]))
            .collect();
        let dir = tmp.join(format!("{prefix}{suffix}"));
        if std::fs::create_dir(&dir).is_ok() {
            return Ok(dir);
        }
    }
    anyhow::bail!("cannot create a temporary folder in {}", tmp.display())
}

/// The image on the system clipboard of this machine.
pub async fn clipboard_image() -> anyhow::Result<ImageContent> {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    clipboard_image_on(platform, Arc::new(run_program)).await
}

/// The image on the system clipboard: macOS via osascript, Windows via PowerShell, Linux via
/// wl-paste or xclip (`platform` as Node names it).
pub async fn clipboard_image_on(platform: &str, exec: Run) -> anyhow::Result<ImageContent> {
    if platform == "darwin" || platform == "win32" {
        let dir = temp_dir("sasacode-clip-")?;
        let file = dir.join("clip.png");
        let shown = file.display().to_string();
        let (cmd, args, allowed): (&str, Vec<String>, &[&str]) = if platform == "darwin" {
            let path = shown.replace('\\', "\\\\").replace('"', "\\\"");
            let script = [
                "try".to_string(),
                "set d to the clipboard as «class PNGf»".into(),
                "on error".into(),
                "error \"no image\"".into(),
                "end try".into(),
                format!("set fh to open for access POSIX file \"{path}\" with write permission"),
                "set eof of fh to 0".into(),
                "write d to fh".into(),
                "close access fh".into(),
            ];
            (
                "osascript",
                script.into_iter().flat_map(|l| ["-e".to_string(), l]).collect(),
                &["no image", "Command failed"],
            )
        } else {
            // The clipboard needs a single-threaded apartment (-STA); '' is a ' inside a PowerShell string.
            let script = [
                "Add-Type -AssemblyName System.Windows.Forms, System.Drawing".to_string(),
                "$i = [System.Windows.Forms.Clipboard]::GetImage()".into(),
                "if ($i -eq $null) { [Console]::Error.WriteLine('no image'); exit 1 }".into(),
                format!(
                    "$i.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)",
                    shown.replace('\'', "''")
                ),
            ]
            .join("; ");
            (
                "powershell.exe",
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-STA".into(),
                    "-Command".into(),
                    script,
                ],
                &["no image", "Command failed", "ENOENT"],
            )
        };
        let result = async {
            exec(cmd.to_string(), args).await?;
            load_image(&file).await
        }
        .await;
        let _ = std::fs::remove_dir_all(&dir);
        return result.map_err(|e| {
            let message = e.to_string();
            if allowed.iter().any(|a| message.contains(a)) {
                anyhow::anyhow!("the clipboard has no image")
            } else {
                e
            }
        });
    }
    if platform == "linux" {
        let tries: [(&str, &[&str]); 2] = [
            ("wl-paste", &["--type", "image/png"]),
            ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"]),
        ];
        for (cmd, args) in tries {
            if let Ok(out) = exec(cmd.to_string(), args.iter().map(|a| a.to_string()).collect()).await
                && !out.is_empty()
            {
                return to_image(&out);
            }
        }
        anyhow::bail!("the clipboard has no image (wl-paste or xclip is needed)");
    }
    anyhow::bail!("the clipboard is not supported on {platform}")
}

/// Words of a command line, honoring quotes and `\ `.
pub fn words(s: &str) -> Vec<String> {
    WORDS
        .captures_iter(s)
        .map(|m| match (m.get(1), m.get(2), m.get(3)) {
            (Some(a), _, _) => a.as_str().to_string(),
            (None, Some(b), _) => b.as_str().to_string(),
            (None, None, Some(c)) => unescape(c.as_str()),
            _ => String::new(),
        })
        .collect()
}

/// Reads the clipboard image (replaced in tests).
pub type ClipboardFn = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<ImageContent>> + Send + Sync>;

type Named = (String, ImageContent);

#[derive(Default)]
struct State {
    pending: Vec<Named>,
    /// Images pasted or dropped into the input, shown there as [Image #n] until the message is sent.
    inline: IndexMap<u64, Named>,
}

struct Attach {
    api: PluginApi,
    clipboard: ClipboardFn,
    drop_paths: bool,
    max_images: usize,
    state: Mutex<State>,
}

impl Attach {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn status(&self) {
        let n = self.state().pending.len();
        self.api
            .ui()
            .set_status("image-attach", (n > 0).then(|| format!("📎 {n}")));
    }

    fn cannot(&self, name: &str, e: &anyhow::Error, level: NotifyLevel) {
        self.api.ui().notify(
            &t!("画像を付けられません: {name}: {error}", name = name, error = e),
            level,
        );
    }

    /// ⌘V of a screenshot or ctrl+v ("") reads the clipboard; a drop is read now, before a
    /// screenshot's temporary file is gone, and stands in the input as [Image #n].
    async fn paste(&self, text: String, input: String) -> Option<String> {
        // Images whose marker was deleted from the input are dropped; a new one takes the lowest free number.
        let shown: HashSet<u64> = MARKER
            .captures_iter(&input)
            .filter_map(|m| m.get(1).and_then(|n| n.as_str().parse().ok()))
            .collect();
        self.state().inline.retain(|n, _| shown.contains(n));
        let add = |name: String, image: ImageContent| {
            let mut state = self.state();
            let mut n = 1;
            while state.inline.contains_key(&n) || shown.contains(&n) {
                n += 1;
            }
            state.inline.insert(n, (name, image));
            format!("[Image #{n}]")
        };
        if text.is_empty() {
            let full = {
                let s = self.state();
                s.inline.len() + s.pending.len() >= self.max_images
            };
            if full {
                self.api
                    .ui()
                    .notify(&t!("画像は{n}枚までです", n = self.max_images), NotifyLevel::Warning);
                return None;
            }
            return match (self.clipboard)().await {
                Ok(image) => Some(format!("{} ", add("clipboard".into(), image))),
                Err(e) => {
                    self.cannot("clipboard", &e, NotifyLevel::Warning);
                    None
                }
            };
        }
        if !self.drop_paths {
            return None;
        }
        let trimmed = js_trim(&text);
        let refs = find_refs(trimmed);
        // Only a paste that is nothing but image paths; anything else is text to keep.
        let mut rest = trimmed.to_string();
        for r in &refs {
            rest.replace_range(r.start..r.end, &" ".repeat(r.end - r.start));
        }
        if refs.is_empty() || !js_trim(&rest).is_empty() {
            return None;
        }
        let mut loaded: Vec<Named> = Vec::new();
        for r in &refs {
            let held = {
                let s = self.state();
                s.inline.len() + s.pending.len()
            };
            if held + loaded.len() >= self.max_images {
                return None;
            }
            let path = resolve_path(&r.path, &self.api.cwd());
            match load_image(&path).await {
                Ok(image) => loaded.push((path.display().to_string(), image)),
                Err(e) => {
                    if e.to_string().contains("bytes") {
                        self.cannot(&r.path, &e, NotifyLevel::Warning);
                    }
                    return None;
                }
            }
        }
        let markers: Vec<String> = loaded.into_iter().map(|(name, image)| add(name, image)).collect();
        Some(format!("{} ", markers.join(" ")))
    }

    async fn user_prompt(&self, content: Vec<UserContent>) -> Option<Vec<UserContent>> {
        let mut attached: Vec<Named> = Vec::new();
        // Images already in the message (the TUI puts a forked or interrupted one back): numbering goes on after them.
        let base = content.iter().filter(|b| matches!(b, UserContent::Image(_))).count();
        let label = |count: usize, name: &str| format!("[Image {}: {name}]", base + count);
        let mut out: Vec<UserContent> = Vec::new();
        let cwd = self.api.cwd();
        for block in &content {
            let UserContent::Text(block_text) = block else {
                out.push(block.clone());
                continue;
            };
            // Split at [Image #n]: the text between is searched for paths, the markers are images already read.
            let mut parts: Vec<(bool, String)> = Vec::new();
            let mut last = 0;
            for m in MARKER.captures_iter(&block_text.text) {
                let (Some(whole), Some(n)) = (m.get(0), m.get(1)) else {
                    continue;
                };
                parts.push((false, block_text.text[last..whole.start()].to_string()));
                parts.push((true, n.as_str().to_string()));
                last = whole.end();
            }
            parts.push((false, block_text.text[last..].to_string()));
            let mut result = String::new();
            for (marker, part) in parts {
                if marker {
                    let found = part
                        .parse::<u64>()
                        .ok()
                        .and_then(|n| self.state().inline.get(&n).cloned());
                    match found {
                        Some(a) => {
                            let name = a.0.clone();
                            attached.push(a);
                            result += &label(attached.len(), &name);
                        }
                        None => result += &format!("[Image #{part}]"),
                    }
                    continue;
                }
                let mut pos = 0;
                // "[Image 1: /path/a.png]" names an image the message already holds: its path is not one to read again.
                let labels: Vec<(usize, usize)> = LABEL.find_iter(&part).map(|m| (m.start(), m.end())).collect();
                for r in find_refs(&part) {
                    if labels.iter().any(|(s, e)| r.start < *e && r.end > *s) {
                        continue;
                    }
                    if !r.explicit && !self.drop_paths {
                        continue;
                    }
                    if attached.len() + self.state().pending.len() >= self.max_images {
                        break;
                    }
                    let path = resolve_path(&r.path, &cwd);
                    let name = path.display().to_string();
                    match load_image(&path).await {
                        Ok(image) => attached.push((name.clone(), image)),
                        Err(e) => {
                            // A dropped path that is not a file is just text; a typed `@` or an oversize file is worth saying.
                            if r.explicit || e.to_string().contains("bytes") {
                                self.cannot(&r.path, &e, NotifyLevel::Warning);
                            }
                            continue;
                        }
                    }
                    result += &part[pos..r.start];
                    result += &label(attached.len(), &name);
                    pos = r.end;
                }
                result += &part[pos..];
            }
            out.push(UserContent::text(result));
        }
        let pending = {
            let mut s = self.state();
            s.inline.clear();
            std::mem::take(&mut s.pending)
        };
        for p in pending {
            let name = p.0.clone();
            attached.push(p);
            out.push(UserContent::text(label(attached.len(), &name)));
        }
        self.status();
        if attached.is_empty() {
            return None;
        }
        out.extend(attached.into_iter().map(|(_, image)| UserContent::Image(image)));
        Some(out)
    }

    async fn image_command(&self, args: &str) {
        let list = words(args);
        let ui = self.api.ui();
        if list.first().map(String::as_str) == Some("clear") {
            self.state().pending.clear();
            self.status();
            ui.notify(&t!("添付を取り消しました"), NotifyLevel::Info);
            return;
        }
        let asked: Vec<Option<String>> = if list.is_empty() {
            vec![None]
        } else {
            list.into_iter().map(Some).collect()
        };
        for w in asked {
            if self.state().pending.len() >= self.max_images {
                ui.notify(&t!("画像は{n}枚までです", n = self.max_images), NotifyLevel::Warning);
                break;
            }
            let (name, image) = match &w {
                None => ("clipboard".to_string(), (self.clipboard)().await),
                Some(w) => {
                    let path = resolve_path(w, &self.api.cwd());
                    (path.display().to_string(), load_image(&path).await)
                }
            };
            match image {
                Ok(image) => self.state().pending.push((name, image)),
                Err(e) => self.cannot(&name, &e, NotifyLevel::Error),
            }
        }
        self.status();
        let n = self.state().pending.len();
        if n > 0 {
            ui.notify(&t!("画像 {n} 枚を次のメッセージに添付します", n = n), NotifyLevel::Info);
        }
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    plugin_with(api, Arc::new(|| Box::pin(clipboard_image())))
}

/// The plugin with another clipboard reader.
pub fn plugin_with(api: PluginApi, clipboard: ClipboardFn) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.define_settings(
            &json!({
                "type": "object",
                "properties": { "dropPaths": { "type": "boolean" }, "maxImages": { "type": "integer", "minimum": 1, "maximum": 20 } },
                "additionalProperties": false,
            }),
            Some(&json!({ "dropPaths": true, "maxImages": 8 })),
        )?;
        let attach = Arc::new(Attach {
            api: api.clone(),
            clipboard,
            drop_paths: settings.get("dropPaths").and_then(Value::as_bool).unwrap_or(true),
            max_images: settings.get("maxImages").and_then(Value::as_u64).unwrap_or(8) as usize,
            state: Mutex::new(State::default()),
        });

        let paster = attach.clone();
        api.ui().on_paste(Arc::new(move |text: String, input: String| {
            let attach = paster.clone();
            Box::pin(async move { attach.paste(text, input).await })
        }));

        let prompter = attach.clone();
        api.on::<hook::UserPrompt, _, _>(move |e, _ctx| {
            let attach = prompter.clone();
            async move {
                Ok(attach.user_prompt(e.content).await.map(|content| UserPromptResult {
                    content: Some(content),
                    handled: false,
                }))
            }
        });

        let runner = attach.clone();
        api.register_command(
            Command::new(
                "image",
                &t!("画像を次のメッセージに添付（引数なし: クリップボード、clear: 取り消し）"),
                move |CommandContext { args }| {
                    let attach = runner.clone();
                    async move {
                        attach.image_command(&args).await;
                        Ok(())
                    }
                },
            )
            .hint("[path… | clear]"),
        );

        let ender = attach.clone();
        api.on::<hook::SessionEnd, _, _>(move |_e, _ctx| {
            {
                let mut s = ender.state();
                s.pending.clear();
                s.inline.clear();
            }
            ender.status();
            async { Ok(None) }
        });
        Ok(())
    })
}
