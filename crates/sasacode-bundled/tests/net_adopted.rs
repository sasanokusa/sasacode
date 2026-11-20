//! packages/bundled/test/adopted.test.ts: goal, background-sessions and auto-reconnect (user plugins
//! that became bundled ones), each driven through a fake host. The jobs' storage is settings.dir in
//! a temp dir where the TS set SASACODE_HOME.

mod support_net;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use serde_json::{Value, json};

use sasacode_bundled::{auto_reconnect, background_sessions, goal};
use sasacode_plugin_api::{Lang, StopCause, ToolResult};
use support_net::*;

async fn fake(plugin: sasacode_plugin_api::PluginFn, root: &Path, settings: Value, entries: Entries) -> Arc<FakeHost> {
    let host = FakeHost::new(root, settings, entries);
    host.load("plugin", plugin)
        .await
        .unwrap_or_else(|e| panic!("load: {e:#}"));
    host
}

fn first_text(r: &ToolResult) -> String {
    r.content.first().and_then(|c| c.as_text()).unwrap_or("").to_string()
}

fn detail(r: &ToolResult, key: &str) -> String {
    r.details
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn goal_set_by_goal_or_set_goal_shown_in_the_prompt_and_footer_and_a_clear_survives_resume() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let entries: Entries = Arc::default();
    let g = fake(goal::plugin, root.path(), json!({}), entries.clone()).await;
    g.session_start().await;
    // No goal: the prompt is left alone (the how-to lives in set_goal's description, A1).
    assert_eq!(g.system_prompt("base").await, "base");
    g.run_command("goal", "ログイン画面を完成させる")
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert!(g.status("goal").unwrap_or_default().contains("ログイン画面"));
    assert!(
        g.injected()
            .last()
            .cloned()
            .unwrap_or_default()
            .contains("ログイン画面を完成させる")
    );
    assert!(
        g.system_prompt("base")
            .await
            .contains("現在のゴール: ログイン画面を完成させる")
    );
    let r = g.exec("set_goal", json!({ "status": "done" })).await;
    assert!(!r.is_error(), "{}", first_text(&r));
    assert_eq!(g.status("goal"), None); // only an active goal is shown
    assert_eq!(g.system_prompt("base").await, "base"); // and only in the prompt
    g.run_command("goal", "clear").await.unwrap_or_else(|e| panic!("{e:#}"));
    let resumed = fake(goal::plugin, root.path(), json!({}), entries).await;
    resumed.session_start().await;
    assert!(first_text(&resumed.exec("goal_status", json!({})).await).contains("未設定"));
}

fn jobs_dir(home: &Path) -> PathBuf {
    home.join("background-sessions")
}

fn bg_settings(home: &Path, poll_ms: u64) -> Value {
    json!({ "pollMs": poll_ms, "dir": jobs_dir(home) })
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sessions_a_job_runs_detached_and_its_exit_code_and_output_come_back_as_a_notice() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let home = root.path().join("home");
    let b = fake(
        background_sessions::plugin,
        root.path(),
        bg_settings(&home, 50),
        Entries::default(),
    )
    .await;
    // The output is not the command's text: the notice quotes the command, so it must come from the log.
    let started = b
        .exec("bg_start", json!({ "command": "echo from-the-$((1+1))-job; exit 3" }))
        .await;
    assert!(
        detail(&started, "logPath").starts_with(&s(&home)),
        "{:?}",
        started.details
    );
    until(750, 20, || !b.injected().is_empty()).await;
    let first = b.injected().first().cloned().unwrap_or_default();
    assert!(first.contains("exit 3"), "{first}");
    assert!(first.contains("from-the-2-job"), "{first}"); // on Windows the log stayed empty (#128)
    assert_eq!(b.tool("bg_start").permissions_as().as_deref(), Some("bash"));

    let long = b.exec("bg_start", json!({ "command": "sleep 30" })).await;
    // As macOS's bash 3.2 may do when the TERM arrives: an exit code is on disk. Still "killed".
    let dir = PathBuf::from(detail(&long, "logPath"));
    let dir = dir.parent().unwrap_or_else(|| panic!("no job dir"));
    std::fs::write(dir.join("exit-code"), "0\n").unwrap_or_else(|e| panic!("{e}"));
    b.run_command("bg", &format!("kill {}", detail(&long, "id")))
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    until(750, 20, || b.injected().len() >= 2).await;
    let second = b.injected().get(1).cloned().unwrap_or_default();
    assert!(second.contains("強制終了"), "{second}");
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sessions_a_job_two_sessions_watched_is_reported_once_and_bg_output_never_offers_a_from_line_below_1()
 {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let home = root.path().join("home-twice");
    let s1 = fake(
        background_sessions::plugin,
        root.path(),
        bg_settings(&home, 20),
        Entries::default(),
    )
    .await;
    let started = s1.exec("bg_start", json!({ "command": "sleep 0.3; echo built" })).await;
    // adopts the running job, and sees its end later
    let s2 = fake(
        background_sessions::plugin,
        root.path(),
        bg_settings(&home, 400),
        Entries::default(),
    )
    .await;
    until(750, 20, || !s1.injected().is_empty()).await;
    sleep(600).await;
    let meta_path = PathBuf::from(detail(&started, "logPath")).with_file_name("meta.json");
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap_or_default()).unwrap_or_default();
    assert_eq!(meta["notified"], true, "{meta}");
    assert_eq!(s1.injected().len() + s2.injected().len(), 1);
    let s3 = fake(
        background_sessions::plugin,
        root.path(),
        bg_settings(&home, 20),
        Entries::default(),
    )
    .await;
    sleep(200).await;
    assert!(s3.injected().is_empty(), "{:?}", s3.injected());
    // bash's own printf, not seq (which Windows' Git Bash may lack); a job starts slowly there, so wait for its end.
    let lines = s3
        .exec("bg_start", json!({ "command": "printf '%s\\n' {1..250}" }))
        .await;
    until(500, 20, || !s3.injected().is_empty()).await;
    let first = s3.injected().first().cloned().unwrap_or_default();
    assert!(first.contains("exit 0"), "{first}"); // writing to the log failed on Windows: exit 1 (#128)
    let log = std::fs::read_to_string(detail(&lines, "logPath")).unwrap_or_default();
    assert!(log.ends_with("249\n250\n"), "{log}");
    let out = first_text(&s3.exec("bg_output", json!({ "id": detail(&lines, "id") })).await);
    assert!(out.contains("Use from_line=1 to read from the start."), "{out}");
    assert!(!out.contains("from_line=-"));
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_reconnect_after_an_error_while_the_endpoint_is_down_it_waits_and_resumes_the_work_once_it_answers() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let reserved = reserve_port();
    let port = reserved.port;
    let url = format!("http://127.0.0.1:{port}/");
    let settings = json!({ "checkUrl": url, "checkIntervalMs": 30, "checkTimeoutMs": 200 });
    let a = fake(auto_reconnect::plugin, root.path(), settings, Entries::default()).await;
    a.session_start().await;
    a.agent_end(StopCause::Error, None).await;
    until(300, 10, || a.status_len() > 0).await;
    assert!(
        a.status("auto-reconnect").unwrap_or_default().contains("オフライン"),
        "{:?}",
        a.status("auto-reconnect")
    );
    let back = reserved.serve_ok();
    until(750, 20, || !a.injected().is_empty()).await;
    assert!(
        a.injected().first().cloned().unwrap_or_default().contains("再開"),
        "{:?}",
        a.injected()
    );
    assert_eq!(a.status_len(), 0);
    back.stop().await;
    // Online and failing for another reason (a bad key): nothing to wait for. A server of its own:
    // the port just freed may already be taken again (as a client's local port) on a busy machine.
    let up = serve(Router::new().fallback(|| async { "ok" })).await;
    let b = fake(
        auto_reconnect::plugin,
        root.path(),
        json!({ "checkUrl": format!("http://127.0.0.1:{}/", up.port), "checkIntervalMs": 30 }),
        Entries::default(),
    )
    .await;
    b.agent_end(StopCause::Error, None).await;
    sleep(100).await;
    assert!(b.injected().is_empty(), "{:?}", b.injected());
    up.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sessions_a_job_from_an_earlier_run_is_judged_by_its_exit_file_and_start_time_not_a_reused_pid() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let home = root.path().join("home-reuse");
    // A process that is alive but is not the job: it started long after the job did. The rustxng
    // stand-in without an idle timeout stays up (there is no `sleep` on a bare Windows PATH).
    let mut stranger = std::process::Command::new(fake_rustxng())
        .args(["--bind", "127.0.0.1", "--port", "0"])
        .env_remove("RUSTXNG_IDLE_TIMEOUT")
        .spawn()
        .unwrap_or_else(|e| panic!("stranger: {e}"));
    let job = |id: &str| {
        let dir = jobs_dir(&home).join(id);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.join("output.log"), "old output\n").unwrap_or_else(|e| panic!("{e}"));
        let meta = json!({
            "id": id, "command": "make", "cwd": root.path(), "runCwd": root.path(), "pid": stranger.id(),
            "startedAt": sasacode_ai::now_ms() - 3_600_000, "status": "running", "sessionId": "s1",
        });
        std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap_or_else(|e| panic!("{e}"));
        dir
    };
    std::fs::write(job("bg-1").join("exit-code"), "0\n").unwrap_or_else(|e| panic!("{e}")); // finished while sasacode was closed
    job("bg-2"); // started an hour ago, but its pid now belongs to a process started just now
    let b = fake(
        background_sessions::plugin,
        root.path(),
        bg_settings(&home, 50),
        Entries::default(),
    )
    .await;
    // Up to 30 s, not 2: on Windows bg-2 is judged by its start time, which PowerShell is started to
    // read, and on the CI runner that alone has taken longer than 2 s (bg-1's notice came alone), or
    // timed out (10 s) once, so that only the next ask answered.
    until(1500, 20, || b.injected().len() >= 2).await;
    let said = b.injected().join("\n");
    assert!(said.contains("bg-1"), "{said}");
    assert!(said.contains("bg-2"), "{said}");
    b.run_command("bg", "kill bg-2")
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    sleep(100).await;
    let alive = matches!(stranger.try_wait(), Ok(None)); // never signalled
    let _ = stranger.kill();
    let _ = stranger.wait();
    assert!(alive);
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_reconnect_a_watch_from_the_session_before_a_switch_injects_nothing_into_the_new_one() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let reserved = reserve_port();
    let settings = json!({
        "checkUrl": format!("http://127.0.0.1:{}/", reserved.port), "checkIntervalMs": 30, "checkTimeoutMs": 200
    });
    let a = fake(auto_reconnect::plugin, root.path(), settings, Entries::default()).await;
    a.session_start().await;
    a.agent_end(StopCause::Error, None).await;
    until(300, 10, || a.status_len() > 0).await;
    a.session_end().await;
    a.session_start().await; // /new or /resume
    let back = reserved.serve_ok();
    sleep(300).await;
    assert!(a.injected().is_empty(), "{:?}", a.injected());
    back.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_reconnect_a_server_errors_check_that_outlasts_a_session_switch_injects_nothing_into_the_new_one() {
    let _l = lang(Lang::Ja).await;
    let root = temp_dir("sasacode-adopted-");
    let hang = Arc::new(AtomicBool::new(true));
    let h = hang.clone();
    let server = serve(Router::new().fallback(move || {
        let hung = h.load(Ordering::SeqCst);
        async move {
            if hung {
                std::future::pending::<()>().await;
            }
            "ok"
        }
    }))
    .await;
    let settings = json!({ "checkUrl": server.url(), "checkIntervalMs": 30, "checkTimeoutMs": 200 });
    let a = fake(auto_reconnect::plugin, root.path(), settings, Entries::default()).await;
    a.session_start().await;
    a.agent_end(StopCause::Error, Some("503 Service Unavailable")).await;
    sleep(50).await; // the first check hangs
    a.session_end().await;
    a.session_start().await;
    sleep(250).await; // the check timed out: offline
    hang.store(false, Ordering::SeqCst);
    sleep(300).await;
    assert!(a.injected().is_empty(), "{:?}", a.injected());
    drop(server);
}

#[test]
fn auto_reconnect_without_a_base_url_it_checks_the_providers_own_api_not_a_third_party() {
    // The TS replaced globalThis.fetch and saw the first URL checked; here the target is asked for.
    let m = model("anthropic", json!({}));
    let target = auto_reconnect::target(None, &m);
    assert!(target.starts_with("https://api.anthropic.com"), "{target}");
}
