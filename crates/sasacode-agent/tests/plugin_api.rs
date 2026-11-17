//! `packages/agent/test/plugin-api-v19.test.ts` and `plugin-api-v110.test.ts`.
//!
//! Dropped asserts: the TS checked argument types at run time ("input requires …",
//! "selectMany requires …", "selected must be …"); in Rust the types are checked by the compiler.
//! "copy inputs at invocation" holds by construction: the input is moved into the call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::events::{AgentEvent, EventBus};
use sasacode_agent::permission::PermissionPolicy;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionEntry, SessionFile};
use sasacode_agent::tool_exec::{
    ApprovalAnswer, ApprovalDecision, ApproveFn, ExecOptions, ToolRunContext, execute_tools,
};
use sasacode_ai::{JsonObject, ModelInfo, ToolCall, ToolResultMessage};
use sasacode_plugin_api::{
    HookContext, HookRunner, InputOptions, Lang, NotifyLevel, PLUGIN_API_VERSION, PermissionMode, PermissionRules,
    PluginApi, SelectManyOptions, SelectOption, ShowTextOptions, TextFormat, ToolContext, ToolDef, ToolKind, ToolRef,
    ToolResult, UiBridge, hook,
};

type Exec = Arc<dyn Fn(JsonObject, ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> + Send + Sync>;

fn make_tool(name: &str, exec: Exec) -> ToolRef {
    ToolDef::new(name, name, json!({"type": "object", "properties": {}}), ToolKind::Exec, move |a, c| exec(a, c))
        .into_ref()
}
fn calling(name: &str, target: &'static str) -> ToolRef {
    make_tool(
        name,
        Arc::new(move |_a, ctx: ToolContext| {
            Box::pin(async move { Ok((ctx.call_tool.unwrap())(target.into(), JsonObject::new()).await) })
        }),
    )
}
fn result(s: &str) -> ToolResult {
    ToolResult::text(s)
}
fn body(r: &ToolResult) -> String {
    serde_json::to_string(&r.content).unwrap()
}
fn obj(v: Value) -> JsonObject {
    v.as_object().cloned().unwrap()
}
fn policy(mode: PermissionMode, rules: Value) -> Arc<PermissionPolicy> {
    Arc::new(PermissionPolicy::new(mode, &serde_json::from_value::<PermissionRules>(rules).unwrap()).unwrap())
}
fn auto() -> Arc<PermissionPolicy> {
    policy(PermissionMode::Auto, json!({}))
}

struct Setup {
    tools: Vec<ToolRef>,
    hooks: HookRunner,
    events: EventBus,
    seen: Arc<Mutex<Vec<AgentEvent>>>,
    permissions: Arc<PermissionPolicy>,
    approve: Option<ApproveFn>,
    session: Option<Arc<SessionFile>>,
    cwd: String,
}

struct Run {
    output: ToolResult,
    messages: Vec<ToolResultMessage>,
}

impl Setup {
    fn new(tools: Vec<ToolRef>, permissions: Arc<PermissionPolicy>, agent: Option<&str>) -> Setup {
        let events = EventBus::default();
        let seen: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
        let s = seen.clone();
        let _ = events.on(move |e| s.lock().unwrap().push(e.clone()));
        Setup {
            tools,
            hooks: HookRunner::new(HookContext { agent: agent.map(str::to_string) }),
            events,
            seen,
            permissions,
            approve: None,
            session: None,
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        }
    }
    fn ctx(&self) -> Arc<ToolRunContext> {
        let (t1, t2) = (self.tools.clone(), self.tools.clone());
        Arc::new(ToolRunContext {
            cwd: self.cwd.clone(),
            hooks: self.hooks.clone(),
            events: self.events.clone(),
            permissions: self.permissions.clone(),
            approve: self.approve.clone(),
            session: self.session.clone(),
            find_tool: Arc::new(move |n| t1.iter().find(|t| t.name() == n).cloned()),
            all_tools: Arc::new(move || t2.clone()),
        })
    }
    async fn run(&self, name: &str, signal: CancellationToken) -> Run {
        let output: Arc<Mutex<Option<ToolResult>>> = Arc::default();
        let messages: Arc<Mutex<Vec<ToolResultMessage>>> = Arc::default();
        let (o, m) = (output.clone(), messages.clone());
        let call = ToolCall { id: "parent".into(), name: name.into(), input: JsonObject::new(), raw_input: None };
        execute_tools(
            self.ctx(),
            vec![call],
            ExecOptions {
                truncated: false,
                signal,
                repairs: HashMap::new(),
                on_result: Arc::new(move |r| {
                    m.lock().unwrap().push(r);
                    Ok(())
                }),
                ancestors: Vec::new(),
                parent_call_id: None,
                on_tool_result: Some(Arc::new(move |r| *o.lock().unwrap() = Some(r))),
            },
        )
        .await
        .unwrap();
        let output = output.lock().unwrap().take().unwrap();
        let messages = messages.lock().unwrap().clone();
        Run { output, messages }
    }
    async fn run_default(&self) -> Run {
        self.run("wrapper", CancellationToken::new()).await
    }
}

#[tokio::test]
async fn nested_calls_obey_target_deny_rules_despite_a_parent_allow_and_emit_ran_false() {
    let ran = Arc::new(Mutex::new(false));
    let r = ran.clone();
    let target = make_tool(
        "target",
        Arc::new(move |_a, _c| {
            let r = r.clone();
            Box::pin(async move {
                *r.lock().unwrap() = true;
                Ok(result("bad"))
            })
        }),
    );
    let h = Setup::new(
        vec![calling("wrapper", "target"), target],
        policy(PermissionMode::Auto, json!({ "allow": ["wrapper"], "deny": ["target"] })),
        None,
    );
    let results: Arc<Mutex<Vec<bool>>> = Arc::default();
    let rs = results.clone();
    h.hooks.on::<hook::ToolResult, _, _>("test", move |ev, _| {
        let rs = rs.clone();
        async move {
            if ev.call.name == "target" {
                rs.lock().unwrap().push(ev.ran);
            }
            Ok(None)
        }
    });
    let Run { output, messages } = h.run_default().await;
    assert!(!*ran.lock().unwrap());
    assert!(output.is_error());
    assert!(body(&output).contains("Permission denied"));
    assert_eq!(*results.lock().unwrap(), vec![false]);
    assert_eq!(messages.len(), 1);
    let seen = h.seen.lock().unwrap();
    let end = seen.iter().find_map(|e| match e {
        AgentEvent::ToolEnd { call, parent_call_id, .. } if call.name == "target" => Some(parent_call_id.clone()),
        _ => None,
    });
    assert_eq!(end, Some(Some("parent".to_string())));
}

#[tokio::test]
async fn rewrites_are_validated_permission_hooks_see_the_actual_input_details_survive_identity_kept() {
    let target = ToolDef::new(
        "target",
        "target",
        json!({ "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } }, "required": ["n"], "additionalProperties": false }),
        ToolKind::Exec,
        |args, _c| async move {
            let n = args.get("n").cloned().unwrap_or(Value::Null);
            Ok(ToolResult::text(n.to_string()).with_details(obj(json!({ "ok": true }))))
        },
    )
    .into_ref();
    let wrapper = make_tool(
        "wrapper",
        Arc::new(|_a, ctx: ToolContext| {
            Box::pin(async move { Ok((ctx.call_tool.unwrap())("target".into(), obj(json!({ "n": 1 }))).await) })
        }),
    );
    let h = Setup::new(vec![wrapper, target], auto(), Some("sub-example"));
    let contexts: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let cs = contexts.clone();
    h.hooks.on::<hook::ToolCall, _, _>("test", move |ev, ctx| {
        let cs = cs.clone();
        async move {
            cs.lock().unwrap().push(ctx.agent.clone());
            if ev.call.name == "target" {
                return Ok(Some(sasacode_plugin_api::ToolCallResult {
                    args: Some(obj(json!({ "n": 2 }))),
                    ..Default::default()
                }));
            }
            Ok(None)
        }
    });
    let permission_args: Arc<Mutex<Vec<Value>>> = Arc::default();
    let pa = permission_args.clone();
    h.hooks.on::<hook::Permission, _, _>("test", move |ev, _| {
        let pa = pa.clone();
        async move {
            if ev.call.name == "target" {
                pa.lock().unwrap().push(ev.args.get("n").cloned().unwrap_or(Value::Null));
            }
            Ok(None)
        }
    });
    let Run { output, .. } = h.run_default().await;
    assert!(body(&output).contains('2'));
    assert_eq!(output.details, Some(obj(json!({ "ok": true }))));
    let sub = Some("sub-example".to_string());
    assert_eq!(*contexts.lock().unwrap(), vec![sub.clone(), sub]);
    // The hook ran in auto mode only if a permission hook is registered: it is, and saw n = 2.
    assert_eq!(*permission_args.lock().unwrap(), vec![json!(2)]);
    h.hooks.on::<hook::ToolCall, _, _>("test2", |ev, _| async move {
        if ev.call.name == "target" {
            return Ok(Some(sasacode_plugin_api::ToolCallResult {
                args: Some(obj(json!({ "n": "bad" }))),
                ..Default::default()
            }));
        }
        Ok(None)
    });
    assert!(body(&h.run_default().await.output).contains("Invalid arguments after tool_call"));
}

#[tokio::test]
async fn target_asks_independently_headless_cannot_approve_unknown_tools_never_run() {
    let mut h = Setup::new(
        vec![
            calling("wrapper", "target"),
            make_tool("target", Arc::new(|_a, _c| Box::pin(async { Ok(result("ran")) }))),
        ],
        policy(PermissionMode::Auto, json!({ "ask": ["target"] })),
        None,
    );
    assert!(body(&h.run_default().await.output).contains("no user is available"));
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let a = asked.clone();
    h.approve = Some(Arc::new(move |r| {
        a.lock().unwrap().push(r.tool.name().to_string());
        Box::pin(async { Ok(ApprovalAnswer::new(ApprovalDecision::Allow)) })
    }));
    assert!(body(&h.run_default().await.output).contains("ran"));
    assert_eq!(*asked.lock().unwrap(), vec!["target".to_string()]);
    assert!(body(&h.run("missing", CancellationToken::new()).await.output).contains("Unknown tool"));
}

#[tokio::test]
async fn cycles_and_excessive_nesting_are_stopped_without_invoking_the_rejected_tool() {
    let h = Setup::new(vec![calling("wrapper", "other"), calling("other", "wrapper")], auto(), None);
    assert!(body(&h.run_default().await.output).contains("cycle"));
    let names: Vec<&'static str> = (0..=10).map(|i| &*Box::leak(format!("t{i}").into_boxed_str())).collect();
    let chain: Vec<ToolRef> = (0..10).map(|i| calling(names[i], names[i + 1])).collect();
    let deep = Setup::new(chain, auto(), None);
    assert!(body(&deep.run("t0", CancellationToken::new()).await.output).contains("depth exceeds 8"));
    let starts = deep.seen.lock().unwrap().iter().filter(|e| matches!(e, AgentEvent::ToolStart { .. })).count();
    assert_eq!(starts, 8);
}

#[tokio::test]
async fn nested_calls_serialize_join_unawaited_calls_and_reject_expired_contexts() {
    let seen: Arc<Mutex<Vec<i64>>> = Arc::default();
    let active: Arc<Mutex<(i32, i32)>> = Arc::default();
    let captured: Arc<Mutex<Option<ToolContext>>> = Arc::default();
    let cap = captured.clone();
    let wrapper = make_tool(
        "wrapper",
        Arc::new(move |_a, ctx: ToolContext| {
            let cap = cap.clone();
            Box::pin(async move {
                *cap.lock().unwrap() = Some(ctx.clone());
                let call = ctx.call_tool.clone().unwrap();
                // Not awaited: they still run, one after the other, before the parent's result counts.
                drop(call("target".into(), obj(json!({ "n": 1 }))));
                drop(call("target".into(), obj(json!({ "n": 2 }))));
                Ok(result("parent"))
            })
        }),
    );
    let (s, act) = (seen.clone(), active.clone());
    let target = make_tool(
        "target",
        Arc::new(move |args, _c| {
            let (s, act) = (s.clone(), act.clone());
            Box::pin(async move {
                {
                    let mut a = act.lock().unwrap();
                    a.0 += 1;
                    a.1 = a.1.max(a.0);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                s.lock().unwrap().push(args.get("n").and_then(Value::as_i64).unwrap());
                act.lock().unwrap().0 -= 1;
                Ok(result("child"))
            })
        }),
    );
    let h = Setup::new(vec![wrapper, target], auto(), None);
    h.run_default().await;
    assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
    assert_eq!(active.lock().unwrap().1, 1);
    let ctx = captured.lock().unwrap().clone().unwrap();
    let late = (ctx.call_tool.unwrap())("target".into(), obj(json!({ "n": 3 }))).await;
    assert!(body(&late).contains("no longer active"));
    assert_eq!(*seen.lock().unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn abort_propagates_to_a_running_child_and_prevents_queued_children_from_executing() {
    let controller = CancellationToken::new();
    let runs = Arc::new(Mutex::new(0));
    let wrapper = make_tool(
        "wrapper",
        Arc::new(|_a, ctx: ToolContext| {
            Box::pin(async move {
                let call = ctx.call_tool.clone().unwrap();
                let one = call("target".into(), JsonObject::new());
                let two = call("target".into(), JsonObject::new());
                let (_, b) = futures::join!(one, two);
                Ok(b)
            })
        }),
    );
    let (c, r) = (controller.clone(), runs.clone());
    let target = make_tool(
        "target",
        Arc::new(move |_a, ctx: ToolContext| {
            let (c, r) = (c.clone(), r.clone());
            Box::pin(async move {
                *r.lock().unwrap() += 1;
                c.cancel();
                assert!(ctx.signal.is_cancelled());
                Ok(result("cancelled"))
            })
        }),
    );
    let h = Setup::new(vec![wrapper, target], auto(), None);
    assert!(body(&h.run("wrapper", controller).await.output).contains("Interrupted"));
    assert_eq!(*runs.lock().unwrap(), 1);
}

#[tokio::test]
async fn nested_calls_are_audited_without_unmatched_tool_messages_in_model_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Setup::new(
        vec![
            calling("wrapper", "target"),
            make_tool("target", Arc::new(|_a, _c| Box::pin(async { Ok(result("child")) }))),
        ],
        auto(),
        None,
    );
    let cwd = dir.path().to_string_lossy().to_string();
    let session = SessionFile::create(&dir.path().join("sessions"), &cwd, Vec::new()).unwrap();
    h.session = Some(session.clone());
    let Run { messages, .. } = h.run_default().await;
    assert_eq!(messages.len(), 1);
    let (_, entries) = SessionFile::open(session.path(), Vec::new()).unwrap();
    let kinds: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Custom { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec!["nested_tool_call", "nested_tool_execute", "nested_tool_result"]);
}

#[tokio::test]
async fn abort_releases_a_pending_child_approval_without_executing_or_adding_an_always_rule() {
    let controller = CancellationToken::new();
    let ran = Arc::new(Mutex::new(false));
    let r = ran.clone();
    let target = make_tool(
        "target",
        Arc::new(move |_a, _c| {
            let r = r.clone();
            Box::pin(async move {
                *r.lock().unwrap() = true;
                Ok(result("bad"))
            })
        }),
    );
    let mut h = Setup::new(
        vec![calling("wrapper", "target"), target],
        policy(PermissionMode::Auto, json!({ "ask": ["target"] })),
        None,
    );
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    h.approve = Some(Arc::new(move |_req| {
        if let Some(tx) = started_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Box::pin(futures::future::pending())
    }));
    let ctl = controller.clone();
    let run = tokio::spawn(async move { h.run("wrapper", ctl).await.output });
    started_rx.await.unwrap();
    controller.cancel();
    let output = run.await.unwrap();
    assert!(body(&output).contains("Interrupted"));
    assert!(!*ran.lock().unwrap());
}

#[tokio::test]
async fn nested_lookup_cannot_reach_a_tool_omitted_from_this_agents_registry() {
    let h = Setup::new(vec![calling("wrapper", "excluded")], auto(), None);
    assert!(body(&h.run_default().await.output).contains("Unknown tool"));
}

// ── the plugin host ──────────────────────────────────────────────────

fn model() -> ModelInfo {
    serde_json::from_value(json!({ "id": "m", "provider": "test", "api": "replay", "contextWindow": 10000, "maxOutput": 1000 }))
        .unwrap()
}

fn host(settings: Value) -> Arc<PluginHost> {
    let agent = Agent::new(AgentOptions::new(model(), "/", "test"));
    PluginHost::new(PluginHostOptions {
        agent,
        cwd: "/".into(),
        settings: Some(Arc::new(move |_| settings.clone())),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    })
}

#[derive(Default)]
struct TestUi {
    interactive: bool,
    notices: Arc<Mutex<Vec<String>>>,
    input: Option<Arc<dyn Fn(String, InputOptions) -> Option<String> + Send + Sync>>,
    select_many: Option<Arc<dyn Fn() -> Option<Vec<String>> + Send + Sync>>,
    show_text: Option<Arc<dyn Fn(ShowTextOptions) -> BoxFuture<'static, ()> + Send + Sync>>,
}

#[async_trait]
impl UiBridge for TestUi {
    fn interactive(&self) -> bool {
        self.interactive
    }
    fn notify(&self, message: &str, _level: NotifyLevel) {
        self.notices.lock().unwrap().push(message.to_string());
    }
    async fn show_text(&self, options: ShowTextOptions) {
        match &self.show_text {
            Some(f) => f(options).await,
            None => self.notify(&format!("{}\n{}", options.title, options.text), NotifyLevel::Info),
        }
    }
    async fn confirm(&self, _t: &str, _m: Option<&str>) -> bool {
        false
    }
    async fn select(&self, _t: &str, _o: &[SelectOption]) -> Option<String> {
        None
    }
    async fn input(&self, title: &str, options: &InputOptions) -> Option<String> {
        self.input.as_ref().and_then(|f| f(title.to_string(), options.clone()))
    }
    async fn select_many(&self, _t: &str, _o: &[SelectOption], _s: &SelectManyOptions) -> Option<Vec<String>> {
        self.select_many.as_ref().and_then(|f| f())
    }
}

fn api_with(ui: Option<TestUi>) -> PluginApi {
    let h = host(json!({}));
    if let Some(ui) = ui {
        h.set_ui(Some(Arc::new(ui)));
    }
    h.api("p")
}
fn opts() -> Vec<SelectOption> {
    vec![SelectOption::new("a", "A"), SelectOption::new("b", "B"), SelectOption::new("c", "C")]
}

#[tokio::test]
async fn the_host_reports_api_1_15_0() {
    assert_eq!(PLUGIN_API_VERSION, "1.15.0");
    assert_eq!(api_with(None).version(), "1.15.0");
}

#[tokio::test]
async fn headless_input_and_select_many_resolve_none_without_asking() {
    let a = api_with(None);
    assert_eq!(a.ui().input("Name?", &InputOptions::default()).await, None);
    assert_eq!(a.ui().select_many("Which?", &opts(), &SelectManyOptions::default()).await, None);
}

#[tokio::test]
async fn a_front_end_without_the_dialogs_resolves_none() {
    let a = api_with(Some(TestUi { interactive: true, ..Default::default() }));
    assert_eq!(a.ui().input("Name?", &InputOptions::default()).await, None);
    assert_eq!(a.ui().select_many("Which?", &opts(), &SelectManyOptions::default()).await, None);
}

#[tokio::test]
async fn input_passes_title_and_options_through_including_an_empty_answer() {
    let seen: Arc<Mutex<Vec<(String, InputOptions)>>> = Arc::default();
    let s = seen.clone();
    let a = api_with(Some(TestUi {
        interactive: true,
        input: Some(Arc::new(move |t, o| {
            s.lock().unwrap().push((t, o));
            Some(String::new())
        })),
        ..Default::default()
    }));
    let o = InputOptions { placeholder: Some("you".into()), initial: Some("x".into()), message: None };
    assert_eq!(a.ui().input("Name?", &o).await, Some(String::new()));
    assert_eq!(*seen.lock().unwrap(), vec![("Name?".to_string(), o)]);
}

#[tokio::test]
async fn select_many_returns_only_offered_values_once_each_in_option_order() {
    let a = api_with(Some(TestUi {
        interactive: true,
        select_many: Some(Arc::new(|| Some(vec!["c".into(), "zzz".into(), "a".into(), "c".into()]))),
        ..Default::default()
    }));
    let picked = a.ui().select_many("Which?", &opts(), &SelectManyOptions { selected: Some(vec!["b".into()]) }).await;
    assert_eq!(picked, Some(vec!["a".to_string(), "c".to_string()]));
    let none = api_with(Some(TestUi {
        interactive: true,
        select_many: Some(Arc::new(|| Some(Vec::new()))),
        ..Default::default()
    }));
    assert_eq!(none.ui().select_many("Which?", &opts(), &SelectManyOptions::default()).await, Some(Vec::new()));
}

#[test]
fn define_settings_merges_nested_defaults_replaces_arrays_and_null_without_mutating_sources() {
    let raw = json!({ "options": { "timeout": 30 }, "values": [2], "nullable": null });
    let defaults = json!({ "options": { "timeout": 10, "enabled": true }, "values": [1, 3], "nullable": "default" });
    let api = host(raw.clone()).api("demo");
    let schema = json!({ "type": "object", "properties": { "options": { "type": "object" }, "values": { "type": "array", "items": { "type": "integer" } }, "nullable": { "type": ["null", "string"] } }, "additionalProperties": false });
    let value = api.define_settings(&schema, Some(&defaults)).unwrap();
    assert_eq!(value, json!({ "options": { "timeout": 30, "enabled": true }, "values": [2], "nullable": null }));
    assert_eq!(api.settings(), raw);
}

fn bad_settings_plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        api.define_settings(
            &json!({ "type": "object", "properties": { "timeout": { "type": "integer", "minimum": 1 } } }),
            None,
        )?;
        Ok(())
    })
}

#[tokio::test]
async fn settings_reject_unknown_keys_wrong_types_ranges_and_unsupported_keywords_without_echoing_secrets() {
    let h = host(json!({ "timeout": "private-token" }));
    let notices: Arc<Mutex<Vec<String>>> = Arc::default();
    h.set_ui(Some(Arc::new(TestUi { notices: notices.clone(), ..Default::default() })));
    assert!(!h.load("demo", bad_settings_plugin).await);
    let all = notices.lock().unwrap().join(",");
    assert!(all.contains("plugins.settings.demo.timeout"), "{all}");
    assert!(!all.contains("private-token"));
    let err = |settings: Value, schema: Value| {
        host(settings).api("demo").define_settings(&schema, None).unwrap_err().to_string()
    };
    assert!(err(json!({ "extra": true }), json!({ "type": "object", "additionalProperties": false })).contains("extra"));
    assert!(
        err(json!({ "timeout": 0 }), json!({ "type": "object", "properties": { "timeout": { "minimum": 1 } } }))
            .contains("minimum")
    );
    assert!(err(json!({}), json!({ "type": "object", "$ref": "https://example.invalid" })).contains("unsupported"));
    assert!(
        err(json!({}), json!({ "type": "object", "properties": { "x": { "pattern": "[" } } })).contains("invalid schema")
    );
}

#[test]
fn settings_support_schema_valued_maps_and_combinations() {
    let schema = json!({ "type": "object", "additionalProperties": { "anyOf": [{ "const": false }, { "type": "string", "minLength": 2, "maxLength": 8, "pattern": "^[a-z]+$" }] } });
    let ok = host(json!({ "a": false, "b": "good" })).api("x").define_settings(&schema, None).unwrap();
    assert_eq!(ok, json!({ "a": false, "b": "good" }));
    let e = host(json!({ "a": "X" })).api("x").define_settings(&schema, None).unwrap_err().to_string();
    assert!(e.contains("anyOf"), "{e}");
    // "__proto__" is an ordinary key in Rust: kept as an own property, nothing polluted.
    let raw: Value = serde_json::from_str(r#"{"__proto__":{"polluted":true},"constructor":"own"}"#).unwrap();
    let value = host(raw.clone()).api("x").define_settings(&json!({ "type": "object" }), Some(&json!({}))).unwrap();
    assert!(value.as_object().unwrap().contains_key("__proto__"));
    let e = host(raw)
        .api("x")
        .define_settings(&json!({ "type": "object", "additionalProperties": false }), None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not an allowed property"), "{e}");
}

#[tokio::test]
async fn show_text_sanitizes_controls_awaits_the_viewer_and_falls_back_without_changing_messages() {
    let h = host(json!({}));
    let seen: Arc<Mutex<Vec<ShowTextOptions>>> = Arc::default();
    let (close_tx, close_rx) = tokio::sync::oneshot::channel::<()>();
    let close_rx = Arc::new(Mutex::new(Some(close_rx)));
    let s = seen.clone();
    h.set_ui(Some(Arc::new(TestUi {
        interactive: true,
        show_text: Some(Arc::new(move |o| {
            s.lock().unwrap().push(o);
            let rx = close_rx.lock().unwrap().take();
            Box::pin(async move {
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
            })
        })),
        ..Default::default()
    })));
    let api = h.api("demo");
    let pending = tokio::spawn(async move {
        api.ui()
            .show_text(ShowTextOptions {
                title: "hello\x1b[2J".into(),
                text: "safe\x1b]52;c;bad\x07".into(),
                format: TextFormat::Markdown,
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!pending.is_finished());
    assert!(!format!("{:?}", seen.lock().unwrap()).contains('\x1b'));
    assert_eq!(seen.lock().unwrap()[0].title, "hello␛[2J");
    close_tx.send(()).unwrap();
    pending.await.unwrap().unwrap();
    let messages = h.api("demo").session().messages();
    let fallback: Arc<Mutex<Vec<String>>> = Arc::default();
    h.set_ui(Some(Arc::new(TestUi { notices: fallback.clone(), ..Default::default() })));
    h.api("demo")
        .ui()
        .show_text(ShowTextOptions { title: "title".into(), text: "body".into(), format: TextFormat::Text })
        .await
        .unwrap();
    assert_eq!(*fallback.lock().unwrap(), vec!["title\nbody".to_string()]);
    assert_eq!(h.api("demo").session().messages(), messages);
}
