//! `sasacode web-search install` / `/web-search install` (`packages/bundled/src/rustxng-install.ts`):
//! RustXNG's own release binary, of one release this sasacode names and checked against the SHA256
//! this file holds for it (not a checksum fetched from where the binary comes from), put where
//! web_search looks after PATH.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use sha2::{Digest, Sha256};

use sasacode_host::t;
use sasacode_plugin_api::{PluginApi, apply_child_env, sasacode_home};

pub const RUSTXNG_RELEASES: &str = "https://github.com/sahenjp/RustXNG/releases";

/// The release that is installed. A newer one is taken by changing this and [`PINNED_SHA256`] (the
/// SHA256 of each file of the release, taken from a download of it) in a sasacode release.
pub const PINNED_TAG: &str = "v0.32.0";

/// The SHA256 of the release's file for each target (`rustxng-<tag>-<target>`, `.exe` on Windows).
const PINNED_SHA256: &[(&str, &str)] = &[
    (
        "aarch64-apple-darwin",
        "266dbf2ee7275ba5f6d847d636720d8d0d9e3399489c2388ef96ff28107feeb5",
    ),
    (
        "x86_64-apple-darwin",
        "c354bed4b0ebcefca907e1119c5f30c63ce7d2a7b97f672cf19f2ad5a9003558",
    ),
    (
        "x86_64-unknown-linux-musl",
        "cf05d5acc182cb702cc447ad78e65e104014c05fbdc11a00c61d975d4fb630ef",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "e86adb048c99c8873513227eb8481b932e813082c579063a12b1f34cc82b5a26",
    ),
    (
        "x86_64-pc-windows-msvc",
        "58ee07c420f5d3787024210c1e2fedc8685b6593ae9d8cfadf56db632950eca1",
    ),
];

/// The SHA256 the download of `target` must have. A debug build (the tests) can name another in
/// `SASACODE_RUSTXNG_SHA256`, for a stand-in binary; a release build cannot be told.
fn expected_sha256(target: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(hash) = std::env::var("SASACODE_RUSTXNG_SHA256") {
        return Some(hash);
    }
    PINNED_SHA256
        .iter()
        .find(|(t, _)| *t == target)
        .map(|(_, hash)| hash.to_string())
}

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
    pub log: Option<LogFn>,
    pub base: Option<String>,
    pub home: Option<PathBuf>,
}

/// Run `program --version`; its trimmed output, None when it does not run here. A file just written
/// can still be open in a child another thread is forking (ETXTBSY): that is tried again briefly.
/// The environment is the one commands get (without the keys sasacode loaded for itself).
async fn version_of(program: &Path) -> Option<String> {
    for attempt in 0..5 {
        let mut cmd = tokio::process::Command::new(program);
        cmd.arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        apply_child_env(&mut cmd, &[]);
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

/// Download, verify and install rustxng ([`PINNED_TAG`]); an existing one is replaced.
pub async fn install_rustxng(opts: InstallOptions) -> anyhow::Result<Installed> {
    let base = opts.base.clone().unwrap_or_else(releases);
    let (platform, arch) = this_platform();
    let target = rustxng_target(platform, arch)?;
    let tag = PINNED_TAG;
    let Some(expected) = expected_sha256(&target) else {
        anyhow::bail!(t!(
            "RustXNG {tag}の{target}向けのチェックサムはsasacodeが持っていません",
            tag = tag,
            target = target
        ));
    };
    let asset = format!("rustxng-{tag}-{target}{EXE}");
    let url = format!("{base}/download/{tag}/{asset}");
    if let Some(log) = &opts.log {
        log(&t!("{url} をダウンロード中…（RustXNG、AGPL-3.0）", url = url));
    }
    let body = get(&url).await?.bytes().await?;
    let actual: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(&expected) {
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

    #[test]
    fn every_target_rustxng_ships_has_a_sha256_for_the_pinned_release() {
        for (platform, arch) in [
            ("darwin", "arm64"),
            ("darwin", "x64"),
            ("linux", "x64"),
            ("linux", "arm64"),
            ("win32", "x64"),
        ] {
            let target = rustxng_target(platform, arch).unwrap();
            let hash = PINNED_SHA256.iter().find(|(t, _)| *t == target).map(|(_, h)| *h);
            let hash = hash.unwrap_or_else(|| panic!("no SHA256 for {target}"));
            assert!(
                hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "{target}: {hash}"
            );
        }
        assert_eq!(PINNED_SHA256.len(), 5);
    }

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
