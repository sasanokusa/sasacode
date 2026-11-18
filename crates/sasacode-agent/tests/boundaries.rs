//! `packages/agent/test/boundaries.test.ts`: regressions from the v0.6.3 and v0.7.0 reviews: symlink
//! escapes, work after an interrupt, runaway loops.
//!
//! Already in `src/session.rs` (unit tests of the session log): "restoring twice gives the same valid
//! history once the repair is recorded", "a torn last line is set aside before the session is
//! appended to", "directories whose names flatten the same get separate session folders".

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::events::AgentEvent;
use sasacode_agent::session::SessionFile;
use sasacode_ai::{AssistantMessage, JsonObject};
use sasacode_plugin_api::{PermissionMode, StopCause, Tool, ToolContext, ToolDef, ToolKind, ToolResult};
use sasacode_tools::{BashTool, EditTool, ReadTool, WriteTool};

use support::*;

/// boundaries.test.ts `turn`: provider "t", empty usage.
fn turn(content: Vec<sasacode_ai::AssistantContent>) -> AssistantMessage {
    reply(content, "t")
}

/// boundaries.test.ts `model`: `{ id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 }`.
fn model_on(api: &str) -> sasacode_ai::ModelInfo {
    model(api, "t", 100_000, 1000)
}

/// The TS module's fixture: `root` with `proj` and `outside/secret.txt`, and `proj/linked` → `outside`.
/// The temp root is used as given, not canonicalized: on macOS /var is itself a link, which is part
/// of what these tests cover.
struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    proj: PathBuf,
    #[cfg_attr(not(unix), allow(dead_code))] // read by the symlink tests only
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let root_dir = temp_dir("sasacode-bounds-");
    let root = root_dir.path().to_path_buf();
    let (proj, outside) = (root.join("proj"), root.join("outside"));
    std::fs::create_dir(&proj).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "SECRET").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, proj.join("linked")).unwrap();
    Fixture {
        _root: root_dir,
        root,
        proj,
        outside,
    }
}

fn s(p: &Path) -> String {
    path_str(p)
}

/// `(await p.check({ tool, args, cwd })).decision`, as its lowercase name.
#[cfg(unix)]
fn decide(
    p: &sasacode_agent::permission::PermissionPolicy,
    tool: Arc<dyn Tool>,
    args: serde_json::Value,
    cwd: &Path,
) -> &'static str {
    p.check(&sasacode_agent::permission::PermissionCheck {
        tool,
        args: obj(args),
        cwd: s(cwd),
    })
    .decision
    .as_str()
}

/// boundaries.test.ts: "paths through a symlink are judged by where they really go"
#[cfg(unix)]
#[test]
fn paths_through_a_symlink_are_judged_by_where_they_really_go() {
    let f = fixture();
    let (read, write): (Arc<dyn Tool>, Arc<dyn Tool>) = (Arc::new(ReadTool), Arc::new(WriteTool));
    let p = policy(PermissionMode::Edits, json!({}));
    assert_eq!(
        decide(&p, read.clone(), json!({ "path": "linked/secret.txt" }), &f.proj),
        "ask"
    );
    assert_eq!(
        decide(
            &p,
            write.clone(),
            json!({ "path": "linked/new.txt", "content": "x" }),
            &f.proj
        ),
        "ask"
    );
    assert_eq!(
        decide(
            &p,
            write.clone(),
            json!({ "path": "real/new.txt", "content": "x" }),
            &f.proj
        ),
        "allow"
    );
    // A deny rule on the real location cannot be dodged through the link …
    let deny = policy(
        PermissionMode::Auto,
        json!({ "deny": [format!("read({}/**)", s(&f.outside))] }),
    );
    assert_eq!(
        decide(&deny, read.clone(), json!({ "path": "linked/secret.txt" }), &f.proj),
        "deny"
    );
    // … and an allow rule for a project folder does not cover what a link in it points to
    // (a repo could turn `src` into a link to ~/.ssh under a user's `read(src/**)` rule).
    let allow = policy(PermissionMode::Ask, json!({ "allow": ["read(linked/**)"] }));
    assert_eq!(
        decide(&allow, read.clone(), json!({ "path": "linked/secret.txt" }), &f.proj),
        "ask"
    );
    // An absolute allow rule for the real location still works.
    let abs = policy(
        PermissionMode::Ask,
        json!({ "allow": [format!("read({}/**)", s(&f.outside))] }),
    );
    assert_eq!(
        decide(
            &abs,
            read,
            json!({ "path": format!("{}/secret.txt", s(&f.outside)) }),
            &f.proj
        ),
        "allow"
    );
}

/// boundaries.test.ts: "a link to a file that does not exist yet is judged by its target"
#[cfg(unix)]
#[test]
fn a_link_to_a_file_that_does_not_exist_yet_is_judged_by_its_target() {
    use std::os::unix::fs::symlink;
    let f = fixture();
    let write: Arc<dyn Tool> = Arc::new(WriteTool);
    symlink("../outside/new.txt", f.proj.join("dangling.txt")).unwrap();
    symlink("../outside/newdir", f.proj.join("dangling-dir")).unwrap();
    let p = policy(PermissionMode::Edits, json!({}));
    assert_eq!(
        decide(
            &p,
            write.clone(),
            json!({ "path": "dangling.txt", "content": "x" }),
            &f.proj
        ),
        "ask"
    );
    assert_eq!(
        decide(
            &p,
            write.clone(),
            json!({ "path": "dangling-dir/a/b.txt", "content": "x" }),
            &f.proj
        ),
        "ask"
    );
    let deny = policy(
        PermissionMode::Auto,
        json!({ "deny": [format!("write({}/**)", s(&f.outside))] }),
    );
    assert_eq!(
        decide(
            &deny,
            write.clone(),
            json!({ "path": "dangling.txt", "content": "x" }),
            &f.proj
        ),
        "deny"
    );
    // A link loop cannot be resolved at all: the user decides, even in auto mode.
    symlink("loop-b", f.proj.join("loop-a")).unwrap();
    symlink("loop-a", f.proj.join("loop-b")).unwrap();
    let auto = policy(PermissionMode::Auto, json!({}));
    assert_eq!(
        decide(&auto, write, json!({ "path": "loop-a", "content": "x" }), &f.proj),
        "ask"
    );
}

/// boundaries.test.ts: "a link whose target goes up through another link is followed as the OS does"
#[cfg(unix)]
#[test]
fn a_link_whose_target_goes_up_through_another_link_is_followed_as_the_os_does() {
    use sasacode_agent::permission::real_path;
    use std::os::unix::fs::symlink;
    let f = fixture();
    std::fs::create_dir(f.outside.join("child")).unwrap();
    symlink("../outside/child", f.proj.join("pivot")).unwrap();
    symlink("pivot/../new-through-pivot.txt", f.proj.join("entry")).unwrap();
    let real_outside = std::fs::canonicalize(&f.outside).unwrap();
    assert_eq!(
        real_path(&s(&f.proj.join("entry"))).unwrap(),
        s(&real_outside.join("new-through-pivot.txt"))
    );
    let p = policy(PermissionMode::Edits, json!({}));
    assert_eq!(
        decide(
            &p,
            Arc::new(WriteTool),
            json!({ "path": "entry", "content": "x" }),
            &f.proj
        ),
        "ask"
    );
    // Existing paths resolve exactly like realpath (on macOS, /var is itself a link).
    assert_eq!(
        real_path(&s(&f.proj.join("linked").join("secret.txt"))).unwrap(),
        s(&std::fs::canonicalize(f.outside.join("secret.txt")).unwrap())
    );
}

fn ctx(cwd: &Path, signal: CancellationToken) -> ToolContext {
    ToolContext {
        signal,
        ..ToolContext::new(cwd)
    }
}

/// boundaries.test.ts: "write stops if interrupted while it waits on the file system"
#[tokio::test]
async fn write_stops_if_interrupted_while_it_waits_on_the_file_system() {
    let f = fixture();
    std::fs::write(f.proj.join("w.txt"), "old").unwrap();
    let ac = CancellationToken::new();
    let mut pending = WriteTool.execute(
        obj(json!({ "path": "w.txt", "content": "new" })),
        ctx(&f.proj, ac.clone()),
    );
    // The TS call ran up to its first await (readFile) before the abort: poll once to get there.
    assert!(futures::poll!(&mut pending).is_pending());
    ac.cancel(); // the tool is now waiting on readFile
    assert!(pending.await.unwrap().is_error());
    assert_eq!(std::fs::read_to_string(f.proj.join("w.txt")).unwrap(), "old");
}

/// boundaries.test.ts: "write and edit do nothing once interrupted"
#[tokio::test]
async fn write_and_edit_do_nothing_once_interrupted() {
    let f = fixture();
    let ac = CancellationToken::new();
    ac.cancel();
    let w = WriteTool
        .execute(
            obj(json!({ "path": "after-abort.txt", "content": "x" })),
            ctx(&f.proj, ac.clone()),
        )
        .await
        .unwrap();
    assert!(w.is_error());
    assert!(!f.proj.join("after-abort.txt").exists());
    std::fs::write(f.proj.join("e.txt"), "a").unwrap();
    let e = EditTool
        .execute(
            obj(json!({ "path": "e.txt", "old_string": "a", "new_string": "b" })),
            ctx(&f.proj, ac),
        )
        .await
        .unwrap();
    assert!(e.is_error());
    assert_eq!(std::fs::read_to_string(f.proj.join("e.txt")).unwrap(), "a");
}

/// boundaries.test.ts: "an interrupt during one tool stops the approved calls after it"
#[tokio::test]
async fn an_interrupt_during_one_tool_stops_the_approved_calls_after_it() {
    let f = fixture();
    let api = api_name();
    register(
        &api,
        vec![turn(vec![
            call("1", "bash", json!({ "command": "sleep 5" })),
            call("2", "write", json!({ "path": "late.txt", "content": "x" })),
        ])],
        None,
    );
    let mut o = AgentOptions::new(model_on(&api), s(&f.proj), "");
    o.permissions = Some(policy(PermissionMode::Auto, json!({})));
    o.tools = vec![Arc::new(WriteTool), Arc::new(BashTool)];
    let agent = Agent::new(o);
    let a = agent.clone();
    let _ = agent.events().on(move |e| {
        if let AgentEvent::ToolStart { call, .. } = e
            && call.name == "bash"
        {
            let a = a.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                a.abort();
            });
        }
    });
    assert_eq!(agent.prompt_text("go").await.unwrap(), StopCause::Aborted);
    assert!(!f.proj.join("late.txt").exists());
    assert!(json(agent.messages().last().unwrap()).contains("Interrupted by the user before this tool ran"));
}

/// boundaries.test.ts: "each result is saved before the next call starts"
#[tokio::test]
async fn each_result_is_saved_before_the_next_call_starts() {
    let f = fixture();
    let session = SessionFile::create(&f.root.join("sessions-each"), &s(&f.proj), Vec::new()).unwrap();
    let saved: Arc<Mutex<Vec<String>>> = Arc::default();
    let (sv, path) = (saved.clone(), session.path().to_path_buf());
    let probe = ToolDef::new(
        "probe",
        "",
        json!({ "type": "object", "properties": {} }),
        ToolKind::Read,
        move |_a: JsonObject, _c| {
            sv.lock().unwrap().push(std::fs::read_to_string(&path).unwrap());
            async { Ok(ToolResult::text("probe ran")) }
        },
    )
    .into_ref();
    let api = api_name();
    register(
        &api,
        vec![
            turn(vec![call("1", "probe", json!({})), call("2", "probe", json!({}))]),
            turn(vec![say("done")]),
        ],
        None,
    );
    let mut o = AgentOptions::new(model_on(&api), s(&f.proj), "");
    o.session = Some(session);
    o.permissions = Some(policy(PermissionMode::Auto, json!({})));
    o.tools = vec![probe];
    Agent::new(o).prompt_text("go").await.unwrap();
    let saved = saved.lock().unwrap();
    assert!(!saved[0].contains("probe ran"));
    // The first call's result was on disk when the second started.
    assert!(saved[1].contains("probe ran"));
}

/// boundaries.test.ts: "the core does not end a run for lack of progress (A6): that is left to plugins and maxTurns"
#[tokio::test]
async fn the_core_does_not_end_a_run_for_lack_of_progress_a6_that_is_left_to_plugins_and_max_turns() {
    let f = fixture();
    let bad = |i: usize| turn(vec![call(&i.to_string(), "no_such_tool", json!({}))]);
    let api = api_name();
    let provider = register(&api, (0..20).map(bad).collect(), None);
    let mut o = AgentOptions::new(model_on(&api), s(&f.proj), "");
    o.permissions = Some(policy(PermissionMode::Auto, json!({})));
    o.tools = vec![Arc::new(ReadTool)];
    o.max_turns = Some(8);
    assert_eq!(Agent::new(o).prompt_text("go").await.unwrap(), StopCause::MaxTurns);
    assert_eq!(provider.requests().len(), 8);
}
