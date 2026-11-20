//! checkpoints (`packages/bundled/src/checkpoints.ts`) — write / edit が変えたファイルの直前の内容を
//! セッションに退避し、/undo で戻す。
//!
//!   - 変更の直前に内容を控え、呼び出しが本当に成功したときだけチェックポイントとして保存する
//!   - /undo で直近の1件（1回の呼び出しが変えたファイル全部）を戻す。繰り返すごとに1件ずつ遡る
//!   - 退避はセッションログに残るので、/resume のあとも戻せる
//!
//! 設定（config の "plugins": { "settings": { "checkpoints": { ... } } }）:
//! - maxFileKB: 退避するファイルの上限（既定 1024）。大きいものは退避せず、/undo のときに「戻せない」と伝える。
//!   UTF-8 でないもの（Shift_JIS、バイナリなど）はバイト列のまま（base64 で）退避する。
//!   bash の変更は対象外（何を書き換えたか分からない）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use sasacode_agent::paths::resolve2;
use sasacode_ai::now_ms;
use sasacode_host::{Lang, current_lang, t};
use sasacode_plugin_api::{Command, NotifyLevel, PluginApi, SelectOption, ToolKind, hook};

use crate::regular_file::read_capped;
use crate::repair::js::{js_trim, num_setting};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Change {
    pub(crate) path: String,
    /// 退避した内容（戻せるときだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<String>,
    /// before は base64 のバイト列: UTF-8 として読むと U+FFFD に化けるファイル（#117）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base64: Option<bool>,
    /// ファイルは元からなかった: 戻すときは削除する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<bool>,
    /// 退避していない（大きい・テキストでない）: 戻せないと伝えるだけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skipped: Option<bool>,
}

impl Change {
    fn new(path: &str) -> Change {
        Change {
            path: path.to_string(),
            before: None,
            base64: None,
            created: None,
            skipped: None,
        }
    }
    fn created(&self) -> bool {
        self.created == Some(true)
    }
    pub(crate) fn skipped(&self) -> bool {
        self.skipped == Some(true)
    }
    fn content(&self) -> Vec<u8> {
        let before = self.before.as_deref().unwrap_or("");
        if self.base64 == Some(true) {
            base64::engine::general_purpose::STANDARD
                .decode(before)
                .unwrap_or_default()
        } else {
            before.as_bytes().to_vec()
        }
    }
}

/// 1回のツール呼び出しの退避。呼び出し単位でまとめて戻す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Checkpoint {
    /// チェックポイントごとに一意。呼び出しの id はサーバーによってはターンごとに同じ call_0 になる（#117）。
    id: String,
    /// 呼び出しの id（参考）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<String>,
    tool: String,
    at: i64,
    changes: Vec<Change>,
}

const KIND: &str = "checkpoint";
const DEFAULT_MAX_KB: f64 = 1024.0;

/// 戻していないチェックポイントだけ、古い順に。
fn stack(api: &PluginApi) -> Vec<Checkpoint> {
    let all: Vec<Value> = api
        .session()
        .entries(KIND)
        .into_iter()
        .filter(Value::is_object)
        .collect();
    let gone: Vec<&Value> = all.iter().filter_map(|e| e.get("undone")).collect();
    all.iter()
        .filter(|e| e.get("changes").is_some() && !e.get("id").is_some_and(|id| gone.contains(&id)))
        .filter_map(|e| serde_json::from_value(e.clone()).ok())
        .collect()
}

pub(crate) fn snapshot(path: &str, max_kb: f64) -> Change {
    let p = Path::new(path);
    if !p.exists() {
        return Change {
            created: Some(true),
            ..Change::new(path)
        };
    }
    // Read before the call is judged: a regular file only, and no more than the limit (a FIFO or a
    // device is not read; one that keeps growing is cut off and not kept).
    match read_capped(p, (max_kb * 1024.0).max(0.0) as u64) {
        // Not UTF-8 text: as a string its undecodable bytes would become U+FFFD, and /undo would write those back (#117).
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) if !text.as_bytes().contains(&0) => Change {
                before: Some(text),
                ..Change::new(path)
            },
            Ok(text) => Change {
                before: Some(base64::engine::general_purpose::STANDARD.encode(text.as_bytes())),
                base64: Some(true),
                ..Change::new(path)
            },
            Err(e) => Change {
                before: Some(base64::engine::general_purpose::STANDARD.encode(e.as_bytes())),
                base64: Some(true),
                ..Change::new(path)
            },
        },
        None => Change {
            skipped: Some(true),
            ..Change::new(path)
        },
    }
}

/// 変更リストを巻き戻す。ファイルごとに1行の報告。
pub(crate) fn restore_changes(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|ch| {
            let path = &ch.path;
            let done: std::io::Result<String> = (|| {
                if ch.skipped() {
                    return Ok(t!("{path}: 内容を退避していないため戻せません", path = path));
                }
                if ch.created() {
                    if !Path::new(path).exists() {
                        return Ok(t!("{path}: すでにありません", path = path));
                    }
                    std::fs::remove_file(path)?;
                    return Ok(t!("{path}: 削除しました（変更前はなかったため）", path = path));
                }
                if let Some(parent) = Path::new(path).parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(path, ch.content())?;
                Ok(t!("{path}: 戻しました", path = path))
            })();
            done.unwrap_or_else(|e| t!("{path}: 戻せませんでした（{error}）", path = path, error = e))
        })
        .collect()
}

/// 1件の変更を巻き戻す。ファイルごとに1行の報告。
fn restore(c: &Checkpoint) -> Vec<String> {
    restore_changes(&c.changes)
}

/// `new Date(at).toLocaleTimeString()` in the interface's language.
fn local_time(at: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at).single() {
        Some(d) if current_lang() == Lang::En => d.format("%-I:%M:%S %p").to_string(),
        Some(d) => d.format("%-H:%M:%S").to_string(),
        None => "Invalid Date".to_string(),
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let max_kb = num_setting(&api.settings(), "maxFileKB", DEFAULT_MAX_KB);
        // 結果が分かるまで、呼び出しごとの退避。
        let pending: Arc<Mutex<HashMap<String, Vec<Change>>>> = Arc::default();

        let (pd, cwd) = (pending.clone(), api.cwd().to_string_lossy().into_owned());
        api.on::<hook::ToolCall, _, _>(move |ev, _c| {
            // bash など、変えたものが分からないものもここで除かれる
            if ev.tool.kind() == ToolKind::Edit {
                let paths = ev.tool.paths(&ev.args, Path::new(&cwd)).unwrap_or_default();
                if !paths.is_empty() {
                    let changes = paths.iter().map(|p| snapshot(&resolve2(&cwd, p), max_kb)).collect();
                    pd.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(ev.call.id.clone(), changes);
                }
            }
            async { Ok(None) }
        });

        let (pd, a) = (pending, api.clone());
        api.on::<hook::ToolResult, _, _>(move |ev, _c| {
            let changes = pd.lock().unwrap_or_else(|e| e.into_inner()).remove(&ev.call.id);
            let outcome = match changes {
                Some(changes) if ev.ran => {
                    // 実際に変わったものだけ残す: 失敗した呼び出しや無変更の書き込みが /undo の邪魔をしないように。
                    let keep: Vec<Change> = changes
                        .into_iter()
                        .filter(|ch| {
                            if ch.skipped() {
                                return !ev.result.is_error();
                            }
                            if ch.created() {
                                return Path::new(&ch.path).exists();
                            }
                            // なくなったり読めなくなったり、退避したものより大きくなったりした: 戻せるに越したことはない
                            let before = ch.content();
                            read_capped(Path::new(&ch.path), before.len() as u64).is_none_or(|now| now != before)
                        })
                        .collect();
                    if keep.is_empty() {
                        Ok(())
                    } else {
                        let cp = Checkpoint {
                            id: uuid::Uuid::new_v4().to_string(),
                            call: Some(ev.call.id.clone()),
                            tool: ev.call.name.clone(),
                            at: now_ms(),
                            changes: keep,
                        };
                        serde_json::to_value(&cp)
                            .map_err(anyhow::Error::from)
                            .and_then(|v| a.session().append(KIND, v))
                    }
                }
                _ => Ok(()),
            };
            async move { outcome.map(|()| None) }
        });

        let a = api.clone();
        api.register_command(
            Command::new(
                "undo",
                &t!("write / edit によるファイル変更を1件戻す"),
                move |c| {
                    let a = a.clone();
                    async move {
                        let arg = js_trim(&c.args).to_string();
                        let left = stack(&a);
                        let ui = a.ui();
                        if arg == "list" {
                            if left.is_empty() {
                                ui.notify(&t!("[undo] 戻せる変更はありません"), NotifyLevel::Info);
                                return Ok(());
                            }
                            let n = left.len();
                            let mut lines: Vec<String> = left
                                .iter()
                                .enumerate()
                                .map(|(i, c)| {
                                    let paths = c
                                        .changes
                                        .iter()
                                        .map(|ch| ch.path.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    format!("{}. {} {} — {paths}", n - i, local_time(c.at), c.tool)
                                })
                                .collect();
                            lines.reverse();
                            ui.notify(
                                &format!("{}\n{}", t!("[undo] 戻せる変更（新しい順）:"), lines.join("\n")),
                                NotifyLevel::Info,
                            );
                            return Ok(());
                        }
                        // The whole path, or its last parts: "index.ts" is not reindex.ts (#117).
                        let cwd = a.cwd().to_string_lossy().into_owned();
                        let exact = resolve2(&cwd, &arg);
                        let rest = arg
                            .strip_prefix("./")
                            .or_else(|| arg.strip_prefix(".\\"))
                            .or_else(|| arg.strip_prefix('/'))
                            .or_else(|| arg.strip_prefix('\\'))
                            .unwrap_or(&arg);
                        let tails: Vec<String> = [std::path::MAIN_SEPARATOR, '/']
                            .iter()
                            .map(|s| format!("{s}{rest}"))
                            .collect();
                        let hit = |p: &str| p == exact || tails.iter().any(|t| p.ends_with(t.as_str()));
                        let mut paths: Vec<&str> = Vec::new();
                        for ch in left.iter().flat_map(|c| c.changes.iter()) {
                            if hit(&ch.path) && !paths.contains(&ch.path.as_str()) {
                                paths.push(&ch.path);
                            }
                        }
                        let has_exact = paths.contains(&exact.as_str());
                        if !arg.is_empty() && paths.len() > 1 && !has_exact {
                            ui.notify(
                            &format!(
                                "{}\n{}",
                                t!(
                                    "[undo] {path} に当たるファイルが複数あります。パスをもっと詳しく指定してください:",
                                    path = arg
                                ),
                                paths.join("\n")
                            ),
                            NotifyLevel::Warning,
                        );
                            return Ok(());
                        }
                        let pick = if arg.is_empty() {
                            left.last()
                        } else {
                            left.iter().rfind(|c| {
                                c.changes
                                    .iter()
                                    .any(|ch| if has_exact { ch.path == exact } else { hit(&ch.path) })
                            })
                        };
                        let Some(pick) = pick else {
                            let msg = if arg.is_empty() {
                                t!("[undo] 戻せる変更はありません")
                            } else {
                                t!("[undo] {path} の変更が見つかりません", path = arg)
                            };
                            ui.notify(&msg, NotifyLevel::Warning);
                            return Ok(());
                        };
                        // 戻した先からまた戻せる（＝変更の前後が入れ替わる）と迷うので、1件は消費したことにする。
                        a.session().append(KIND, json!({ "undone": pick.id }))?;
                        ui.notify(
                            &format!("{}\n{}", t!("[undo] 戻しました:"), restore(pick).join("\n")),
                            NotifyLevel::Info,
                        );
                        Ok(())
                    }
                },
            )
            .hint("[list | <path>]")
            .complete(|prefix| {
                let out: Vec<SelectOption> = ["list"]
                    .iter()
                    .filter(|s| s.starts_with(&prefix))
                    .map(|v| SelectOption::new(*v, *v))
                    .collect();
                async move { out }
            }),
        );
        Ok(())
    })
}
