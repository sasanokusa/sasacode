//! background-sessions (`packages/bundled/src/background-sessions.ts`) — run long CLI tasks in the
//! background and get re-fired when they finish.
//!
//! Instead of the model polling a long build/test/train command, `bg_start` returns immediately
//! and, when the command exits, a notification is injected into the session (`session.inject "now"`,
//! which starts a run if the agent is idle) with the exit code and the tail of the output.
//!
//! Jobs are detached processes with their own process group, so they outlive sasacode itself; the
//! plugin adopts still-running jobs on the next start and notifies about the ones that finished
//! while it was away. State lives in `~/.sasacode/background-sessions/<id>/` (meta.json, output.log,
//! exit-code; under SASACODE_HOME when that is set).
//!
//! Security: bg_start declares `permissions_as("bash")`, so it is judged by the rules written for
//! bash — the user's `bash(sudo *)` deny/ask rules, the guard preset, `bash(...)` allow rules —
//! matched against its command. It cannot become a way around them, and rules under its own name
//! (`bg_start(make*)`) still apply.
//!
//! Settings (`plugins.settings.background-sessions`): dir (storage path), pollMs (liveness check),
//! tailLines (lines of output in the completion notice), keep (finished jobs retained),
//! autoNotify (default true — set false to only show a UI notification without re-firing the agent),
//! foreignGraceMs (how long the session that started a job gets to claim its completion notice
//! before another session reports it).
//!
//! Runs jobs with bash: Git Bash on Windows, where a job is stopped by ending its process tree
//! (taskkill) instead of signalling its process group. Tool descriptions and results are English;
//! the system-prompt addition and the completion notice injected into the session are Japanese
//! (both are read by the model); notices shown to the user go through t().

use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use sasacode_ai::json::{js_number, stringify_pretty};
use sasacode_host::t;
use sasacode_plugin_api::{
    Command, CommandContext, InjectWhen, NotifyLevel, PermissionRules, PluginApi, SelectOption, ToolDef, ToolKind,
    ToolResult, apply_child_env, create_private_dir, detach_session, error_result, hook, redact_secrets, sasacode_home,
};
use sasacode_tools::{KillSignal, bash_path, kill_tree};

use crate::net_util::{js_num_str, js_round, js_trim, js_words, utf16_len};

const DEFAULT_POLL_MS: f64 = 1500.0;
const DEFAULT_TAIL_LINES: f64 = 30.0;
const DEFAULT_KEEP: f64 = 40.0;
const DEFAULT_FOREIGN_GRACE_MS: f64 = 10_000.0;
const MAX_LOG_BYTES: u64 = 32 * 1024 * 1024; // larger logs: read them with bash tail/sed instead
const TAIL_READ_BYTES: u64 = 256 * 1024;
const DEFAULT_PAGE_LINES: f64 = 200.0;
const MAX_PAGE_LINES: f64 = 1000.0;

/// A numeric setting or argument: a finite number ≥ `min`, else `d`.
fn num(v: Option<&Value>, d: f64, min: f64) -> f64 {
    match v.and_then(Value::as_f64) {
        Some(x) if x.is_finite() && x >= min => x,
        _ => d,
    }
}

fn now() -> f64 {
    sasacode_ai::now_ms() as f64
}

fn fmt_dur(ms: f64) -> String {
    let s = js_round(ms / 1000.0).max(0.0) as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m{:02}s", s % 60);
    }
    format!("{}h{:02}m", m / 60, m % 60)
}

/// One job as meta.json keeps it. `exit_code`: absent (None), null (Some(None)) or a number.
#[derive(Debug, Clone, Default, PartialEq)]
struct Meta {
    id: String,
    command: String,
    cwd: String,
    run_cwd: String,
    pid: f64,
    started_at: f64,
    status: String,
    /// Session that started the job; its completion notice goes there first.
    session_id: Option<String>,
    exit_code: Option<Option<f64>>,
    finished_at: Option<f64>,
    notified: Option<bool>,
    killed: Option<bool>,
    spawn_error: Option<String>,
}

impl Meta {
    /// Read leniently, as `JSON.parse(…) as Meta` does: what is missing or of another type is left out.
    fn from_value(v: &Value) -> Meta {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let n = |k: &str| v.get(k).and_then(Value::as_f64);
        Meta {
            id: s("id").unwrap_or_default(),
            command: s("command").unwrap_or_default(),
            cwd: s("cwd").unwrap_or_default(),
            run_cwd: s("runCwd").unwrap_or_default(),
            pid: n("pid").unwrap_or(0.0),
            started_at: n("startedAt").unwrap_or(0.0),
            status: s("status").unwrap_or_default(),
            session_id: s("sessionId"),
            exit_code: match v.get("exitCode") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(x) => Some(x.as_f64()),
            },
            finished_at: n("finishedAt"),
            notified: v.get("notified").and_then(Value::as_bool),
            killed: v.get("killed").and_then(Value::as_bool),
            spawn_error: s("spawnError"),
        }
    }

    /// The fields in meta.json's order, the optional ones only when set.
    fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("command".into(), json!(self.command));
        m.insert("cwd".into(), json!(self.cwd));
        m.insert("runCwd".into(), json!(self.run_cwd));
        m.insert("pid".into(), js_number(self.pid));
        m.insert("startedAt".into(), js_number(self.started_at));
        m.insert("status".into(), json!(self.status));
        if let Some(id) = &self.session_id {
            m.insert("sessionId".into(), json!(id));
        }
        if let Some(code) = self.exit_code {
            m.insert("exitCode".into(), code.map(js_number).unwrap_or(Value::Null));
        }
        if let Some(at) = self.finished_at {
            m.insert("finishedAt".into(), js_number(at));
        }
        if let Some(n) = self.notified {
            m.insert("notified".into(), json!(n));
        }
        if let Some(k) = self.killed {
            m.insert("killed".into(), json!(k));
        }
        if let Some(e) = &self.spawn_error {
            m.insert("spawnError".into(), json!(e));
        }
        Value::Object(m)
    }

    fn exit_text(&self) -> Option<String> {
        self.exit_code.flatten().map(js_num_str)
    }
}

#[derive(Debug, Clone)]
struct Job {
    meta: Meta,
    dir: PathBuf,
    log_path: PathBuf,
    exit_path: PathBuf,
    /// Started by this process (its exit is seen directly), and whether it has exited.
    has_child: bool,
    child_exited: bool,
}

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}
fn log_path_of(dir: &Path) -> PathBuf {
    dir.join("output.log")
}
fn exit_path_of(dir: &Path) -> PathBuf {
    dir.join("exit-code")
}

fn to_job(meta: Meta, dir: &Path) -> Job {
    Job {
        meta,
        dir: dir.to_path_buf(),
        log_path: log_path_of(dir),
        exit_path: exit_path_of(dir),
        has_child: false,
        child_exited: false,
    }
}

fn read_raw_meta(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(meta_path(dir)).ok()?).ok()
}

/// Replace meta.json whole: written to a file of its own, then renamed over it. Two sessions writing
/// it at once with `fs::write` could leave the tail of the longer text after the shorter one (both
/// truncate, then both write), and the job vanished from the list and from the next start.
fn replace_meta(dir: &Path, text: &str) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("meta.json.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, meta_path(dir)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn write_meta(job: &mut Job) -> std::io::Result<()> {
    // Another sasacode in this directory may have reported it meanwhile: written back without it, the next start
    // reported the job again (#121).
    if job.meta.notified.is_none() && read_meta(&job.dir).and_then(|m| m.notified) == Some(true) {
        job.meta.notified = Some(true);
    }
    replace_meta(&job.dir, &stringify_pretty(&job.meta.to_value()))?;
    // That check is not enough on its own: the reporter's write can land between our read and our write, and
    // ours then drops the flag. A reporter creates `claimed` before it writes, so once ours is down, `claimed`
    // shows whether we may have written over one: then write it back with the flag.
    if job.meta.notified.is_none() && job.dir.join("claimed").is_dir() {
        job.meta.notified = Some(true);
        replace_meta(&job.dir, &stringify_pretty(&job.meta.to_value()))?;
    }
    Ok(())
}

fn read_meta(dir: &Path) -> Option<Meta> {
    read_raw_meta(dir)
        .filter(Value::is_object)
        .map(|v| Meta::from_value(&v))
}

/// `parseInt(text.trim(), 10)`, None for NaN.
fn parse_int(text: &str) -> Option<f64> {
    let t = js_trim(text);
    let (sign, digits) = match t.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, t.strip_prefix('+').unwrap_or(t)),
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    digits[..end].parse::<f64>().ok().map(|v| sign * v)
}

fn read_exit_code(exit_path: &Path) -> Option<f64> {
    parse_int(&std::fs::read_to_string(exit_path).ok()?)
}

fn mtime_of(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64() * 1000.0)
}

/// Last `lines` lines of a log, reading at most TAIL_READ_BYTES from the end.
fn tail_of(path: &Path, lines: usize) -> String {
    let read = || -> std::io::Result<(Vec<u8>, u64)> {
        let mut f = std::fs::File::open(path)?;
        let size = f.metadata()?.len();
        let start = size.saturating_sub(TAIL_READ_BYTES);
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        f.take(size - start).read_to_end(&mut buf)?;
        Ok((buf, start))
    };
    let Ok((buf, start)) = read() else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&buf);
    let mut s: &str = &text;
    if start > 0
        && let Some(i) = s.find('\n')
    {
        s = &s[i + 1..]; // drop the partial first line
    }
    let mut ls: Vec<&str> = s.split('\n').collect();
    if ls.last() == Some(&"") {
        ls.pop();
    }
    redact_secrets(&ls[ls.len().saturating_sub(lines)..].join("\n")).into_owned()
}

/// A finished job's log with the values sasacode hides replaced. The job writes its log by itself,
/// so nothing can filter it as it is written; what the model is shown is filtered as it is read.
fn redact_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_LOG_BYTES)
        && let Ok(bytes) = std::fs::read(path)
    {
        let text = String::from_utf8_lossy(&bytes);
        let redacted = redact_secrets(&text);
        if redacted != text {
            let _ = std::fs::write(path, redacted.as_bytes());
        }
    }
}

/// Whether a process exists (`process.kill(pid, 0)`, EPERM counting as alive).
async fn is_alive(pid: f64) -> bool {
    if pid <= 0.0 || !pid.is_finite() {
        return false;
    }
    let pid = (pid as u64).to_string();
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("tasklist");
        c.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("ps");
        c.args(["-p", &pid, "-o", "pid="]);
        c
    };
    let out = cmd
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;
    match out {
        Ok(out) if cfg!(windows) => String::from_utf8_lossy(&out.stdout).contains(&pid),
        Ok(out) => out.status.success() && !js_trim(&String::from_utf8_lossy(&out.stdout)).is_empty(),
        Err(_) => false,
    }
}

/// When a process started (ms since the epoch): /proc on Linux, PowerShell on Windows, ps elsewhere. None if unknown.
async fn process_start_ms(pid: f64) -> Option<f64> {
    let pid = (pid as u64).to_string();
    if cfg!(windows) {
        let script = format!("(Get-Process -Id {pid}).StartTime.ToUniversalTime().ToString('o')");
        let run = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        // Bounded: PowerShell can be slow to start, or hang where its environment is incomplete.
        let out = tokio::time::timeout(Duration::from_secs(10), run).await.ok()?.ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return chrono::DateTime::parse_from_rfc3339(&text)
            .ok()
            .map(|d| d.timestamp_millis() as f64);
    }
    if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = stat.get(stat.rfind(')')? + 2..)?;
        let ticks: f64 = after.split(' ').nth(19)?.parse().ok()?;
        let boot: f64 = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        return (boot != 0.0).then_some((boot + ticks / 100.0) * 1000.0);
    }
    let out = tokio::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    // `Wed Nov 18 10:00:00 2026`, in local time.
    let text = js_words(&String::from_utf8_lossy(&out.stdout)).join(" ");
    let at = chrono::NaiveDateTime::parse_from_str(&text, "%a %b %d %H:%M:%S %Y").ok()?;
    use chrono::TimeZone;
    chrono::Local
        .from_local_datetime(&at)
        .earliest()
        .map(|d| d.timestamp_millis() as f64)
}

/// jobAlive's verdicts on whether a job's pid is still the job, by pid and start.
static SAME_PROCESS: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a job from an earlier run of sasacode is still running. Its exit-code file is written
/// when it ends, which settles it; otherwise the pid must be alive and have started when the job
/// did (a pid reused while sasacode was closed belongs to another process).
async fn job_alive(pid: f64, started_at: f64, exit_path: &Path) -> bool {
    if exit_path.exists() || !is_alive(pid).await {
        return false;
    }
    // A process's start time does not change: asked once per job, not on every poll.
    let key = format!("{}@{}", js_num_str(pid), js_num_str(started_at));
    let known = SAME_PROCESS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .copied();
    if let Some(same) = known {
        return same;
    }
    let same = match process_start_ms(pid).await {
        // Unknown: taken for the job, but asked again on the next polls. Kept on the first miss, a
        // PowerShell that timed out once on a busy machine left a reused pid's job running for good.
        None => {
            let mut misses = START_MISSES.lock().unwrap_or_else(|e| e.into_inner());
            let n = misses.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n < START_ATTEMPTS {
                return true;
            }
            true
        }
        Some(started) => (started - started_at).abs() < 10_000.0,
    };
    SAME_PROCESS.lock().unwrap_or_else(|e| e.into_inner()).insert(key, same);
    same
}

/// How often a job's start time is asked for before an unknown answer is kept.
const START_ATTEMPTS: u32 = 3;
static START_MISSES: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Send `signal` to the job's process group, else to the process itself. Windows: the whole tree ends now.
fn signal(pid: f64, sig: KillSignal) -> bool {
    let Ok(pid) = u32::try_from(pid as i64) else {
        return false;
    };
    if cfg!(windows) {
        return kill_tree(pid, sig);
    }
    if kill_tree(pid, sig) {
        return true;
    }
    let name = match sig {
        KillSignal::Term => "-TERM",
        KillSignal::Kill => "-KILL",
    };
    std::process::Command::new("kill")
        .args([name, &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `a.localeCompare(b, undefined, { numeric: true })` for job ids: digit runs compare as numbers.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let chunks = |s: &str| -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, run)) if *d == digit => run.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    };
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let o = if x.0 && y.0 {
            let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
            nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny))
        } else {
            x.1.to_lowercase().cmp(&y.1.to_lowercase())
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ca.len().cmp(&cb.len())
}

/// What a finished job tells the user and the session, delivered once no lock is held.
struct Notice {
    message: String,
    level: NotifyLevel,
    inject: Option<String>,
}

struct Settings {
    root: PathBuf,
    poll_ms: f64,
    tail_lines: f64,
    keep: f64,
    foreign_grace_ms: f64,
    auto_notify: bool,
}

struct Bg {
    api: PluginApi,
    s: Settings,
    jobs: Mutex<IndexMap<String, Job>>,
}

impl Bg {
    fn jobs(&self) -> MutexGuard<'_, IndexMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cwd(&self) -> String {
        self.api.cwd().display().to_string()
    }

    // ── storage ────────────────────────────────────────────────────────

    fn disk_metas(&self) -> Vec<(Meta, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.s.root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|n| {
                let dir = self.s.root.join(n);
                read_meta(&dir).filter(|m| !m.id.is_empty()).map(|m| (m, dir))
            })
            .collect()
    }

    fn save(&self, job: &mut Job) {
        if let Err(e) = write_meta(job) {
            self.api.log(&format!(
                "background-sessions: cannot write meta for {}: {e}",
                job.meta.id
            ));
        }
    }

    // ── lifecycle ──────────────────────────────────────────────────────

    fn update_status(&self) {
        let running = self.jobs().values().filter(|j| j.meta.status == "running").count();
        let text = (running > 0).then(|| t!("background-sessions: {n} 実行中", n = running));
        self.api.ui().set_status("bg", text);
    }

    /// Mark a job finished and record it; the notice to deliver when this session reports it now.
    /// `code` None reads the exit-code file. Foreign jobs' notices are scheduled instead.
    fn finalize(self: &Arc<Self>, job: &mut Job, code: Option<f64>) -> Option<Notice> {
        if job.meta.status != "running" {
            return None;
        }
        let fresh = read_meta(&job.dir);
        let killed = job.meta.killed == Some(true) || fresh.and_then(|f| f.killed) == Some(true);
        job.meta.status = if killed { "killed" } else { "done" }.to_string();
        // For a job adopted after its exit, the exit-code file's mtime is when it really finished.
        if job.meta.finished_at.is_none() {
            job.meta.finished_at = Some(if job.has_child {
                now()
            } else {
                mtime_of(&job.exit_path)
                    .or_else(|| mtime_of(&job.log_path))
                    .unwrap_or_else(now)
            });
        }
        job.meta.exit_code = Some(code.or_else(|| read_exit_code(&job.exit_path)));
        if job.meta.spawn_error.as_deref().is_some_and(|e| !e.is_empty()) {
            job.meta.exit_code = Some(None);
        }
        redact_log(&job.log_path);
        self.save(job); // the result is on disk even if nobody gets to notify
        self.notify_once(job)
    }

    /// Report a finished job exactly once (across processes and sessions: `claimed` is created
    /// atomically) with a UI notice plus `session.inject(..., "now")`, which re-fires the agent when
    /// it is idle. The session that started the job reports it immediately; other sessions wait
    /// `foreignGraceMs` first, so the notice lands in the session that owns the job.
    fn notify_once(self: &Arc<Self>, job: &mut Job) -> Option<Notice> {
        let owner = job.meta.session_id.clone().filter(|o| !o.is_empty());
        let me = self.api.session().id().filter(|s| !s.is_empty());
        let foreign = matches!((&owner, &me), (Some(o), Some(m)) if o != m);
        if !foreign {
            return self.claim(job, false);
        }
        // The session that started the job gets first refusal; we only clean up if it is gone.
        let weak = Arc::downgrade(self);
        let snapshot = job.clone();
        let grace = Duration::from_secs_f64(self.s.foreign_grace_ms / 1000.0);
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let Some(bg) = weak.upgrade() else { return };
            let notice = {
                let mut jobs = bg.jobs();
                match jobs.get_mut(&snapshot.meta.id) {
                    Some(job) => bg.claim(job, true),
                    None => {
                        drop(jobs);
                        let mut job = snapshot;
                        bg.claim(&mut job, true)
                    }
                }
            };
            bg.deliver(notice);
        });
        None
    }

    fn claim(&self, job: &mut Job, foreign: bool) -> Option<Notice> {
        if std::fs::create_dir(job.dir.join("claimed")).is_err() {
            return None; // atomic: one notifier across sasacode processes
        }
        job.meta.notified = Some(true);
        self.save(job);
        let m = &job.meta;
        let elapsed = fmt_dur(m.finished_at.unwrap_or_else(now) - m.started_at);
        // A stopped job is "killed" whatever its wrapper managed to record (macOS's bash 3.2 can still
        // write an exit code, 0 even, as the TERM arrives).
        let killed = m.status == "killed" || m.exit_code.flatten().is_none();
        let spawn_error = m.spawn_error.clone().filter(|e| !e.is_empty());
        let badge = match &spawn_error {
            Some(e) => format!("起動に失敗しました ({e})"),
            None if killed => "強制終了しました".to_string(),
            None => format!("exit {}", m.exit_text().unwrap_or_default()),
        };
        let shown = match &spawn_error {
            Some(e) => t!("起動に失敗しました ({error})", error = e),
            None if killed => t!("強制終了しました"),
            None => badge.clone(),
        };
        let message = t!(
            "バックグラウンドジョブ {id} が終了: `{command}` — {badge}、実行時間 {elapsed}",
            id = m.id,
            command = m.command,
            badge = shown,
            elapsed = elapsed
        );
        let level = if m.exit_code.flatten() == Some(0.0) && !killed {
            NotifyLevel::Info
        } else {
            NotifyLevel::Warning
        };
        let inject = self.s.auto_notify.then(|| {
            let tail = tail_of(&job.log_path, self.s.tail_lines as usize);
            let owner = m.session_id.clone().unwrap_or_default();
            [
                "[background-sessions] バックグラウンドジョブの終了通知".to_string(),
                if foreign {
                    format!("（このジョブは別のセッション {owner} で開始されたものです）")
                } else {
                    String::new()
                },
                format!(
                    "{}: `{}` (作業ディレクトリ {}) — {badge}、実行時間 {elapsed}。",
                    m.id, m.command, m.run_cwd
                ),
                format!("出力ログ: {}", job.log_path.display()),
                format!(
                    "出力の末尾:\n---\n{}\n---",
                    if tail.is_empty() {
                        "(出力はありません)"
                    } else {
                        &tail
                    }
                ),
                "この結果を確認し、必要なら修正・再実行・完了報告など次の対応を続けてください。".to_string(),
            ]
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
        });
        Some(Notice { message, level, inject })
    }

    fn deliver(&self, notice: Option<Notice>) {
        let Some(n) = notice else { return };
        self.update_status();
        self.api.ui().notify(&n.message, n.level);
        if let Some(text) = n.inject {
            self.api.session().inject_text(&text, InjectWhen::Now);
        }
    }

    /// Finalize the job `id` in the table (when it is still running there), then report it.
    fn finalize_id(self: &Arc<Self>, id: &str, code: Option<f64>) {
        let notice = {
            let mut jobs = self.jobs();
            match jobs.get_mut(id) {
                Some(job) => {
                    if job.has_child {
                        job.child_exited = true;
                    }
                    self.finalize(job, code)
                }
                None => None,
            }
        };
        self.update_status();
        self.deliver(notice);
    }

    async fn check_running(self: &Arc<Self>) {
        let watched: Vec<(String, f64, f64, PathBuf)> = self
            .jobs()
            .values()
            // A job this process started is finalized by its exit handler.
            .filter(|j| j.meta.status == "running" && !j.has_child)
            .map(|j| (j.meta.id.clone(), j.meta.pid, j.meta.started_at, j.exit_path.clone()))
            .collect();
        for (id, pid, started_at, exit_path) in watched {
            if !job_alive(pid, started_at, &exit_path).await {
                self.finalize_id(&id, None);
            }
        }
    }

    /// Keep at most `keep` finished job directories.
    fn prune(&self) {
        let mut finished: Vec<(Meta, PathBuf)> = self
            .disk_metas()
            .into_iter()
            .filter(|(m, _)| m.status != "running")
            .collect();
        finished.sort_by(|a, b| b.0.started_at.partial_cmp(&a.0.started_at).unwrap_or(Ordering::Equal));
        for (_, dir) in finished.into_iter().skip(self.s.keep as usize) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    // ── helpers shared by the tools ────────────────────────────────────

    /// The job by id, when it was started from this working directory: another project's job (its
    /// command, its log) is not one the tools here may read or stop.
    fn find_job(&self, id: &str) -> Option<(Meta, PathBuf, bool)> {
        let cwd = self.cwd();
        self.find_job_any(id).filter(|(m, _, _)| m.cwd == cwd)
    }

    /// The job by id from any working directory: this process's (with its table entry) or one on
    /// disk. For the user's own `/bg <id>`, never for a tool.
    fn find_job_any(&self, id: &str) -> Option<(Meta, PathBuf, bool)> {
        if let Some(job) = self.jobs().get(id) {
            return Some((job.meta.clone(), job.dir.clone(), true));
        }
        self.disk_metas()
            .into_iter()
            .find(|(m, _)| m.id == id)
            .map(|(m, d)| (m, d, false))
    }

    fn list_rows(&self, all: bool) -> Vec<Meta> {
        let cwd = self.cwd();
        let mut rows: Vec<Meta> = self
            .disk_metas()
            .into_iter()
            .map(|(m, _)| m)
            .filter(|m| all || m.cwd == cwd)
            .collect();
        rows.sort_by(|a, b| a.started_at.partial_cmp(&b.started_at).unwrap_or(Ordering::Equal));
        // include jobs this process started but whose meta write is still racing
        for j in self.jobs().values() {
            if !rows.iter().any(|m| m.id == j.meta.id) {
                rows.push(j.meta.clone());
            }
        }
        rows.sort_by(|a, b| a.started_at.partial_cmp(&b.started_at).unwrap_or(Ordering::Equal));
        rows
    }

    /// SIGTERM to the job's process group, SIGKILL 2 s later if it is still alive. The job is marked
    /// killed on disk first, so whichever process sees it exit reports it as killed. False when the
    /// process was already gone.
    async fn stop(self: &Arc<Self>, m: &Meta, dir: &Path) -> bool {
        let own = {
            let mut jobs = self.jobs();
            match jobs.get_mut(&m.id) {
                Some(job) => {
                    job.meta.killed = Some(true);
                    Some(job.has_child)
                }
                None => None,
            }
        };
        if own.is_none()
            && let Some(Value::Object(mut fresh)) = read_raw_meta(dir)
        {
            fresh.insert("killed".into(), Value::Bool(true));
            let _ = replace_meta(dir, &stringify_pretty(&Value::Object(fresh)));
        }
        // Never signal a process that is not the job's: it ended, and its pid may now be another's.
        if own != Some(true) && !job_alive(m.pid, m.started_at, &exit_path_of(dir)).await {
            if own.is_some() {
                self.finalize_id(&m.id, None);
            }
            return false;
        }
        if !signal(m.pid, KillSignal::Term) {
            if own.is_some() {
                self.finalize_id(&m.id, None);
            }
            return false;
        }
        let pid = m.pid;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if is_alive(pid).await {
                signal(pid, KillSignal::Kill);
            }
        });
        self.update_status();
        true
    }

    fn job_row(&self, m: &Meta) -> String {
        let r#where = if m.run_cwd == m.cwd {
            m.run_cwd.clone()
        } else {
            format!("{} (session {})", m.run_cwd, m.cwd)
        };
        let label = if m.status == "running" {
            "running".to_string()
        } else {
            format!(
                "{}({})",
                if m.status == "killed" { "killed" } else { "done" },
                m.exit_text().unwrap_or_else(|| "?".into())
            )
        };
        let dur = fmt_dur(m.finished_at.unwrap_or_else(now) - m.started_at);
        let pad_end = |s: &str, n: usize| format!("{s}{}", " ".repeat(n.saturating_sub(utf16_len(s))));
        format!(
            "{} {} {}{dur}  `{}`  {where}",
            pad_end(&m.id, 7),
            pad_end(&label, 11),
            " ".repeat(7usize.saturating_sub(utf16_len(&dur))),
            m.command
        )
    }

    // ── tools ──────────────────────────────────────────────────────────

    async fn bg_start(self: &Arc<Self>, args: &sasacode_ai::JsonObject, ctx_cwd: &Path) -> ToolResult {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("").to_string();
        if js_trim(&command).is_empty() {
            return error_result("command is empty; pass the shell command to run");
        }
        let run_cwd = start_dir(args, ctx_cwd).unwrap_or_else(|| ctx_cwd.to_path_buf());
        let shown_cwd = run_cwd.display().to_string();
        match std::fs::metadata(&run_cwd) {
            Ok(m) if !m.is_dir() => return error_result(format!("{shown_cwd} is not a directory; pass a valid cwd")),
            Ok(_) => {}
            Err(_) => {
                return error_result(format!(
                    "working directory {shown_cwd} does not exist; create it first or pass a different cwd"
                ));
            }
        }

        // Claim a job directory (`bg-N`) with mkdir: atomic, so ids stay unique across processes.
        let taken: Vec<u64> = std::fs::read_dir(&self.s.root)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        e.file_name()
                            .to_str()
                            .and_then(|n| n.strip_prefix("bg-"))
                            .map(str::to_string)
                    })
                    .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .filter_map(|n| n.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut n = taken.iter().max().copied().unwrap_or(0) + 1;
        let mut dir = None;
        for _ in 0..1000 {
            let candidate = self.s.root.join(format!("bg-{n}"));
            if std::fs::create_dir(&candidate).is_ok() {
                dir = Some(candidate);
                break;
            }
            n += 1;
        }
        let Some(dir) = dir else {
            return error_result(format!(
                "could not allocate a job directory under {}; check disk space and permissions",
                self.s.root.display()
            ));
        };

        let mut job = to_job(
            Meta {
                id: format!("bg-{n}"),
                command: command.clone(),
                cwd: ctx_cwd.display().to_string(),
                run_cwd: shown_cwd.clone(),
                pid: 0.0,
                started_at: now(),
                status: "running".into(),
                session_id: self.api.session().id(),
                ..Meta::default()
            },
            &dir,
        );
        job.has_child = true;
        let _ = std::fs::write(&job.log_path, "");
        self.save(&mut job);

        // The wrapper records the exit code to a file via `trap … EXIT`, so even a command that
        // leaves with `exit 3` (or sets its own EXIT trap: it runs in its own bash -c) is recorded
        // and completion is detectable after a restart of sasacode. Detached: the job gets its own
        // session and process group and outlives us. The wrapper opens the log itself: on Windows a
        // detached child could not write to a file handle passed in stdio (#128).
        let script =
            "exec >>\"$3\" 2>&1\ntrap 'code=$?; printf \"%s\\n\" \"$code\" > \"$1\"' EXIT\nbash -c \"$2\"\nexit $?";
        let spawned = bash_path().and_then(|bash| {
            let mut cmd = tokio::process::Command::new(bash);
            cmd.args(["-c", script, "bg-job"])
                .arg(&job.exit_path)
                .arg(&command)
                .arg(&job.log_path)
                .current_dir(&run_cwd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            apply_child_env(
                &mut cmd,
                &[
                    ("PAGER".into(), "cat".into()),
                    ("GIT_PAGER".into(), "cat".into()),
                    ("SASACODE".into(), "1".into()),
                ],
            );
            detach_session(&mut cmd);
            Ok(cmd.spawn()?)
        });
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                job.meta.spawn_error = Some(e.to_string());
                let notice = self.finalize(&mut job, None);
                self.deliver(notice);
                return error_result(format!("failed to start `{command}`: {e}"));
            }
        };
        job.meta.pid = child.id().map(f64::from).unwrap_or(0.0);
        self.save(&mut job);
        let (id, pid, log_path) = (job.meta.id.clone(), job.meta.pid, job.log_path.clone());
        self.jobs().insert(id.clone(), job);
        let weak = Arc::downgrade(self);
        let waited = id.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let Some(bg) = weak.upgrade() else { return };
            match status {
                Ok(status) => bg.finalize_id(&waited, status.code().map(f64::from)),
                Err(e) => {
                    if let Some(job) = bg.jobs().get_mut(&waited) {
                        job.meta.spawn_error = Some(e.to_string());
                    }
                    bg.finalize_id(&waited, None);
                }
            }
        });
        self.update_status();

        let mut lines = vec![
            format!("Started {id} in the background (pid {}, in {shown_cwd}):", js_num_str(pid)),
            format!("  $ {command}"),
            format!("Output log: {}", log_path.display()),
            "A notification with the exit code and the output tail arrives here automatically when it finishes — do not poll it (no sleep loops, no repeated bg_output). Continue with other work, or end your turn and wait for the notice.".to_string(),
        ];
        if !self.api.ui().interactive() {
            lines.push("Headless run: when this run ends the process may exit before the notice — it is then delivered at the next sasacode start. If you need the result inside this run, wait in the foreground (e.g. bash `sleep`) and read it with bg_output.".to_string());
        }
        let mut details = sasacode_ai::JsonObject::new();
        details.insert("id".into(), json!(id));
        details.insert("pid".into(), js_number(pid));
        details.insert("logPath".into(), json!(log_path.display().to_string()));
        ToolResult::text(lines.join("\n")).with_details(details)
    }

    fn bg_list(&self, args: &sasacode_ai::JsonObject) -> ToolResult {
        let all = args.get("all").and_then(Value::as_bool) == Some(true);
        let rows = self.list_rows(all);
        if rows.is_empty() {
            let hint = if all {
                ""
            } else {
                " (bg_list all=true shows other directories)"
            };
            return ToolResult::text(format!("No background jobs.{hint}"));
        }
        let mut lines: Vec<String> = rows.iter().map(|m| self.job_row(m)).collect();
        let running = rows.iter().filter(|m| m.status == "running").count();
        lines.push(format!(
            "[{} job(s), {running} running. Running ones notify you automatically when they finish.]",
            rows.len()
        ));
        let mut details = sasacode_ai::JsonObject::new();
        details.insert("jobs".into(), Value::Array(rows.iter().map(Meta::to_value).collect()));
        ToolResult::text(lines.join("\n")).with_details(details)
    }

    fn bg_output(&self, args: &sasacode_ai::JsonObject) -> ToolResult {
        let asked = match args.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => "undefined".into(),
        };
        let Some((meta, dir, _)) = self.find_job(&asked) else {
            return error_result(format!("no such job: {asked}; use bg_list to see the ids"));
        };
        let path = log_path_of(&dir);
        let shown = path.display().to_string();
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_LOG_BYTES => {
                return error_result(format!(
                    "{shown} is over {}MB; read it with bash (tail/sed/grep) instead",
                    MAX_LOG_BYTES / 1024 / 1024
                ));
            }
            Ok(_) => {}
            Err(_) => {
                return error_result(format!(
                    "log of {} is missing ({shown}); the job may have been pruned",
                    meta.id
                ));
            }
        }
        let text = std::fs::read(&path)
            .map(|b| redact_secrets(&String::from_utf8_lossy(&b)).into_owned())
            .unwrap_or_default();
        let mut all: Vec<&str> = text.split('\n').collect();
        if all.last() == Some(&"") {
            all.pop();
        }
        let total = all.len();
        let want = num(args.get("lines"), DEFAULT_PAGE_LINES, 1.0).min(MAX_PAGE_LINES) as usize;
        let running = if meta.status == "running" {
            " (job still running)"
        } else {
            ""
        };
        let from_line = args
            .get("from_line")
            .and_then(Value::as_f64)
            .filter(|f| *f != 0.0 && !f.is_nan());
        let (from, to, note) = match from_line {
            Some(from_line) => {
                let from = (from_line.min(total as f64 + 1.0).max(1.0) as usize) - 1;
                let to = (from + want).min(total);
                let next = if to < total {
                    format!("Use from_line={} to continue.", to + 1)
                } else {
                    "End of log.".into()
                };
                let note = format!(
                    "[Showing lines {}-{to} of {total} of {}'s log{running}. {next} Full log: {shown}]",
                    from + 1,
                    meta.id
                );
                (from, to, note)
            }
            None => {
                let to = total;
                let from = total.saturating_sub(want);
                let back = if from > 0 {
                    let previous = from as i64 + 1 - want as i64;
                    let chunk = if previous > 1 {
                        format!(" (or from_line={previous} for the previous chunk)")
                    } else {
                        String::new()
                    };
                    format!("Use from_line=1 to read from the start{chunk}.")
                } else {
                    String::new()
                };
                let note = format!(
                    "[Showing the last {} of {total} lines of {}'s log{running}. {back} Full log: {shown}]",
                    to - from,
                    meta.id
                );
                (from, to, note)
            }
        };
        let mut details = sasacode_ai::JsonObject::new();
        details.insert("id".into(), json!(meta.id));
        details.insert("status".into(), json!(meta.status));
        details.insert("from".into(), json!(from));
        details.insert("to".into(), json!(to));
        details.insert("total".into(), json!(total));
        ToolResult::text(format!("{}\n{note}", all[from..to].join("\n"))).with_details(details)
    }

    async fn bg_kill(self: &Arc<Self>, args: &sasacode_ai::JsonObject, ctx_cwd: &Path) -> ToolResult {
        let asked = match args.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => "undefined".into(),
        };
        let Some((m, dir, _)) = self.find_job(&asked) else {
            return error_result(format!("no such job: {asked}; use bg_list to see the ids"));
        };
        let cwd = ctx_cwd.display().to_string();
        if m.cwd != cwd {
            return error_result(format!(
                "{} was started from {}, not this working directory ({cwd}); it is not killed here. Open a session in {} to stop it.",
                m.id, m.cwd, m.cwd
            ));
        }
        if m.status != "running" {
            return ToolResult::text(format!(
                "{} is already {} (exit {}); nothing to kill.",
                m.id,
                m.status,
                m.exit_text().unwrap_or_else(|| "unknown".into())
            ));
        }
        let pid = js_num_str(m.pid);
        if !self.stop(&m, &dir).await {
            return error_result(format!(
                "{}'s process (pid {pid}) is already gone; it will be reported as finished momentarily",
                m.id
            ));
        }
        let mut details = sasacode_ai::JsonObject::new();
        details.insert("id".into(), json!(m.id));
        details.insert("pid".into(), js_number(m.pid));
        ToolResult::text(format!(
            "Sent SIGTERM to the process group of {} (pid {pid}); SIGKILL follows in 2s if needed. A notice arrives here automatically once it has exited.",
            m.id
        ))
        .with_details(details)
    }

    // ── command: /bg [id | kill <id>] ──────────────────────────────────

    async fn run_command(self: &Arc<Self>, args: &str) {
        let parts = js_words(args);
        let rows = self.list_rows(false);
        let ui = self.api.ui();
        let Some(first) = parts.first() else {
            let message = if rows.is_empty() {
                t!("バックグラウンドジョブはありません")
            } else {
                let mut lines = vec![t!("バックグラウンドジョブ:")];
                lines.extend(rows.iter().map(|m| self.job_row(m)));
                lines.push(t!("詳細: /bg <id>　停止: /bg kill <id>"));
                lines.join("\n")
            };
            return ui.notify(&message, NotifyLevel::Info);
        };
        if *first == "kill" {
            let id = parts.get(1).copied().unwrap_or("");
            let Some(meta) = rows.iter().find(|m| m.id == id) else {
                return ui.notify(&t!("ジョブが見つかりません: {id}", id = id), NotifyLevel::Warning);
            };
            if meta.status != "running" {
                return ui.notify(
                    &t!(
                        "{id} は既に終了しています ({status})",
                        id = meta.id,
                        status = meta.status
                    ),
                    NotifyLevel::Info,
                );
            }
            let dir = self.find_job(&meta.id).map(|(_, d, _)| d).unwrap_or_default();
            if !self.stop(meta, &dir).await {
                return ui.notify(
                    &t!("{id} のプロセスはもう終了しています", id = meta.id),
                    NotifyLevel::Info,
                );
            }
            return ui.notify(
                &t!(
                    "{id} に SIGTERM を送信しました (pid {pid})",
                    id = meta.id,
                    pid = js_num_str(meta.pid)
                ),
                NotifyLevel::Info,
            );
        }
        let found = rows
            .iter()
            .find(|m| m.id == *first)
            .cloned()
            .or_else(|| self.find_job_any(first).map(|(m, _, _)| m));
        let Some(meta) = found else {
            return ui.notify(&t!("ジョブが見つかりません: {id}", id = *first), NotifyLevel::Warning);
        };
        let dir = self.find_job_any(&meta.id).map(|(_, d, _)| d).unwrap_or_default();
        let tail = tail_of(&log_path_of(&dir), 40);
        let lines = [
            self.job_row(&meta),
            t!("出力ログ: {path}", path = log_path_of(&dir).display()),
            t!("末尾 40 行:"),
            if tail.is_empty() {
                t!("(出力はありません)")
            } else {
                tail
            },
        ];
        ui.notify(&lines.join("\n"), NotifyLevel::Info);
    }
}

/// The directory `bg_start` runs in when the call names one (`~` and relative paths resolved from the
/// session's working directory, as the other tools resolve a path): what the permission check sees
/// and what the command starts in. `None`: the session's own directory.
fn start_dir(args: &sasacode_ai::JsonObject, cwd: &Path) -> Option<PathBuf> {
    let named = args.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty())?;
    Some(PathBuf::from(sasacode_tools::util::resolve_path(named, cwd)))
}

/// What the permission rules see of a `bg_start`: the command, and the directory when it is not the
/// session's own. "Always allow" makes its rule from this, so a command allowed here is not allowed
/// in another directory.
fn start_target(args: &sasacode_ai::JsonObject, session_cwd: &Path) -> String {
    let command = args.get("command").and_then(Value::as_str).unwrap_or("").to_string();
    match start_dir(args, session_cwd) {
        Some(dir) if !same_dir(&dir, session_cwd) => format!("{command} (in {})", dir.display()),
        _ => command,
    }
}

/// Whether two paths are the same directory, as the file system sees them where it can.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// What an approval shows of a `bg_start`: the command, and the directory when it is not the session's.
fn start_summary(args: &sasacode_ai::JsonObject) -> String {
    let command = args.get("command").and_then(Value::as_str).unwrap_or("");
    match args.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
        Some(dir) => format!("{command} (in {dir})"),
        None => command.to_string(),
    }
}

fn job_id_param() -> Value {
    json!({ "type": "string", "description": "Job id from bg_start, e.g. bg-3" })
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let root = match settings.get("dir").and_then(Value::as_str).filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => sasacode_home().join("background-sessions"),
        };
        let s = Settings {
            root,
            poll_ms: num(settings.get("pollMs"), DEFAULT_POLL_MS, 50.0),
            tail_lines: num(settings.get("tailLines"), DEFAULT_TAIL_LINES, 1.0),
            keep: num(settings.get("keep"), DEFAULT_KEEP, 1.0),
            foreign_grace_ms: num(settings.get("foreignGraceMs"), DEFAULT_FOREIGN_GRACE_MS, 0.0),
            auto_notify: settings.get("autoNotify") != Some(&Value::Bool(false)),
        };
        if let Err(e) = create_private_dir(&s.root) {
            api.log(&format!("background-sessions: cannot create {}: {e}", s.root.display()));
        }
        let bg = Arc::new(Bg {
            api: api.clone(),
            s,
            jobs: Mutex::new(IndexMap::new()),
        });

        // The liveness check: ends with the plugin (nothing else holds it then).
        let weak: Weak<Bg> = Arc::downgrade(&bg);
        let poll = Duration::from_secs_f64(bg.s.poll_ms / 1000.0);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll).await;
                let Some(bg) = weak.upgrade() else { return };
                bg.check_running().await;
            }
        });

        // Adopt jobs from previous runs: keep watching the running ones, report the finished ones.
        let cwd = bg.cwd();
        for (meta, dir) in bg.disk_metas() {
            if meta.cwd != cwd {
                continue; // another project's session owns it
            }
            if meta.status != "running" {
                if meta.notified != Some(true) {
                    // It finished while we were away (or a notifier crashed mid-notify): report it now.
                    let _ = std::fs::remove_dir_all(dir.join("claimed"));
                    let code = meta.exit_code.flatten();
                    let mut job = to_job(
                        Meta {
                            status: "running".into(),
                            ..meta
                        },
                        &dir,
                    );
                    let notice = bg.finalize(&mut job, code);
                    bg.deliver(notice);
                }
                continue;
            }
            let job = to_job(meta, &dir);
            let (id, pid, started_at, exit_path) = (
                job.meta.id.clone(),
                job.meta.pid,
                job.meta.started_at,
                job.exit_path.clone(),
            );
            bg.jobs().insert(id.clone(), job);
            if !job_alive(pid, started_at, &exit_path).await {
                bg.finalize_id(&id, None);
            }
        }
        bg.update_status();
        bg.prune();

        // ── model-facing context ───────────────────────────────────────────
        let prompter = bg.clone();
        api.on::<hook::SystemPrompt, _, _>(move |ev, _ctx| {
            // Purely a function of which jobs are running: no timestamps or elapsed times, so the prompt
            // (and with it the provider's prompt cache) only changes when a job starts or finishes.
            let mut running: Vec<Meta> =
                prompter.list_rows(false).into_iter().filter(|m| m.status == "running").collect();
            running.sort_by(|a, b| natural_cmp(&a.id, &b.id));
            let result = (!running.is_empty()).then(|| {
                let lines = running.iter().map(|m| format!("- {}: `{}`", m.id, m.command)).collect::<Vec<_>>().join("\n");
                sasacode_plugin_api::SystemPromptResult {
                    prompt: Some(format!(
                        "{}\n\n## Background jobs (background-sessions plugin)\n実行中のバックグラウンドジョブ:\n{lines}\n終了時は自動的に通知が届きます。ポーリング（sleep ループや繰り返しの確認）はせず、他の作業を進めるかターンを終えてください。",
                        ev.prompt
                    )),
                }
            });
            async move { Ok(result) }
        });

        // ── tools ──────────────────────────────────────────────────────────
        let starter = bg.clone();
        let target_cwd = bg.clone();
        api.register_tool(
            ToolDef::new(
                "bg_start",
                concat!(
                    "Start a long-running shell command in the background and return immediately with a job id. ",
                    "Its combined stdout/stderr goes to a log file read with bg_output. When the command exits you are notified here automatically with its exit code and output tail, so do not poll it. ",
                    "Use for work that takes minutes (builds, test suites, training, downloads, servers you want to keep running)."
                ),
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to run (executed with bash -c)" },
                        "cwd": { "type": "string", "description": "Working directory for the command (default: the session working directory)" },
                    },
                    "required": ["command"],
                    "additionalProperties": false,
                }),
                ToolKind::Exec,
                move |args, ctx| {
                    let bg = starter.clone();
                    async move { Ok(bg.bg_start(&args, &ctx.cwd).await) }
                },
            )
            .concurrent(true)
            // The command, and the directory when it is not the session's: a rule made for the
            // command here does not cover it elsewhere.
            .match_target(move |a| start_target(a, Path::new(&target_cwd.cwd())))
            // The directory it runs in, resolved as the tool resolves it, is a path of the call.
            .paths(|a, cwd| start_dir(a, cwd).map(|d| d.display().to_string()).into_iter().collect())
            // Judged by the rules written for bash (guard preset, the user's bash(sudo *) deny rules,
            // bash(...) allow rules) against the command: no way around them.
            .permissions_as("bash")
            .summary(start_summary)
            .into_ref(),
        );

        let lister = bg.clone();
        let list_root = bg.s.root.display().to_string();
        api.register_tool(
            ToolDef::new(
                "bg_list",
                concat!(
                    "List background jobs started with bg_start (id, status, exit code, elapsed, command). ",
                    "Cheap and non-blocking; use it to see what is still running. Running ones notify you on completion, so this is only for an overview."
                ),
                json!({
                    "type": "object",
                    "properties": { "all": { "type": "boolean", "description": "Include jobs from other working directories (default false)" } },
                    "additionalProperties": false,
                }),
                ToolKind::Read,
                move |args, _ctx| {
                    let bg = lister.clone();
                    async move { Ok(bg.bg_list(&args)) }
                },
            )
            .concurrent(true)
            .paths(move |_a, _cwd| vec![list_root.clone()])
            // What the rule below matches: a listing of other projects' jobs (their commands) asks.
            .match_target(|a| if a.get("all").and_then(Value::as_bool) == Some(true) { "all" } else { "" }.to_string())
            .summary(|_a| "background jobs".to_string())
            .into_ref(),
        );

        let reader = bg.clone();
        let output_root = bg.s.root.display().to_string();
        api.register_tool(
            ToolDef::new(
                "bg_output",
                concat!(
                    "Read the combined stdout/stderr log of a background job started with bg_start. ",
                    "Returns the tail by default; pass from_line to page forward from an earlier line. Returns [Showing x-y of z lines] markers when cut."
                ),
                json!({
                    "type": "object",
                    "properties": {
                        "id": job_id_param(),
                        "from_line": { "type": "integer", "minimum": 1, "description": "1-based first line to return (default: the tail)" },
                        "lines": { "type": "integer", "minimum": 1, "description": format!("How many lines to return (default {}, max {})", js_num_str(DEFAULT_PAGE_LINES), js_num_str(MAX_PAGE_LINES)) },
                    },
                    "required": ["id"],
                    "additionalProperties": false,
                }),
                ToolKind::Read,
                move |args, _ctx| {
                    let bg = reader.clone();
                    async move { Ok(bg.bg_output(&args)) }
                },
            )
            .concurrent(true)
            .paths(move |_a, _cwd| vec![output_root.clone()])
            .summary(|a| {
                let id = match a.get("id") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => "undefined".into(),
                };
                format!("output of {id}")
            })
            .into_ref(),
        );

        let killer = bg.clone();
        api.register_tool(
            ToolDef::new(
                "bg_kill",
                concat!(
                    "Stop a background job started with bg_start: SIGTERM to its whole process group, SIGKILL 2 seconds later if it is still alive. ",
                    "Use for jobs that are stuck or no longer needed. Only jobs started from the current working directory can be killed. ",
                    "A completion notice still arrives when it has exited."
                ),
                json!({
                    "type": "object",
                    "properties": { "id": job_id_param() },
                    "required": ["id"],
                    "additionalProperties": false,
                }),
                ToolKind::Exec,
                move |args, ctx| {
                    let bg = killer.clone();
                    async move { Ok(bg.bg_kill(&args, &ctx.cwd).await) }
                },
            )
            .concurrent(true)
            .match_target(|a| a.get("id").and_then(Value::as_str).unwrap_or("").to_string())
            .summary(|a| format!("kill {}", a.get("id").and_then(Value::as_str).unwrap_or("")))
            .into_ref(),
        );
        // Reading this directory's job logs cannot hurt anything; bg_start / bg_kill keep the normal
        // permission flow (bg_start as bash, via permissions_as). A listing of every directory's jobs
        // shows other projects' commands, so it asks.
        api.add_permission_rules(PermissionRules {
            allow: vec!["bg_list".into(), "bg_output".into()],
            ask: vec!["bg_list(all)".into()],
            ..PermissionRules::default()
        })?;

        let runner = bg.clone();
        let completer = bg.clone();
        api.register_command(
            Command::new(
                "bg",
                &t!("バックグラウンドジョブの一覧・詳細表示・停止"),
                move |CommandContext { args }| {
                    let bg = runner.clone();
                    async move {
                        bg.run_command(&args).await;
                        Ok(())
                    }
                },
            )
            .hint("[id] | kill <id>")
            .complete(move |prefix: String| {
                let bg = completer.clone();
                async move {
                    let mut values: Vec<String> = bg.list_rows(false).into_iter().map(|m| m.id).collect();
                    values.push("kill".into());
                    values
                        .into_iter()
                        .filter(|v| v.starts_with(&prefix))
                        .map(|v| {
                            let description = if v == "kill" {
                                t!("ジョブを停止")
                            } else {
                                bg.jobs().get(&v).map(|j| j.meta.command.clone()).unwrap_or_default()
                            };
                            SelectOption {
                                value: v.clone(),
                                label: v,
                                description: Some(description),
                            }
                        })
                        .collect()
                }
            }),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_ids_and_exit_codes_read_as_in_js() {
        assert_eq!(fmt_dur(4_400.0), "4s");
        assert_eq!(fmt_dur(65_000.0), "1m05s");
        assert_eq!(fmt_dur(3_725_000.0), "1h02m");
        assert_eq!(natural_cmp("bg-2", "bg-10"), Ordering::Less);
        assert_eq!(parse_int("3\n"), Some(3.0));
        assert_eq!(parse_int("x"), None);
        let meta =
            Meta::from_value(&json!({ "id": "bg-1", "pid": 12, "startedAt": 5, "status": "done", "exitCode": null }));
        assert_eq!(meta.exit_code, Some(None));
        assert_eq!(
            stringify_pretty(&meta.to_value()),
            "{\n  \"id\": \"bg-1\",\n  \"command\": \"\",\n  \"cwd\": \"\",\n  \"runCwd\": \"\",\n  \"pid\": 12,\n  \"startedAt\": 5,\n  \"status\": \"done\",\n  \"exitCode\": null\n}"
        );
    }

    /// Two sessions finish the same job at once: one claims and reports it, the other only records the end.
    /// However the writes interleave, `notified` stays on disk, or the next start reports the job again.
    #[test]
    fn a_job_reported_while_another_session_records_its_end_stays_reported() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let meta = Meta::from_value(&json!({ "id": "bg-1", "pid": 12, "startedAt": 5, "status": "done" }));
        let lost = (0..300)
            .filter(|i| {
                let dir = root.path().join(i.to_string());
                std::fs::create_dir(&dir).unwrap_or_else(|e| panic!("{e}"));
                let barrier = std::sync::Barrier::new(2);
                std::thread::scope(|s| {
                    s.spawn(|| {
                        // Longer than the reporter's text, as an adopted job's (an mtime's fraction in
                        // finishedAt): a torn write would leave its tail after the shorter one.
                        let mut job = to_job(meta.clone(), &dir);
                        job.meta.command = "x".repeat(300);
                        job.meta.finished_at = Some(1_795_126_537_691.651_4);
                        barrier.wait();
                        let _ = write_meta(&mut job);
                    });
                    s.spawn(|| {
                        let mut job = to_job(meta.clone(), &dir);
                        barrier.wait();
                        std::fs::create_dir(dir.join("claimed")).unwrap_or_else(|e| panic!("{e}"));
                        job.meta.notified = Some(true);
                        let _ = write_meta(&mut job);
                    });
                });
                read_meta(&dir).and_then(|m| m.notified) != Some(true)
            })
            .count();
        assert_eq!(lost, 0, "notified was lost in {lost} of 300 runs");
    }
}
