//! `sasacode update`: the installer's download and checks, run against the binary in use
//! (`packages/cli/src/update.ts`). The installer script (scripts/install.sh) is the reference for the
//! target names and the checksum; release asset names and URLs must stay exactly these.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use sasacode_bundled::gui::managed_gui;
use sasacode_host::t;
use sasacode_plugin_api::NotifyLevel;

use crate::config::sasacode_home;
use crate::list_models::{is_js_space, js_to_number};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Sent to the release host (Bun's fetch always sent one).
const USER_AGENT: &str = concat!("sasacode/", env!("CARGO_PKG_VERSION"));
/// The newest release is asked for this often; a check is cheap and the answer rarely changes.
const CHECK_EVERY_MS: i64 = 24 * 60 * 60_000;

/// Where releases are downloaded from (`SASACODE_DOWNLOAD_BASE` for tests and mirrors).
pub fn download_base() -> String {
    std::env::var("SASACODE_DOWNLOAD_BASE").unwrap_or_else(|_| "https://github.com/sasanokusa/sasacode/releases".into())
}

pub fn current_version() -> &'static str {
    VERSION
}

/// "v0.9.5" vs "0.9.4" → 1. Versions are numbers; anything unparsable compares as 0.
pub fn compare_versions(a: &str, b: &str) -> i32 {
    let parts = |s: &str| -> Vec<f64> { s.strip_prefix('v').unwrap_or(s).split('.').map(js_to_number).collect() };
    let (x, y) = (parts(a), parts(b));
    // `(x[i] || 0)`: NaN and missing parts are 0.
    let at = |v: &[f64], i: usize| v.get(i).copied().filter(|n| !n.is_nan()).unwrap_or(0.0);
    for i in 0..x.len().max(y.len()) {
        let d = at(&x, i) - at(&y, i);
        if d != 0.0 && !d.is_nan() {
            return if d > 0.0 { 1 } else { -1 };
        }
    }
    0
}

/// A client that does not follow redirects (`redirect: "manual"`).
fn no_redirect_client() -> &'static reqwest::Client {
    static C: LazyLock<reqwest::Client> = LazyLock::new(|| {
        sasacode_ai::http::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| sasacode_ai::http::client().clone())
    });
    &C
}

/// The newest release tag, read from the redirect of …/latest/download/…: the releases page can
/// lag behind the actual latest release for a while (scripts/install.sh pins versions the same way).
pub async fn latest_release(base: &str) -> anyhow::Result<String> {
    static TAG: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new(r"/download/([^/]+)/SHA256SUMS").ok());
    let r = no_redirect_client()
        .head(format!("{base}/latest/download/SHA256SUMS"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await?;
    let location = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match TAG.as_ref().and_then(|re| re.captures(location)).and_then(|c| c.get(1)) {
        Some(m) => Ok(m.as_str().to_string()),
        None => anyhow::bail!(t!(
            "cannot tell the latest version ({status} from {base})",
            status = r.status().as_u16(),
            base = base
        )),
    }
}

/// The newest release, when it is newer than this build. Asked at most once a day.
pub async fn check_for_update() -> anyhow::Result<Option<String>> {
    check_for_update_in(&sasacode_home(), &download_base()).await
}

/// [`check_for_update`] with the home directory and the download base given.
pub async fn check_for_update_in(home: &Path, base: &str) -> anyhow::Result<Option<String>> {
    let cache = home.join("update-check.json");
    let checked_at = std::fs::read(&cache)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("checkedAt").and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0);
    if ((sasacode_ai::now_ms() as f64) - checked_at) < CHECK_EVERY_MS as f64 {
        return Ok(None);
    }
    // The timestamp is written before the question, so an offline day is not retried on every start.
    // A read-only home must not break startup.
    let _ = std::fs::write(&cache, format!("{{\"checkedAt\":{}}}", sasacode_ai::now_ms()));
    let latest = latest_release(base).await?;
    Ok((compare_versions(&latest, VERSION) > 0).then_some(latest))
}

/// A quiet heads-up; nothing at all when the question cannot be answered.
pub async fn notify_if_newer(notify: Arc<dyn Fn(String, NotifyLevel) + Send + Sync>) {
    if let Ok(Some(latest)) = check_for_update().await {
        let how = if std::env::var("SASACODE_INSTALLED_BY").as_deref() == Ok("npm") {
            "npm install -g sasacode@latest"
        } else {
            "sasacode update"
        };
        notify(
            t!(
                "a newer version is available: {latest} (this is {version}) — run `{how}`",
                latest = latest,
                version = VERSION,
                how = how
            ),
            NotifyLevel::Info,
        );
    }
}

/// `spawnSync`: the exit code and stdout of a child (environment without sasacode's own secrets),
/// None when it cannot be started. With `timeout`, a child still running then is killed.
fn spawn_sync(
    program: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Option<Duration>,
) -> Option<(Option<i32>, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(sasacode_plugin_api::child_env(&[]))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000; // windowsHide
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let status = match timeout {
        None => child.wait().ok(),
        Some(limit) => {
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    Some((status.and_then(|s| s.code()), out))
}

/// Whether this Windows CPU has AVX2 (kernel32's PF_AVX2_INSTRUCTIONS_AVAILABLE); true when that cannot be told.
pub fn windows_has_avx2() -> bool {
    let kernel32 = r#"[DllImport("kernel32.dll")] public static extern bool IsProcessorFeaturePresent(int f);"#;
    let script = format!(
        "(Add-Type -MemberDefinition '{kernel32}' -Name K -Namespace SasacodeCpu -PassThru)::IsProcessorFeaturePresent(40)"
    );
    // Bounded: PowerShell can hang where its environment is incomplete; unknown counts as AVX2.
    let r = spawn_sync(
        Path::new("powershell.exe"),
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        None,
        Some(Duration::from_secs(20)),
    );
    r.map(|(_, out)| out.trim_matches(is_js_space).to_string()).as_deref() != Some("False")
}

/// The release files of a target: a .zip holding an .exe for Windows, a .tar.gz elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFiles {
    pub archive: String,
    pub binary: String,
}

pub fn release_files(target: &str) -> ReleaseFiles {
    let name = format!("sasacode-{target}");
    if target.starts_with("windows-") {
        ReleaseFiles {
            archive: format!("{name}.zip"),
            binary: format!("{name}.exe"),
        }
    } else {
        ReleaseFiles {
            archive: format!("{name}.tar.gz"),
            binary: name,
        }
    }
}

/// Unpack a release archive into `dir`. Windows' own tar (bsdtar) reads .zip; Git's GNU tar would not.
pub fn unpack(archive: &str, dir: &Path) -> bool {
    let tar = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    };
    let flag = if archive.ends_with(".zip") { "-xf" } else { "-xzf" };
    matches!(spawn_sync(&tar, &[flag, archive], Some(dir), None), Some((Some(0), _)))
}

/// Node's `process.platform` for this build.
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Node's `process.arch` for this build.
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// The archive name for this machine, with the platform choices scripts/install.sh (install.ps1) makes.
pub fn release_target() -> anyhow::Result<String> {
    let os = match node_platform() {
        "darwin" => "darwin",
        "linux" => "linux",
        "win32" => "windows",
        platform => anyhow::bail!(t!("unsupported OS: {platform}", platform = platform)),
    };
    let mut arch = match node_arch() {
        "x64" => "x64".to_string(),
        "arm64" => "arm64".to_string(),
        arch => anyhow::bail!(t!("unsupported CPU: {arch}", arch = arch)),
    };
    if os == "windows" {
        return Ok(if arch == "x64" && !windows_has_avx2() {
            "windows-x64-baseline".to_string()
        } else {
            format!("windows-{arch}")
        });
    }
    // An x64 shell under Rosetta on Apple Silicon should still get the native build.
    if os == "darwin" && arch == "x64" {
        let translated = spawn_sync(Path::new("sysctl"), &["-n", "sysctl.proc_translated"], None, None)
            .map(|(_, out)| out.trim_matches(is_js_space).to_string());
        if translated.as_deref() == Some("1") {
            arch = "arm64".to_string();
        }
    }
    if os == "linux" {
        let musl = Path::new("/etc/alpine-release").exists()
            || matches!(
                spawn_sync(Path::new("sh"), &["-c", "ls /lib/ld-musl-*"], None, None),
                Some((Some(0), _))
            );
        if musl {
            arch = format!("{arch}-musl");
        } else if arch == "x64" {
            let cpuinfo = std::fs::read("/proc/cpuinfo").map_err(|e| anyhow::anyhow!("{e}, open '/proc/cpuinfo'"))?;
            if !String::from_utf8_lossy(&cpuinfo).contains("avx2") {
                arch = "x64-baseline".to_string(); // CPUs without AVX2
            }
        }
    }
    Ok(format!("{os}-{arch}"))
}

/// A directory cargo builds into: the target directory carries cargo's CACHEDIR.TAG (when cargo made
/// it), and each profile directory (debug, release) its `.fingerprint` directory.
fn is_cargo_target_dir(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("CACHEDIR.TAG")).is_ok_and(|s| s.contains("created by cargo"))
        || dir.join(".fingerprint").is_dir()
}

/// What stands in for the installed binary; refuses anything it must not touch.
fn installed_binary() -> anyhow::Result<PathBuf> {
    // The npm package's launcher keeps its own binary per version: npm does the updating there.
    if std::env::var("SASACODE_INSTALLED_BY").as_deref() == Ok("npm") {
        anyhow::bail!(t!(
            "this sasacode was installed with npm: update it with `npm install -g sasacode@latest`"
        ));
    }
    let exec = std::env::current_exe()?;
    // The file itself, not a link to it.
    #[cfg(unix)]
    let exec = std::fs::canonicalize(&exec).unwrap_or(exec);
    // From a source checkout the running binary is one cargo built (inside its target directory):
    // never that.
    if exec.ancestors().skip(1).any(is_cargo_target_dir) {
        anyhow::bail!(t!(
            "this sasacode runs from source: update the checkout (git pull && cargo build --release) instead of `sasacode update`"
        ));
    }
    Ok(exec)
}

/// `mkdtempSync(join(tmpdir(), "sasacode-update-"))`, removed again when dropped (the `finally`).
struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> std::io::Result<TempDir> {
        use std::hash::{BuildHasher, Hasher};
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut last = None;
        for attempt in 0..16u32 {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u32(attempt);
            h.write_u32(std::process::id());
            let mut n = h.finish();
            let suffix: String = (0..6)
                .map(|_| {
                    let c = CHARS[(n % CHARS.len() as u64) as usize] as char;
                    n /= CHARS.len() as u64;
                    c
                })
                .collect();
            let dir = std::env::temp_dir().join(format!("{prefix}{suffix}"));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(TempDir(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no temporary directory")))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `path + suffix`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// `sasacode update [--check] [tag]`: download the release for this machine and take its place.
pub async fn update_command(args: &[String]) -> anyhow::Result<i32> {
    // The window is built on this machine (`sasacode gui install`), so an update leaves it as it was:
    // whoever has one is told.
    let has_gui = managed_gui().is_file() || which::which("sasacode-gui").is_ok();
    update_command_with(args, &download_base(), has_gui, &mut std::io::stdout()).await
}

/// [`update_command`] with the download base and the output given, for someone without the window.
pub async fn update_command_in(args: &[String], base: &str, out: &mut (dyn Write + Send)) -> anyhow::Result<i32> {
    update_command_with(args, base, false, out).await
}

/// What an update says to someone who has the window: it stays the version it was.
pub fn gui_update_hint(to: &str) -> String {
    t!(
        "the desktop GUI is not updated with sasacode: sasacode gui install builds it for {to}",
        to = to
    )
}

/// [`update_command_in`], and [`gui_update_hint`] after an update when `has_gui`.
pub async fn update_command_with(
    args: &[String],
    base: &str,
    has_gui: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<i32> {
    let tag = args.iter().find(|a| !a.starts_with('-'));
    let wanted = match tag {
        Some(t) => t.clone(),
        None => latest_release(base).await?,
    };
    if args.iter().any(|a| a == "--check") {
        let line = if compare_versions(&wanted, VERSION) > 0 {
            t!(
                "newer version available: {wanted} (this is {version})",
                wanted = wanted,
                version = VERSION
            )
        } else {
            t!("sasacode is up to date ({version})", version = VERSION)
        };
        let _ = writeln!(out, "{line}");
        return Ok(0);
    }
    let target = installed_binary()?; // before downloading anything: a source checkout stops here
    if compare_versions(&wanted, VERSION) <= 0 {
        let _ = writeln!(out, "{}", t!("sasacode is up to date ({version})", version = VERSION));
        return Ok(0);
    }
    let files = release_files(&release_target()?);
    let version = install(&format!("{base}/download/{wanted}"), &files, &target, out).await?;
    if has_gui {
        let _ = writeln!(out, "{}", gui_update_hint(&version));
    }
    Ok(0)
}

/// Download the release files at `url`, check them, and put the binary in place of `target`.
/// Returns the version the new binary says it is.
async fn install(
    url: &str,
    files: &ReleaseFiles,
    target: &Path,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<String> {
    let ReleaseFiles { archive, binary } = files;
    let dir = TempDir::new("sasacode-update-")?;
    let _ = writeln!(out, "{}", t!("downloading {url} …", url = format!("{url}/{archive}")));
    let client = sasacode_ai::http::client();
    let get = |u: String| client.get(u).header(reqwest::header::USER_AGENT, USER_AGENT);
    let sums = get(format!("{url}/SHA256SUMS")).send().await?.text().await?;
    let body = get(format!("{url}/{archive}")).send().await?.bytes().await?;
    let suffix = format!(" {archive}");
    let expected = sums
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .find(|l| l.ends_with(&suffix))
        .and_then(|l| l.split(is_js_space).next())
        .filter(|e| !e.is_empty());
    let Some(expected) = expected else {
        anyhow::bail!(t!("{file} is not listed in SHA256SUMS", file = archive));
    };
    let actual: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        anyhow::bail!(t!("checksum mismatch for {file}", file = archive));
    }
    std::fs::write(dir.0.join(archive), &body)?;
    if !unpack(archive, &dir.0) {
        anyhow::bail!(t!("could not unpack {file}", file = archive));
    }
    // The same check the installer does: a binary that does not run here must not become the binary.
    let fresh = dir.0.join(binary);
    let version = spawn_sync(&fresh, &["--version"], None, None)
        .map(|(_, out)| out.trim_matches(is_js_space).to_string())
        .filter(|v| !v.is_empty());
    let Some(version) = version else {
        anyhow::bail!(t!("the downloaded binary does not run on this system"));
    };
    let staging = with_suffix(target, ".new");
    std::fs::copy(&fresh, &staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
    }
    if cfg!(windows) {
        // Windows will not replace a running .exe, but lets it be renamed: move it aside first
        // (the next update removes it).
        let old = with_suffix(target, ".old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old)?;
    }
    std::fs::rename(&staging, target)?; // the running process keeps its own file until it exits
    let _ = writeln!(
        out,
        "{}",
        t!(
            "updated {from} → {to}: {target} (restart sasacode to use it)",
            from = VERSION,
            to = version,
            target = target.display()
        )
    );
    Ok(version)
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
