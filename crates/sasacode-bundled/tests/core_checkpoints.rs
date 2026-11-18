//! packages/bundled/test/checkpoints.test.ts — checkpoints: write / edit が変えたファイルの直前の内容を
//! 退避し、/undo で戻す。The TS drove the plugin through a fake api; here it runs in a real host,
//! and the hooks are fired as the harness fires them (tool_call, the tool, tool_result).
//! These assertions quote the Japanese strings, so each test holds the language lock set to ja.

mod support_core;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionEntry;
use sasacode_ai::ToolCall;
use sasacode_plugin_api::{
    CommandContext, Lang, PermissionMode, PluginApi, ToolCallEvent, ToolContext, ToolRef, ToolResult, ToolResultEvent,
    hook,
};
use sasacode_tools::{EditTool, WriteTool};
use support_core::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn write_tool() -> ToolRef {
    Arc::new(WriteTool)
}

fn edit_tool() -> ToolRef {
    Arc::new(EditTool)
}

/// Just enough of a session to drive the plugin: its hooks, its command, its session log.
struct Fake {
    agent: Agent,
    host: Arc<PluginHost>,
    api: PluginApi,
    notices: Arc<Mutex<Vec<String>>>,
    root: String,
    n: AtomicUsize,
}

async fn fake(root: &str, settings: Value, entries: Vec<Value>) -> Fake {
    let mut o = AgentOptions::new(model("replay", 10_000, 1000), root, "s");
    o.permissions = Some(policy(PermissionMode::Edits));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: root.into(),
        settings: Some(Arc::new(move |_: &str| settings.clone())),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    let ui = NoticeUi::default();
    let notices = ui.notices.clone();
    host.set_ui(Some(Arc::new(ui)));
    // A resumed session: the log's entries are there before the plugin loads.
    let log: Vec<SessionEntry> = entries
        .into_iter()
        .map(|data| SessionEntry::Custom {
            plugin: "checkpoints".into(),
            kind: "checkpoint".into(),
            data,
        })
        .collect();
    host.set_session_entries(&log);
    let slot: Arc<Mutex<Option<PluginApi>>> = Arc::default();
    let s = slot.clone();
    assert!(
        host.load("checkpoints", move |api: PluginApi| {
            *s.lock().unwrap_or_else(|e| e.into_inner()) = Some(api.clone());
            bundled("checkpoints")(api)
        })
        .await
    );
    let api = slot.lock().unwrap().clone().unwrap();
    Fake {
        agent,
        host,
        api,
        notices,
        root: root.into(),
        n: AtomicUsize::new(0),
    }
}

impl Fake {
    /// The session log as `entries` would hand it to a resumed session.
    fn entries(&self) -> Vec<Value> {
        self.api.session().entries("checkpoint")
    }

    fn notices(&self) -> String {
        self.notices.lock().unwrap().join(",")
    }

    async fn undo(&self, args: &str) {
        let c = self
            .host
            .commands()
            .into_iter()
            .find(|c| c.command.name == "undo")
            .unwrap();
        (c.command.run)(CommandContext { args: args.into() }).await.unwrap();
    }

    /// One call with the given id as the harness runs it: tool_call, then the tool, then tool_result.
    async fn call_as(&self, id: &str, tool: &ToolRef, args: Value, denied: bool) {
        let args = obj(args);
        let c = ToolCall {
            id: id.into(),
            name: tool.name().into(),
            input: args.clone(),
            raw_input: None,
        };
        let hooks = self.agent.hooks();
        hooks
            .emit::<hook::ToolCall>(ToolCallEvent {
                call: c.clone(),
                tool: tool.clone(),
                args: args.clone(),
            })
            .await;
        if denied {
            let result = ToolResult {
                content: Vec::new(),
                is_error: Some(true),
                details: None,
            };
            hooks
                .emit::<hook::ToolResult>(ToolResultEvent {
                    call: c,
                    result,
                    ran: false,
                })
                .await;
            return;
        }
        let result = tool
            .execute(args, ToolContext::new(&self.root))
            .await
            .unwrap_or_else(|e| ToolResult::error(e.to_string()));
        hooks
            .emit::<hook::ToolResult>(ToolResultEvent {
                call: c,
                result,
                ran: true,
            })
            .await;
    }

    async fn call(&self, tool: &ToolRef, args: Value) {
        let id = format!("c{}", self.n.fetch_add(1, Ordering::Relaxed) + 1);
        self.call_as(&id, tool, args, false).await;
    }

    async fn denied(&self, tool: &ToolRef, args: Value) {
        let id = format!("c{}", self.n.fetch_add(1, Ordering::Relaxed) + 1);
        self.call_as(&id, tool, args, true).await;
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// A test body in the Japanese interface, with its own root directory.
fn ja<F, Fut>(f: F)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _lang = lang_lock(Lang::Ja);
    let root = temp_dir("sasacode-checkpoints-");
    rt().block_on(f(path_str(root.path())));
}

#[test]
fn write_over_a_file_undo_puts_the_old_content_back() {
    ja(|root| async move {
        let f = Path::new(&root).join("a.txt");
        std::fs::write(&f, "before").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "a.txt", "content": "after" }))
            .await;
        assert_eq!(read(&f), "after");
        g.undo("").await;
        assert_eq!(read(&f), "before");
        assert!(g.notices().contains("戻しました"), "{}", g.notices());
    });
}

#[test]
fn write_that_creates_a_file_undo_deletes_it_again() {
    ja(|root| async move {
        let f = Path::new(&root).join("new.txt");
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "new.txt", "content": "x" }))
            .await;
        assert!(f.exists());
        g.undo("").await;
        assert!(!f.exists());
        assert!(g.notices().contains("削除しました"), "{}", g.notices());
    });
}

#[test]
fn edit_undo_by_path_restores_the_edited_file() {
    ja(|root| async move {
        let f = Path::new(&root).join("e.txt");
        std::fs::write(&f, "a b c").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(
            &edit_tool(),
            json!({ "path": "e.txt", "old_string": "b", "new_string": "X" }),
        )
        .await;
        assert_eq!(read(&f), "a X c");
        g.undo("e.txt").await;
        assert_eq!(read(&f), "a b c");
    });
}

#[test]
fn a_denied_failing_or_no_op_call_leaves_no_checkpoint() {
    ja(|root| async move {
        let f = Path::new(&root).join("n.txt");
        std::fs::write(&f, "same").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.denied(&write_tool(), json!({ "path": "n.txt", "content": "y" }))
            .await; // denied: never ran
        g.call(
            &edit_tool(),
            json!({ "path": "n.txt", "old_string": "nope", "new_string": "z" }),
        )
        .await; // old_string not found
        g.call(&write_tool(), json!({ "path": "n.txt", "content": "same" }))
            .await; // writes the same content back
        assert_eq!(read(&f), "same");
        g.undo("list").await;
        assert!(g.notices().contains("戻せる変更はありません"), "{}", g.notices());
    });
}

#[test]
fn two_changes_to_one_file_undo_one_at_a_time() {
    ja(|root| async move {
        let f = Path::new(&root).join("m.txt");
        std::fs::write(&f, "1").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "m.txt", "content": "2" })).await;
        g.call(&write_tool(), json!({ "path": "m.txt", "content": "3" })).await;
        g.undo("").await;
        assert_eq!(read(&f), "2");
        g.undo("").await;
        assert_eq!(read(&f), "1");
        g.undo("").await;
        assert!(g.notices().contains("戻せる変更はありません"), "{}", g.notices());
    });
}

#[test]
fn checkpoints_survive_a_resume_they_live_in_the_session_log() {
    ja(|root| async move {
        let f = Path::new(&root).join("r.txt");
        std::fs::write(&f, "r1").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "r.txt", "content": "r2" })).await;
        let resumed = fake(&root, json!({}), g.entries()).await;
        resumed.undo("").await;
        assert_eq!(read(&f), "r1");
    });
}

#[test]
fn a_file_over_the_limit_is_not_kept_undo_says_so_instead_of_guessing() {
    ja(|root| async move {
        let f = Path::new(&root).join("big.txt");
        std::fs::write(&f, "old").unwrap();
        let g = fake(&root, json!({ "maxFileKB": 0 }), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "big.txt", "content": "new" }))
            .await;
        assert_eq!(read(&f), "new");
        g.undo("").await;
        assert!(g.notices().contains("戻せません"), "{}", g.notices());
        assert_eq!(read(&f), "new");
    });
}

#[test]
fn a_file_that_is_not_utf8_comes_back_byte_for_byte_117() {
    ja(|root| async move {
        let f = Path::new(&root).join("readme-sjis.txt");
        let sjis: &[u8] = &[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea, 0x0a]; // "日本語\n" in Shift_JIS
        std::fs::write(&f, sjis).unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "readme-sjis.txt", "content": "new\n" }))
            .await;
        g.undo("").await;
        assert!(g.notices().contains("戻しました"), "{}", g.notices());
        assert_eq!(std::fs::read(&f).unwrap(), sjis);
    });
}

#[test]
fn checkpoints_whose_calls_share_an_id_undo_one_at_a_time_117() {
    ja(|root| async move {
        // A server that sends no ids gets call_0 in every turn (openai-chat).
        let (a, b) = (Path::new(&root).join("ida.txt"), Path::new(&root).join("idb.txt"));
        std::fs::write(&a, "a0").unwrap();
        std::fs::write(&b, "b0").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        for (path, content) in [("ida.txt", "a1"), ("idb.txt", "b1")] {
            g.call_as(
                "call_0",
                &write_tool(),
                json!({ "path": path, "content": content }),
                false,
            )
            .await;
        }
        g.undo("").await;
        assert_eq!(read(&b), "b0");
        assert_eq!(read(&a), "a1");
        g.undo("").await;
        assert_eq!(read(&a), "a0");
    });
}

#[test]
fn undo_name_matches_whole_path_parts_index_ts_is_not_reindex_ts_117() {
    ja(|root| async move {
        let (index, reindex) = (Path::new(&root).join("index.ts"), Path::new(&root).join("reindex.ts"));
        std::fs::write(&index, "index v0").unwrap();
        std::fs::write(&reindex, "reindex v0").unwrap();
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "index.ts", "content": "index v1" }))
            .await;
        g.call(&write_tool(), json!({ "path": "reindex.ts", "content": "reindex v1" }))
            .await;
        g.undo("index.ts").await;
        assert_eq!(read(&index), "index v0");
        assert_eq!(read(&reindex), "reindex v1");
    });
}

#[test]
fn undo_name_that_fits_two_files_lists_them_and_restores_neither_117() {
    ja(|root| async move {
        for d in ["p1", "p2"] {
            std::fs::create_dir_all(Path::new(&root).join(d)).unwrap();
            std::fs::write(Path::new(&root).join(d).join("same.ts"), "0").unwrap();
        }
        let g = fake(&root, json!({}), Vec::new()).await;
        g.call(&write_tool(), json!({ "path": "p1/same.ts", "content": "1" }))
            .await;
        g.call(&write_tool(), json!({ "path": "p2/same.ts", "content": "1" }))
            .await;
        g.undo("same.ts").await;
        assert!(g.notices().contains("複数"), "{}", g.notices());
        let both = read(&Path::new(&root).join("p1/same.ts")) + &read(&Path::new(&root).join("p2/same.ts"));
        assert_eq!(both, "11");
        g.undo("p1/same.ts").await;
        assert_eq!(read(&Path::new(&root).join("p1/same.ts")), "0");
    });
}
