//! packages/bundled/test/web-search-install.test.ts: RustXNG's release binary, checksum-checked,
//! from a local stand-in for GitHub's release pages. The install through `/web-search install`
//! needs SASACODE_HOME, SASACODE_RUSTXNG_BASE and PATH set, so it runs in a child test process.

// The install case needs a shell script for the binary: without one (Windows) only the targets are tested.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod support_net;

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde_json::json;
use sha2::{Digest, Sha256};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_bundled::rustxng_install::{
    InstallOptions, install_rustxng, managed_rustxng, managed_rustxng_in, rustxng_target, this_platform,
};
use sasacode_plugin_api::{Lang, PermissionMode};
use support_net::*;

fn target() -> String {
    let (platform, arch) = this_platform();
    rustxng_target(platform, arch).unwrap_or_else(|e| panic!("{e}"))
}

/// A release page like GitHub's: /latest redirects to the tag, assets sit under /download/<tag>/.
async fn releases(binary: &'static str, sum: Option<String>) -> Server {
    let sum = sum.unwrap_or_else(|| {
        Sha256::digest(binary.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    });
    let asset = format!("rustxng-v9.9.9-{}", target());
    serve(Router::new().fallback(move |uri: Uri| {
        let (sum, asset) = (sum.clone(), asset.clone());
        async move {
            let path = uri.path();
            if path == "/latest" {
                (
                    StatusCode::FOUND,
                    [(
                        header::LOCATION,
                        "https://github.com/sahenjp/RustXNG/releases/tag/v9.9.9",
                    )],
                )
                    .into_response()
            } else if path == format!("/download/v9.9.9/{asset}") {
                binary.into_response()
            } else if path == format!("/download/v9.9.9/{asset}.sha256") {
                format!("{sum}  {asset}\n").into_response()
            } else {
                (StatusCode::NOT_FOUND, "missing").into_response()
            }
        }
    }))
    .await
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

// The stand-in binary is a shell script: there is no such thing on Windows.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn install_the_newest_release_checksum_checked_into_the_sasacode_bin_then_web_search_is_offered_without_a_restart()
 {
    let _l = lang(Lang::Ja).await;
    if is_child() {
        return installed_by_the_command().await;
    }
    let home = temp_dir("sasacode-rustxng-");
    let bad = releases(BINARY, Some("0".repeat(64))).await;
    let opts = InstallOptions {
        base: Some(format!("http://127.0.0.1:{}", bad.port)),
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
    bad.stop().await;

    let good = releases(BINARY, None).await;
    run_child(
        "install_the_newest_release_checksum_checked_into_the_sasacode_bin_then_web_search_is_offered_without_a_restart",
        &[
            ("SASACODE_HOME", s(home.path())),
            ("SASACODE_RUSTXNG_BASE", format!("http://127.0.0.1:{}", good.port)),
            ("PATH", String::new()), // no rustxng on PATH
        ],
    )
    .await;
    good.stop().await;
}

/// In the child: SASACODE_HOME is a temp dir, SASACODE_RUSTXNG_BASE the good release page, PATH empty.
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
