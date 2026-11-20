//! Host helpers every part of sasacode uses (`sasacodeHome`, `hideFromChildren`, `childEnv`,
//! `saveOutput` in packages/plugin-api).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};

/// Where sasacode keeps its files: $SASACODE_HOME, or ~/.sasacode.
pub fn sasacode_home() -> PathBuf {
    if let Some(h) = std::env::var_os("SASACODE_HOME") {
        return PathBuf::from(h);
    }
    home_dir().join(".sasacode")
}

/// The user's home directory ($HOME, else %USERPROFILE%, like Node's os.homedir()).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn hidden() -> &'static Mutex<HashSet<String>> {
    static H: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Keep these variables out of child_env(): secrets sasacode loaded for itself (~/.sasacode/.env).
pub fn hide_from_children<I: IntoIterator<Item = S>, S: Into<String>>(names: I) {
    let mut h = hidden().lock().unwrap_or_else(|e| e.into_inner());
    for n in names {
        h.insert(n.into());
    }
}

fn secrets() -> &'static RwLock<Vec<String>> {
    static S: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Keys, tokens and header values that every record sasacode keeps shows as `[REDACTED]`. Under 8
/// characters is ignored (ordinary words would go too).
pub fn hide_secrets<I: IntoIterator<Item = S>, S: AsRef<str>>(values: I) {
    let mut list = secrets().write().unwrap_or_else(|e| e.into_inner());
    for v in values.into_iter().map(|v| v.as_ref().to_string()) {
        if v.chars().count() >= 8 && !list.contains(&v) {
            list.push(v);
        }
    }
    list.sort_by_key(|s| std::cmp::Reverse(s.len()));
}

/// `text` without the values given to [`hide_secrets`] (longest first).
pub fn redact_secrets(text: &str) -> std::borrow::Cow<'_, str> {
    let list = secrets().read().unwrap_or_else(|e| e.into_inner());
    let mut out = std::borrow::Cow::Borrowed(text);
    for s in list.iter().filter(|s| text.contains(s.as_str())) {
        out = out.replace(s.as_str(), "[REDACTED]").into();
    }
    out
}

/// The environment for a child process: this one's minus hidden variables, plus `extra`.
pub fn child_env(extra: &[(String, String)]) -> BTreeMap<String, String> {
    let h = hidden().lock().unwrap_or_else(|e| e.into_inner());
    let mut env: BTreeMap<String, String> = std::env::vars().filter(|(k, _)| !h.contains(k)).collect();
    for (k, v) in extra {
        env.insert(k.clone(), v.clone());
    }
    env
}

/// Apply [`child_env`] to a command: clears the inherited environment and sets the filtered one.
pub fn apply_child_env(cmd: &mut tokio::process::Command, extra: &[(String, String)]) {
    cmd.env_clear();
    cmd.envs(child_env(extra));
}

const OUTPUT_DAYS: u64 = 7;

/// Save long output privately in sasacode_home()/tmp; prune files older than a week. Returns the path.
pub fn save_output(prefix: &str, content: &str) -> std::io::Result<PathBuf> {
    static PRUNED: std::sync::Once = std::sync::Once::new();
    let dir = sasacode_home().join("tmp");
    create_private_dir(&dir)?;
    PRUNED.call_once(|| {
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(OUTPUT_DAYS * 86_400);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    });
    let rand: String = uuid::Uuid::new_v4().simple().to_string().chars().take(6).collect();
    let file = dir.join(format!("{prefix}-{}-{rand}.log", sasacode_ai::now_ms()));
    write_private(&file, redact_secrets(content).as_bytes())?;
    Ok(file)
}

/// Start a child in a session of its own, without a controlling terminal, as Node's
/// `detached: true` did: a command that opens /dev/tty (ssh or git credential prompts, gpg) fails
/// at once instead of waiting for input nobody can give. The child also leads a new process group
/// (pgid = its pid), so `kill(-pid)` reaches everything it starts. Do not combine with
/// `process_group`, which would make `setsid` fail. Windows: nothing (no sessions; callers use
/// `CREATE_NO_WINDOW` and kill the process tree).
pub fn detach_session(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // SAFETY: the closure runs in the forked child before exec. setsid(2) is async-signal-safe,
        // allocates nothing and touches no state shared with the parent; it is the only call made.
        // This is the one `unsafe` in the code base (rust/DECISIONS.md ADR-012).
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
        }
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// mkdir -p with mode 0700 on Unix.
pub fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// Write a file readable by the user only (mode 0600 on Unix).
pub fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_values_are_replaced_longest_first_and_short_ones_are_ignored() {
        hide_secrets(["host-t-secret-12345", "host-t-secret-1234567", "tiny", "1234567"]);
        let shown = redact_secrets("a host-t-secret-1234567 b host-t-secret-12345 c tiny 1234567");
        assert_eq!(shown, "a [REDACTED] b [REDACTED] c tiny 1234567");
        assert!(matches!(
            redact_secrets("nothing to hide"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
