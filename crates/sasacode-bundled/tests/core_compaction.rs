//! The compaction and splitPoint tests of packages/bundled/test/bundled.test.ts (lines 78–390).
//! The two `for (const window of [8192, 16_384])` loops are one test per window.

mod support_core;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Notify;

use sasacode_agent::agent::content_chars;
use sasacode_agent::plugins::PluginHost;
use sasacode_ai::{
    AssistantContent, AssistantMessage, ImageContent, Message, Request, StopReason, ThinkingContent, ToolResultMessage,
    UserContent, UserMessage,
};
use sasacode_bundled::compaction::split_point;
use sasacode_plugin_api::{CommandContext, Lang, PermissionMode, PluginApi, StopCause, ToolDef, ToolKind, ToolResult};
use support_core::*;

/// bundled.test.ts `reply(content, extra)`.
fn reply_with(content: Vec<AssistantContent>, extra: impl FnOnce(&mut AssistantMessage)) -> AssistantMessage {
    let mut m = reply(content);
    extra(&mut m);
    m
}

/// `usage: { ...emptyUsage(), input }`.
fn input(n: u64) -> impl FnOnce(&mut AssistantMessage) {
    move |m| {
        m.usage = sasacode_ai::Usage::default();
        m.usage.input = n;
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        content: vec![UserContent::text(text)],
        timestamp: 0,
    })
}

fn result(id: &str, name: &str, text: &str) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: name.into(),
        content: vec![UserContent::text(text)],
        is_error: false,
        timestamp: 0,
    })
}

fn chars(history: &[Message]) -> usize {
    history.iter().map(content_chars).sum()
}

fn long_history() -> Vec<Message> {
    let mut history = Vec::new();
    for i in 0..6 {
        history.push(user(&format!("question {i} {}", "x".repeat(40_000))));
        history.push(Message::Assistant(reply(vec![say(&format!("answer {i}"))])));
    }
    history
}

/// A session of about `chars` characters: questions, file reads with long results, answers.
fn reading_session(n: usize) -> Vec<Message> {
    let mut history = Vec::new();
    let mut i = 0;
    while chars(&history) < n {
        history.push(user(&format!("step {i}: {}", "why ".repeat(150))));
        history.push(Message::Assistant(reply(vec![
            say(&format!("reading f{i}.ts")),
            call(&format!("c{i}"), "read", json!({ "path": format!("f{i}.ts") })),
        ])));
        history.push(result(
            &format!("c{i}"),
            "read",
            &format!("const v{i} = 1;\n").repeat(200),
        ));
        history.push(Message::Assistant(reply(vec![say(&format!(
            "f{i}.ts sets v{i}. {}",
            "so ".repeat(300)
        ))])));
        i += 1;
    }
    history
}

/// A session of about `chars` characters in Japanese, as a Japanese user's is: the files read are code.
fn japanese_session(n: usize) -> Vec<Message> {
    let mut history = Vec::new();
    let mut i = 0;
    while chars(&history) < n {
        history.push(user(&format!(
            "手順{i}：{}",
            "このファイルがなぜ必要なのかを調べてください。".repeat(6)
        )));
        history.push(Message::Assistant(reply(vec![
            say(&format!("f{i}.tsを読みます。")),
            call(&format!("c{i}"), "read", json!({ "path": format!("f{i}.ts") })),
        ])));
        history.push(result(
            &format!("c{i}"),
            "read",
            &format!("// 設定値{i}\nconst v{i} = 1;\n").repeat(40),
        ));
        history.push(Message::Assistant(reply(vec![say(&format!(
            "f{i}.tsはv{i}を設定しています。{}",
            "そのため、この値を変えると読み込みの動作が変わります。".repeat(8)
        ))])));
        i += 1;
    }
    history
}

/// One prompt and then `steps` tool calls, as an agent working on its own makes: no user message after the first.
fn autonomous_run(steps: usize, interrupt: bool) -> Vec<Message> {
    let mut history = vec![user("build the simulator")];
    for i in 0..steps {
        if interrupt && i == steps - 1 {
            history.push(user("how is it going?"));
        }
        history.push(Message::Assistant(reply(vec![
            say(&format!("step {i}: patching web/sim{i}.js")),
            call(
                &format!("c{i}"),
                "bash",
                json!({ "command": format!("python3 patch{i}.py") }),
            ),
        ])));
        history.push(result(
            &format!("c{i}"),
            "bash",
            &format!("ok {i}\n{}", "x".repeat(2_000)),
        ));
    }
    history
}

fn utf16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The summarizer's request, estimated as the core sizes replies, leaves room for its max_tokens.
fn expect_fits(req: &Request, window: u64) {
    let input = (((utf16(&req.system) + chars(&req.messages)) as f64 / 4.0) * 1.1).ceil() as u64;
    assert!(req.tools.is_empty());
    let max = req.max_tokens.unwrap_or(0);
    assert!(input + max <= window, "{input} + {max} > {window}");
}

fn text_of_content(m: &Message) -> String {
    let user = |c: &[UserContent]| c.iter().filter_map(UserContent::as_text).collect::<String>();
    match m {
        Message::User(u) => user(&u.content),
        Message::Tool(t) => user(&t.content),
        Message::Assistant(a) => a
            .content
            .iter()
            .map(|c| match c {
                AssistantContent::Text(t) => t.text.as_str(),
                _ => "",
            })
            .collect(),
    }
}

/// Tokens a local tokenizer (Qwen, Gemma, Llama) makes of a request, roughly: 4 ASCII characters a token, a Japanese character a token.
fn tokens(system: &str, messages: &[Message]) -> usize {
    let text = format!("{system}{}", messages.iter().map(text_of_content).collect::<String>());
    let wide: usize = text.chars().filter(|c| !c.is_ascii()).map(char::len_utf16).sum();
    ((utf16(&text) - wide) as f64 / 4.0 + wide as f64).ceil() as usize
}

fn json_of_messages(m: &[Message]) -> String {
    serde_json::to_string(m).unwrap_or_default()
}

fn compaction(settings: Value) -> HashMap<String, Value> {
    HashMap::from([("compaction".to_string(), settings)])
}

fn small(window: u64) -> SetupOpts {
    SetupOpts {
        context_window: Some(window),
        max_output: Some(32_000),
        ..SetupOpts::default()
    }
}

/// `host.commands.find((c) => c.name === "compact")!.run({ args })`.
async fn compact(host: &PluginHost, args: &str) -> anyhow::Result<()> {
    let c = host
        .commands()
        .into_iter()
        .find(|c| c.command.name == "compact")
        .expect("compact command");
    (c.command.run)(CommandContext { args: args.into() }).await
}

fn role(m: &Message) -> &'static str {
    match m {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::Tool(_) => "tool",
    }
}

fn call_ids(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|m| match m {
            Message::Assistant(a) => a.tool_calls().map(|c| c.id.clone()).collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn compaction_proactive_at_the_threshold_keeping_the_recent_tail() {
    let s = setup(
        vec![
            reply_with(vec![say("big")], input(85_000)),
            reply(vec![say("SUMMARY TEXT")]),
        ],
        &["compaction"],
        SetupOpts::default(),
    )
    .await;
    s.agent.reset(long_history());
    let _ = s.agent.prompt_text("next question").await;
    let requests = s.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty()); // the summarizer call
    let messages = s.agent.messages();
    assert!(json_of(&messages[0]).contains("SUMMARY TEXT"));
    let before_last = &messages[messages.len() - 2];
    assert_eq!(role(before_last), "user");
    assert_eq!(text_of_content(before_last), "next question");
    // The tail under the keep budget (~15k tokens here) starts at a user message: question 5 onward.
    assert_eq!(
        messages.iter().map(role).collect::<Vec<_>>(),
        vec!["user", "user", "assistant", "user", "assistant"]
    );
    assert!(json_of(&messages[1]).contains("question 5"));
}

#[tokio::test]
async fn compaction_on_context_limit_it_compacts_and_the_turn_is_retried() {
    let s = setup(
        vec![
            reply_with(vec![call("1", "read", json!({ "path": "nope" }))], input(99_500)),
            reply(vec![say("SUMMARY")]),
            reply(vec![say("continued")]),
        ],
        &["compaction"],
        SetupOpts {
            settings: compaction(json!({ "threshold": 2 })),
            ..SetupOpts::default()
        },
    )
    .await;
    s.agent.reset(long_history());
    assert_eq!(s.agent.prompt_text("next question").await.unwrap(), StopCause::Done);
    let requests = s.requests();
    assert_eq!(requests.len(), 3);
    assert!(json_of(&requests[2].messages[0]).contains("SUMMARY"));
    assert_eq!(role(last(&requests[2])), "tool");
}

#[test]
fn compaction_compact_shows_that_it_is_summarizing_and_an_empty_summary_leaves_the_history_as_it_was() {
    let _lang = lang_lock(Lang::Ja);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let thinking = AssistantContent::Thinking(ThinkingContent {
            thinking: "all the output went here".into(),
            signature: None,
            redacted: None,
            reasoning_field: None,
        });
        let s = setup(vec![reply(vec![thinking])], &["compaction"], SetupOpts::default()).await;
        let history = long_history();
        s.agent.reset(history.clone());
        let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let (rec, host): (_, Weak<PluginHost>) = (seen.clone(), Arc::downgrade(&s.host));
        s.host.set_on_change(Some(Arc::new(move || {
            if let Some(h) = host.upgrade() {
                rec.lock().unwrap().push(h.status().values().cloned().collect());
            }
        })));
        let e = compact(&s.host, "").await.unwrap_err();
        assert!(e.to_string().contains("empty summary"), "{e}");
        assert_eq!(s.agent.messages(), history);
        let seen = seen.lock().unwrap().clone();
        assert!(seen[0][0].contains("要約中"), "{seen:?}");
        assert!(s.host.status().is_empty());
    });
}

#[tokio::test]
async fn compaction_a_summary_whose_stream_was_cut_leaves_the_history_as_it_was_95() {
    let cut = reply_with(vec![say("The user asked to refac")], |m| {
        m.stop_reason = StopReason::Error;
        m.error_message = Some("the stream ended before it was complete".into());
    });
    let s = setup(vec![cut], &["compaction"], SetupOpts::default()).await;
    let history = long_history();
    s.agent.reset(history.clone());
    let e = compact(&s.host, "").await.unwrap_err();
    assert!(format!("{e:#}").contains("ended before"), "{e:#}");
    assert_eq!(s.agent.messages(), history);
}

#[tokio::test]
async fn compaction_a_tool_result_clipped_in_the_middle_of_an_emoji_is_sent_well_formed_88() {
    let s = setup(vec![reply(vec![say("SUMMARY")])], &["compaction"], SetupOpts::default()).await;
    s.agent.reset(vec![
        user("look"),
        Message::Assistant(reply(vec![call("c1", "read", json!({ "path": "e.txt" }))])),
        result("c1", "read", &format!("{}{}", "x".repeat(1999), "😀".repeat(10))),
        Message::Assistant(reply(vec![say("ok")])),
    ]);
    compact(&s.host, "").await.unwrap();
    let requests = s.requests();
    let sent = text_of_content(&requests[0].messages[0]);
    // A Rust string is always well-formed; the half of the emoji cut off is U+FFFD, as toWellFormed() made it.
    assert!(
        sent.contains(&format!("{}\u{fffd}… [19 more chars]", "x".repeat(1999))),
        "{sent}"
    );
}

/// A tool that says it started and finishes when released.
fn slow_tool(
    started: Arc<Notify>,
    release: Arc<Notify>,
) -> impl FnOnce(PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    move |api| {
        Box::pin(async move {
            api.register_tool(
                ToolDef::new(
                    "slow",
                    "waits",
                    json!({ "type": "object", "properties": {} }),
                    ToolKind::Other,
                    move |_a, _c| {
                        let (started, release) = (started.clone(), release.clone());
                        async move {
                            started.notify_one();
                            release.notified().await;
                            Ok(ToolResult::text("finished"))
                        }
                    },
                )
                .into_ref(),
            );
            Ok(())
        })
    }
}

#[tokio::test]
async fn compaction_compact_while_a_tool_runs_keeps_the_call_so_its_result_does_not_arrive_without_it_118() {
    let s = setup(
        vec![
            reply(vec![call("call-1", "slow", json!({}))]),
            reply(vec![say("SUMMARY")]),
            reply(vec![say("done")]),
        ],
        &["compaction"],
        SetupOpts::default(),
    )
    .await;
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    assert!(s.host.load("slow", slow_tool(started.clone(), release.clone())).await);
    s.agent.set_permissions(policy(PermissionMode::Auto));
    s.agent.reset(long_history());
    let (run, ()) = tokio::join!(s.agent.prompt_text("go"), async {
        started.notified().await;
        compact(&s.host, "").await.unwrap();
        release.notify_one();
    });
    run.unwrap();
    let requests = s.requests();
    let sent = &requests[2].messages;
    let calls = call_ids(sent);
    assert!(sent.iter().all(|m| match m {
        Message::Tool(t) => calls.contains(&t.tool_call_id),
        _ => true,
    }));
    assert!(json_of(&sent[0]).contains("SUMMARY"));
}

async fn window_fits(window: u64) {
    // vLLM and llama.cpp reject input + max_tokens over the window: an 8,192-token summary could not be asked for.
    let budget = (window as f64 * 0.15).floor() as u64;
    let auto = setup(
        vec![
            reply_with(vec![call("1", "read", json!({ "path": "nope" }))], input(window - 300)),
            reply(vec![say("SUMMARY")]),
            reply(vec![say("continued")]),
        ],
        &["compaction"],
        SetupOpts {
            settings: compaction(json!({ "threshold": 2 })),
            ..small(window)
        },
    )
    .await;
    auto.agent.reset(reading_session(window as usize * 4));
    assert_eq!(auto.agent.prompt_text("next question").await.unwrap(), StopCause::Done);
    let requests = auto.requests();
    assert_eq!(requests[1].max_tokens, Some(budget));
    expect_fits(&requests[1], window);
    assert!(json_of(&auto.agent.messages()[0]).contains("SUMMARY"));

    // /compact summarizes all of it: a full window fits with its tool results clipped harder, every step kept.
    let full = setup(vec![reply(vec![say("SUMMARY")])], &["compaction"], small(window)).await;
    full.agent.reset(reading_session(window as usize * 4));
    compact(&full.host, "").await.unwrap();
    let requests = full.requests();
    assert_eq!(requests[0].max_tokens, Some(budget));
    expect_fits(&requests[0], window);
    assert!(json_of_messages(&requests[0].messages).contains("step 0:"));

    // More than the window holds (a session carried over from a larger model): the oldest part makes way.
    let manual = setup(vec![reply(vec![say("SUMMARY")])], &["compaction"], small(window)).await;
    let history = reading_session(window as usize * 12);
    manual.agent.reset(history.clone());
    compact(&manual.host, "keep the paths").await.unwrap();
    let requests = manual.requests();
    let req = &requests[0];
    assert_eq!(req.max_tokens, Some(budget));
    expect_fits(req, window);
    // The latest step and the instructions are there.
    let sent = json_of_messages(&req.messages);
    let last = history.len() / 4 - 1;
    assert!(!sent.contains("step 0:"));
    assert!(sent.contains(&format!("step {last}:")));
    assert!(sent.contains(&format!("f{last}.ts sets v{last}")));
    assert!(sent.contains("keep the paths"));
    let messages = manual.agent.messages();
    assert_eq!(messages.len(), 1);
    assert!(json_of(&messages[0]).contains("SUMMARY"));
}

#[tokio::test]
async fn compaction_in_a_8192_token_window_the_summary_request_fits_on_context_limit_and_on_compact_60() {
    window_fits(8192).await;
}

#[tokio::test]
async fn compaction_in_a_16384_token_window_the_summary_request_fits_on_context_limit_and_on_compact_60() {
    window_fits(16_384).await;
}

async fn japanese_cut_to_fit(window: u64) {
    // About 4 characters a token undercounts Japanese 3-4 times: the request went over the window.
    let s = setup(vec![reply(vec![say("要約")])], &["compaction"], small(window)).await;
    let history = japanese_session(window as usize * 4);
    s.agent.reset(history.clone());
    compact(&s.host, "パスを残して").await.unwrap();
    let requests = s.requests();
    let req = &requests[0];
    let max = req.max_tokens.unwrap_or(0);
    assert_eq!(max, (window as f64 * 0.15).floor() as u64);
    let used = tokens(&req.system, &req.messages) as u64 + max;
    assert!(used <= window, "{used} > {window}");
    assert!(used as f64 > window as f64 * 0.6, "{used}"); // not much more left out than needed
    let sent = json_of_messages(&req.messages);
    let last = history.len() / 4 - 1;
    assert!(!sent.contains("手順0："));
    assert!(sent.contains(&format!("手順{last}：")));
    assert!(sent.contains(&format!("f{last}.tsはv{last}を設定しています")));
    assert!(sent.contains("パスを残して"));
    assert_eq!(s.agent.messages().len(), 1);
}

#[tokio::test]
async fn compaction_a_japanese_history_more_than_a_8192_token_window_holds_is_cut_to_fit_the_latest_of_it_kept_60() {
    japanese_cut_to_fit(8192).await;
}

#[tokio::test]
async fn compaction_a_japanese_history_more_than_a_16384_token_window_holds_is_cut_to_fit_the_latest_of_it_kept_60() {
    japanese_cut_to_fit(16_384).await;
}

#[tokio::test]
async fn compaction_compact_whose_instructions_take_more_than_half_the_room_leaves_the_history_as_it_was_60() {
    // A history that must be cut: instructions leaving room for a few characters of it had those
    // summarized, and the summary replaced every message.
    let window: u64 = 8192;
    // What instructions can take: the input, less the summary's 15%, the system prompt (278 + 8) and the request's framing (159).
    let room = ((window as f64 - (window as f64 * 0.15).floor()) / 1.1 * 4.0).floor() as usize - 278 - 8 - 159;
    for length in [
        (room as f64 * 0.4).floor() as usize,
        (room as f64 * 0.55).ceil() as usize,
        room - 600,
        room * 2,
    ] {
        let s = setup(vec![reply(vec![say("SUMMARY")])], &["compaction"], small(window)).await;
        let history = reading_session(window as usize * 12);
        s.agent.reset(history.clone());
        let args: String = "be thorough ".repeat(length).chars().take(length).collect();
        let run = compact(&s.host, &args).await;
        if (length as f64) < room as f64 / 2.0 {
            run.unwrap();
            let requests = s.requests();
            expect_fits(&requests[0], window);
            let last = history.len() / 4 - 1;
            assert!(json_of_messages(&requests[0].messages).contains(&format!("f{last}.ts sets v{last}")));
            assert_eq!(s.agent.messages().len(), 1);
            continue;
        }
        let e = run.unwrap_err();
        assert!(e.to_string().contains("instructions"), "{length}: {e}");
        assert_eq!(s.agent.messages(), history);
        assert!(s.requests().is_empty());
        assert!(s.host.status().is_empty());
    }
}

#[tokio::test]
async fn compaction_a_large_window_still_asks_for_an_8192_token_summary_of_everything_60() {
    let s = setup(
        vec![reply(vec![say("SUMMARY")])],
        &["compaction"],
        SetupOpts {
            context_window: Some(1_000_000),
            max_output: Some(64_000),
            ..SetupOpts::default()
        },
    )
    .await;
    s.agent.reset(reading_session(200_000));
    compact(&s.host, "").await.unwrap();
    let requests = s.requests();
    assert_eq!(requests[0].max_tokens, Some(8192));
    let sent = json_of_messages(&requests[0].messages);
    assert!(sent.contains("step 0:"));
    assert!(sent.contains("… [800 more chars]")); // results clipped as before, at 2,000 characters
}

#[test]
fn split_point_keeps_the_tail_from_a_user_message() {
    let b = Message::Assistant(reply(vec![say("b")]));
    let d = Message::Assistant(reply(vec![say("d")]));
    let msgs = vec![user(&"a".repeat(400)), b.clone(), user(&"c".repeat(100)), d.clone()];
    assert_eq!(split_point(&msgs, 50.0), 2);
    assert_eq!(split_point(&msgs, 10_000.0), 0);
    // From the last user message the tail would keep less than half the budget: it starts at the assistant turn before.
    let short = vec![user(&"a".repeat(400)), b, user("c"), d];
    assert_eq!(split_point(&short, 50.0), 1);
}

#[test]
fn split_point_keeps_the_latest_steps_of_a_long_run_of_tool_calls_after_one_prompt() {
    // No user message within the budget: the whole history was summarized, nothing of the latest
    // steps kept, and the model then made up what those steps had done (session 4d674043).
    let keep = 10_000;
    let history = autonomous_run(200, false);
    let split = split_point(&history, keep as f64);
    let tail = &history[split..];
    assert!(split < history.len());
    assert_eq!(role(&tail[0]), "assistant");
    assert!(tokens("", tail) <= keep);
    assert!(tokens("", tail) > keep / 2);
    // Every tool result kept has its call kept before it.
    let calls = call_ids(tail);
    for m in tail {
        if let Message::Tool(t) = m {
            assert!(calls.contains(&t.tool_call_id));
        }
    }
    assert!(json_of(tail.last().unwrap()).contains("ok 199"));

    // A short message from the user at the end of such a run does not leave only itself and what came after.
    let interrupted = autonomous_run(200, true);
    let after = &interrupted[split_point(&interrupted, keep as f64)..];
    assert_eq!(role(&after[0]), "assistant");
    assert!(tokens("", after) > keep / 2);
    assert!(json_of_messages(after).contains("how is it going?"));
}

#[tokio::test]
async fn compaction_on_context_limit_in_a_long_run_of_tool_calls_the_latest_steps_are_kept_as_they_were() {
    let s = setup(
        vec![
            reply_with(vec![call("x", "read", json!({ "path": "nope" }))], input(99_500)),
            reply(vec![say("SUMMARY")]),
            reply(vec![say("continued")]),
        ],
        &["compaction"],
        SetupOpts {
            settings: compaction(json!({ "threshold": 2 })),
            ..SetupOpts::default()
        },
    )
    .await;
    s.agent.reset(autonomous_run(200, false));
    assert_eq!(s.agent.prompt_text("next question").await.unwrap(), StopCause::Done);
    let requests = s.requests();
    let retried = &requests[2].messages;
    assert!(json_of(&retried[0]).contains("SUMMARY"));
    assert_eq!(role(&retried[1]), "assistant");
    assert!(retried.len() > 10);
    assert!(json_of_messages(retried).contains("ok 199"));
}

#[test]
fn split_point_keeps_a_japanese_tail_within_its_budget_a_japanese_character_counted_as_a_token_60() {
    // At 4 characters a token the tail was about 3 times the budget, and the window soon filled again.
    let keep = (16_384f64 * 0.15).floor() as usize;
    let history = japanese_session(40_000);
    let tail = &history[split_point(&history, keep as f64)..];
    assert_eq!(role(&tail[0]), "user");
    assert!(tokens("", tail) <= keep);
    assert!(tokens("", tail) > keep / 2);
}

#[test]
fn split_point_does_not_read_through_the_base64_of_images_in_the_tail_60() {
    // Looking for Japanese in each screenshot's megabytes of base64 held up every compaction for about 0.6 s.
    let session = |data: &str| -> Vec<Message> {
        let data: Arc<str> = Arc::from(data);
        (0..40)
            .flat_map(|i| {
                [
                    Message::User(UserMessage {
                        content: vec![
                            UserContent::text(format!("look at shot {i}")),
                            UserContent::Image(ImageContent {
                                media_type: "image/png".into(),
                                data: data.clone(),
                            }),
                        ],
                        timestamp: 0,
                    }),
                    Message::Assistant(reply(vec![say("ok")])),
                ]
            })
            .collect()
    };
    let shots = session(&"A".repeat(5_000_000));
    let t0 = Instant::now();
    let split = split_point(&shots, 30_000.0); // the tail kept of a 200K window
    assert!(t0.elapsed().as_millis() < 100, "{:?}", t0.elapsed());
    assert_eq!(split, split_point(&session(""), 30_000.0));
    assert!(split > 0);
    assert!(split < shots.len());
}
