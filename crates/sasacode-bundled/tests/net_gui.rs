//! packages/bundled/test/gui.test.ts: /gui and open_gui with a stand-in for sasacode-gui (a shell
//! script, unix only). The PATH case runs in a child test process with PATH set.

mod support_net;

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use serde_json::{Value, json};

#[cfg(unix)]
use sasacode_agent::agent::{Agent, AgentOptions};
#[cfg(unix)]
use sasacode_agent::session::SessionFile;
#[cfg(unix)]
use sasacode_ai::{Message, UserContent, UserMessage};
use sasacode_bundled::gui::{GuiTarget, gui_args, own_program_for};
#[cfg(unix)]
use sasacode_bundled::gui::{open_gui, own_program};
#[cfg(unix)]
use sasacode_plugin_api::{CommandContext, Lang, NotifyLevel, PermissionMode};
#[cfg(unix)]
use support_net::*;

/// A stand-in for sasacode-gui: writes where it was started and what it was given, then `then`.
#[cfg(unix)]
struct FakeGui {
    command: String,
    out: PathBuf,
}

#[cfg(unix)]
impl FakeGui {
    fn new(root: &Path, name: &str, then: &str) -> FakeGui {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        let command = dir.join("sasacode-gui");
        let out = dir.join("given");
        let o = out.display();
        // /bin/mv: the PATH case leaves only the stand-in's own folder on PATH.
        write_script(
            &command,
            &format!(
                "#!/bin/sh\n{{ pwd; for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > \"{o}.part\"\n/bin/mv \"{o}.part\" \"{o}\"\n{then}\n"
            ),
        );
        FakeGui {
            command: s(&command),
            out,
        }
    }

    /// Waits for it: a program started for the first time can take longer than the launcher waits.
    async fn given(&self) -> Vec<String> {
        until(200, 25, || self.out.exists()).await;
        let text = std::fs::read_to_string(&self.out).unwrap_or_default();
        text.trim_end().split('\n').map(str::to_string).collect()
    }
}

/// What the stand-in is given: where it started, then its arguments. The window is told which
/// sasacode to run: the TS one under `bun test` told it none (a sasacode run from source).
#[cfg(unix)]
fn given(root: &Path, session: Option<&str>) -> Vec<String> {
    let mut args = vec![s(root)];
    args.extend(gui_args(&GuiTarget {
        cwd: s(root),
        session: session.map(str::to_string),
        program: own_program(),
    }));
    args
}

/// The plugin in a host, with what it told the user.
#[cfg(unix)]
struct WithGui {
    agent: Agent,
    host: Arc<sasacode_agent::plugins::PluginHost>,
    notes: Notices,
}

#[cfg(unix)]
impl WithGui {
    async fn new(root: &Path, settings: Value, session: Option<Arc<SessionFile>>) -> WithGui {
        let mut o = AgentOptions::new(model("none", json!({})), s(root), "");
        o.permissions = Some(policy(PermissionMode::Edits));
        o.session = session;
        let agent = Agent::new(o);
        let mut all = HashMap::new();
        all.insert("gui".to_string(), settings);
        let (host, notes) = host_for(&agent, &s(root), all, Lang::Ja);
        assert!(host.load("gui", bundled("gui")).await);
        WithGui { agent, host, notes }
    }

    async fn run(&self) {
        let c = self
            .host
            .commands()
            .into_iter()
            .find(|c| c.command.name == "gui")
            .map(|c| c.command);
        let c = c.unwrap_or_else(|| panic!("no /gui"));
        let _ = (c.run)(CommandContext { args: String::new() }).await;
    }

    fn texts(&self) -> Vec<String> {
        texts(&self.notes)
    }
}

#[test]
fn the_windows_arguments_the_sasacode_to_run_the_conversation_then_the_folder() {
    let t = GuiTarget {
        cwd: "/p".into(),
        ..GuiTarget::default()
    };
    assert_eq!(gui_args(&t), ["/p"]);
    let t = GuiTarget {
        cwd: "/p".into(),
        session: Some("17cae3ae".into()),
        program: Some("/usr/local/bin/sasacode".into()),
    };
    assert_eq!(
        gui_args(&t),
        ["--sasacode", "/usr/local/bin/sasacode", "--session", "17cae3ae", "/p"]
    );
}

#[test]
fn a_compiled_sasacode_names_itself_to_the_window_and_when_sasacode_bin_chooses_it_does_not() {
    assert_eq!(
        own_program_for("/usr/local/bin/sasacode", None).as_deref(),
        Some("/usr/local/bin/sasacode")
    );
    let win = "C:\\Users\\me\\.sasacode\\bin\\sasacode.exe";
    assert_eq!(own_program_for(win, None).as_deref(), Some(win));
    // (The TS case of a sasacode run from source by bun is gone: the Rust one is always compiled.)
    assert_eq!(own_program_for("/usr/local/bin/sasacode", Some("/opt/sasacode")), None);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn gui_before_anything_is_said_opens_the_window_on_the_folder_alone() {
    let _l = lang(Lang::Ja).await;
    let (_d, root) = real_temp_dir("sasacode-gui-");
    let fake = FakeGui::new(&root, "fresh", "exit 0");
    // A session that has an id and, with nothing said in it, no log.
    let session = SessionFile::create(&root.join("sessions"), &s(&root), Vec::new()).unwrap_or_else(|e| panic!("{e}"));
    let g = WithGui::new(&root, json!({ "command": fake.command }), Some(session.clone())).await;
    g.run().await;
    assert!(!session.path().exists());
    assert_eq!(fake.given().await, given(&root, None));
    assert_eq!(g.texts(), ["GUIを開きました。"]);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn gui_in_a_conversation_opens_the_window_on_it_and_says_the_two_do_not_follow_each_other() {
    let _l = lang(Lang::Ja).await;
    let (_d, root) = real_temp_dir("sasacode-gui-");
    let fake = FakeGui::new(&root, "talking", "exit 0");
    let session = SessionFile::create(&root.join("sessions"), &s(&root), Vec::new()).unwrap_or_else(|e| panic!("{e}"));
    let g = WithGui::new(&root, json!({ "command": fake.command }), Some(session.clone())).await;
    let hello = Message::User(UserMessage {
        content: vec![UserContent::text("こんにちは")],
        timestamp: sasacode_ai::now_ms(),
    });
    g.agent.replace_messages(vec![hello]).unwrap_or_else(|e| panic!("{e}"));
    g.run().await;
    assert_eq!(fake.given().await, given(&root, Some(session.id())));
    assert!(
        g.texts().join("\n").contains("互いの発言を読み込みません"),
        "{:?}",
        g.texts()
    );
}

#[cfg(unix)]
#[test]
fn the_window_is_found_on_path_when_no_command_is_set_at_the_time_it_is_asked_for() {
    let test = "the_window_is_found_on_path_when_no_command_is_set_at_the_time_it_is_asked_for";
    if is_child() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build();
        return rt.unwrap_or_else(|e| panic!("{e}")).block_on(found_on_path());
    }
    let (_d, root) = real_temp_dir("sasacode-gui-");
    // PATH is the stand-in's folder, still empty: nothing there yet.
    std::fs::create_dir_all(root.join("on-path")).unwrap_or_else(|e| panic!("{e}"));
    run_child_blocking(test, &[("PATH", s(&root.join("on-path"))), ("NET_GUI_ROOT", s(&root))]);
}

/// In the child: PATH is $NET_GUI_ROOT/on-path, which has no sasacode-gui yet.
#[cfg(unix)]
async fn found_on_path() {
    let _l = lang(Lang::Ja).await;
    let root = PathBuf::from(std::env::var("NET_GUI_ROOT").unwrap_or_default());
    let g = WithGui::new(&root, json!({}), None).await;
    g.run().await;
    let first = g.notes.lock().unwrap_or_else(|e| e.into_inner()).first().cloned();
    let (text, level) = first.unwrap_or_else(|| panic!("nothing said"));
    assert!(text.contains("sasacode-guiが見つかりません"), "{text}");
    assert_eq!(level, NotifyLevel::Error);
    assert!(
        text.contains("\n  cargo install --locked --path crates/sasacode-gui\n"),
        "{text}"
    );

    // Installed meanwhile: the same session finds it.
    let fake = FakeGui::new(&root, "on-path", "exit 0");
    g.run().await;
    assert_eq!(fake.given().await, given(&root, None));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_leaves_at_once_with_an_error_is_reported_not_announced_as_opened() {
    let _l = lang(Lang::Ja).await;
    let (_d, root) = real_temp_dir("sasacode-gui-");
    // Waited for longer than the launcher does by itself: what is checked is what an exit is taken for.
    let grace = Some(Duration::from_secs(10));
    let old = FakeGui::new(
        &root,
        "old",
        "echo 'sasacode-gui: unknown option --session' >&2; exit 2",
    );
    let target = |session: Option<&str>| GuiTarget {
        cwd: s(&root),
        session: session.map(str::to_string),
        program: None,
    };
    let e = open_gui(target(Some("17cae3ae")), Some(old.command.clone()), grace).await;
    let e = e.err().map(|e| e.to_string()).unwrap_or_default();
    let re = regex::Regex::new(r"(?s)すぐに終了しました.*終了コード2").unwrap_or_else(|e| panic!("{e}"));
    assert!(re.is_match(&e), "{e}");
    let nowhere = s(&root.join("nowhere").join("sasacode-gui"));
    let e = open_gui(target(None), Some(nowhere), grace)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("sasacode-guiを起動できませんでした"), "{e}");
    // A launcher that starts the window and leaves has not failed.
    let launcher = FakeGui::new(&root, "launcher", "exit 0");
    let opened = open_gui(target(None), Some(launcher.command.clone()), grace).await;
    assert_eq!(opened.map_err(|e| e.to_string()), Ok(launcher.command.clone()));
}
