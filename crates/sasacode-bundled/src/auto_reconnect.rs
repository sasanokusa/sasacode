//! auto-reconnect (`packages/bundled/src/auto-reconnect.ts`) — ネットワーク障害で接続が切れたとき、
//! 復旧を検知して自動で接続を再試行する。
//!
//! - agent_end が cause: "error" で終わったとき、接続チェック URL にアクセスできるか確認する。
//! - オフラインなら一定間隔で疎通チェックを続け、オンラインに復帰したら
//!   「中断した作業を再開せよ」というメッセージを inject して自動で再試行する。
//! - 回線は生きていてサーバーが一時的なエラー（429・5xx・overloaded など）を返し続けたときは、
//!   SDK の再試行（約35秒）を使い切った後も、間隔を空けて（既定 30・60・120・120 秒）再試行する。
//!   待っている間にユーザーが入力したら、その再試行はやめる。
//! - /reconnect コマンドで手動でも同じ処理を起こせる（status で状態確認のみ）。
//!
//! 設定（config の "plugins": { "settings": { "auto-reconnect": { ... } } }）:
//! - checkUrl:         疎通チェック先 URL（応答が1つでも返ればオンラインとみなす）。既定は使用中モデルの
//!   エンドポイント（ローカルの LLM サーバーが落ちた場合も、その復帰を待てる）
//! - checkIntervalMs:  復旧待ちのポーリング間隔（既定 5000）
//! - checkTimeoutMs:   1回の疎通チェックのタイムアウト（既定 3000）
//! - maxWaitMs:        復旧を待つ最大時間。超えたらあきらめて通知する（既定 600000 = 10分）
//! - retryMessage:     復旧後に inject するメッセージ
//! - quiet:            true にすると通知を出さない（ステータス表示と inject のみ）
//! - serverRetries:    サーバーの一時的なエラーで再試行する回数（既定 4。0 で再試行しない）
//! - serverRetryMs:    その最初の待ち時間。毎回倍にし、4倍で頭打ち（既定 30000）

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use sasacode_ai::ModelInfo;
use sasacode_host::t;
use sasacode_plugin_api::{Command, CommandContext, InjectWhen, NotifyLevel, PluginApi, SelectOption, StopCause, hook};

use crate::net_util::{js_num_str, js_round, js_trim, setting_str, utf16_prefix};

/// The server answered but could not serve now: worth asking again later. 4xx other than these is the request's fault.
pub static TRANSIENT: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: a fixed pattern. JS's \b and \d are ASCII.
    Regex::new(r"(?i)^(?:408|409|425|429|5[0-9][0-9])(?-u:\b)|overloaded|temporarily unavailable|rate.?limit")
        .expect("fixed pattern")
});

fn official_host(api: &str) -> Option<&'static str> {
    match api {
        "anthropic" => Some("https://api.anthropic.com"),
        "openai-chat" | "openai-responses" => Some("https://api.openai.com"),
        "openai-codex" => Some("https://chatgpt.com"),
        _ => None,
    }
}

/// The URL whose answering means the network is up: the endpoint that failed is the one worth
/// waiting for (a local LLM server, a VPN host), and asking it leaks nothing to a third party.
/// Without a baseUrl, the provider's official API.
pub fn target(check_url: Option<&str>, model: &ModelInfo) -> String {
    check_url
        .map(str::to_string)
        .or_else(|| model.base_url.clone())
        .or_else(|| official_host(&model.api).map(str::to_string))
        .unwrap_or_else(|| "https://api.openai.com".to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Error,
    Manual,
}

struct Settings {
    check_url: Option<String>,
    check_interval: f64,
    check_timeout: f64,
    max_wait: f64,
    retry_message: String,
    quiet: bool,
    server_retries: f64,
    server_retry_ms: f64,
}

#[derive(Default)]
struct State {
    waiting: bool,
    /// 待機中に別のランが終わったサーバーの一時エラー。監視が再開させずに終われば、それを再試行する（#51）。
    missed: Option<String>,
    stopped: bool,
    /// Bumped by session_start: a watch from the session before (/new, /resume) must not inject into this one (#122).
    session: u64,
    attempts: f64,
    cancel_wait: Option<tokio::sync::oneshot::Sender<()>>,
}

struct Reconnect {
    api: PluginApi,
    s: Settings,
    state: Mutex<State>,
}

fn ms(x: f64) -> Duration {
    Duration::from_secs_f64((x / 1000.0).max(0.0))
}

impl Reconnect {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn target(&self) -> String {
        target(self.s.check_url.as_deref(), &self.api.agent().model())
    }

    /// 1つでも HTTP 応答が返ればオンライン（4xx/5xx でも回線は生きている）。
    async fn is_online(&self) -> bool {
        let req = crate::rustxng_install::no_redirects()
            .get(self.target())
            .header("cache-control", "no-store")
            .header("pragma", "no-cache");
        matches!(
            tokio::time::timeout(ms(self.s.check_timeout), req.send()).await,
            Ok(Ok(_))
        )
    }

    async fn wait_for_online(&self, generation: u64) -> bool {
        let deadline = tokio::time::Instant::now() + ms(self.s.max_wait);
        loop {
            {
                let s = self.lock();
                if s.stopped || generation != s.session || tokio::time::Instant::now() >= deadline {
                    return false;
                }
            }
            tokio::time::sleep(ms(self.s.check_interval)).await;
            if self.is_online().await {
                return true;
            }
        }
    }

    /// オフラインなら復旧を待ち、復帰したら inject して再試行させる。None: already watching.
    fn monitor_and_retry(self: &Arc<Self>, trigger: Trigger) -> Option<BoxFuture<'static, ()>> {
        {
            let mut s = self.lock();
            if s.waiting || s.stopped {
                return None;
            }
            // 最初の await より前に立てる。agent_end と /reconnect が同時に来ても、再開の inject は1回だけ。
            s.waiting = true;
        }
        let me = self.clone();
        Some(
            async move {
                let resumed = me.watch(trigger).await;
                let error = {
                    let mut s = me.lock();
                    s.waiting = false;
                    s.missed.take()
                };
                if let Some(error) = error
                    && !resumed
                    && let Some(retry) = me.retry_server(error)
                {
                    retry.await;
                }
            }
            .boxed(),
        )
    }

    /// 作業を再開させた（inject した）とき true。
    async fn watch(&self, trigger: Trigger) -> bool {
        let generation = self.lock().session;
        let ui = self.api.ui();
        if self.is_online().await {
            if trigger != Trigger::Manual || generation != self.lock().session {
                return false;
            }
            ui.notify(&t!("オンラインです。接続に問題ありません。"), NotifyLevel::Info);
            self.api.session().inject_text(
                "接続に問題ありません。直前で中断した作業があれば、続きから再開してください。",
                InjectWhen::Now,
            );
            return true;
        }
        ui.set_status("auto-reconnect", Some(t!("オフライン — ネットワーク復旧待ち")));
        if !self.s.quiet {
            ui.notify(
                &t!(
                    "ネットワークに接続できません（{target}）。復旧を待って自動再試行します。",
                    target = self.target()
                ),
                NotifyLevel::Warning,
            );
        }
        let ok = self.wait_for_online(generation).await;
        {
            let s = self.lock();
            if s.stopped || generation != s.session {
                return false;
            }
        }
        ui.set_status("auto-reconnect", None);
        if ok {
            if !self.s.quiet {
                ui.notify(&t!("ネットワークが復旧しました。自動再試行します。"), NotifyLevel::Info);
            }
            self.api.session().inject_text(&self.s.retry_message, InjectWhen::Now);
            true
        } else {
            ui.notify(
                &t!(
                    "ネットワークが復旧しませんでした（{s}秒待機）。/reconnect で手動再試行できます。",
                    s = js_num_str(js_round(self.s.max_wait / 1000.0))
                ),
                NotifyLevel::Warning,
            );
            false
        }
    }

    /// True after `wait`, or false when cancelled (the user typed, the session ended).
    async fn wait(&self, wait: Duration) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.lock().cancel_wait = Some(tx);
        let go = tokio::select! {
            _ = tokio::time::sleep(wait) => true,
            _ = rx => false,
        };
        self.lock().cancel_wait = None;
        go
    }

    /// サーバーが一時的に応答できないとき、間隔を空けて同じ作業を再試行する。None: already waiting.
    fn retry_server(self: &Arc<Self>, error: String) -> Option<BoxFuture<'static, ()>> {
        let generation = {
            let mut s = self.lock();
            if s.waiting || s.stopped {
                return None;
            }
            s.waiting = true;
            s.session
        };
        let me = self.clone();
        Some(
            async move {
                let online = me.is_online().await;
                me.lock().waiting = false;
                if !online {
                    // 待つ間に次のセッションになっていたら、そちらへは再開させない (#135)
                    if generation == me.lock().session
                        && let Some(monitor) = me.monitor_and_retry(Trigger::Error)
                    {
                        monitor.await;
                    }
                    return;
                }
                let retries = me.s.server_retries;
                let attempt = {
                    let mut s = me.lock();
                    if s.attempts >= retries {
                        s.attempts = 0.0;
                        None
                    } else {
                        let attempt = s.attempts;
                        s.attempts += 1.0;
                        s.waiting = true;
                        Some(attempt)
                    }
                };
                let ui = me.api.ui();
                let Some(attempt) = attempt else {
                    if retries != 0.0 && !retries.is_nan() {
                        ui.notify(
                            &t!("サーバーのエラーが続いたため、再試行をやめました。"),
                            NotifyLevel::Warning,
                        );
                    }
                    return;
                };
                let wait_ms = (me.s.server_retry_ms * 2f64.powf(attempt)).min(me.s.server_retry_ms * 4.0);
                let secs = js_num_str(js_round(wait_ms / 1000.0));
                ui.set_status(
                    "auto-reconnect",
                    Some(t!(
                        "サーバーの一時的なエラー — {s}秒後に再試行（{n}/{max}）",
                        s = secs,
                        n = js_num_str(attempt + 1.0),
                        max = js_num_str(retries)
                    )),
                );
                if !me.s.quiet {
                    ui.notify(
                        &t!(
                            "サーバーが一時的に応答できません（{error}）。{s}秒待って再試行します。",
                            error = utf16_prefix(&error, 160),
                            s = secs
                        ),
                        NotifyLevel::Warning,
                    );
                }
                let go = me.wait(ms(wait_ms)).await;
                let resume = {
                    let mut s = me.lock();
                    s.waiting = false;
                    if go {
                        s.missed = None; // この再開で、待機中に終わったランの作業も続く
                    }
                    go && !s.stopped && generation == s.session
                };
                ui.set_status("auto-reconnect", None);
                if resume {
                    me.api.session().inject_text(
                        "サーバーの一時的なエラーで中断しました。中断した作業を、続きから再開してください。",
                        InjectWhen::Now,
                    );
                }
            }
            .boxed(),
        )
    }
}

fn spawn(work: Option<BoxFuture<'static, ()>>) {
    if let Some(work) = work {
        tokio::spawn(work);
    }
}

fn number(settings: &Value, key: &str, fallback: f64) -> f64 {
    settings.get(key).and_then(Value::as_f64).unwrap_or(fallback)
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        // Build the shared client now (loading the TLS roots takes a while): checkTimeoutMs bounds
        // each check as fetch's timeout did, and the first check must not spend it on setup.
        drop(tokio::task::spawn_blocking(|| {
            crate::rustxng_install::no_redirects();
        }));
        let settings = api.settings();
        let s = Settings {
            check_url: setting_str(&settings, "checkUrl"),
            check_interval: number(&settings, "checkIntervalMs", 5_000.0),
            check_timeout: number(&settings, "checkTimeoutMs", 3_000.0),
            max_wait: number(&settings, "maxWaitMs", 10.0 * 60_000.0),
            retry_message: setting_str(&settings, "retryMessage").unwrap_or_else(|| {
                "ネットワークが復旧しました。直前のエラー（接続失敗）で中断した作業を、続きから再開してください。"
                    .into()
            }),
            quiet: settings.get("quiet").and_then(Value::as_bool).unwrap_or(false),
            server_retries: number(&settings, "serverRetries", 4.0),
            server_retry_ms: number(&settings, "serverRetryMs", 30_000.0),
        };
        let r = Arc::new(Reconnect {
            api: api.clone(),
            s,
            state: Mutex::new(State::default()),
        });

        let on_start = r.clone();
        api.on::<hook::SessionStart, _, _>(move |_e, _ctx| {
            {
                let mut s = on_start.lock();
                s.stopped = false;
                s.session += 1;
            }
            async { Ok(None) }
        });

        // 待っている間にユーザーが入力したら、そちらを優先して再試行はやめる。
        let on_prompt = r.clone();
        api.on::<hook::UserPrompt, _, _>(move |_e, _ctx| {
            {
                let mut s = on_prompt.lock();
                if let Some(cancel) = s.cancel_wait.take() {
                    let _ = cancel.send(());
                    s.attempts = 0.0;
                }
            }
            async { Ok(None) }
        });

        let on_end = r.clone();
        api.on::<hook::SessionEnd, _, _>(move |_e, _ctx| {
            {
                let mut s = on_end.lock();
                s.stopped = true;
                if let Some(cancel) = s.cancel_wait.take() {
                    let _ = cancel.send(());
                }
            }
            on_end.api.ui().set_status("auto-reconnect", None);
            async { Ok(None) }
        });

        // モデル呼び出しなどでランがエラー終了し、かつ回線が落ちていれば復旧待ちに入る。
        let on_agent_end = r.clone();
        api.on::<hook::AgentEnd, _, _>(move |e, _ctx| {
            let r = &on_agent_end;
            let work = {
                let mut s = r.lock();
                if e.cause == StopCause::Done {
                    s.attempts = 0.0;
                }
                let transient = e.error.as_deref().filter(|e| !e.is_empty() && TRANSIENT.is_match(e));
                if e.cause != StopCause::Error || s.stopped {
                    None
                } else if s.waiting {
                    if let Some(error) = transient {
                        s.missed = Some(error.to_string());
                    }
                    None
                } else {
                    Some(transient.map(str::to_string))
                }
            };
            // 待機中はブロックしない（バックグラウンドで監視）
            match work {
                Some(Some(error)) => spawn(r.retry_server(error)),
                Some(None) => spawn(r.monitor_and_retry(Trigger::Error)),
                None => {}
            }
            async { Ok(None) }
        });

        let runner = r.clone();
        let command = Command::new(
            "reconnect",
            &t!("ネットワーク復旧を待って接続を再試行する。引数なしで再試行、status で状態確認のみ。"),
            move |CommandContext { args }| {
                let r = runner.clone();
                let arg = js_trim(&args).to_string();
                // Started before the first await, as /reconnect and agent_end can come at once (#43).
                let monitor = if arg.is_empty() {
                    r.monitor_and_retry(Trigger::Manual)
                } else {
                    None
                };
                spawn(monitor);
                async move {
                    if arg == "status" {
                        let online = r.is_online().await;
                        let target = r.target();
                        if online {
                            r.api.ui().notify(
                                &t!("オンラインです（{target} に到達可能）。", target = target),
                                NotifyLevel::Info,
                            );
                        } else {
                            r.api.ui().notify(
                                &t!("オフラインです（{target} に接続できません）。", target = target),
                                NotifyLevel::Warning,
                            );
                        }
                    } else if !arg.is_empty() {
                        r.api.ui().notify(
                            &t!(
                                "不明な引数 \"{arg}\" です。/reconnect [status] を使ってください。",
                                arg = arg
                            ),
                            NotifyLevel::Warning,
                        );
                    }
                    Ok(())
                }
            },
        )
        .hint("[status]")
        .complete(|prefix: String| async move {
            ["status"]
                .iter()
                .filter(|a| a.starts_with(&prefix))
                .map(|a| {
                    SelectOption::new(
                        *a,
                        if *a == "status" {
                            t!("状態確認のみ")
                        } else {
                            a.to_string()
                        },
                    )
                })
                .collect()
        });
        api.register_command(command);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_told_from_the_requests_fault() {
        for e in [
            "503 down",
            "529 overloaded_error",
            "429 Too Many",
            "Rate limit hit",
            "408 timeout",
            "x temporarily unavailable",
        ] {
            assert!(TRANSIENT.is_match(e), "{e}");
        }
        for e in ["401 Invalid API key", "fetch failed", "5030 nope", "400 bad"] {
            assert!(!TRANSIENT.is_match(e), "{e}");
        }
    }
}
