//! packages/cli/test/plugins.test.ts, what remains with the plugins shipped inside sasacode
//! (rust/DECISIONS.md ADR-002), and the switches for the plugins that were separate packages
//! (ADR-011). The tests of discovering, loading, installing, removing and trusting plugins from disk
//! are dropped with external plugins (ADR-002); the one-trust-decision test is in src/trust.rs.

use std::sync::Arc;

use serde_json::{Value, json};

use sasacode_cli::setup::{Harness, SetupOptions, setup};

use crate::support::{Home, Role, child, isolated, merged, provider, replay_provider, sink, with_quiet_builtins};

fn configure(t: &Home, extra: Value) {
    t.config(&merged(
        json!({ "providers": { "test": replay_provider() }, "model": "test/m" }),
        extra,
    ));
}

/// Setup and the plugins, as a headless run loads them.
async fn loaded(t: &Home, trust_project: bool) -> Arc<Harness> {
    let h = setup(SetupOptions {
        no_session: true,
        trust_project,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    h.load_plugins(None).await;
    h
}

fn tools(h: &Harness) -> Vec<String> {
    h.agent.get_tools().iter().map(|t| t.name().to_string()).collect()
}

fn plugins(h: &Harness) -> Vec<String> {
    h.host.plugins().into_iter().map(|p| p.name).collect()
}

fn commands(h: &Harness) -> Vec<String> {
    h.host.commands().into_iter().map(|c| c.command.name).collect()
}

fn has(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n == name)
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_disabled_and_tools_disabled_switch_things_off_p3() {
    let Some(t) = child() else { return };
    configure(&t, json!({}));
    // Without them, all are there.
    let all = tools(&*loaded(&t, true).await);
    for tool in ["read", "write", "edit", "bash", "task", "todo_write", "web_fetch"] {
        assert!(has(&all, tool), "{tool} missing from {all:?}");
    }
    t.project_config(&json!({ "plugins": { "disabled": ["todo"] }, "tools": { "disabled": ["bash", "web_fetch"] } }));
    let h = loaded(&t, true).await; // disabling plugins is a trusted setting
    let tools = tools(&h);
    assert!(!has(&tools, "todo_write"), "{tools:?}");
    assert!(!has(&plugins(&h), "todo"));
    assert!(!has(&tools, "bash"), "{tools:?}");
    assert!(!has(&tools, "web_fetch"), "{tools:?}");
    assert!(has(&tools, "read"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_is_off_by_default_loads_only_when_plugins_enabled_names_it() {
    let Some(t) = child() else { return };
    assert!(
        !sasacode_bundled::find("git-context")
            .expect("git-context ships")
            .on_by_default
    );
    configure(&t, json!({}));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "git-context"));
    assert!(!has(&tools(&h), "git_context"));
    // ask-user was a separate package too, but loads without being named.
    assert!(has(&tools(&h), "ask_user"), "{:?}", tools(&h));
    configure(&t, json!({ "plugins": { "enabled": ["git-context"] } }));
    let h = loaded(&t, false).await;
    assert!(has(&plugins(&h), "git-context"), "{:?}", plugins(&h));
    assert!(has(&tools(&h), "git_context"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_disabled_wins_over_plugins_enabled() {
    let Some(t) = child() else { return };
    configure(
        &t,
        json!({ "plugins": { "enabled": ["git-context"], "disabled": ["git-context"] } }),
    );
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "git-context"));
    assert!(!has(&tools(&h), "git_context"));
}

#[tokio::test(flavor = "multi_thread")]
async fn jev_guards_own_enabled_setting_turns_it_on_as_it_did_when_jev_guard_was_installed() {
    let Some(t) = child() else { return };
    assert!(
        !sasacode_bundled::find("jev-guard")
            .expect("jev-guard ships")
            .on_by_default
    );
    configure(&t, json!({}));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "jev-guard"));
    assert!(!has(&commands(&h), "jev"));
    let on = json!({ "jev-guard": { "enabled": true } });
    configure(&t, json!({ "plugins": { "settings": on } }));
    let h = loaded(&t, false).await;
    assert!(has(&plugins(&h), "jev-guard"), "{:?}", plugins(&h));
    assert!(has(&commands(&h), "jev"));
    // plugins.disabled still wins.
    configure(&t, json!({ "plugins": { "settings": on, "disabled": ["jev-guard"] } }));
    assert!(!has(&plugins(&*loaded(&t, false).await), "jev-guard"));
}

/// The names `/mcp` completes: its servers, after "reconnect".
async fn mcp_names(h: &Harness) -> (Vec<String>, Vec<String>) {
    let mcp: Vec<_> = h
        .host
        .commands()
        .into_iter()
        .filter(|c| c.command.name == "mcp")
        .collect();
    let owners = mcp.iter().map(|c| c.owner.clone()).collect();
    let complete = mcp
        .last()
        .and_then(|c| c.command.complete.clone())
        .expect("/mcp completes");
    (
        owners,
        complete(String::new()).await.into_iter().map(|o| o.value).collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn free_search_joins_the_mcp_plugins_servers_under_one_mcp_command() {
    // PATH holds only the keychain stub, so uvx is not found and nothing is downloaded.
    let Some(t) = child() else { return };
    configure(
        &t,
        json!({ "plugins": { "enabled": ["free-search"] }, "mcpServers": { "mine": { "command": "no-such-server" } } }),
    );
    let h = loaded(&t, false).await;
    assert!(has(&plugins(&h), "free-search"), "{:?}", plugins(&h));
    let (owners, names) = mcp_names(&h).await;
    assert_eq!(owners, ["mcp"]);
    assert_eq!(names, ["reconnect", "mine", "free-search"]);
    // A server the user named free-search is theirs.
    configure(
        &t,
        json!({ "plugins": { "enabled": ["free-search"] }, "mcpServers": { "free-search": { "command": "mine" } } }),
    );
    let h = loaded(&t, false).await;
    assert_eq!(mcp_names(&h).await.1, ["reconnect", "free-search"]);
    // Without the mcp plugin, free-search connects its server and `/mcp` on its own.
    configure(
        &t,
        json!({ "plugins": { "enabled": ["free-search"], "disabled": ["mcp"] } }),
    );
    let h = loaded(&t, false).await;
    let (owners, names) = mcp_names(&h).await;
    assert_eq!(owners, ["free-search"]);
    assert_eq!(names, ["reconnect", "free-search"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_enabled_in_an_untrusted_project_config_is_ignored() {
    let Some(t) = child() else { return };
    configure(&t, json!({}));
    t.project_config(&json!({ "plugins": { "enabled": ["git-context"] } }));
    let h = loaded(&t, false).await;
    assert!(!has(&plugins(&h), "git-context"));
    assert!(
        h.warnings()
            .iter()
            .any(|w| w.contains("not applied until you trust this project")),
        "{:?}",
        h.warnings()
    );
    // Trusted, it applies.
    assert!(has(&plugins(&*loaded(&t, true).await), "git-context"));
}

#[tokio::test(flavor = "multi_thread")]
async fn git_is_run_only_in_a_folder_allowed_for_it_not_in_a_project_merely_trusted() {
    let Some(t) = child() else { return };
    configure(&t, json!({ "plugins": { "enabled": ["git-context"] } }));
    let cwd = std::path::PathBuf::from(t.cwd());
    let _h = loaded(&t, true).await;
    assert!(!sasacode_bundled::project_trust::is_trusted(&cwd));
    let _h = setup(SetupOptions {
        no_session: true,
        git: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    assert!(sasacode_bundled::project_trust::is_trusted(&cwd));
}

/// What the GUI asks the binary without starting a run (its cli.rs): `plugin list --json` is all
/// that is left of `sasacode plugin`, the bundled plugins on or off as a run here loads them.
#[test]
fn the_gui_asks_the_binary_for_the_plugins_commands_skills_and_trust_other_plugin_use_is_an_error() {
    let bin = crate::support::Bin::new(&json!({ "plugins": { "enabled": ["git-context"], "disabled": ["goal"] } }));
    let listed = bin.run(["plugin", "list", "--json"]);
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    let plugins: Vec<Value> = serde_json::from_str(listed.stdout.trim()).expect("one JSON array");
    let on = |name: &str| {
        let p = plugins
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(p["source"], "bundled");
        p["enabled"].as_bool().expect("enabled")
    };
    assert!(on("git-context") && on("ask-user") && on("checkpoints"));
    assert!(!on("goal") && !on("workflows"));
    let commands = bin.run(["list", "commands", "--json"]);
    let commands: Vec<Value> = serde_json::from_str(commands.stdout.trim()).expect("the commands");
    assert!(commands.iter().any(|c| c["name"] == "help" && c["headless"] == false));
    let trust = bin.run(["trust", "--json"]);
    let trust: Value = serde_json::from_str(trust.stdout.trim()).expect("the trust status");
    assert_eq!((&trust["needed"], &trust["complete"]), (&json!(false), &json!(true)));
    for other in [&["plugin", "list"][..], &["plugin", "add", "x"]] {
        let r = bin.run(other);
        assert_eq!(r.code, 1);
        assert!(
            r.stderr.contains("`sasacode plugin` was removed in 0.20"),
            "{}",
            r.stderr
        );
    }
}

/// `list models --json`, as the GUI asks for it: the servers' models, the one a run would use
/// marked `current`. The built-in providers go to a server that knows nothing.
#[test]
fn the_gui_asks_the_binary_for_the_models_the_one_a_run_uses_is_current() {
    let server = sasacode_testing::FakeServer::start(Vec::new()).expect("the fake server");
    let sink = sink();
    let providers = with_quiet_builtins(&sink.url(), json!({ "fake": provider("openai-chat", server.url()) }));
    // openai-codex off: it would look for its login in the (stubbed) keychain, as a run does.
    let config = json!({
        "providers": providers, "model": "fake/m", "maxRetries": 0, "plugins": { "disabled": ["openai-codex"] },
    });
    let bin = crate::support::Bin::new(&config);
    let r = bin.run(["list", "models", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let models: Vec<Value> = serde_json::from_str(r.stdout.trim()).expect("one JSON array");
    let current: Vec<&Value> = models.iter().filter(|m| m["current"] == true).collect();
    assert_eq!(current.len(), 1, "{models:?}");
    assert_eq!(current[0]["id"], "fake/m");
    assert!(server.requests().is_empty(), "no model was asked to answer");
}

/// `--slash` with a command that fails: its error, and exit 1 (here in text mode, so on stderr).
#[tokio::test(flavor = "multi_thread")]
async fn slash_a_command_that_fails_is_an_error_and_exit_1() {
    let t = match isolated() {
        Role::Child(t) => t,
        Role::Parent(out) => {
            assert!(out.stderr.contains("/hello failed: kaboom"), "{}", out.stderr);
            return;
        }
    };
    configure(&t, json!({}));
    let h = setup(SetupOptions {
        no_session: true,
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    // A stand-in for a plugin whose command throws.
    let loaded = h
        .host
        .load("asker", |api| {
            Box::pin(async move {
                let hello =
                    sasacode_plugin_api::Command::new("hello", "say hello", |_| async { anyhow::bail!("kaboom") });
                api.register_command(hello);
                Ok(())
            })
        })
        .await;
    assert!(loaded);
    let opts = sasacode_cli::headless::HeadlessOptions { slash: true };
    let code = sasacode_cli::headless::run_headless(
        &h,
        "/hello boom".into(),
        sasacode_cli::headless::OutputFormat::Text,
        None::<tokio::io::Empty>,
        opts,
    )
    .await;
    assert_eq!(code, 1);
}

/// A stdio server that writes the folder it starts in to `mark` and exits.
#[cfg(unix)]
fn pwd_server(mark: &std::path::Path, cwd: Option<&str>) -> Value {
    let mut server = json!({
        "command": "/bin/sh",
        "args": ["-c", "pwd -P > \"$MARK\""],
        "env": { "MARK": mark.to_string_lossy() },
    });
    if let Some(cwd) = cwd {
        server["cwd"] = cwd.into();
    }
    server
}

/// The folder the server wrote to `mark` once it started.
#[cfg(unix)]
async fn started_in(mark: &std::path::Path) -> std::path::PathBuf {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(mark)
            && !text.is_empty()
        {
            return std::path::PathBuf::from(text.trim_end());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the server never started: {}", mark.display());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stdio_server_starts_in_the_project_only_when_the_project_or_its_cwd_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = child() else { return };
    let marks = t.home.parent().expect("the root").join("marks");
    let elsewhere = t.home.parent().expect("the root").join("elsewhere");
    std::fs::create_dir_all(&marks).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mark = |n: &str| marks.join(n);
    // The global servers: no cwd, the project written out, and a folder of the user's own.
    configure(
        &t,
        json!({ "mcpServers": {
            "g.x": pwd_server(&mark("global"), None),
            "explicit": pwd_server(&mark("explicit"), Some("${SASACODE_PROJECT}")),
            "own": pwd_server(&mark("own"), Some(&elsewhere.to_string_lossy())),
            "tuned": pwd_server(&mark("tuned"), None),
            "swapped": pwd_server(&mark("swapped-global"), None),
        } }),
    );
    // The project's own server, which the user trusted. It may set other things on a global server
    // without taking over how it starts, but a command it writes itself is its own.
    t.project_config(&json!({ "mcpServers": {
        "mine": pwd_server(&mark("project"), None),
        "tuned": { "disabled": false, "timeout": 5000 },
        "swapped": pwd_server(&mark("swapped-project"), None),
    } }));
    let _h = loaded(&t, true).await;
    let proj = t.proj.canonicalize().unwrap();
    let home = t.home.canonicalize().unwrap();
    let global = started_in(&mark("global")).await;
    assert_eq!(global, home.join("tmp").join("mcp").join("g%2Ex"));
    assert_eq!(std::fs::metadata(&global).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(started_in(&mark("project")).await, proj);
    assert_eq!(started_in(&mark("explicit")).await, proj);
    assert_eq!(started_in(&mark("own")).await, elsewhere.canonicalize().unwrap());
    assert_eq!(
        started_in(&mark("tuned")).await,
        home.join("tmp").join("mcp").join("tuned")
    );
    assert_eq!(started_in(&mark("swapped-project")).await, proj);
}
