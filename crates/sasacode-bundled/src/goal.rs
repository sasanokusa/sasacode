//! goal (`packages/bundled/src/goal.ts`) — セッション単位の「ゴール（最終目的）」を人間とエージェントの
//! 両方から設定する
//!
//!   - 人間は /goal コマンド、エージェントは set_goal ツールで設定する
//!   - 状態はセッションエントリにのみ保存する（ファイルには何も書かない）
//!   - ゴールは1セッションに1件だけ。置き換えた古いゴールは残さない

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use sasacode_host::t;
use sasacode_plugin_api::{
    Command, CommandContext, InjectWhen, NotifyLevel, PermissionRules, PluginApi, SelectOption, ToolDef, ToolKind,
    ToolResult, error_result, hook,
};

use crate::web_fetch::util::{js_trim, utf16_len, utf16_prefix};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Active,
    Paused,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalState {
    text: String,
    status: Status,
    source: Source,
    updated_at: String,
}

fn status_label(s: Status) -> String {
    match s {
        Status::Active => t!("作業中"),
        Status::Paused => t!("一時停止"),
        Status::Done => t!("達成"),
    }
}

fn source_label(s: Source) -> String {
    match s {
        Source::Human => t!("/goal コマンド"),
        Source::Agent => t!("set_goal ツール"),
    }
}

const ENTRY_KIND: &str = "goal-snapshot";
const FOOTER_CHARS: usize = 50;

fn usage() -> String {
    t!(
        "使い方:\n  /goal                   現在のゴールを表示\n  /goal <テキスト>        ゴールを設定\n  /goal clear             ゴールを削除\n  /goal done              達成とした\n  /goal pause             一時停止\n  /goal resume            作業中に戻す\n  /goal help              この表示\n\nエージェントは set_goal ツールで同じことができます。"
    )
}

/// `new Date().toISOString()`.
fn iso_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// `new Date(iso).toLocaleString()` as Bun prints it (en-US, local time): `9/26/2026, 12:00:00 PM`.
fn locale_string(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(d) => d
            .with_timezone(&chrono::Local)
            .format("%-m/%-d/%Y, %-I:%M:%S %p")
            .to_string(),
        Err(_) => "Invalid Date".to_string(),
    }
}

struct Goal {
    api: PluginApi,
    goal: Mutex<Option<GoalState>>,
}

impl Goal {
    fn current(&self) -> Option<GoalState> {
        self.goal.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    // null records a cleared goal, so /resume does not bring it back.
    fn persist(&self) {
        let value = self
            .current()
            .and_then(|g| serde_json::to_value(g).ok())
            .unwrap_or(Value::Null);
        if let Err(e) = self.api.session().append(ENTRY_KIND, value) {
            self.api.log(&format!("goal: {e}"));
        }
    }

    /// text があれば内容も変える。status だけなら状態のみ動かす。両方無いは false。
    fn change(&self, text: Option<&str>, status: Option<Status>, source: Source) -> bool {
        let body = js_trim(text.unwrap_or(""));
        let status = status.unwrap_or(Status::Active);
        {
            let mut goal = self.goal.lock().unwrap_or_else(|e| e.into_inner());
            if !body.is_empty() {
                *goal = Some(GoalState {
                    text: body.to_string(),
                    status,
                    source,
                    updated_at: iso_now(),
                });
            } else if let Some(g) = goal.as_mut() {
                g.status = status;
                g.updated_at = iso_now();
            } else {
                return false;
            }
        }
        self.persist();
        self.paint();
        true
    }

    fn clear(&self) {
        *self.goal.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.persist();
        self.paint();
    }

    fn render(&self) -> String {
        let Some(goal) = self.current() else {
            return t!("ゴールは未設定です。人間は /goal、エージェントは set_goal ツールで設定できます。");
        };
        t!(
            "🎯 ゴール: {goal}\n  状態: {status}　設定元: {source}\n  更新: {updated}",
            goal = goal.text,
            status = status_label(goal.status),
            source = source_label(goal.source),
            updated = locale_string(&goal.updated_at)
        )
    }

    fn paint(&self) {
        let show = self
            .current()
            .filter(|g| g.status == Status::Active)
            .map(|g| g.text)
            .filter(|s| !s.is_empty());
        let Some(show) = show else {
            return self.api.ui().set_status("goal", None);
        };
        let cut = if utf16_len(&show) > FOOTER_CHARS {
            format!("{}…", utf16_prefix(&show, FOOTER_CHARS))
        } else {
            show
        };
        self.api.ui().set_status("goal", Some(format!("🎯 {cut}")));
    }

    fn details(&self) -> sasacode_ai::JsonObject {
        let mut details = sasacode_ai::JsonObject::new();
        details.insert(
            "goal".into(),
            self.current()
                .and_then(|g| serde_json::to_value(g).ok())
                .unwrap_or(Value::Null),
        );
        details
    }

    fn set_goal(&self, args: &sasacode_ai::JsonObject) -> ToolResult {
        let body = js_trim(args.get("text").and_then(Value::as_str).unwrap_or("")).to_string();
        let status = match args.get("status").and_then(Value::as_str) {
            Some("active") => Some(Status::Active),
            Some("paused") => Some(Status::Paused),
            Some("done") => Some(Status::Done),
            _ => None,
        };
        if body.is_empty() && status.is_none() {
            return error_result(
                "set_goal: pass text to set a goal, or status to change the status of the current one.",
            );
        }
        let text = (!body.is_empty()).then_some(body.as_str());
        if !self.change(text, Some(status.unwrap_or(Status::Active)), Source::Agent) {
            return error_result(
                "set_goal: no goal is set yet, so status alone cannot be applied. Pass text to set one.",
            );
        }
        let verb = if body.is_empty() { "状態を変更" } else { "設定" };
        ToolResult::text(format!("[goal] {verb}しました。\n{}", self.render())).with_details(self.details())
    }

    async fn run(&self, args: &str) {
        let a = js_trim(args);
        let low = a.to_lowercase();
        let ui = self.api.ui();
        if a.is_empty() {
            return ui.notify(&self.render(), NotifyLevel::Info);
        }
        if low == "help" || low == "-h" || low == "--help" {
            return ui.notify(&usage(), NotifyLevel::Info);
        }
        if low == "clear" {
            let Some(gone) = self.current() else {
                return ui.notify(
                    &t!("[goal] 削除するゴールがありません（未設定です）"),
                    NotifyLevel::Warning,
                );
            };
            self.clear();
            return ui.notify(&t!("[goal] 削除しました: {goal}", goal = gone.text), NotifyLevel::Info);
        }
        if low == "done" || low == "pause" || low == "resume" {
            let status = match low.as_str() {
                "done" => Status::Done,
                "pause" => Status::Paused,
                _ => Status::Active,
            };
            if !self.change(None, Some(status), Source::Human) {
                return ui.notify(
                    &t!("[goal] 対象のゴールがありません（未設定です）"),
                    NotifyLevel::Warning,
                );
            }
            let goal = self.current().map(|g| g.text).unwrap_or_default();
            let message = match status {
                Status::Done => t!("[goal] 達成としました: {goal}", goal = goal),
                Status::Paused => t!("[goal] 一時停止としました: {goal}", goal = goal),
                Status::Active => t!("[goal] 作業中としました: {goal}", goal = goal),
            };
            return ui.notify(&message, NotifyLevel::Info);
        }
        if !self.change(Some(a), Some(Status::Active), Source::Human) {
            return ui.notify(&t!("[goal] 設定する内容が読み取れません（空です）"), NotifyLevel::Error);
        }
        ui.notify(
            &format!("{}\n{}", t!("[goal] 設定しました:"), self.render()),
            NotifyLevel::Info,
        );
        let text = self.current().map(|g| g.text).unwrap_or_default();
        self.api.session().inject_text(
            &format!("[goal] ゴールを設定しました: {text}\nこのゴールに向けて作業を進めてください。"),
            InjectWhen::Now,
        );
    }
}

const SUBS: [&str; 5] = ["clear", "done", "pause", "resume", "help"];

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let goal = Arc::new(Goal {
            api: api.clone(),
            goal: Mutex::new(None),
        });

        let on_start = goal.clone();
        api.on::<hook::SessionStart, _, _>(move |_e, _ctx| {
            let entries = on_start.api.session().entries(ENTRY_KIND);
            let last = entries.last().filter(|l| l.get("text").is_some_and(Value::is_string));
            let restored = last.and_then(|l| serde_json::from_value::<GoalState>(l.clone()).ok());
            *on_start.goal.lock().unwrap_or_else(|e| e.into_inner()) = restored;
            on_start.paint();
            async { Ok(None) }
        });

        // They only touch this session's goal: no reason to ask (user deny / ask rules still apply).
        api.add_permission_rules(PermissionRules {
            allow: vec!["set_goal".into(), "goal_status".into()],
            ..PermissionRules::default()
        })?;

        let setter = goal.clone();
        api.register_tool(
            ToolDef::new(
                "set_goal",
                concat!(
                    "Set, update, or change the status of this session's single goal (the final purpose of the current work). ",
                    "Call it when the user asks you to set, fix, or change a goal, purpose, or final purpose ",
                    "(for example 「これをgoalに設定して」「目的を固定して」): distil the purpose out of the conversation ",
                    "and pass it as one or two sentences. Write the end state to be reached, not the list of steps. ",
                    "Pass status 'done' when the goal has been reached, 'paused' to park it, 'active' to resume it. ",
                    "Pass only status (no text) to move the existing goal without rewriting its text."
                ),
                json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The goal as one or two self-contained sentences: the end state to reach, not the steps to get there.",
                        },
                        "status": {
                            "type": "string",
                            "enum": ["active", "paused", "done"],
                            "description": "'active' (default) = still being worked on. 'done' = reached. 'paused' = parked.",
                        },
                    },
                    "additionalProperties": false,
                }),
                ToolKind::Other,
                move |args, _ctx| {
                    let goal = setter.clone();
                    async move { Ok(goal.set_goal(&args)) }
                },
            )
            .concurrent(false)
            .summary(|a| {
                let status = a.get("status").and_then(Value::as_str).filter(|s| !s.is_empty() && *s != "active");
                let text = utf16_prefix(a.get("text").and_then(Value::as_str).unwrap_or(""), 60);
                format!(
                    "goal{} {}",
                    status.map(|s| format!(":{s}")).unwrap_or_default(),
                    if text.is_empty() { "(status only)" } else { text }
                )
            })
            .into_ref(),
        );

        let reader = goal.clone();
        api.register_tool(
            ToolDef::new(
                "goal_status",
                concat!(
                    "Read the current session goal: its text, its status, who set it, and when. Call it when you need to check ",
                    "what the current purpose is, or before reporting that the work is finished."
                ),
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                ToolKind::Read,
                move |_args, _ctx| {
                    let goal = reader.clone();
                    async move {
                        if goal.current().is_none() {
                            return Ok(ToolResult::text("ゴールは未設定です。"));
                        }
                        Ok(ToolResult::text(goal.render()).with_details(goal.details()))
                    }
                },
            )
            .into_ref(),
        );

        // A goal is a fact worth carrying in the prompt; how to reach it is not (A1). The rules for
        // setting one live in the set_goal tool's own description, so they are not repeated here.
        let prompter = goal.clone();
        api.on::<hook::SystemPrompt, _, _>(move |e, _ctx| {
            let prompt = match prompter.current().filter(|g| g.status == Status::Active) {
                Some(g) => format!("{}\n\n現在のゴール: {}", e.prompt, g.text),
                None => e.prompt,
            };
            async move { Ok(Some(sasacode_plugin_api::SystemPromptResult { prompt: Some(prompt) })) }
        });

        let runner = goal.clone();
        api.register_command(
            Command::new(
                "goal",
                &t!("このセッションのゴール（最終目的）を設定・表示・達成・一時停止・削除する"),
                move |CommandContext { args }| {
                    let goal = runner.clone();
                    async move {
                        goal.run(&args).await;
                        Ok(())
                    }
                },
            )
            .hint(&t!("[<目的>|clear|done|pause|resume|help]"))
            .complete(|prefix: String| async move {
                let p = prefix.to_lowercase();
                SUBS.iter()
                    .filter(|s| p.is_empty() || s.starts_with(&p))
                    .map(|s| SelectOption::new(*s, *s))
                    .collect()
            }),
        );

        goal.paint();
        Ok(())
    })
}
