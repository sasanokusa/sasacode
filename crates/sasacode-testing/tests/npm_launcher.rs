//! The npm package's launcher (npm/sasacode), run with Node against a fake release server
//! (`packages/e2e/test/npm-launcher.test.ts`): it must fetch the binary for this machine once, refuse
//! one whose checksum is wrong, and then behave as the binary itself (arguments in, exit code out).
//!
//! Skipped with a message when `node` is not on the PATH. On Windows the TS suite compiled a stand-in
//! .exe with `bun build --compile`; that part (the two tests below on Windows, and "on Windows, a CPU
//! the x64 build does not run on gets the baseline build") is deferred until there is a stand-in
//! binary to package.

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::{Request, State};
    use axum::http::StatusCode;
    use axum::response::Response;
    use sasacode_testing::harness::workspace_root;
    use sha2::{Digest, Sha256};

    const TARGETS: [&str; 7] = [
        "darwin-arm64",
        "darwin-x64",
        "linux-x64",
        "linux-x64-baseline",
        "linux-arm64",
        "linux-x64-musl",
        "linux-arm64-musl",
    ];

    struct Release {
        rel: PathBuf,
        downloads: AtomicUsize,
        corrupt: AtomicBool,
    }

    /// The package as npm installs it at a version the fake server has, and a release with a
    /// "binary" per target: a script that reports its arguments and exits with a chosen code.
    struct Fixture {
        root: tempfile::TempDir,
        pkg: PathBuf,
        release: Arc<Release>,
        base: String,
    }

    fn node_missing() -> bool {
        let found = std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            eprintln!("skipped: node is not on the PATH");
        }
        !found
    }

    async fn fixture() -> Fixture {
        let root = tempfile::Builder::new().prefix("sasacode-npm-").tempdir().unwrap();
        let pkg = root.path().join("pkg");
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        std::fs::copy(
            workspace_root().join("npm/sasacode/bin/sasacode.js"),
            pkg.join("bin/sasacode.js"),
        )
        .unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"sasacode","version":"9.9.9","type":"module"}"#,
        )
        .unwrap();
        let rel = root.path().join("release");
        std::fs::create_dir(&rel).unwrap();
        let mut sums = Vec::new();
        for t in TARGETS {
            let name = format!("sasacode-{t}");
            let script = "#!/bin/sh\n[ \"$1\" = --version ] && { echo 9.9.9; exit 0; }\necho \"args:$* by:$SASACODE_INSTALLED_BY\"\n[ \"$1\" = fail ] && exit 3\nexit 0\n";
            std::fs::write(rel.join(&name), script).unwrap();
            std::fs::set_permissions(rel.join(&name), std::fs::Permissions::from_mode(0o755)).unwrap();
            let tarball = format!("{name}.tar.gz");
            let ok = std::process::Command::new("tar")
                .args(["czf", &tarball, &name])
                .current_dir(&rel)
                .status()
                .unwrap();
            assert!(ok.success(), "tar failed");
            let digest = Sha256::digest(std::fs::read(rel.join(&tarball)).unwrap());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            sums.push(format!("{hex}  {tarball}"));
        }
        std::fs::write(rel.join("SHA256SUMS"), format!("{}\n", sums.join("\n"))).unwrap();

        let release = Arc::new(Release {
            rel,
            downloads: AtomicUsize::new(0),
            corrupt: AtomicBool::new(false),
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let app = Router::new().fallback(serve).with_state(release.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Fixture {
            root,
            pkg,
            release,
            base,
        }
    }

    async fn serve(State(release): State<Arc<Release>>, req: Request) -> Response {
        let file = req
            .uri()
            .path()
            .strip_prefix("/download/v9.9.9/")
            .filter(|f| !f.is_empty() && !f.contains(".."));
        let Some(path) = file.map(|f| release.rel.join(f)).filter(|p| p.is_file()) else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("not found"))
                .unwrap();
        };
        let name = path.to_string_lossy();
        let archive = name.ends_with(".tar.gz") || name.ends_with(".zip");
        if archive {
            release.downloads.fetch_add(1, Ordering::SeqCst);
        }
        let mut body = std::fs::read(&path).unwrap();
        if archive && release.corrupt.load(Ordering::SeqCst) {
            body.push(b'x');
        }
        Response::new(Body::from(body))
    }

    struct Launched {
        code: i32,
        stdout: String,
        stderr: String,
    }

    async fn launch(f: &Fixture, home: &Path, args: &[&str]) -> Launched {
        let mut cmd = tokio::process::Command::new("node");
        cmd.arg(f.pkg.join("bin/sasacode.js"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", f.root.path())
            .env("USERPROFILE", f.root.path())
            .env("SASACODE_HOME", home)
            .env("SASACODE_DOWNLOAD_BASE", &f.base)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
            .await
            .expect("the launcher did not exit within 60 s")
            .expect("node runs");
        Launched {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn kept(home: &Path) -> Vec<String> {
        std::fs::read_dir(home.join("npm-bin/v9.9.9"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test]
    async fn the_first_run_fetches_the_binary_for_this_machine_later_runs_start_it_without_downloading() {
        if node_missing() {
            return;
        }
        let f = fixture().await;
        let home = f.root.path().join("home-a");
        let first = launch(&f, &home, &["-p", "hello world"]).await;
        assert!(first.stderr.contains("downloading sasacode v9.9.9"), "{}", first.stderr);
        assert_eq!(first.stdout.trim(), "args:-p hello world by:npm");
        assert_eq!(first.code, 0, "{}", first.stderr);
        assert_eq!(kept(&home).len(), 1);

        let before = f.release.downloads.load(Ordering::SeqCst);
        let again = launch(&f, &home, &["fail"]).await;
        assert_eq!(f.release.downloads.load(Ordering::SeqCst), before);
        assert!(!again.stderr.contains("downloading"), "{}", again.stderr);
        assert_eq!(again.code, 3); // the binary's exit code comes through
    }

    #[tokio::test]
    async fn a_download_whose_checksum_does_not_match_is_refused_and_nothing_is_kept() {
        if node_missing() {
            return;
        }
        let f = fixture().await;
        f.release.corrupt.store(true, Ordering::SeqCst);
        let home = f.root.path().join("home-b");
        let r = launch(&f, &home, &["--version"]).await;
        assert_eq!(r.code, 1, "{}", r.stderr);
        assert!(r.stderr.contains("checksum mismatch"), "{}", r.stderr);
        assert_eq!(kept(&home), Vec::<String>::new());
    }
}

#[cfg(not(unix))]
#[test]
fn deferred_on_windows() {
    eprintln!("skipped: the npm launcher tests need a compiled stand-in binary on Windows (deferred)");
}
