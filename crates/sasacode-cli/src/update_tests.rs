//! packages/cli/test/update.test.ts: what the newest release is, how often that is asked, and what
//! must never be replaced. The home directory and the download base are passed in, not set in the
//! environment, and each test has its own server.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::list_models::test_server::{Res, TestServer};

/// These assertions quote the English strings.
fn english() {
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En);
}

/// What GitHub does for …/latest/download/…: the redirect pins the version.
fn release_server(asked: Arc<AtomicUsize>) -> TestServer {
    TestServer::start(move |req| {
        asked.fetch_add(1, Ordering::SeqCst);
        if req.path == "/latest/download/SHA256SUMS" {
            let host = req.header("host").unwrap_or_default();
            Res::redirect(&format!("http://{host}/download/v9.9.9/SHA256SUMS"))
        } else {
            Res::text(404, "not here")
        }
    })
}

#[test]
fn versions_compare_as_numbers_not_as_text() {
    assert_eq!(compare_versions("v0.9.10", "0.9.9"), 1);
    assert_eq!(compare_versions("0.9.4", "v0.9.4"), 0);
    assert_eq!(compare_versions("v1.0.0", "0.9.9"), 1);
    assert_eq!(compare_versions("0.9.4", "0.9.5"), -1);
    // Unparsable parts are 0, never an error.
    assert_eq!(compare_versions("0.20.0-rc1", "0.20"), 0);
    assert_eq!(compare_versions("v0.20.1", "0.20"), 1);
}

#[tokio::test]
async fn the_newest_release_is_read_from_the_download_redirect() {
    let server = release_server(Arc::new(AtomicUsize::new(0)));
    assert_eq!(latest_release(&server.url()).await.unwrap(), "v9.9.9");
}

#[tokio::test]
async fn a_newer_version_is_reported_and_asked_about_once_a_day() {
    let asked = Arc::new(AtomicUsize::new(0));
    let server = release_server(asked.clone());
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        check_for_update_in(home.path(), &server.url()).await.unwrap(),
        Some("v9.9.9".to_string())
    );
    let after = asked.load(Ordering::SeqCst);
    // The cache says: already asked today.
    assert_eq!(check_for_update_in(home.path(), &server.url()).await.unwrap(), None);
    assert_eq!(asked.load(Ordering::SeqCst), after);
}

#[tokio::test]
async fn update_in_a_source_checkout_points_back_at_the_checkout() {
    english();
    let server = release_server(Arc::new(AtomicUsize::new(0)));
    assert!(!current_version().is_empty());
    let target = release_target().unwrap();
    let ok = ["darwin-", "linux-", "windows-"].iter().any(|os| {
        target
            .strip_prefix(os)
            .is_some_and(|arch| arch.starts_with("arm64") || arch.starts_with("x64"))
    });
    assert!(ok, "{target}");
    let mut out = Vec::new();
    let e = update_command_in(&[], &server.url(), &mut out).await.unwrap_err();
    assert!(e.to_string().contains("runs from source"), "{e}");
}

#[tokio::test]
async fn update_check_says_whether_a_newer_version_exists() {
    english();
    let server = release_server(Arc::new(AtomicUsize::new(0)));
    let mut out = Vec::new();
    assert_eq!(
        update_command_in(&["--check".into()], &server.url(), &mut out)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("newer version available: v9.9.9 (this is {VERSION})\n")
    );
    let mut out = Vec::new();
    let args = ["--check".to_string(), "v0.0.1".to_string()];
    assert_eq!(update_command_in(&args, &server.url(), &mut out).await.unwrap(), 0);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("sasacode is up to date ({VERSION})\n")
    );
}

#[test]
fn release_asset_names_stay_what_the_installers_download() {
    assert_eq!(
        release_files("darwin-arm64"),
        ReleaseFiles {
            archive: "sasacode-darwin-arm64.tar.gz".into(),
            binary: "sasacode-darwin-arm64".into()
        }
    );
    assert_eq!(
        release_files("linux-x64-musl"),
        ReleaseFiles {
            archive: "sasacode-linux-x64-musl.tar.gz".into(),
            binary: "sasacode-linux-x64-musl".into()
        }
    );
    assert_eq!(
        release_files("windows-x64-baseline"),
        ReleaseFiles {
            archive: "sasacode-windows-x64-baseline.zip".into(),
            binary: "sasacode-windows-x64-baseline.exe".into()
        }
    );
}

#[cfg(unix)]
#[test]
fn unpack_extracts_a_tar_gz() {
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("sasacode-x"), "bin").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("a.tar.gz");
    let made = std::process::Command::new("tar")
        .args(["-czf", &archive.display().to_string(), "sasacode-x"])
        .current_dir(src.path())
        .status()
        .unwrap();
    assert!(made.success());
    assert!(unpack("a.tar.gz", dir.path()));
    assert_eq!(std::fs::read_to_string(dir.path().join("sasacode-x")).unwrap(), "bin");
    assert!(!unpack("missing.tar.gz", dir.path()));
}

/// A release server for `install`: SHA256SUMS and a .tar.gz holding `sasacode-test`, a script that
/// prints its version. `sums` gets the archive's real checksum and returns the SHA256SUMS text.
#[cfg(unix)]
fn release_files_server(sums: impl Fn(&str) -> String) -> (TestServer, ReleaseFiles) {
    use std::os::unix::fs::PermissionsExt;
    let src = tempfile::tempdir().unwrap();
    let script = src.path().join("sasacode-test");
    std::fs::write(&script, "#!/bin/sh\necho 9.9.9\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let archive = src.path().join("sasacode-test.tar.gz");
    let made = std::process::Command::new("tar")
        .args(["-czf", "sasacode-test.tar.gz", "sasacode-test"])
        .current_dir(src.path())
        .status()
        .unwrap();
    assert!(made.success());
    let bytes = std::fs::read(&archive).unwrap();
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let sums = sums(&hash);
    let server = TestServer::start(move |req| match req.path.as_str() {
        "/download/v9.9.9/SHA256SUMS" => Res::text(200, &sums),
        "/download/v9.9.9/sasacode-test.tar.gz" => Res {
            status: 200,
            headers: vec![("content-type".into(), "application/gzip".into())],
            body: bytes.clone(),
        },
        _ => Res::text(404, "not here"),
    });
    (server, release_files("test"))
}

#[cfg(unix)]
#[tokio::test]
async fn install_checks_the_download_and_takes_the_binarys_place() {
    use std::os::unix::fs::PermissionsExt;
    english();
    let (server, files) =
        release_files_server(|hash| format!("0000  sasacode-other.tar.gz\r\n{hash}  sasacode-test.tar.gz\r\n"));
    assert_eq!(files.archive, "sasacode-test.tar.gz");
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join("sasacode");
    std::fs::write(&target, "old").unwrap();
    let mut out = Vec::new();
    install(&format!("{}/download/v9.9.9", server.url()), &files, &target, &mut out)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "#!/bin/sh\necho 9.9.9\n");
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
    assert!(!with_suffix(&target, ".new").exists());
    let said = String::from_utf8(out).unwrap();
    assert_eq!(
        said,
        format!(
            "downloading {}/download/v9.9.9/sasacode-test.tar.gz …\nupdated {VERSION} → 9.9.9: {} (restart sasacode to use it)\n",
            server.url(),
            target.display()
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn install_refuses_a_download_that_does_not_match_its_checksum() {
    english();
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join("sasacode");
    std::fs::write(&target, "old").unwrap();
    let (server, files) = release_files_server(|_| "abc  sasacode-test.tar.gz\n".to_string());
    let e = install(
        &format!("{}/download/v9.9.9", server.url()),
        &files,
        &target,
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "checksum mismatch for sasacode-test.tar.gz");
    let (server, files) = release_files_server(|hash| format!("{hash}  sasacode-other.tar.gz\n"));
    let e = install(
        &format!("{}/download/v9.9.9", server.url()),
        &files,
        &target,
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "sasacode-test.tar.gz is not listed in SHA256SUMS");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
}
