//! `cargo xtask build`: release executables of `sasacode`, one file each (scripts/build.ts did this
//! with `bun build --compile`).
//!
//!   cargo xtask build                    → dist/sasacode[.exe] for this machine
//!   cargo xtask build --all              → dist/sasacode-<target>[.exe] for every release target
//!   cargo xtask build --targets darwin   → only the targets whose name contains "darwin"
//!   cargo xtask build … --cross          → with `cross` (cross-rs) instead of cargo: the Linux
//!                                          targets on CI, for an old glibc and a musl toolchain
//!
//! The target names are the release assets' (`sasacode-<target>.tar.gz`, `.zip` for Windows), which
//! scripts/install.sh, scripts/install.ps1, npm/sasacode and `sasacode update` download: keep them.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, bail};

use crate::checks::root;

pub struct Target {
    /// The name in the release asset: `sasacode-<name>.tar.gz`.
    pub name: &'static str,
    pub triple: &'static str,
}

const fn target(name: &'static str, triple: &'static str) -> Target {
    Target { name, triple }
}

/// Every release target. The `-baseline` ones were Bun's builds for x64 CPUs without AVX2. Rust's
/// x86_64 targets assume no more than SSE2 unless told otherwise, so a baseline target is the same
/// executable as its twin, published under both names for the installers that ask for it.
/// Linux: glibc builds for the usual distributions, static musl ones for Alpine and the like.
pub const TARGETS: [Target; 10] = [
    target("darwin-arm64", "aarch64-apple-darwin"),
    target("darwin-x64", "x86_64-apple-darwin"),
    target("linux-x64", "x86_64-unknown-linux-gnu"),
    target("linux-x64-baseline", "x86_64-unknown-linux-gnu"),
    target("linux-arm64", "aarch64-unknown-linux-gnu"),
    target("linux-x64-musl", "x86_64-unknown-linux-musl"),
    target("linux-arm64-musl", "aarch64-unknown-linux-musl"),
    target("windows-x64", "x86_64-pc-windows-msvc"),
    target("windows-x64-baseline", "x86_64-pc-windows-msvc"),
    target("windows-arm64", "aarch64-pc-windows-msvc"),
];

/// Windows executables link the C runtime statically, so `sasacode.exe` runs on a Windows without the
/// Visual C++ runtime installed, as Bun's single file did.
const CRT_STATIC: &str = "-C target-feature=+crt-static";

struct Options {
    targets: Option<Vec<&'static Target>>,
    cross: bool,
}

fn parse(args: &[String]) -> anyhow::Result<Options> {
    let mut all = false;
    let mut filter: Option<&str> = None;
    let mut cross = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all" => all = true,
            "--cross" => cross = true,
            "--targets" => filter = Some(args.next().context("--targets needs a filter, e.g. --targets linux")?),
            other => bail!("unknown argument {other} (cargo xtask build [--all | --targets <filter>] [--cross])"),
        }
    }
    let targets = match (filter, all) {
        (Some(filter), _) => {
            let chosen: Vec<&Target> = TARGETS.iter().filter(|t| t.name.contains(filter)).collect();
            if chosen.is_empty() {
                let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
                bail!("no release target matches {filter:?} (targets: {})", names.join(", "));
            }
            Some(chosen)
        }
        (None, true) => Some(TARGETS.iter().collect()),
        (None, false) => None,
    };
    Ok(Options { targets, cross })
}

/// Where cargo puts its output, as `cargo metadata` reports it (CARGO_TARGET_DIR and config included).
fn target_directory(cargo: &str, root: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).context("reading cargo metadata")?;
    let dir = metadata["target_directory"]
        .as_str()
        .context("cargo metadata has no target_directory")?;
    Ok(PathBuf::from(dir))
}

/// `cargo build --release` of the `sasacode` binary, for `triple` (None: this machine).
fn cargo_build(program: &str, root: &Path, triple: Option<&str>) -> anyhow::Result<()> {
    let mut command = Command::new(program);
    command
        .args([
            "build",
            "--release",
            "--locked",
            "--package",
            "sasacode-cli",
            "--bin",
            "sasacode",
        ])
        .current_dir(root);
    let windows_msvc = triple.map_or(cfg!(all(windows, target_env = "msvc")), |t| {
        t.ends_with("-windows-msvc")
    });
    if let Some(triple) = triple {
        command.args(["--target", triple]);
    }
    if windows_msvc {
        // RUSTFLAGS, when set, wins over the per-target variable: add to whichever cargo will read.
        let (name, current) = match (std::env::var("RUSTFLAGS"), triple) {
            (Ok(flags), _) => ("RUSTFLAGS".to_string(), flags),
            (Err(_), Some(triple)) => {
                let name = format!("CARGO_TARGET_{}_RUSTFLAGS", triple.to_uppercase().replace('-', "_"));
                let current = std::env::var(&name).unwrap_or_default();
                (name, current)
            }
            (Err(_), None) => ("RUSTFLAGS".to_string(), String::new()),
        };
        command.env(name, format!("{current} {CRT_STATIC}").trim());
    }
    let status = command.status().with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} build failed for {}", triple.unwrap_or("this machine"));
    }
    Ok(())
}

pub fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let options = parse(args)?;
    let root = root();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let program = if options.cross { "cross" } else { cargo.as_str() };
    let target_dir = target_directory(&cargo, &root)?;
    let dist = root.join("dist");
    if dist.exists() {
        std::fs::remove_dir_all(&dist).context("clearing dist/")?;
    }
    std::fs::create_dir_all(&dist).context("creating dist/")?;

    let copy = |from: PathBuf, name: String| -> anyhow::Result<()> {
        let to = dist.join(&name);
        std::fs::copy(&from, &to).with_context(|| format!("copying {} to dist/{name}", from.display()))?;
        println!("built dist/{name}");
        Ok(())
    };
    let Some(targets) = options.targets else {
        cargo_build(program, &root, None)?;
        let exe = std::env::consts::EXE_SUFFIX;
        return copy(
            target_dir.join("release").join(format!("sasacode{exe}")),
            format!("sasacode{exe}"),
        )
        .map(|()| ExitCode::SUCCESS);
    };
    let mut built: Vec<&str> = Vec::new();
    for target in &targets {
        if !built.contains(&target.triple) {
            cargo_build(program, &root, Some(target.triple))?;
            built.push(target.triple);
        }
        // Windows executables end in .exe; the release zips hold `sasacode-windows-<arch>.exe`.
        let exe = if target.triple.contains("windows") { ".exe" } else { "" };
        let from = target_dir
            .join(target.triple)
            .join("release")
            .join(format!("sasacode{exe}"));
        copy(from, format!("sasacode-{}{exe}", target.name))?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn names(options: &Options) -> Vec<&'static str> {
        options
            .targets
            .as_ref()
            .map(|t| t.iter().map(|t| t.name).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_release_targets_keep_the_asset_names() {
        // The names install.sh, install.ps1, npm/sasacode and `sasacode update` download.
        let names: Vec<&str> = TARGETS.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "darwin-arm64",
                "darwin-x64",
                "linux-x64",
                "linux-x64-baseline",
                "linux-arm64",
                "linux-x64-musl",
                "linux-arm64-musl",
                "windows-x64",
                "windows-x64-baseline",
                "windows-arm64",
            ]
        );
    }

    #[test]
    fn targets_are_chosen_by_a_part_of_their_name() {
        assert_eq!(parse(&args(&[])).map(|o| o.targets.is_none()).ok(), Some(true));
        assert_eq!(names(&parse(&args(&["--all"])).expect("--all")).len(), 10);
        assert_eq!(
            names(&parse(&args(&["--targets", "musl", "--cross"])).expect("musl")),
            ["linux-x64-musl", "linux-arm64-musl"]
        );
        assert_eq!(
            names(&parse(&args(&["--targets", "darwin"])).expect("darwin")),
            ["darwin-arm64", "darwin-x64"]
        );
        assert!(parse(&args(&["--targets", "plan9"])).is_err());
        assert!(parse(&args(&["--targets"])).is_err());
        assert!(parse(&args(&["--minify"])).is_err());
    }
}
