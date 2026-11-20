//! syntax-check: fast syntax verification right after `edit`, `write`, or `hash_edit` succeeds,
//! appending an immediate `[syntax-check]` diagnostic so broken edits are fixed on the next turn.
//!
//! Only what an edit broke is reported: a file that already failed before the call (JSX in `.js`
//! without bun, a newer Python than the interpreter, JSON with comments) stays silent, until this
//! plugin has reported it once: from then on it is reported until it parses again.
//!
//! The external checkers (bun / node / python) run without approval, so they must never run the
//! project's code: they get the text on stdin and no path, run in an empty private directory (bun
//! reads `bunfig.toml` and `.env` from its working directory), Python runs isolated (`-I -S`), and
//! a binary that lives inside the project is never started. bun's macros (`import … with { type:
//! "macro" }`, which run the imported function while the text is transpiled) are switched off, and
//! bun is only used once a probe has shown that this bun refuses them: an older bun that does not
//! is not used (node checks plain JavaScript instead).
//!
//! settings: { externalCheck?: boolean (default true), timeoutMs?: number (default 1500), maxBytes?: number (default 524288) }

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, ToolResultResult, child_env, create_private_dir, hook, sasacode_home};
use sasacode_tools::util::resolve_path;

use crate::project_bin::ProjectRoot;
use crate::repair::js::num_setting;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxIssue {
    pub line: usize,
    pub message: String,
}

/// Exact Rust lexical & delimiter checker (`//`, nested `/* */`, strings, raw strings `r#"..."#`,
/// char literals vs lifetimes `'a` / `'static`, and `()[]{}` balance).
pub fn check_rust_syntax(src: &str) -> Option<SyntaxIssue> {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut stack: Vec<(u8, usize)> = Vec::new();

    while i < n {
        let ch = b[i];
        if ch == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        // Line comment
        if ch == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Nested block comment
        if ch == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let start_line = line;
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed block comment `/*`".to_string(),
                });
            }
            continue;
        }
        // Raw strings: r"...", r#"..."#, br"...", br#"..."#, cr"...", cr#"..."#
        let prev_ident = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if !prev_ident && let Some((prefix_end, hashes)) = raw_string_start(b, i) {
            let start_line = line;
            i = prefix_end;
            let mut closed = false;
            while i < n {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'"' && i + 1 + hashes <= n && b[i + 1..i + 1 + hashes].iter().all(|&c| c == b'#') {
                    i += 1 + hashes;
                    closed = true;
                    break;
                } else {
                    i += 1;
                }
            }
            if !closed {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed raw string literal".to_string(),
                });
            }
            continue;
        }
        // Normal / byte / c strings: "...", b"...", c"..."
        if ch == b'"' || (!prev_ident && (ch == b'b' || ch == b'c') && i + 1 < n && b[i + 1] == b'"') {
            let start_line = line;
            i += if ch == b'"' { 1 } else { 2 };
            let mut closed = false;
            while i < n {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'\\' {
                    i += 1;
                    if i < n {
                        if b[i] == b'\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                } else if b[i] == b'"' {
                    i += 1;
                    closed = true;
                    break;
                } else {
                    i += 1;
                }
            }
            if !closed {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed string literal".to_string(),
                });
            }
            continue;
        }
        // Char / byte-char literal vs lifetime ('a, 'static, '_)
        if ch == b'\'' {
            if let Some(end) = char_literal_end(src, i) {
                i = end;
                continue;
            }
            // Lifetime or label: skip `'ident`
            i += 1;
            continue;
        }
        match ch {
            b'(' | b'[' | b'{' => stack.push((ch, line)),
            b')' | b']' | b'}' => {
                let expected = match ch {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                match stack.pop() {
                    Some((open, _)) if open == expected => {}
                    Some((open, open_line)) => {
                        return Some(SyntaxIssue {
                            line,
                            message: format!(
                                "mismatched delimiter `{}` (expected closing for `{}` opened on line {open_line})",
                                ch as char, open as char
                            ),
                        });
                    }
                    None => {
                        return Some(SyntaxIssue {
                            line,
                            message: format!("unexpected closing delimiter `{}`", ch as char),
                        });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    if let Some((open, open_line)) = stack.last() {
        return Some(SyntaxIssue {
            line: *open_line,
            message: format!("unclosed delimiter `{}` opened on line {open_line}", *open as char),
        });
    }
    None
}

fn raw_string_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut k = i;
    if k + 1 < b.len() && (b[k] == b'b' || b[k] == b'c') && b[k + 1] == b'r' {
        k += 2;
    } else if b[k] == b'r' {
        k += 1;
    } else {
        return None;
    }
    let mut hashes = 0usize;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        k += 1;
    }
    (k < b.len() && b[k] == b'"').then_some((k + 1, hashes))
}

/// Returns the byte offset just after the closing `'` if `src[i..]` starts a char literal,
/// or `None` if it is a lifetime (`'a`, `'static`, `'_`).
fn char_literal_end(src: &str, i: usize) -> Option<usize> {
    let rest = &src[i + 1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        let (esc_idx, esc) = chars.next()?;
        let after_esc = i + 1 + esc_idx + esc.len_utf8();
        let tail = &src[after_esc..];
        let close = match esc {
            'x' => tail.get(2..)?.starts_with('\'').then_some(after_esc + 3),
            'u' => {
                let end_brace = tail.strip_prefix('{')?.find('}')?;
                let after = after_esc + 1 + end_brace + 1;
                src.get(after..)?.starts_with('\'').then_some(after + 1)
            }
            _ => tail.starts_with('\'').then_some(after_esc + 1),
        };
        return close;
    }
    if first == '\'' || first == '\n' || first == '\r' {
        return None;
    }
    let (_, second) = chars.next()?;
    (second == '\'').then_some(i + 1 + first.len_utf8() + 1)
}

/// The checker's command: in `cwd`, with nothing in its environment that points it at a script.
fn checker_command(program: &Path, args: &[&str], cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(cwd);
    cmd.env_clear();
    cmd.envs(checker_env(child_env(&[])));
    cmd
}

/// What a shell or a dev tool may point at a script of the project: node and bun run what these name
/// before reading stdin (`--require`, `--import`, `--preload`, `--config`).
const CHILD_ENV_REMOVED: [&str; 6] = [
    "NODE_OPTIONS",
    "NODE_PATH",
    "BUN_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
];

fn checker_env(mut env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.retain(|k, _| !CHILD_ENV_REMOVED.iter().any(|r| k.eq_ignore_ascii_case(r)));
    env
}

/// Runs a checker with `input` on stdin, in `cwd`. `None`: it passed, did not start, or timed out.
async fn run_external(program: &Path, args: &[&str], input: &str, cwd: &Path, timeout_ms: u64) -> Option<String> {
    let mut cmd = checker_command(program, args, cwd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let run = async {
        let mut child = cmd.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        // A child that ends without reading closes the pipe: what it printed still says why.
        let feed = async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        };
        let (_, out) = tokio::join!(feed, child.wait_with_output());
        out.ok()
    };
    let out = tokio::time::timeout(Duration::from_millis(timeout_ms), run)
        .await
        .ok()??;
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let combined = if !stderr.is_empty() { stderr } else { stdout };
    (!combined.is_empty()).then_some(combined)
}

fn parse_line_number(msg: &str) -> usize {
    static SPECIFIC: OnceLock<Regex> = OnceLock::new();
    static FALLBACK: OnceLock<Regex> = OnceLock::new();
    let specific = SPECIFIC.get_or_init(|| {
        Regex::new(r#"(?m)(?:^\s*at\s+[^\n]*?:|File\s+"[^"]+",\s*line\s+|^\S[^\s:]*\.[A-Za-z0-9]+:)(\d+)"#)
            .expect("valid regex")
    });
    if let Some(n) = specific
        .captures(msg)
        .and_then(|c| c[1].parse::<usize>().ok())
        .filter(|&l| l > 0)
    {
        return n;
    }
    let fallback = FALLBACK.get_or_init(|| Regex::new(r"(?i)(?:line |:)(\d+)").expect("valid regex"));
    fallback
        .captures(msg)
        .and_then(|c| c[1].parse::<usize>().ok())
        .filter(|&l| l > 0)
        .unwrap_or(1)
}

fn snippet(text: &str, line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let idx = line.saturating_sub(1).min(lines.len() - 1);
    let from = idx.saturating_sub(2);
    let to = (idx + 2).min(lines.len() - 1);
    (from..=to)
        .map(|i| {
            let marker = if i == idx { ">" } else { " " };
            format!("{marker} {:>4} | {}", i + 1, lines[i])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JSON that its tools read with comments and trailing commas: not for a strict parser.
fn is_jsonc(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with("tsconfig")
        || name.starts_with("jsconfig")
        || path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == ".vscode")
}

/// The lowercase extension of a file this plugin checks.
fn checked_ext(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "json" if is_jsonc(path) => None,
        "json" | "rs" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "py" => Some(ext),
        _ => None,
    }
}

async fn read_text(path: &Path, max_bytes: u64) -> Option<String> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    String::from_utf8(tokio::fs::read(path).await.ok()?).ok()
}

/// Prints one line this plugin reads back; anything else Python says is not a syntax error.
const PYTHON_SCRIPT: &str = "import ast, sys
try:
    ast.parse(sys.stdin.buffer.read(), sys.argv[1])
except SyntaxError as e:
    sys.exit('syntax-check:%d:%s: %s' % (e.lineno or 1, type(e).__name__, e.msg))";

/// `macro: false`: the transpiler does not run the functions a macro import names.
const BUN_SCRIPT: &str =
    "new Bun.Transpiler({ loader: process.argv[1], macro: false }).transformSync(await Bun.stdin.text())";

/// What bun says where a macro call is, with macros off. It is not a syntax error.
const MACROS_OFF: &str = "Macros are disabled";

/// Text for the probe: a macro import from a place that does not exist, so that nothing can run
/// even by a bun that ignores `macro: false`.
const MACRO_PROBE: &str =
    "import { x } from \"/sasacode-syntax-check-no-such-directory/m.ts\" with { type: \"macro\" };\nx();\n";

/// Which bun programs refuse macros, by path: asked once per process.
fn macro_probes() -> &'static Mutex<HashMap<PathBuf, bool>> {
    static P: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    P.get_or_init(Mutex::default)
}

struct Checker {
    project: ProjectRoot,
    external: bool,
    timeout_ms: u64,
    /// Where the external checkers run. `None` inside: there is no such place, they do not run.
    child_cwd: OnceLock<Option<PathBuf>>,
}

impl Checker {
    fn new(root: &Path, external: bool, timeout_ms: u64) -> Self {
        Checker {
            project: ProjectRoot::new(root),
            external,
            timeout_ms,
            child_cwd: OnceLock::new(),
        }
    }

    /// `found` with its links resolved, unless that is inside the project: nothing the model can
    /// write there is started or used as a working directory.
    fn outside(&self, found: &Path) -> Option<PathBuf> {
        self.project.outside(found)
    }

    /// An empty directory of the user's own (the shared temp directory is anyone's to write).
    fn child_cwd(&self) -> Option<&Path> {
        self.child_cwd
            .get_or_init(|| {
                let dir = sasacode_home().join("tmp").join("syntax-check");
                create_private_dir(&dir).ok()?;
                self.outside(&dir)
            })
            .as_deref()
    }

    fn as_found(&self, found: PathBuf) -> Option<PathBuf> {
        self.project.as_found(found)
    }

    /// bun or node from PATH.
    fn tool(&self, name: &str) -> Option<PathBuf> {
        self.project.find(name)
    }

    fn python(&self) -> Option<PathBuf> {
        self.python_from(which::which("python3").ok())
    }

    /// The interpreter behind the project's `.venv` (it has the project's Python version), else
    /// the one PATH has (`on_path`). Of a venv always the real binary behind its link: started
    /// through the venv, Python reads its `pyvenv.cfg` and runs its `.pth` files even when
    /// isolated, and the model can write both.
    fn python_from(&self, on_path: Option<PathBuf>) -> Option<PathBuf> {
        let venv = self.project.root.join(".venv");
        [venv.join("bin").join("python"), venv.join("Scripts").join("python.exe")]
            .into_iter()
            .chain(on_path.clone())
            .filter_map(|p| self.outside(&p))
            .find(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.to_ascii_lowercase().starts_with("python")
            })
            // A shim that links to a program of another name (mise, rye) is no venv's link.
            .or_else(|| self.as_found(on_path?))
    }

    async fn run(&self, program: &Path, args: &[&str], text: &str) -> Option<String> {
        run_external(program, args, text, self.child_cwd()?, self.timeout_ms).await
    }

    /// Does this bun refuse macros? Only then is it used: from a bun that runs them, transpiling
    /// the model's text would run the model's code. Anything but bun's own "Macros are disabled"
    /// (cannot resolve, no such option, no answer in time) counts as "no", and is remembered.
    async fn bun_refuses_macros(&self, bun: &Path) -> bool {
        let known = macro_probes()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(bun)
            .copied();
        if let Some(known) = known {
            return known;
        }
        let Some(cwd) = self.child_cwd() else {
            return false;
        };
        let out = run_external(
            bun,
            &["-e", BUN_SCRIPT, "ts"],
            MACRO_PROBE,
            cwd,
            self.timeout_ms.max(5000),
        )
        .await;
        let refuses = out.is_some_and(|e| e.contains(MACROS_OFF));
        macro_probes()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(bun.to_path_buf(), refuses);
        refuses
    }

    async fn inspect(&self, path: &Path, text: &str) -> Option<SyntaxIssue> {
        let ext = checked_ext(path)?;
        match ext.as_str() {
            "json" => serde_json::from_str::<Value>(text).err().map(|e| SyntaxIssue {
                line: e.line().max(1),
                message: e.to_string(),
            }),
            "rs" => check_rust_syntax(text),
            "py" if self.external => self.check_python(path, text).await,
            _ if self.external => self.check_js(&ext, text).await,
            _ => None,
        }
    }

    async fn check_js(&self, ext: &str, text: &str) -> Option<SyntaxIssue> {
        if let Some(bun) = self.tool("bun")
            && self.bun_refuses_macros(&bun).await
        {
            return self.check_bun(&bun, ext, text).await;
        }
        if !matches!(ext, "js" | "mjs" | "cjs") {
            return None;
        }
        self.check_node(&self.tool("node")?, ext, text).await
    }

    async fn check_bun(&self, bun: &Path, ext: &str, text: &str) -> Option<SyntaxIssue> {
        // Plain .js often holds JSX. Not tsx for .ts: its grammar refuses `<T>value` casts.
        let loader = match ext {
            "tsx" => "tsx",
            "ts" | "mts" | "cts" => "ts",
            _ => "jsx",
        };
        let err = self.run(bun, &["-e", BUN_SCRIPT, loader], text).await?;
        // A parse error names its place in the text ("at input.tsx:2:31"); the rest is bun's own
        // trouble, and so is a macro call, which is refused rather than run.
        let mut error: Option<&str> = None;
        for l in err.lines() {
            let l = l.trim();
            if l.starts_with("error:") {
                error = Some(l);
            } else if l.starts_with("at input.")
                && let Some(summary) = error.take().filter(|e| !e.contains(MACROS_OFF))
            {
                return Some(SyntaxIssue {
                    line: parse_line_number(l),
                    message: summary.to_string(),
                });
            }
        }
        None
    }

    async fn check_node(&self, node: &Path, ext: &str, text: &str) -> Option<SyntaxIssue> {
        // From stdin node cannot tell a module by its name or package.json: a .js file may be either.
        let kinds: &[&str] = match ext {
            "mjs" => &["module"],
            "cjs" => &["commonjs"],
            _ => &["module", "commonjs"],
        };
        let mut failed: Option<String> = None;
        for kind in kinds {
            let kind = format!("--input-type={kind}");
            let err = self.run(node, &["--check", &kind], text).await?;
            failed.get_or_insert(err);
        }
        let err = failed?;
        let summary = err.lines().find(|l| l.contains("SyntaxError"))?;
        // node has no JSX: a `<` where an expression starts is most likely an element.
        if summary.contains("Unexpected token '<'") {
            return None;
        }
        let line = err
            .lines()
            .find_map(|l| l.strip_prefix("[stdin]:")?.trim().parse::<usize>().ok())
            .unwrap_or(1);
        Some(SyntaxIssue {
            line: line.max(1),
            message: summary.trim().to_string(),
        })
    }

    async fn check_python(&self, path: &Path, text: &str) -> Option<SyntaxIssue> {
        let python = self.python()?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("input.py");
        let err = self
            .run(&python, &["-I", "-S", "-B", "-c", PYTHON_SCRIPT, name], text)
            .await?;
        let (line, message) = err
            .lines()
            .find_map(|l| l.strip_prefix("syntax-check:"))?
            .split_once(':')?;
        Some(SyntaxIssue {
            line: line.parse::<usize>().ok()?.max(1),
            message: message.to_string(),
        })
    }
}

#[derive(Default)]
struct State {
    /// By agent and call id (a subagent's ids may repeat the main agent's): the file a call is about to
    /// change, and its text.
    before: HashMap<(Option<String>, String), (PathBuf, String)>,
    /// Files with an error this plugin reported that have not parsed since.
    reported: HashSet<PathBuf>,
}

/// Calls whose result never came (an aborted run) must not pile up.
const MAX_PENDING: usize = 64;

/// One name for a file however a call spelled it (`./a.py`, through a link), so that `reported` and
/// `before` find it again.
fn same_file(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_edit(name: &str) -> bool {
    matches!(name, "edit" | "write" | "hash_edit")
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let external = settings.get("externalCheck") != Some(&Value::Bool(false));
        let timeout_ms = num_setting(&settings, "timeoutMs", 1500.0).clamp(100.0, 10_000.0) as u64;
        let max_bytes = num_setting(&settings, "maxBytes", 524_288.0).max(1024.0) as u64;
        let cwd = api.cwd();
        let checker = Arc::new(Checker::new(&cwd, external, timeout_ms));
        let state: Arc<Mutex<State>> = Arc::default();

        // The text before the call: what tells an error the edit made from one that was there.
        let (st, root) = (state.clone(), cwd.clone());
        api.on::<hook::ToolCall, _, _>(move |ev, ctx| {
            let (st, root, agent) = (st.clone(), root.clone(), ctx.agent.clone());
            async move {
                let raw_path = ev.args.get("path").and_then(Value::as_str);
                if let Some(raw_path) = raw_path.filter(|_| is_edit(&ev.call.name)) {
                    let path = PathBuf::from(resolve_path(raw_path, &root));
                    if checked_ext(&path).is_some()
                        && let Some(text) = read_text(&path, max_bytes).await
                    {
                        let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
                        if s.before.len() >= MAX_PENDING {
                            s.before.clear();
                        }
                        s.before.insert((agent, ev.call.id.clone()), (same_file(&path), text));
                    }
                }
                Ok(None)
            }
        });

        let st = state.clone();
        api.on::<hook::SessionStart, _, _>(move |_, _| {
            *st.lock().unwrap_or_else(|e| e.into_inner()) = State::default();
            async { Ok(None) }
        });

        api.on::<hook::ToolResult, _, _>(move |ev, ctx| {
            let (cwd, checker, st) = (cwd.clone(), checker.clone(), state.clone());
            let key = (ctx.agent.clone(), ev.call.id.clone());
            async move {
                let before = st.lock().unwrap_or_else(|e| e.into_inner()).before.remove(&key);
                if !ev.ran || ev.result.is_error() || !is_edit(&ev.call.name) {
                    return Ok(None);
                }
                let raw_path = ev
                    .result
                    .details
                    .as_ref()
                    .and_then(|d| d.get("path"))
                    .and_then(Value::as_str)
                    .or_else(|| ev.call.input.get("path").and_then(Value::as_str));
                let Some(raw_path) = raw_path else {
                    return Ok(None);
                };
                let resolved = resolve_path(raw_path, &cwd);
                let path = Path::new(&resolved);
                let Some(text) = read_text(path, max_bytes).await else {
                    return Ok(None);
                };
                let file = same_file(path);
                let Some(issue) = checker.inspect(path, &text).await else {
                    st.lock().unwrap_or_else(|e| e.into_inner()).reported.remove(&file);
                    return Ok(None);
                };
                // Already failing before this call, and never reported: not something the edit broke
                // (most likely something the checker cannot read), so nothing for the model to fix.
                let known = st.lock().unwrap_or_else(|e| e.into_inner()).reported.contains(&file);
                if !known
                    && let Some((_, old)) = before.as_ref().filter(|(p, _)| *p == file)
                    && checker.inspect(path, old).await.is_some()
                {
                    return Ok(None);
                }
                st.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .reported
                    .insert(file);
                let disp = path
                    .strip_prefix(&cwd)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| resolved.clone());
                let ctx_lines = snippet(&text, issue.line);
                let note = format!(
                    "[syntax-check] Syntax error in {disp} (line {}): {}\n{ctx_lines}\nFix this syntax error before continuing.",
                    issue.line, issue.message
                );
                let mut result = ev.result;
                result.content.push(UserContent::text(note));
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checker whose children run in `child_cwd`, however unsafe a place that is.
    fn checker_in(root: &Path, child_cwd: &Path) -> Checker {
        let c = Checker::new(root, true, 20_000);
        let _ = c.child_cwd.set(Some(child_cwd.to_path_buf()));
        c
    }

    fn project() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("sasacode-syntax-unit-")
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn parses_bun_and_python_line_numbers_past_code_frame_colons() {
        let bun_err = "1 | const url = \"http://localhost:3000\";\n2 | export function f(x: number {\n                                  ^\nerror: Expected \")\" but found \"{\"\n    at input.ts:2:31\n";
        assert_eq!(parse_line_number(bun_err), 2);
        let py_err =
            "  File \"/tmp/mod.py\", line 14\n    return (1 + 2\n           ^\nSyntaxError: '(' was never closed\n";
        assert_eq!(parse_line_number(py_err), 14);
    }

    #[test]
    fn json_read_with_comments_is_not_checked() {
        for name in [
            "tsconfig.json",
            "tsconfig.app.json",
            "packages/web/TSConfig.build.json",
            "jsconfig.json",
            ".vscode/settings.json",
            "sub/.vscode/launch.json",
            "notes.jsonc",
        ] {
            assert_eq!(checked_ext(Path::new(name)), None, "{name}");
        }
        for name in ["package.json", "vscode/settings.json", "src/config.json"] {
            assert_eq!(checked_ext(Path::new(name)).as_deref(), Some("json"), "{name}");
        }
    }

    /// The interpreter is isolated: even with the project as its working directory (the worst
    /// place to run it) a module the project names like a standard one is not imported.
    #[tokio::test]
    async fn python_imports_nothing_from_the_project() {
        let dir = project();
        let root = dir.path();
        let marker = root.join("MARK_PY");
        std::fs::write(
            root.join("ast.py"),
            format!(
                "open({:?}, 'w').write('x')\ndef parse(*a, **k): pass\n",
                marker.to_string_lossy()
            ),
        )
        .unwrap();
        let c = checker_in(root, root);
        let Some(python) = c.python() else {
            eprintln!("skipped: no python3");
            return;
        };
        // The fixture is live: without isolation this interpreter does import the project's ast.py.
        let plain = std::process::Command::new(&python)
            .args(["-B", "-c", "import ast"])
            .current_dir(root)
            .env_remove("PYTHONSAFEPATH")
            .output();
        if plain.is_err() || !marker.exists() {
            eprintln!("skipped: this python does not import from its working directory");
            return;
        }
        std::fs::remove_file(&marker).unwrap();

        let bad = c
            .inspect(&root.join("mod.py"), "x = 1\ndef f():\n    return (1 + 2\n")
            .await;
        let bad = bad.expect("a broken file is still reported");
        assert_eq!(bad.line, 3, "{bad:?}");
        assert!(bad.message.starts_with("SyntaxError"), "{bad:?}");
        assert_eq!(c.inspect(&root.join("mod.py"), "def f():\n    return 1\n").await, None);
        assert!(!marker.exists(), "the project's ast.py ran");
    }

    /// `NODE_OPTIONS=--require ./x.js` or `BUN_OPTIONS=--preload ./x.js` set by a shell or a dev tool
    /// would run project code before the checker reads its input.
    #[test]
    fn checkers_get_no_environment_that_loads_a_script() {
        let env: BTreeMap<String, String> = [
            ("NODE_OPTIONS", "--require ./evil.js"),
            ("BUN_OPTIONS", "--preload ./evil.js"),
            ("NODE_PATH", "."),
            ("PYTHONPATH", "."),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let kept: Vec<String> = checker_env(env).into_keys().collect();
        assert_eq!(kept, ["PATH"]);
    }

    /// bun reads `bunfig.toml` (and its preloads) from its working directory, so that is never the project.
    #[tokio::test]
    async fn bun_runs_no_preload_from_the_project() {
        let dir = project();
        let root = dir.path();
        let marker = root.join("MARK_BUN");
        std::fs::write(
            root.join("evil.js"),
            format!("require('fs').writeFileSync({:?}, 'x');\n", marker.to_string_lossy()),
        )
        .unwrap();
        std::fs::write(root.join("bunfig.toml"), "preload = [\"./evil.js\"]\n").unwrap();
        let ok = "export const a: number = 1;\n";
        let bad = "export function f(x: number {\n  return x;\n}\n";

        // The fixture is live: run in the project, bun does load the preload.
        let unsafe_checker = checker_in(root, root);
        if unsafe_checker.tool("bun").is_none() {
            eprintln!("skipped: no bun");
            return;
        }
        unsafe_checker.inspect(&root.join("app.ts"), ok).await;
        if !marker.exists() {
            eprintln!("skipped: this bun does not preload from its working directory");
            return;
        }
        std::fs::remove_file(&marker).unwrap();

        // As the plugin runs it: in its own directory.
        let c = Checker::new(root, true, 20_000);
        let cwd = c.child_cwd().expect("a private directory").to_path_buf();
        assert!(!cwd.starts_with(&c.project.root_real), "{cwd:?}");
        let issue = c.inspect(&root.join("app.ts"), bad).await;
        let issue = issue.expect("a broken file is still reported");
        assert_eq!(issue.line, 1, "{issue:?}");
        assert_eq!(c.inspect(&root.join("app.ts"), ok).await, None);
        assert!(!marker.exists(), "the project's preload ran");
    }

    /// Macros are off while bun transpiles the checked text: a macro import is not run, and neither
    /// it nor bun's refusal is reported as a syntax error.
    #[tokio::test]
    async fn bun_runs_no_macro_and_a_macro_import_is_no_syntax_error() {
        let dir = project();
        let root = dir.path();
        let marker = root.join("MARK_MACRO");
        let macro_file = root.join("m.ts");
        std::fs::write(
            &macro_file,
            format!(
                "import {{ writeFileSync }} from \"node:fs\";\nexport function x() {{ writeFileSync({:?}, \"x\"); return 1; }}\n",
                marker.to_string_lossy()
            ),
        )
        .unwrap();
        let text = format!(
            "import {{ x }} from {:?} with {{ type: \"macro\" }};\nexport const a: number = x();\n",
            macro_file.to_string_lossy()
        );

        let c = Checker::new(root, true, 20_000);
        let Some(bun) = c.tool("bun") else {
            eprintln!("skipped: no bun");
            return;
        };
        // The fixture is live: a transpiler left to its defaults does run the macro.
        let cwd = c.child_cwd().expect("a private directory").to_path_buf();
        let live = std::process::Command::new(&bun)
            .args([
                "-e",
                "new Bun.Transpiler({ loader: 'ts' }).transformSync(await Bun.stdin.text())",
            ])
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().expect("stdin").write_all(text.as_bytes())?;
                child.wait()
            });
        if live.is_err() || !marker.exists() {
            eprintln!("skipped: this bun does not run macros");
            return;
        }
        std::fs::remove_file(&marker).unwrap();

        // As the plugin runs it: whether or not this bun can refuse them, nothing runs.
        assert_eq!(c.inspect(&root.join("app.ts"), &text).await, None);
        assert!(!marker.exists(), "the macro ran");
        if !c.bun_refuses_macros(&bun).await {
            eprintln!("skipped: this bun does not refuse macros (it is not used for the check)");
        } else {
            assert_eq!(c.check_bun(&bun, "ts", &text).await, None);
            assert!(!marker.exists(), "the macro ran");
            // A real error next to a macro import is still reported.
            let broken = format!("{text}export function f(x: number {{\n  return x;\n}}\n");
            let issue = c
                .check_bun(&bun, "ts", &broken)
                .await
                .expect("a broken file is reported");
            assert_eq!(issue.line, 3, "{issue:?}");
            assert!(!issue.message.contains(MACROS_OFF), "{issue:?}");
            assert!(!marker.exists(), "the macro ran");
        }
    }

    /// JSX in plain .js and casts in .ts are no errors for bun.
    #[tokio::test]
    async fn bun_reads_jsx_in_js_and_casts_in_ts() {
        let dir = project();
        let c = Checker::new(dir.path(), true, 20_000);
        let Some(bun) = c.tool("bun") else {
            eprintln!("skipped: no bun");
            return;
        };
        let jsx = "export const View = () => <div className=\"a\">hi</div>;\n";
        for ext in ["js", "mjs", "cjs", "jsx", "tsx"] {
            assert_eq!(c.check_bun(&bun, ext, jsx).await, None, "{ext}");
        }
        let cast = "const n = <number>(1 as unknown);\nconst id = <T>(x: T) => x;\n";
        assert_eq!(c.check_bun(&bun, "ts", cast).await, None);
        let broken = "export const View = () => <div>hi;\n";
        assert!(c.check_bun(&bun, "js", broken).await.is_some());
    }

    /// node gets the text on stdin: a .js file is a module or a script, and JSX is not its to judge.
    #[tokio::test]
    async fn node_accepts_modules_scripts_and_jsx() {
        let dir = project();
        let c = Checker::new(dir.path(), true, 20_000);
        let Some(node) = c.tool("node") else {
            eprintln!("skipped: no node");
            return;
        };
        let module = "import x from \"y\";\nexport const a = await x();\n";
        let script = "const x = require(\"y\");\nif (!x) return;\nmodule.exports = x;\n";
        assert_eq!(c.check_node(&node, "js", module).await, None);
        assert_eq!(c.check_node(&node, "js", script).await, None);
        assert_eq!(c.check_node(&node, "mjs", module).await, None);
        assert_eq!(c.check_node(&node, "cjs", script).await, None);
        assert!(c.check_node(&node, "cjs", module).await.is_some());
        assert_eq!(c.check_node(&node, "js", "export const v = <div/>;\n").await, None);
        let issue = c
            .check_node(&node, "js", "import x from \"y\";\nexport const a = (1;\n")
            .await
            .expect("a broken file is reported");
        assert_eq!(issue.line, 2, "{issue:?}");
        assert!(issue.message.contains("SyntaxError"), "{issue:?}");
    }

    /// The project's `.venv` only says which interpreter: the real one behind its link is started,
    /// and nothing that is a file of the project.
    #[cfg(unix)]
    #[test]
    fn python_is_the_real_interpreter_behind_the_venv() {
        let (dir, elsewhere) = (project(), project());
        let root = dir.path();
        let bin = root.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let c = Checker::new(root, true, 1500);
        let inside = |p: &Path| p.starts_with(&c.project.root_real) || p.starts_with(&c.project.root);

        // A file of the project, however it is named, is not an interpreter to start.
        std::fs::write(bin.join("python"), "#!/bin/sh\ntouch pwned\n").unwrap();
        assert!(c.python().is_none_or(|p| !inside(&p)));

        // A link out of the project: the interpreter it names.
        let real = elsewhere.path().join("python3.99");
        std::fs::write(&real, "").unwrap();
        std::fs::remove_file(bin.join("python")).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("python")).unwrap();
        assert_eq!(c.python(), Some(std::fs::canonicalize(&real).unwrap()));

        // A link to something that is no Python is not started with Python's arguments.
        let shell = elsewhere.path().join("sh");
        std::fs::write(&shell, "").unwrap();
        std::fs::remove_file(bin.join("python")).unwrap();
        std::os::unix::fs::symlink(&shell, bin.join("python")).unwrap();
        assert_eq!(c.python_from(None), None);

        // An activated venv of the project puts its link on PATH: still the real interpreter.
        std::fs::remove_file(bin.join("python")).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("python3")).unwrap();
        assert_eq!(
            c.python_from(Some(bin.join("python3"))),
            Some(std::fs::canonicalize(&real).unwrap())
        );
        // ... also when PATH spells the project another way than the working directory does.
        let alias = elsewhere.path().join("alias");
        std::os::unix::fs::symlink(root, &alias).unwrap();
        let aliased = alias.join(".venv").join("bin").join("python3");
        std::fs::remove_file(bin.join("python3")).unwrap();
        std::fs::write(bin.join("python3"), "").unwrap();
        assert_eq!(c.python_from(Some(aliased.clone())), None);
        assert_eq!(c.as_found(aliased), None);

        // A version manager's shim outside the project links to a program of another name, and
        // tells by the name it is started under what to run: started as found.
        let shims = elsewhere.path().join("shims");
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::write(elsewhere.path().join("mise"), "").unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("mise"), shims.join("python3")).unwrap();
        assert_eq!(c.python_from(Some(shims.join("python3"))), Some(shims.join("python3")));
    }

    /// bun, node and the working directory are refused when they lie inside the project.
    #[test]
    fn nothing_inside_the_project_is_used() {
        let dir = project();
        let root = dir.path();
        let c = Checker::new(root, true, 1500);
        let local = root.join("node_modules").join(".bin");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("bun"), "").unwrap();
        assert_eq!(c.outside(&local.join("bun")), None);
        assert_eq!(c.outside(&root.join("missing")), None);
        assert!(c.outside(&std::env::temp_dir().join("sasacode-not-there")).is_some());
    }
}
