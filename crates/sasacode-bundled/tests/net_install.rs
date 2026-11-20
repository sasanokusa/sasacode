//! packages/bundled/test/web-search-install.test.ts: RustXNG's release binary, checked against the
//! SHA256 sasacode holds for the release it names, from a local stand-in for GitHub's release pages. The install through `/web-search install`
//! needs SASACODE_HOME, SASACODE_RUSTXNG_BASE and PATH set, so it runs in a child test process.

// The install case needs a shell script for the binary: without one (Windows) only the targets are tested.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod support_net;

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use serde_json::json;
use sha2::{Digest, Sha256};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_bundled::rustxng_install::{
    InstallOptions, PINNED_TAG, install_rustxng, managed_rustxng, managed_rustxng_in, rustxng_target, this_platform,
};
use sasacode_plugin_api::{Lang, PermissionMode};
use support_net::*;

fn target() -> String {
    let (platform, arch) = this_platform();
    rustxng_target(platform, arch).unwrap_or_else(|e| panic!("{e}"))
}

/// A release page like GitHub's: the assets of the pinned release sit under /download/<tag>/.
async fn releases(binary: &'static str) -> Server {
    let asset = format!("rustxng-{PINNED_TAG}-{}", target());
    serve(Router::new().fallback(move |uri: Uri| {
        let asset = asset.clone();
        async move {
            if uri.path() == format!("/download/{PINNED_TAG}/{asset}") {
                binary.into_response()
            } else {
                (StatusCode::NOT_FOUND, "missing").into_response()
            }
        }
    }))
    .await
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn the_release_target_for_each_platform_rustxng_ships() {
    let _l = lang(Lang::Ja).await;
    let t = |p: &str, a: &str| rustxng_target(p, a).map_err(|e| e.to_string());
    assert_eq!(t("darwin", "arm64").as_deref(), Ok("aarch64-apple-darwin"));
    assert_eq!(t("darwin", "x64").as_deref(), Ok("x86_64-apple-darwin"));
    assert_eq!(t("linux", "x64").as_deref(), Ok("x86_64-unknown-linux-musl"));
    assert_eq!(t("win32", "x64").as_deref(), Ok("x86_64-pc-windows-msvc"));
    assert_eq!(t("linux", "arm64").as_deref(), Ok("aarch64-unknown-linux-gnu"));
    assert!(t("freebsd", "x64").err().unwrap_or_default().contains("cargo install"));
}

const BINARY: &str = "#!/bin/sh\necho 'RustXNG 9.9.9'\n";

const INSTALLED_BY_THE_COMMAND: &str =
    "install_the_pinned_release_checksum_checked_into_the_sasacode_bin_then_web_search_is_offered_without_a_restart";

// The stand-in binary is a shell script: there is no such thing on Windows.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn install_the_pinned_release_checksum_checked_into_the_sasacode_bin_then_web_search_is_offered_without_a_restart()
 {
    let _l = lang(Lang::Ja).await;
    if is_child() {
        return installed_by_the_command().await;
    }
    let home = temp_dir("sasacode-rustxng-");
    // A file that is not the one sasacode holds the SHA256 of (here, any stand-in) is refused,
    // whatever the page it comes from says about it.
    let page = releases(BINARY).await;
    let opts = InstallOptions {
        base: Some(format!("http://127.0.0.1:{}", page.port)),
        home: Some(home.path().to_path_buf()),
        ..InstallOptions::default()
    };
    let e = install_rustxng(opts)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("チェックサム"), "{e}");
    assert!(!managed_rustxng_in(home.path()).exists());

    // A debug build can be told the stand-in's SHA256 (a release build cannot): the rest of the
    // install, through the command, is then what runs.
    run_child(
        INSTALLED_BY_THE_COMMAND,
        &[
            ("SASACODE_HOME", s(home.path())),
            ("SASACODE_RUSTXNG_BASE", format!("http://127.0.0.1:{}", page.port)),
            ("SASACODE_RUSTXNG_SHA256", sha256(BINARY)),
            ("PATH", String::new()), // no rustxng on PATH
        ],
    )
    .await;
    page.stop().await;
}

/// A shell script whose `--version` names the SASACODE_TEST_SECRET it was started with (or none).
#[cfg(unix)]
const LEAKY: &str = "#!/bin/sh\necho \"RustXNG ${SASACODE_TEST_SECRET:-none}\"\n";

// The check that the downloaded binary runs is a child like any other: it gets the environment
// commands get, without the keys sasacode loaded for itself.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_version_check_does_not_hand_the_downloaded_program_sasacodes_keys() {
    let _l = lang(Lang::Ja).await;
    if is_child() {
        sasacode_plugin_api::hide_from_children(["SASACODE_TEST_SECRET"]);
        let opts = InstallOptions {
            base: std::env::var("SASACODE_RUSTXNG_BASE").ok(),
            home: Some(std::path::PathBuf::from(
                std::env::var("SASACODE_HOME").unwrap_or_default(),
            )),
            ..InstallOptions::default()
        };
        let done = install_rustxng(opts).await.unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(done.version, "RustXNG none");
        return;
    }
    let home = temp_dir("sasacode-rustxng-");
    let page = releases(LEAKY).await;
    run_child(
        "the_version_check_does_not_hand_the_downloaded_program_sasacodes_keys",
        &[
            ("SASACODE_HOME", s(home.path())),
            ("SASACODE_RUSTXNG_BASE", format!("http://127.0.0.1:{}", page.port)),
            ("SASACODE_RUSTXNG_SHA256", sha256(LEAKY)),
            ("SASACODE_TEST_SECRET", "hunter2".to_string()),
        ],
    )
    .await;
    page.stop().await;
}

/// In the child: SASACODE_HOME is a temp dir, SASACODE_RUSTXNG_BASE the release page, SASACODE_RUSTXNG_SHA256 its binary's, PATH empty.
#[cfg(unix)]
async fn installed_by_the_command() {
    let home = sasacode_plugin_api::sasacode_home();
    let mut o = AgentOptions::new(model("none", json!({})), s(&home), "");
    o.permissions = Some(policy(PermissionMode::Edits));
    let agent = Agent::new(o);
    let (host, notes) = host_for(&agent, &s(&home), Default::default(), Lang::Ja);
    host.load("web-search", bundled("web-search")).await;
    let names = |a: &Agent| a.get_tools().iter().map(|t| t.name().to_string()).collect::<Vec<_>>();
    assert!(!names(&agent).contains(&"web_search".to_string()));
    let command = host
        .commands()
        .into_iter()
        .find(|c| c.command.name == "web-search")
        .map(|c| c.command);
    let command = command.unwrap_or_else(|| panic!("no /web-search"));
    (command.run)(sasacode_plugin_api::CommandContext { args: "install".into() })
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    let managed = managed_rustxng();
    let said = texts(&notes).join("\n");
    assert!(
        said.contains(&format!(
            "RustXNG 9.9.9 を {} に入れました。web_search が使えます。",
            managed.display()
        )),
        "{said}"
    );
    assert!(
        std::fs::read_to_string(&managed)
            .unwrap_or_default()
            .contains("RustXNG 9.9.9")
    );
    assert!(names(&agent).contains(&"web_search".to_string()));
}
