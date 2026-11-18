//! `sasacode web-search install` / `/web-search install` (`packages/bundled/src/rustxng-install.ts`):
//! RustXNG's own release binary, checked against the .sha256 published next to it, put where
//! web_search looks after PATH.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use sha2::{Digest, Sha256};

use sasacode_host::t;
use sasacode_plugin_api::{PluginApi, sasacode_home};

pub const RUSTXNG_RELEASES: &str = "https://github.com/sahenjp/RustXNG/releases";

fn releases() -> String {
    std::env::var("SASACODE_RUSTXNG_BASE").unwrap_or_else(|_| RUSTXNG_RELEASES.to_string())
}

const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

/// sasacode's own rustxng: kept off PATH, used when PATH has none.
pub fn managed_rustxng() -> PathBuf {
    managed_rustxng_in(&sasacode_home())
}

/// [`managed_rustxng`] under another sasacode home.
pub fn managed_rustxng_in(home: &Path) -> PathBuf {
    home.join("bin").join(format!("rustxng{EXE}"))
}

/// This machine as Node names it (`process.platform`, `process.arch`).
pub fn this_platform() -> (&'static str, &'static str) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    };
    (platform, arch)
}

/// The release's target for a machine (Node's platform and arch names): RustXNG ships macOS, x86_64
/// Linux (musl: any distribution), arm64 Linux (glibc, from v0.27.1) and x86_64 Windows.
pub fn rustxng_target(platform: &str, arch: &str) -> anyhow::Result<String> {
    let target = match (platform, arch) {
        ("darwin", "arm64") => "aarch64-apple-darwin",
        ("darwin", _) => "x86_64-apple-darwin",
        ("linux", "x64") => "x86_64-unknown-linux-musl",
        // On musl, the --version check below refuses it.
        ("linux", "arm64") => "aarch64-unknown-linux-gnu",
        // Windows on arm64 runs it emulated.
        ("win32", _) => "x86_64-pc-windows-msvc",
        _ => anyhow::bail!(t!(
            "RustXNG の配布物に {platform}-{arch} 向けのものがありません。cargo install --git https://github.com/sahenjp/RustXNG.git で入れられます。",
            platform = platform,
            arch = arch
        )),
    };
    Ok(target.to_string())
}

/// The newest release tag, read from the redirect of …/releases/latest.
async fn latest_tag(base: &str) -> anyhow::Result<String> {
    let client = no_redirects();
    let r = client.head(format!("{base}/latest")).send().await?;
    let location = r
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let tag = location
        .rsplit_once("/tag/")
        .map(|(_, tag)| tag)
        .filter(|tag| !tag.is_empty() && !tag.contains(['/', '?', '#']));
    let Some(tag) = tag else {
        anyhow::bail!(t!(
            "RustXNG の最新版がわかりません（{base} が {status} を返しました）",
            base = base,
            status = r.status().as_u16()
        ));
    };
    Ok(percent_encoding::percent_decode_str(tag).decode_utf8()?.into_owned())
}

/// A client that reports redirects instead of following them.
pub(crate) fn no_redirects() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        sasacode_ai::http::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| sasacode_ai::http::client().clone())
    })
}

async fn get(url: &str) -> anyhow::Result<reqwest::Response> {
    let r = sasacode_ai::http::client().get(url).send().await?;
    if !r.status().is_success() {
        anyhow::bail!("{} {url}", r.status().as_u16());
    }
    Ok(r)
}

/// What [`install_rustxng`] put where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub path: String,
    pub version: String,
}

/// Takes a line of progress.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Options of [`install_rustxng`]. `base` and `home` stand in for SASACODE_RUSTXNG_BASE and the
/// sasacode home (tests: the environment cannot be changed from inside one).
#[derive(Clone, Default)]
pub struct InstallOptions {
    /// A release tag; the newest one when None.
    pub tag: Option<String>,
    pub log: Option<LogFn>,
    pub base: Option<String>,
    pub home: Option<PathBuf>,
}

/// Run `program --version`; its trimmed output, None when it does not run here. A file just written
/// can still be open in a child another thread is forking (ETXTBSY): that is tried again briefly.
async fn version_of(program: &Path) -> Option<String> {
    for attempt in 0..5 {
        let mut cmd = tokio::process::Command::new(program);
        cmd.arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(20), cmd.output()).await {
            Ok(Ok(out)) => {
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return (!v.is_empty()).then_some(v);
            }
            Ok(Err(e)) if e.raw_os_error() == Some(26) && attempt < 4 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => return None,
        }
    }
    None
}

/// Download, verify and install rustxng (the newest release, or `tag`); an existing one is replaced.
pub async fn install_rustxng(opts: InstallOptions) -> anyhow::Result<Installed> {
    let base = opts.base.clone().unwrap_or_else(releases);
    let (platform, arch) = this_platform();
    let target = rustxng_target(platform, arch)?;
    let tag = match &opts.tag {
        Some(tag) => tag.clone(),
        None => latest_tag(&base).await?,
    };
    let asset = format!("rustxng-{tag}-{target}{EXE}");
    let url = format!("{base}/download/{tag}/{asset}");
    if let Some(log) = &opts.log {
        log(&t!("{url} をダウンロード中…（RustXNG、AGPL-3.0）", url = url));
    }
    let sums = get(&format!("{url}.sha256")).await?.text().await?;
    let expected = sums.split_whitespace().next().unwrap_or("").to_string();
    let body = get(&url).await?.bytes().await?;
    let actual: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected {
        anyhow::bail!(t!("{file} のチェックサムが一致しません", file = asset));
    }
    let path = match &opts.home {
        Some(home) => managed_rustxng_in(home),
        None => managed_rustxng(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let staging = PathBuf::from(format!("{}.new", path.display()));
    std::fs::write(&staging, &body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
    }
    // A binary that does not run here must not take the place of one that does.
    let Some(version) = version_of(&staging).await else {
        let _ = std::fs::remove_file(&staging);
        anyhow::bail!(t!("ダウンロードした rustxng がこの環境で動きません"));
    };
    if cfg!(windows) && path.exists() {
        // A running sidecar's .exe cannot be replaced, only renamed: move it aside (the next install removes it).
        let old = PathBuf::from(format!("{}.old", path.display()));
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&path, &old)?;
    }
    std::fs::rename(&staging, &path)?;
    Ok(Installed {
        path: path.display().to_string(),
        version,
    })
}

/// Not a plugin of its own: installing is reached through web-search and the CLI.
pub fn plugin(_api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async { Ok(()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    // web-search-install.test.ts: "the release target for each platform RustXNG ships"
    #[test]
    fn the_release_target_for_each_platform_rustxng_ships() {
        let target = |p, a| rustxng_target(p, a).unwrap();
        assert_eq!(target("darwin", "arm64"), "aarch64-apple-darwin");
        assert_eq!(target("darwin", "x64"), "x86_64-apple-darwin");
        assert_eq!(target("linux", "x64"), "x86_64-unknown-linux-musl");
        assert_eq!(target("win32", "x64"), "x86_64-pc-windows-msvc");
        assert_eq!(target("linux", "arm64"), "aarch64-unknown-linux-gnu");
        let err = rustxng_target("freebsd", "x64").unwrap_err().to_string();
        assert!(err.contains("cargo install"), "{err}");
    }
}
