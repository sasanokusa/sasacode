//! `sasacode gui install [--path <checkout>]`: builds the desktop window (sasacode-gui) on this
//! machine with cargo, into $SASACODE_HOME/gui (cargo's `--root`), where `sasacode gui` and `/gui`
//! look before PATH. The window is not in the release, so Rust is needed; without it, this says
//! where to get it and stops. Why it is built here rather than downloaded: docs/gui.md.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sasacode_bundled::gui::{gui_root_in, gui_version, managed_gui_in, stale_note};
use sasacode_host::t;
use sasacode_plugin_api::{apply_child_env, sasacode_home};

/// Where the source comes from by default (`SASACODE_GUI_GIT` for tests and mirrors).
pub const REPO: &str = "https://github.com/sasanokusa/sasacode";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit this release was built from, put in by the release workflow (`SASACODE_COMMIT`). A
/// tag can be moved; the commit cannot. A build made by hand does not have it.
const COMMIT: Option<&str> = option_env!("SASACODE_COMMIT");

/// What cargo builds the window from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The commit this sasacode's release was built from.
    Commit { git: String, rev: String },
    /// This sasacode's own release tag (`v<version>`) of the repository: for a build made by hand,
    /// which does not know its commit.
    Tag { git: String, tag: String },
    /// The sasacode-gui crate of a checkout (`--path`): a development build, whose tag may not exist.
    Checkout(PathBuf),
}

impl Source {
    fn describe(&self) -> String {
        match self {
            Source::Commit { git, rev } => format!("{git} {rev}"),
            Source::Tag { git, tag } => format!("{git} {tag}"),
            Source::Checkout(dir) => dir.display().to_string(),
        }
    }
}

/// The source for `--path <dir>` (the repository or its crates/sasacode-gui), else the commit this
/// release was built from, else (a build made by hand) this version's tag.
pub fn source_for(path: Option<&Path>, git: Option<&str>) -> anyhow::Result<Source> {
    source_for_commit(path, git, COMMIT)
}

/// [`source_for`] with the commit the release was built from given (None: not known).
pub fn source_for_commit(path: Option<&Path>, git: Option<&str>, commit: Option<&str>) -> anyhow::Result<Source> {
    let Some(dir) = path else {
        let git = git.filter(|g| !g.is_empty()).unwrap_or(REPO).to_string();
        return match commit.filter(|c| !c.is_empty()) {
            // A full commit hash and nothing else: it goes to `--rev`, which also takes a branch or a tag.
            Some(rev) if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(Source::Commit {
                git,
                rev: rev.to_ascii_lowercase(),
            }),
            Some(rev) => anyhow::bail!(t!(
                "this sasacode was built from \"{rev}\", which is not a commit hash: build from a checkout with sasacode gui install --path <checkout>",
                rev = rev
            )),
            None => Ok(Source::Tag {
                git,
                tag: format!("v{VERSION}"),
            }),
        };
    };
    // cargo runs elsewhere (below), so the path must not depend on this folder.
    let dir = &std::path::absolute(dir)?;
    let in_repo = dir.join("crates").join("sasacode-gui");
    if in_repo.join("Cargo.toml").is_file() {
        return Ok(Source::Checkout(in_repo));
    }
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
    if manifest.lines().any(|l| l.trim() == "name = \"sasacode-gui\"") {
        return Ok(Source::Checkout(dir.to_path_buf()));
    }
    anyhow::bail!(t!(
        "{path} is not a sasacode checkout (no crates/sasacode-gui): give --path the repository",
        path = dir.display()
    ))
}

/// cargo: on `path` (PATH), else in `cargo_home`/bin, else in `home`/.cargo/bin, where rustup puts
/// it (a shell started before rustup ran does not have that on PATH yet).
pub fn find_cargo_in(
    path: Option<OsString>,
    cargo_home: Option<PathBuf>,
    home: Option<PathBuf>,
    cwd: &Path,
) -> Option<PathBuf> {
    if let Ok(found) = which::which_in("cargo", path, cwd) {
        return Some(found);
    }
    let exe = format!("cargo{}", std::env::consts::EXE_SUFFIX);
    [cargo_home, home.map(|h| h.join(".cargo"))]
        .into_iter()
        .flatten()
        .map(|dir| dir.join("bin").join(&exe))
        .find(|c| c.is_file())
}

/// [`find_cargo_in`] for this process.
pub fn find_cargo() -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    find_cargo_in(
        std::env::var_os("PATH"),
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
        dirs_home(),
        &cwd,
    )
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// cargo's arguments: `--locked` builds with the repository's Cargo.lock (the versions CI tests),
/// `--force` builds again over the one there (a rebuild for a new sasacode keeps its version only
/// when the source is the same).
pub fn cargo_args(source: &Source, root: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["install", "--locked", "--force", "--root"]
        .iter()
        .map(OsString::from)
        .collect();
    args.push(root.as_os_str().to_owned());
    match source {
        Source::Commit { git, rev } => {
            args.extend(["--git", git.as_str(), "--rev", rev.as_str(), "sasacode-gui"].map(OsString::from));
        }
        Source::Tag { git, tag } => {
            args.extend(["--git", git.as_str(), "--tag", tag.as_str(), "sasacode-gui"].map(OsString::from));
        }
        Source::Checkout(dir) => {
            args.push("--path".into());
            args.push(dir.as_os_str().to_owned());
        }
    }
    args
}

/// This process's PATH with `dir` first, under the name the environment has it by (Windows: Path).
pub fn path_with(dir: &Path) -> Option<(String, String)> {
    let found = std::env::vars_os().find(|(k, _)| k.to_str().is_some_and(|k| k.eq_ignore_ascii_case("PATH")));
    let (key, value) = match found {
        Some((k, v)) => (k.into_string().ok()?, v),
        None => ("PATH".to_string(), OsString::new()),
    };
    let rest = std::env::split_paths(&value).filter(|p| !p.as_os_str().is_empty());
    let joined = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest)).ok()?;
    Some((key, joined.into_string().ok()?))
}

/// What to say when there is no cargo.
pub fn no_rust() -> String {
    t!(
        "sasacode gui install builds the desktop GUI on this machine with Rust, and cargo was not found (on PATH or in ~/.cargo/bin).\nInstall Rust from this page, open a new terminal, and run sasacode gui install again:\n  https://rustup.rs"
    )
}

/// `sasacode gui install [--path <checkout>]`.
pub async fn gui_install(args: &[String]) -> anyhow::Result<i32> {
    let path = match args {
        [] => None,
        [flag, dir] if flag == "--path" => Some(PathBuf::from(dir)),
        _ => anyhow::bail!(t!("usage: sasacode gui install [--path <sasacode checkout>]")),
    };
    let source = source_for(path.as_deref(), std::env::var("SASACODE_GUI_GIT").ok().as_deref())?;
    let Some(cargo) = find_cargo() else {
        eprintln!("{}", no_rust());
        return Ok(1);
    };
    let home = sasacode_home();
    let root = gui_root_in(&home);
    // From a commit or tag cargo builds in a folder it throws away; from a checkout, in the checkout's target.
    let building = match &source {
        Source::Commit { .. } | Source::Tag { .. } => t!(
            "building sasacode-gui with {cargo} from {source} (a few minutes: cargo builds it from scratch every time)",
            cargo = cargo.display(),
            source = source.describe()
        ),
        Source::Checkout(_) => t!(
            "building sasacode-gui with {cargo} from {source} (a few minutes the first time)",
            cargo = cargo.display(),
            source = source.describe()
        ),
    };
    println!("{building}");
    // Run from sasacode's own folder, not this one: rustup picks the toolchain of the folder cargo
    // starts in (a project's rust-toolchain.toml may pin one too old, or one not installed).
    std::fs::create_dir_all(&root)?;
    let mut cmd = tokio::process::Command::new(&cargo);
    cmd.args(cargo_args(&source, &root))
        .current_dir(&root)
        .stdin(std::process::Stdio::null());
    // The environment bash gets: the build scripts of every crate run in it. Its PATH also has the
    // folder cargo installs into, or cargo says to put it on PATH: sasacode looks there by itself.
    let extra: Vec<(String, String)> = path_with(&root.join("bin")).into_iter().collect();
    apply_child_env(&mut cmd, &extra);
    let status = cmd.status().await?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        let line = match &source {
            Source::Tag { tag, .. } => t!(
                "cargo could not build sasacode-gui (exit code {code}). If this sasacode is a development build whose tag {tag} is not released, build from a checkout: sasacode gui install --path <checkout>",
                code = code,
                tag = tag
            ),
            Source::Commit { .. } | Source::Checkout(_) => {
                t!("cargo could not build sasacode-gui (exit code {code})", code = code)
            }
        };
        eprintln!("{line}");
        return Ok(code);
    }
    let program = managed_gui_in(&home);
    let built = gui_version(&program.display().to_string()).await;
    let Some(built) = built else {
        anyhow::bail!(t!("cargo finished, but {path} does not run", path = program.display()));
    };
    println!(
        "{}",
        t!(
            "installed sasacode-gui {version}: {path}\nopen it with sasacode gui [folder], or /gui in the TUI",
            version = built,
            path = program.display()
        )
    );
    if built != VERSION {
        println!("{}", stale_note(&built));
    }
    Ok(0)
}

#[cfg(test)]
#[path = "gui_install_tests.rs"]
mod tests;
