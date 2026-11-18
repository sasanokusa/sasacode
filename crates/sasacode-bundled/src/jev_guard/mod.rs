//! jev-guard (`plugins/jev-guard/index.ts`): a second opinion on tool calls from TypeSafe's Jev, a
//! decision model that answers typed questions with calibrated probabilities (no text). It runs
//! after the rules and the permission mode (the permission hook) and, within the limits the core
//! sets:
//!   - stops calls that would run unasked (allow → ask, or deny when clearly dangerous),
//!   - in agent mode, lets calls it finds safe run without asking (in place of the model judge),
//!   - hands softDeny calls it finds safe to the user instead of refusing them.
//!
//! It never takes the user's say away: a call the user would be asked about is still asked about.
//! Jev only sees facts the harness gathered (the command, its targets, git state) and the user's
//! own request, never tool output, since content written to steer it can move its answers.
//! Off until `plugins.enabled` names it (and then until `enabled` or /jev on).

pub mod backends;
pub mod lang;

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_agent::paths;
use sasacode_ai::JsonObject;
use sasacode_ai::json::{js_f64, stringify};
use sasacode_plugin_api::{
    Command, Decision, NotifyLevel, PermissionMode, PermissionResult, PluginApi, SelectOption, Tool, ToolKind,
    UserContent, Verdict, VerdictSource, apply_child_env, hook,
};

use crate::repair::js::{js_space, js_string, js_trim, utf16_len, utf16_prefix};
use backends::{
    Backend, BackendName, BackendSettings, Completer, Deps, NoulCriteria, Question, Questions, RawAnswers, completer,
    detect_service, make_backend, parse_native,
};
use lang::{L, set_lang};

// ── what Jev is asked ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// P(allow) for "safe".
    pub allow: f64,
    /// P(allow) for "safe" when no user can be asked (headless).
    pub headless_allow: f64,
    /// P(deny) for "dangerous".
    pub deny: f64,
    /// Least confidence of the verdict for "safe" or "dangerous".
    pub confidence: f64,
    /// P(ask) + P(deny) at or above this makes the call risky.
    pub risk: f64,
    /// A concern at or above this is a flag (and makes the call risky).
    pub flag: f64,
    /// Every concern must be below this for "safe".
    pub clear: f64,
}

pub const DEFAULT_THRESHOLDS: Thresholds = Thresholds {
    allow: 0.8,
    headless_allow: 0.9,
    deny: 0.85,
    confidence: 0.6,
    risk: 0.6,
    flag: 0.7,
    clear: 0.3,
};

struct Concern {
    id: &'static str,
    ja: &'static str,
    en: &'static str,
    instructions: &'static str,
    yes: &'static str,
    no: &'static str,
}

impl Concern {
    fn label(&self) -> String {
        L(self.ja, self.en)
    }
}

const CONCERNS: [Concern; 3] = [
    Concern {
        id: "destroys_data",
        ja: "データ消失",
        en: "data loss",
        instructions: "Can this call permanently delete or overwrite data that neither git nor a rebuild can bring back?",
        yes: "Deletes or overwrites uncommitted work, files outside git, databases, or whole directories other than build output",
        no: "Deletes only build output, caches, git-ignored files or committed files, or deletes nothing",
    },
    Concern {
        id: "outside_workspace",
        ja: "作業ディレクトリ外",
        en: "outside the workspace",
        instructions: "Does this call change anything outside the workspace directory and temporary directories?",
        yes: "Writes, deletes or moves files elsewhere in the home directory or the system, pushes, publishes, deploys, or sends data to a server",
        no: "Only reads, or only changes files inside the workspace or in a temporary directory",
    },
    Concern {
        id: "touches_secrets",
        ja: "認証情報・セキュリティ設定",
        en: "credentials or security settings",
        instructions: "Does this call read, send, or change credentials or security settings?",
        yes: "SSH keys, API keys, tokens, passwords, .env files, keychains, sudo, firewall, or permissions of system files",
        no: "Touches none of these",
    },
];

/// The questions every call is asked: the verdict, then the concerns.
pub fn questions() -> &'static Questions {
    static Q: OnceLock<Questions> = OnceLock::new();
    Q.get_or_init(|| {
        let mut q = Questions::new();
        let criteria: IndexMap<String, String> = [
            ("allow", "Routine for the task and safe to run without asking: reads, builds, tests, or changes inside the workspace that git or a rebuild can restore"),
            ("ask", "Plausible, but a person should confirm first: it changes things outside the workspace, deletes work that is not committed, installs, publishes or pushes, or its effect is unclear"),
            ("deny", "Clearly destructive or dangerous: wipes the home directory or system files, destroys data that cannot be restored, leaks credentials, or weakens security"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        q.insert(
            "verdict".into(),
            Question::Choice {
                instructions: "A coding agent wants to make the tool call in `call`, working in `workspace`. How should the harness handle it?".into(),
                criteria,
            },
        );
        for c in &CONCERNS {
            q.insert(
                c.id.into(),
                Question::Noul {
                    instructions: c.instructions.into(),
                    criteria: Some(NoulCriteria { yes: c.yes.into(), no: c.no.into() }),
                },
            );
        }
        q
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Probabilities {
    #[serde(with = "js_f64")]
    pub allow: f64,
    #[serde(with = "js_f64")]
    pub ask: f64,
    #[serde(with = "js_f64")]
    pub deny: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerdictAnswer {
    /// What the service chose (a Decision when it keeps to the question).
    pub choice: String,
    pub probabilities: Probabilities,
    #[serde(with = "js_f64")]
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Concerns {
    #[serde(with = "js_f64")]
    pub destroys_data: f64,
    #[serde(with = "js_f64")]
    pub outside_workspace: f64,
    #[serde(with = "js_f64")]
    pub touches_secrets: f64,
}

impl Concerns {
    fn get(&self, id: &str) -> f64 {
        match id {
            "destroys_data" => self.destroys_data,
            "outside_workspace" => self.outside_workspace,
            _ => self.touches_secrets,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JevAnswers {
    pub verdict: VerdictAnswer,
    pub concerns: Concerns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reading {
    Safe,
    Risky,
    Dangerous,
    Unsure,
}

impl Reading {
    pub fn as_str(self) -> &'static str {
        match self {
            Reading::Safe => "safe",
            Reading::Risky => "risky",
            Reading::Dangerous => "dangerous",
            Reading::Unsure => "unsure",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    pub reading: Reading,
    pub summary: String,
    pub answers: JevAnswers,
}

/// `n.toFixed(2)`: ties (a multiple of 1/8 with an odd numerator) round away from zero, where
/// Rust's formatting rounds them to even.
fn fmt(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0.00".into();
    }
    let a = n.abs();
    let eighths = a * 8.0;
    let tie = eighths.fract() == 0.0 && eighths % 2.0 == 1.0 && a < 1e15;
    let s = if tie {
        format!("{:.2}", (a * 100.0).ceil() / 100.0)
    } else {
        format!("{a:.2}")
    };
    if n < 0.0 { format!("-{s}") } else { s }
}

/// Turn Jev's probabilities into one reading. Thresholds are calibrated from the bench, not guessed per call.
pub fn interpret(a: &JevAnswers, t: &Thresholds, interactive: bool) -> Judgement {
    let p = &a.verdict.probabilities;
    let flags: Vec<&Concern> = CONCERNS.iter().filter(|c| a.concerns.get(c.id) >= t.flag).collect();
    let confident = a.verdict.confidence >= t.confidence;
    let reading = if p.deny >= t.deny && confident {
        Reading::Dangerous
    } else if p.deny >= 0.5 || p.ask + p.deny >= t.risk || !flags.is_empty() {
        Reading::Risky
    } else if p.allow >= (if interactive { t.allow } else { t.headless_allow })
        && confident
        && CONCERNS.iter().all(|c| a.concerns.get(c.id) < t.clear)
    {
        Reading::Safe
    } else {
        Reading::Unsure
    };
    let label = match reading {
        Reading::Safe => L("安全", "safe"),
        Reading::Risky => L("要確認", "risky"),
        Reading::Dangerous => L("危険", "dangerous"),
        Reading::Unsure => L("判断保留", "unsure"),
    };
    let mut parts = vec![format!(
        "allow {} · ask {} · deny {}",
        fmt(p.allow),
        fmt(p.ask),
        fmt(p.deny)
    )];
    for c in flags {
        parts.push(format!("{} {}", c.label(), fmt(a.concerns.get(c.id))));
    }
    let parts = parts.join(" · ");
    Judgement {
        reading,
        summary: L(format!("Jev: {label}（{parts}）"), format!("Jev: {label} ({parts})")),
        answers: a.clone(),
    }
}

/// `Number(v)` of what a service sent as a probability.
fn number_of(v: Option<&Value>) -> f64 {
    match v {
        None | Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => {
            let s = js_trim(s);
            if s.is_empty() {
                0.0
            } else {
                s.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// Answers from any backend, in the shape interpret() reads.
pub fn to_answers(raw: &RawAnswers) -> anyhow::Result<JevAnswers> {
    let Some(v) = raw.choice.get("verdict") else {
        anyhow::bail!("no answer for verdict");
    };
    let p = &v.probabilities;
    let concern = |id: &str| {
        raw.noul
            .get(id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("no answer for {id}"))
    };
    Ok(JevAnswers {
        verdict: VerdictAnswer {
            choice: v.choice.clone(),
            probabilities: Probabilities {
                allow: number_of(p.get("allow")),
                ask: number_of(p.get("ask")),
                deny: number_of(p.get("deny")),
            },
            confidence: v.confidence,
        },
        concerns: Concerns {
            destroys_data: concern("destroys_data")?,
            outside_workspace: concern("outside_workspace")?,
            touches_secrets: concern("touches_secrets")?,
        },
    })
}

/// Parse a systemone response in TypeSafe's own dialect; an error when it is not what was asked.
pub fn parse_answers(body: &Value) -> anyhow::Result<JevAnswers> {
    to_answers(&parse_native(body, questions())?)
}

// ── what Jev sees ────────────────────────────────────────────────────

/// Where one shell command ends and the next begins (the core splits commands the same way), except the & of &> and >&:
/// split there, `echo x &> ~/.zshrc` lost the file it writes (#113).
fn chain() -> &'static fancy_regex::Regex {
    static R: OnceLock<fancy_regex::Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern.
    R.get_or_init(|| fancy_regex::Regex::new(r"&&|\|\||;|\||\n|(?<!>)&(?!>)").expect("valid regex"))
}

/// `command.split(CHAIN)`.
fn split_chain(command: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut last = 0;
    for m in chain().find_iter(command) {
        let Ok(m) = m else { break };
        out.push(&command[last..m.start()]);
        last = m.end();
    }
    out.push(&command[last..]);
    out
}

fn is_inside(path: &str, dir: &str) -> bool {
    let rel = paths::relative(dir, path);
    rel.is_empty() || (!rel.starts_with("..") && !paths::is_absolute(&rel))
}

/// Words of one shell command, quotes removed. Good enough to find paths, not a shell.
pub fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else if ch == '\\' && q == '"' && chars.peek().is_some() {
                cur.extend(chars.next());
            } else {
                cur.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            any = true;
        } else if ch == '\\' && chars.peek().is_some() {
            cur.extend(chars.next());
        } else if js_space(ch) {
            if !cur.is_empty() || any {
                words.push(std::mem::take(&mut cur));
            }
            any = false;
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() || any {
        words.push(cur);
    }
    words
}

/// Commands whose plain arguments are files they change.
const FILE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "unlink", "shred", "truncate", "mv", "cp", "ln", "chmod", "chown", "chgrp", "touch", "tee", "mkdir",
];
/// Commands that change the files they are given only with an in-place flag.
const IN_PLACE: &[&str] = &["sed", "perl"];
/// Words that run the command after them.
const PREFIXES: &[&str] = &["sudo", "env", "command", "exec", "nice", "nohup", "time", "xargs"];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    // INFALLIBLE: constant patterns.
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// `path.basename(p)` (POSIX).
fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return String::new();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// Files a command may change: the arguments of commands that change files (rm, mv, cp, chmod,
/// sed -i …) and redirection targets. Files it only reads are left out: listing them made reads
/// look like changes outside the workspace.
pub fn path_args(command: &str) -> Vec<String> {
    static ASSIGN: OnceLock<Regex> = OnceLock::new();
    static IN_PLACE_FLAG: OnceLock<Regex> = OnceLock::new();
    static REDIRECT: OnceLock<Regex> = OnceLock::new();
    static DESCRIPTOR: OnceLock<Regex> = OnceLock::new();
    static SED_SCRIPT: OnceLock<fancy_regex::Regex> = OnceLock::new();
    let assign = re(&ASSIGN, r"^[A-Za-z_][A-Za-z0-9_]*=");
    let in_place_flag = re(&IN_PLACE_FLAG, r"^-[a-zA-Z]*i");
    // `.` of JS stops at line terminators.
    let redirect = re(&REDIRECT, r"^([0-9]*|&)>>?(&?)([^\n\r\u{2028}\u{2029}]*)$");
    let descriptor = re(&DESCRIPTOR, r"^([0-9]+|-)$");
    // INFALLIBLE: a constant pattern.
    let sed_script =
        SED_SCRIPT.get_or_init(|| fancy_regex::Regex::new(r"^s(.).*\1.*\1[gip0-9]*$").expect("valid regex"));

    let mut out: Vec<String> = Vec::new();
    let add = |out: &mut Vec<String>, p: &str| {
        if !p.is_empty() && !p.starts_with('-') && !p.contains("://") && !p.starts_with("/dev/") && !p.starts_with('&')
        {
            out.push(p.to_string());
        }
    };
    for segment in split_chain(command) {
        let mut words = shell_words(js_trim(segment));
        while words
            .first()
            .is_some_and(|w| assign.is_match(w) || PREFIXES.contains(&w.as_str()))
        {
            words.remove(0);
        }
        let program = basename(words.first().map(String::as_str).unwrap_or(""));
        let program = program.as_str();
        let changes_files = FILE_COMMANDS.contains(&program)
            || (IN_PLACE.contains(&program) && words.iter().any(|w| in_place_flag.is_match(w)));
        // chmod 600 f, chown user f: the first plain argument is the mode or owner.
        let mut skip_first = ["chmod", "chown", "chgrp"].contains(&program);
        let mut i = 1;
        while i < words.len() {
            let w = words[i].as_str();
            if let Some(r) = redirect.captures(w) {
                // >&2 and 2>&1 copy a descriptor; >& file, like &> file, writes the file.
                let inline = r.get(3).map_or("", |m| m.as_str());
                let target = if inline.is_empty() {
                    i += 1;
                    words.get(i).map(String::as_str).unwrap_or("")
                } else {
                    inline
                };
                let copies = r.get(2).is_some_and(|m| !m.as_str().is_empty());
                if !(copies && descriptor.is_match(target)) {
                    add(&mut out, target);
                }
                i += 1;
                continue;
            }
            if w.starts_with('<') {
                if w == "<" || w == "<<" || w == "<<<" {
                    i += 1;
                }
                i += 1;
                continue;
            }
            // sed -i '' 's/a/b/' file: the empty suffix and the script are not files.
            if skip_first && !w.starts_with('-') {
                skip_first = false;
                i += 1;
                continue;
            }
            let script = sed_script.is_match(w).unwrap_or(false);
            if changes_files && !(IN_PLACE.contains(&program) && (w.is_empty() || script || w.starts_with('-'))) {
                add(&mut out, w);
            }
            i += 1;
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for p in out {
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    seen.truncate(12);
    seen
}

fn expand(p: &str, cwd: &str, home: &str) -> String {
    let mut e = p.to_string();
    for prefix in ["~", "$HOME", "${HOME}"] {
        if let Some(rest) = p.strip_prefix(prefix)
            && (rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\'))
        {
            e = format!("{home}{rest}");
            break;
        }
    }
    // Git Bash, which runs the commands on Windows, spells C:\x as /c/x.
    if paths::WINDOWS {
        let b = e.as_bytes();
        if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/') {
            e = format!("{}:{}", &e[1..2], &e[2..]);
        }
    }
    paths::resolve2(cwd, &e)
}

/// Where a path really goes: the deepest existing ancestor through symlinks, plus the rest as written.
fn real(p: &str) -> String {
    let parts: Vec<&str> = p.split(paths::is_sep).collect();
    let wild = parts.iter().position(|s| s.contains(['*', '?', '[', '{']));
    let sep = paths::sep().to_string();
    let (mut head, mut rest): (String, Vec<String>) = match wild {
        Some(w) if w > 0 => {
            let h = parts[..w].join(&sep);
            (
                if h.is_empty() { sep.clone() } else { h },
                parts[w..].iter().map(|s| s.to_string()).collect(),
            )
        }
        _ => (p.to_string(), Vec::new()),
    };
    // C: alone is that drive's working directory.
    if paths::WINDOWS && head.len() == 2 && head.as_bytes()[0].is_ascii_alphabetic() && head.as_bytes()[1] == b':' {
        head.push_str(&sep);
    }
    while !Path::new(&head).exists() && paths::dirname(&head) != head {
        rest.insert(0, basename(&head));
        head = paths::dirname(&head);
    }
    // Spelled as realpathSync.native does: no `\\?\` on Windows (its `?` would read as a glob).
    if let Ok(r) = std::fs::canonicalize(&head) {
        head = sasacode_plugin_api::win32_path::without_verbatim(&r.to_string_lossy());
    }
    if rest.is_empty() {
        head
    } else {
        paths::join(&head, &rest.join(&sep))
    }
}

/// git's output, or None when it fails or takes over two seconds.
async fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    apply_child_env(&mut cmd, &[]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(2), cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What git knows about a path: can it be brought back if the call destroys it?
async fn git_state(path: &str, root: Option<&str>) -> &'static str {
    let Some(root) = root.filter(|r| is_inside(path, r)) else {
        return "not in a git repository";
    };
    let status = git(root, &["status", "--porcelain", "--ignored", "-z", "--", path])
        .await
        .unwrap_or_default();
    let status: Vec<&str> = status.split('\0').filter(|l| !l.is_empty()).collect();
    let tracked = git(root, &["ls-files", "-z", "--", path])
        .await
        .is_some_and(|s| !s.is_empty());
    let changed = status.iter().any(|l| !l.starts_with("??") && !l.starts_with("!!"));
    let untracked = status.iter().any(|l| l.starts_with("??"));
    let ignored = status.iter().any(|l| l.starts_with("!!"));
    if changed {
        return "tracked by git, with uncommitted changes";
    }
    if tracked {
        return if untracked {
            "tracked by git and committed, plus untracked files"
        } else {
            "tracked by git and committed"
        };
    }
    if untracked {
        return "untracked (not in git, cannot be restored from it)";
    }
    if ignored {
        return "ignored by git (build output, caches, dependencies)";
    }
    // Nothing reported is not "committed" (#113): .git itself is the history, and git cannot bring it back.
    if is_inside(path, &paths::join(root, ".git")) {
        return "the repository's .git (its history: git cannot restore it)";
    }
    "unknown to git (it reports nothing for this path)"
}

fn has_glob(p: &str) -> bool {
    p.contains(['*', '?', '['])
}

/// Up to `limit` paths under `cwd` that `pattern` matches, as Bun.Glob's scanSync found them
/// (`dot`, files and directories, symlinked directories not entered). None when the pattern
/// does not compile.
fn scan(pattern: &str, cwd: &Path, limit: usize) -> Option<Vec<String>> {
    let glob = globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .ok()?
        .compile_matcher();
    let depth = if pattern.contains("**") {
        usize::MAX
    } else {
        pattern.split('/').count()
    };
    let mut found = Vec::new();
    fn walk(dir: &Path, rel: &str, depth: usize, glob: &globset::GlobMatcher, found: &mut Vec<String>, limit: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            if found.len() >= limit {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if glob.is_match(&path) {
                found.push(path.clone());
            }
            if depth > 1 && entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&entry.path(), &path, depth - 1, glob, found, limit);
            }
        }
    }
    walk(cwd, "", depth, &glob, &mut found, limit);
    Some(found)
}

/// What a glob argument names in the workspace, at most 12 paths; the glob itself when it matches nothing or cannot be read (#113).
fn globbed(p: &str, cwd: &str) -> Vec<String> {
    if !has_glob(p) || paths::is_absolute(p) || p.starts_with('~') || p.starts_with('$') {
        return vec![p.to_string()];
    }
    match scan(p, Path::new(cwd), 12) {
        Some(found) if !found.is_empty() => found,
        _ => vec![p.to_string()],
    }
}

/// Scratch space: changes there do not touch the user's files.
fn temp_dirs() -> Vec<String> {
    let tmp = std::env::temp_dir().to_string_lossy().into_owned();
    let mut out: Vec<String> = Vec::new();
    for d in [
        tmp.as_str(),
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
    ] {
        let r = real(d);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

fn tildify(p: &str, home: &str) -> String {
    match p.strip_prefix(home) {
        Some(rest) if rest.is_empty() || rest.starts_with(paths::sep()) => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// Best-effort removal of secrets before anything leaves the machine.
pub fn redact(s: &str) -> String {
    static R: OnceLock<[(Regex, &'static str); 5]> = OnceLock::new();
    let rules = R.get_or_init(|| {
        // INFALLIBLE: constant patterns. `\b` of JS is ASCII.
        let r = |p: &str| Regex::new(p).expect("valid regex");
        [
            (
                r(r#"(?i)(?-u:\b)([A-Za-z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|PASS|AUTH)[A-Za-z0-9_]*)=("[^"]*"|'[^']*'|\S+)"#),
                "${1}=[redacted]",
            ),
            (r(r"(?-u:\b)(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+"), "${1} [redacted]"),
            (r(r#"(?i)(-H\s*["']?(?:Authorization|X-Api-Key|Api-Key)\s*:\s*)[^"'\n]+"#), "${1}[redacted]"),
            (
                r(r"(?-u:\b)(sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})"),
                "[redacted]",
            ),
            (r(r"(://[^/\s:@]+:)[^@\s/]+@"), "${1}[redacted]@"),
        ]
    });
    let mut out = s.to_string();
    for (re, with) in rules {
        out = re.replace_all(&out, *with).into_owned();
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if utf16_len(s) > max {
        format!("{}…", utf16_prefix(s, max))
    } else {
        s.to_string()
    }
}

pub struct CallFacts<'a> {
    pub tool: &'a dyn Tool,
    pub args: &'a JsonObject,
    pub cwd: &'a str,
    pub user_request: Option<&'a str>,
    /// `os.homedir()`.
    pub home: &'a str,
}

/// The state Jev judges: facts the harness gathered, and the user's own words.
pub async fn build_state(f: &CallFacts<'_>) -> JsonObject {
    let command = f.tool.match_target(f.args);
    let mut call = JsonObject::new();
    call.insert("tool".into(), json!(f.tool.name()));
    match &command {
        Some(c) => call.insert("command".into(), json!(clip(&redact(c), 4000))),
        None => call.insert("arguments".into(), json!(clip(&redact(&stringify(f.args)), 4000))),
    };
    let written = f
        .tool
        .paths(f.args, Path::new(f.cwd))
        .unwrap_or_else(|| command.as_deref().map(path_args).unwrap_or_default());
    let top = git(f.cwd, &["rev-parse", "--show-toplevel"])
        .await
        .map(|t| js_trim(&t).to_string());
    // Spelled like the targets (C:/x from git on Windows).
    let root = top.filter(|t| !t.is_empty()).map(|t| real(&paths::resolve(&[&t])));
    let workspace = real(f.cwd);
    let temp = temp_dirs();
    let mut targets = Vec::new();
    for p in written.iter().flat_map(|p| globbed(p, f.cwd)).take(12) {
        let place = real(&expand(&p, f.cwd, f.home));
        let meta = std::fs::metadata(&place).ok();
        let exists = match &meta {
            Some(m) if m.is_dir() => "directory",
            Some(_) => "file",
            None if has_glob(&p) => "a glob that names nothing here (what it matches is unchecked)",
            None => "does not exist",
        };
        let location = if place == f.home {
            "the home directory itself"
        } else if is_inside(&place, &workspace) {
            "inside the workspace"
        } else if temp.iter().any(|t| is_inside(&place, t)) {
            "a temporary directory"
        } else {
            "outside the workspace"
        };
        let git = if meta.is_some() {
            git_state(&place, root.as_deref()).await
        } else {
            "n/a"
        };
        targets.push(json!({
            "path": p, "resolves_to": tildify(&place, f.home), "location": location, "exists": exists, "git": git,
        }));
    }
    let mut state = JsonObject::new();
    state.insert("workspace".into(), json!(tildify(&workspace, f.home)));
    state.insert("call".into(), Value::Object(call));
    if !targets.is_empty() {
        state.insert("targets".into(), Value::Array(targets));
    }
    if let Some(r) = f.user_request.filter(|r| !r.is_empty()) {
        state.insert("user_request".into(), json!(clip(&redact(r), 1500)));
    }
    state
}

// ── the plugin ───────────────────────────────────────────────────────

/// What to do with the verdict, given Jev's reading. None: leave it as it is.
pub fn adjust(verdict: &Verdict, mode: PermissionMode, j: &Judgement) -> Option<PermissionResult> {
    let result = |decision: Decision, reason: String| {
        Some(PermissionResult {
            decision: Some(decision),
            reason: Some(reason),
        })
    };
    let (decision, source, reason) = (verdict.decision, verdict.source, &verdict.reason);
    match decision {
        Decision::Allow => match j.reading {
            Reading::Dangerous => result(
                Decision::Deny,
                L(
                    format!("{}。人の確認なしに実行されるはずだった呼び出しを止めました", j.summary),
                    format!(
                        "{}. Stopped a call that would have run without asking anyone",
                        j.summary
                    ),
                ),
            ),
            Reading::Risky => result(Decision::Ask, j.summary.clone()),
            _ => None,
        },
        Decision::Ask => {
            let agent_mode = source == VerdictSource::Mode && mode == PermissionMode::Agent;
            if j.reading == Reading::Safe && agent_mode {
                return result(Decision::Allow, j.summary.clone());
            }
            // Undecided agent-mode calls fall through to the model judge.
            if j.reading == Reading::Unsure && agent_mode {
                return None;
            }
            result(Decision::Ask, format!("{reason} / {}", j.summary))
        }
        Decision::Deny => (source == VerdictSource::Soft && j.reading == Reading::Safe).then(|| PermissionResult {
            decision: Some(Decision::Ask),
            reason: Some(L(
                format!("{reason} → {}。確認に回しました", j.summary),
                format!("{reason} → {}. Handed to you to decide", j.summary),
            )),
        }),
    }
}

/// Calls worth a judgement: commands and outside actions; not reads, not edits inside the workspace.
fn judgeable(tool: &dyn Tool, args: &JsonObject, cwd: &str, skip: &[String]) -> bool {
    if skip.iter().any(|s| s == tool.name()) || tool.kind() == ToolKind::Read {
        return false;
    }
    if tool.kind() == ToolKind::Edit {
        let ws = real(cwd);
        return tool
            .paths(args, Path::new(cwd))
            .unwrap_or_default()
            .iter()
            .any(|p| !is_inside(&real(p), &ws));
    }
    true
}

/// Session entry holding what the user last asked, so a resumed session still knows it.
const REQUEST_ENTRY: &str = "jev-guard-request";
/// Session entry: /jev on or off, which outlasts the config's `enabled` for that session.
const ENABLED_ENTRY: &str = "jev-guard-enabled";

/// What the plugin takes from the process; tests replace it.
#[derive(Clone, Default)]
pub struct PluginDeps {
    pub backend: Deps,
    /// `os.homedir()`; the real one when None.
    pub home: Option<String>,
}

#[derive(Default)]
struct Stats {
    calls: u64,
    failures: u64,
    changed: u64,
}

#[derive(Default)]
struct State {
    enabled: bool,
    backend: Option<Result<Arc<dyn Backend>, String>>,
    warned: String,
    user_request: Option<String>,
    cache: IndexMap<String, JevAnswers>,
    stats: Stats,
    recent: Vec<String>,
}

struct Guard {
    api: PluginApi,
    settings: BackendSettings,
    enabled_default: bool,
    thresholds: Thresholds,
    skip: Vec<String>,
    timeout_ms: Option<f64>,
    deps: PluginDeps,
    complete: Completer,
    state: Mutex<State>,
}

impl Guard {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn home(&self) -> String {
        self.deps
            .home
            .clone()
            .unwrap_or_else(|| sasacode_plugin_api::home_dir().to_string_lossy().into_owned())
    }

    /// Named, or the first of Command Code / TypeSafe with a key (Command Code when neither has one,
    /// so the warning names the key to set).
    async fn pick(&self) -> Result<Arc<dyn Backend>, String> {
        let name = match &self.settings.backend {
            Some(b) => b.clone(),
            None => detect_service(&self.deps.backend)
                .await
                .unwrap_or("commandcode")
                .to_string(),
        };
        make_backend(self.complete.clone(), &name, &self.settings, &self.deps.backend).map_err(|e| format!("{e:#}"))
    }

    /// `backend ??= pick()`: a failed pick is kept too, until /jev on picks again.
    async fn backend(&self) -> Result<Arc<dyn Backend>, String> {
        if let Some(b) = &self.lock().backend {
            return b.clone();
        }
        let b = self.pick().await;
        self.lock().backend.get_or_insert(b).clone()
    }

    fn warn(&self, message: &str) {
        {
            let mut s = self.lock();
            s.stats.failures += 1;
            if s.warned == message {
                return;
            }
            s.warned = message.to_string();
        }
        self.api.ui().notify(
            &L(
                format!("jev-guard: {message}（Jev なしで判定を続けます）"),
                format!("jev-guard: {message} (deciding without Jev from here)"),
            ),
            NotifyLevel::Warning,
        );
    }

    async fn ask(&self, state: &JsonObject) -> Option<JevAnswers> {
        let b = match self.backend().await {
            Ok(b) => b,
            Err(e) => {
                self.warn(&e);
                return None;
            }
        };
        let key = format!("{}\0{}", b.label(), stringify(state));
        {
            let mut s = self.lock();
            if let Some(hit) = s.cache.get(&key) {
                return Some(hit.clone());
            }
            s.stats.calls += 1;
        }
        let ms = self.timeout_ms.unwrap_or(if b.name() == BackendName::Chat {
            60_000.0
        } else {
            5000.0
        });
        let state = Value::Object(state.clone());
        // The deadline also cancels what the backend started (the chat model's request), as the
        // TS's AbortSignal.timeout did.
        let deadline = CancellationToken::new();
        let _cancel_on_exit = deadline.clone().drop_guard();
        let limit = Duration::try_from_secs_f64(ms.max(0.0) / 1000.0).unwrap_or(Duration::MAX);
        let asked = tokio::time::timeout(limit, b.ask(&state, questions(), &deadline)).await;
        let answers = match asked {
            Err(_) => Err(anyhow::anyhow!("The operation timed out.")),
            Ok(r) => r.and_then(|raw| to_answers(&raw)),
        };
        match answers {
            Ok(a) => {
                let mut s = self.lock();
                if s.cache.len() >= 256 {
                    s.cache.shift_remove_index(0);
                }
                s.cache.insert(key, a.clone());
                Some(a)
            }
            Err(e) => {
                self.warn(&format!("{}: {e:#}", b.label()));
                None
            }
        }
    }

    async fn status(&self) {
        let ui = self.api.ui();
        if !self.lock().enabled {
            ui.notify(
                &L(
                    "Jev ガード: 無効。/jev on で有効（このセッション）。いつも使うなら config の plugins.settings[\"jev-guard\"] に {\"enabled\": true}",
                    "Jev guard: off. /jev on turns it on (for this session); to have it always, put {\"enabled\": true} in plugins.settings[\"jev-guard\"] in the config",
                ),
                NotifyLevel::Info,
            );
            return;
        }
        let b = self.backend().await;
        let s = self.lock();
        let endpoint = match b {
            Ok(b) => b.label(),
            Err(e) => L(
                format!("接続先の設定エラー: {e}"),
                format!("endpoint setting error: {e}"),
            ),
        };
        let mut lines = vec![
            format!("{} · {endpoint}", L("Jev ガード: 有効", "Jev guard: on")),
            L(
                format!(
                    "問い合わせ {} 回 · 判定を変えた {} 回 · 失敗 {} 回",
                    s.stats.calls, s.stats.changed, s.stats.failures
                ),
                format!(
                    "{} checks · {} decisions changed · {} failures",
                    s.stats.calls, s.stats.changed, s.stats.failures
                ),
            ),
        ];
        lines.extend(s.recent.iter().skip(s.recent.len().saturating_sub(5)).cloned());
        drop(s);
        ui.notify(&lines.join("\n"), NotifyLevel::Info);
    }

    async fn command(&self, args: &str) -> anyhow::Result<()> {
        let arg = js_trim(args).to_lowercase();
        let ui = self.api.ui();
        if arg.is_empty() || arg == "status" {
            self.status().await;
            return Ok(());
        }
        if arg != "on" && arg != "off" {
            ui.notify(
                &L("使い方: /jev [on | off]", "usage: /jev [on | off]"),
                NotifyLevel::Warning,
            );
            return Ok(());
        }
        if arg == "off" {
            self.lock().enabled = false;
            self.api.session().append(ENABLED_ENTRY, json!(false))?;
            ui.notify(
                &L(
                    "Jev ガードを無効にしました（このセッション）",
                    "Jev guard is off (for this session)",
                ),
                NotifyLevel::Info,
            );
            return Ok(());
        }
        // A backend without a key would only fail on every call: say so now, and stay off.
        {
            let mut s = self.lock();
            s.backend = None;
            s.warned.clear();
        }
        let b = self.pick().await;
        self.lock().backend = Some(b.clone());
        let problem = match &b {
            Err(e) => Some(e.clone()),
            Ok(b) => b.problem().await,
        };
        let b = match (problem, b) {
            (None, Ok(b)) => b,
            (problem, _) => {
                self.lock().backend = None;
                let problem = problem.unwrap_or_default();
                ui.notify(
                    &L(
                        format!("Jev ガードを有効にできません: {problem}"),
                        format!("Cannot turn Jev guard on: {problem}"),
                    ),
                    NotifyLevel::Warning,
                );
                return Ok(());
            }
        };
        self.lock().enabled = true;
        self.api.session().append(ENABLED_ENTRY, json!(true))?;
        let label = b.label();
        ui.notify(
            &L(
                format!("Jev ガードを有効にしました（このセッション · {label}）"),
                format!("Jev guard is on (for this session · {label})"),
            ),
            NotifyLevel::Info,
        );
        Ok(())
    }
}

fn settings_of(s: &Value) -> (BackendSettings, Thresholds, Vec<String>, Option<f64>, bool) {
    let string = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_string);
    let settings = BackendSettings {
        // Whatever is set is the name, as `makeBackend` was given it.
        backend: s.get("backend").filter(|v| !v.is_null()).map(js_string),
        model: string("model"),
        endpoint: string("endpoint"),
        api_key_env: string("apiKeyEnv"),
    };
    let mut t = DEFAULT_THRESHOLDS;
    if let Some(o) = s.get("thresholds").and_then(Value::as_object) {
        for (k, v) in o {
            let Some(v) = v.as_f64() else { continue };
            match k.as_str() {
                "allow" => t.allow = v,
                "headlessAllow" => t.headless_allow = v,
                "deny" => t.deny = v,
                "confidence" => t.confidence = v,
                "risk" => t.risk = v,
                "flag" => t.flag = v,
                "clear" => t.clear = v,
                _ => {}
            }
        }
    }
    let skip = match s.get("skip").and_then(Value::as_array) {
        Some(a) => a.iter().map(js_string).collect(),
        None => vec!["todo_write".into(), "task".into()],
    };
    let timeout = s.get("timeoutMs").and_then(Value::as_f64);
    (settings, t, skip, timeout, s.get("enabled") == Some(&Value::Bool(true)))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    plugin_with(api, PluginDeps::default())
}

/// [`plugin`] with what it takes from the process (transport, environment, keychain, home) given.
pub fn plugin_with(api: PluginApi, deps: PluginDeps) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        set_lang(api.ui().lang());
        let (settings, thresholds, skip, timeout_ms, enabled_default) = settings_of(&api.settings());
        let guard = Arc::new(Guard {
            complete: completer(&api),
            api: api.clone(),
            settings,
            // The config's `enabled` is the default; /jev on|off decides for the session (and a resumed one).
            enabled_default,
            thresholds,
            skip,
            timeout_ms,
            deps,
            state: Mutex::new(State {
                enabled: enabled_default,
                ..State::default()
            }),
        });

        let g = guard.clone();
        api.register_command(
            Command::new(
                "jev",
                &L(
                    "Jev ガード（実行前の安全判断）の状態と有効・無効",
                    "Jev guard (a safety check before each call): status, on and off",
                ),
                move |c| {
                    let g = g.clone();
                    async move { g.command(&c.args).await }
                },
            )
            .hint("[on | off]")
            .complete(|prefix| {
                let out: Vec<SelectOption> = [
                    ("on", L("このセッションで有効にする", "turn it on for this session")),
                    ("off", L("このセッションで無効にする", "turn it off for this session")),
                ]
                .into_iter()
                .filter(|(v, _)| v.starts_with(&prefix))
                .map(|(v, d)| SelectOption::new(v, v).describe(&d))
                .collect();
                async move { out }
            }),
        );

        // The user's own words only: user_prompt fires for what the user typed, not for messages plugins
        // inject (a background job's output, a harness note), which must not pass for the user's request.
        let g = guard.clone();
        api.on::<hook::SessionStart, _, _>(move |_e, _c| {
            let session = g.api.session();
            let last = session.entries(REQUEST_ENTRY).pop();
            let chosen = session.entries(ENABLED_ENTRY).pop();
            let mut s = g.lock();
            s.user_request = last.and_then(|v| v.as_str().map(str::to_string));
            s.enabled = chosen.and_then(|v| v.as_bool()).unwrap_or(g.enabled_default);
            async { Ok(None) }
        });

        let g = guard.clone();
        api.on::<hook::UserPrompt, _, _>(move |ev, _c| {
            let text = ev
                .content
                .iter()
                .filter_map(UserContent::as_text)
                .collect::<Vec<_>>()
                .join("\n");
            let outcome = (|| {
                let mut s = g.lock();
                if js_trim(&text).is_empty() || s.user_request.as_deref() == Some(text.as_str()) {
                    return Ok(());
                }
                s.user_request = Some(text.clone());
                drop(s);
                // Kept while off too, so /jev on in the middle of a task knows what was asked.
                g.api.session().append(REQUEST_ENTRY, json!(text))
            })();
            async move { outcome.map(|()| None) }
        });

        let g = guard;
        api.on::<hook::Permission, _, _>(move |ev, _c| {
            let g = g.clone();
            async move {
                let user_request = {
                    let s = g.lock();
                    if !s.enabled {
                        return Ok(None);
                    }
                    s.user_request.clone()
                };
                let (decision, source) = (ev.verdict.decision, ev.verdict.source);
                // A hard deny stays; nothing Jev says can change it.
                if decision == Decision::Deny && source != VerdictSource::Soft {
                    return Ok(None);
                }
                let cwd = ev.cwd.to_string_lossy().into_owned();
                if !judgeable(ev.tool.as_ref(), &ev.args, &cwd, &g.skip) {
                    return Ok(None);
                }
                let home = g.home();
                let state = build_state(&CallFacts {
                    tool: ev.tool.as_ref(),
                    args: &ev.args,
                    cwd: &cwd,
                    user_request: user_request.as_deref(),
                    home: &home,
                })
                .await;
                let Some(answers) = g.ask(&state).await else {
                    return Ok(None);
                };
                let j = interpret(&answers, &g.thresholds, g.api.ui().interactive());
                let result = adjust(&ev.verdict, ev.mode, &j);
                let call = state.get("call").cloned().unwrap_or(Value::Null);
                let label = match call.get("command") {
                    Some(c) => js_string(c),
                    None => ev.tool.name().to_string(),
                };
                let changed = result.as_ref().and_then(|r| r.decision).filter(|d| *d != decision);
                {
                    let mut s = g.lock();
                    let shift = changed
                        .map(|d| format!("（{} → {}）", decision.as_str(), d.as_str()))
                        .unwrap_or_default();
                    s.recent.push(format!("{} → {}{shift}", clip(&label, 60), j.summary));
                    if s.recent.len() > 20 {
                        s.recent.remove(0);
                    }
                    if changed.is_some() {
                        s.stats.changed += 1;
                    }
                }
                let mut entry = json!({
                    "tool": ev.tool.name(), "call": call, "verdict": ev.verdict,
                    "answers": answers, "reading": j.reading.as_str(),
                });
                if let (Some(r), Some(o)) = (&result, entry.as_object_mut()) {
                    let mut out = JsonObject::new();
                    if let Some(d) = r.decision {
                        out.insert("decision".into(), json!(d.as_str()));
                    }
                    if let Some(reason) = &r.reason {
                        out.insert("reason".into(), json!(reason));
                    }
                    o.insert("result".into(), Value::Object(out));
                }
                g.api.session().append("judgement", entry)?;
                Ok(result)
            }
        });
        Ok(())
    })
}
