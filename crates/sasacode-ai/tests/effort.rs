//! The reasoning effort sent to OpenAI-style servers, and the fallbacks when one is refused
//! (`packages/ai/test/effort.test.ts`).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use sasacode_ai::provider::Fetch;
use sasacode_ai::{ModelInfo, Request, SamplingOptions, ThinkingLevel};
use serde_json::{Value, json};

fn ok() -> Reply {
    Reply::Sse(sse(&[chat_chunk(json!({ "content": "hi" }), Some("stop"))], false))
}

fn refuse(message: &str) -> Reply {
    Reply::Status(
        400,
        json!({ "error": { "message": message, "type": "invalid_request_error" } }).to_string(),
    )
}

const LOW_OR_HIGH: &str = "Invalid option: expected one of \"low\"|\"high\" (reasoning_effort)";

type Sent = Arc<Mutex<Vec<Option<String>>>>;

/// A server that takes only the listed efforts, and records what each request sent.
fn server(accepts: &[Option<&str>], refusal: &str, in_stream: bool) -> (Sent, Arc<dyn Fetch>) {
    let accepts: Vec<Option<String>> = accepts.iter().map(|a| a.map(str::to_string)).collect();
    let refusal = refusal.to_string();
    let sent: Sent = Arc::default();
    let seen = sent.clone();
    let (fetch, _) = fake(move |s, _| {
        let effort = s.body["reasoning_effort"].as_str().map(str::to_string);
        seen.lock().unwrap().push(effort.clone());
        if accepts.contains(&effort) {
            ok()
        } else if in_stream {
            Reply::Sse(sse(
                &[json!({ "error": { "message": refusal, "type": "BadRequestError", "param": null, "code": 400 } })],
                false,
            ))
        } else {
            refuse(&refusal)
        }
    });
    (sent, fetch)
}

fn efforts(sent: &Sent) -> Vec<Option<String>> {
    sent.lock().unwrap().clone()
}

fn want(efforts: &[Option<&str>]) -> Vec<Option<String>> {
    efforts.iter().map(|e| e.map(str::to_string)).collect()
}

static N: AtomicUsize = AtomicUsize::new(0);

/// A fresh model (each its own remembered efforts), with `extra` fields.
fn model_with(extra: Value) -> ModelInfo {
    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
    let mut m = json!({
        "id": format!("m{n}"), "provider": "p", "api": "openai-chat", "baseUrl": "http://x/v1", "contextWindow": 1000,
        "maxOutput": 100, "reasoning": true,
    });
    for (k, v) in extra.as_object().into_iter().flatten() {
        m[k] = v.clone();
    }
    model(m)
}

fn fresh() -> ModelInfo {
    model_with(json!({}))
}

fn ask_request(fetch: Arc<dyn Fetch>, thinking: ThinkingLevel, m: ModelInfo) -> Request {
    Request {
        system: "s".into(),
        messages: vec![user("q")],
        thinking: Some(thinking),
        ..request(m, fetch)
    }
}

async fn ask(fetch: Arc<dyn Fetch>, thinking: ThinkingLevel, m: ModelInfo) -> anyhow::Result<String> {
    let (message, _) = run(provider("openai-chat").as_ref(), ask_request(fetch, thinking, m)).await?;
    Ok(message.content.iter().filter_map(|c| c.as_text()).collect())
}

#[tokio::test]
async fn off_is_sent_as_none_so_servers_that_think_by_default_vllm_stop_thinking() {
    let (sent, fetch) = server(&[Some("none")], LOW_OR_HIGH, false);
    assert_eq!(ask(fetch, ThinkingLevel::Off, fresh()).await.unwrap(), "hi");
    assert_eq!(efforts(&sent), want(&[Some("none")]));
}

#[tokio::test]
async fn a_refused_effort_falls_back_to_the_closest_one_and_the_servers_answer_is_remembered() {
    let (sent, fetch) = server(&[Some("low"), Some("high")], LOW_OR_HIGH, false); // Command Code: no "none", no "xhigh"
    let m = fresh();
    ask(fetch.clone(), ThinkingLevel::Off, m.clone()).await.unwrap();
    assert_eq!(efforts(&sent), want(&[Some("none"), Some("minimal"), Some("low")]));
    ask(fetch, ThinkingLevel::Off, m).await.unwrap();
    assert_eq!(efforts(&sent)[3..], want(&[Some("low")]));
    let (x, fetch) = server(&[Some("high")], LOW_OR_HIGH, false);
    ask(fetch, ThinkingLevel::Max, fresh()).await.unwrap();
    assert_eq!(efforts(&x), want(&[Some("max"), Some("xhigh"), Some("high")]));
}

#[tokio::test]
async fn vllm_refuses_inside_an_http_200_stream_that_is_recognized_too() {
    let (sent, fetch) = server(
        &[Some("xhigh")],
        "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.",
        true,
    );
    assert_eq!(ask(fetch, ThinkingLevel::High, fresh()).await.unwrap(), "hi");
    assert_eq!(efforts(&sent), want(&[Some("high"), Some("xhigh")]));
}

#[tokio::test]
async fn a_model_that_cannot_think_at_all_gets_no_effort_other_errors_are_not_retried() {
    let (sent, fetch) = server(&[None], "\"llama\" does not support thinking", false);
    assert_eq!(ask(fetch, ThinkingLevel::High, fresh()).await.unwrap(), "hi");
    assert_eq!(
        efforts(&sent),
        want(&[Some("high"), Some("xhigh"), Some("medium"), None])
    );
    let (t, fetch) = server(&[], "messages: too long", false);
    let err = ask(fetch, ThinkingLevel::High, fresh()).await.unwrap_err();
    assert!(err.to_string().contains("too long"), "{err}");
    assert_eq!(efforts(&t), want(&[Some("high")]));
}

#[tokio::test]
async fn a_model_known_not_to_reason_is_sent_no_effort() {
    let (sent, fetch) = server(&[None], LOW_OR_HIGH, false);
    ask(fetch, ThinkingLevel::High, model_with(json!({ "reasoning": false })))
        .await
        .unwrap();
    assert_eq!(efforts(&sent), want(&[None]));
}

#[tokio::test]
async fn off_on_llama_cpp_also_turns_the_templates_enable_thinking_off_on_every_attempt_the_callers_kwargs_win_67() {
    /// What each request sent as [reasoning_effort, chat_template_kwargs], from a server taking only `accepts`.
    async fn sent(
        thinking: ThinkingLevel,
        m: ModelInfo,
        accepts: &[Option<&str>],
        extra_body: Option<Value>,
    ) -> Vec<Value> {
        let accepts: Vec<Value> = accepts.iter().map(|a| a.map_or(Value::Null, |a| json!(a))).collect();
        let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
        let seen = bodies.clone();
        let (fetch, _) = fake(move |s, _| {
            let effort = s.body.get("reasoning_effort").cloned().unwrap_or(Value::Null);
            let kwargs = s.body.get("chat_template_kwargs").cloned().unwrap_or(Value::Null);
            seen.lock().unwrap().push(json!([effort, kwargs]));
            if accepts.contains(&effort) {
                ok()
            } else {
                refuse(LOW_OR_HIGH)
            }
        });
        let mut req = ask_request(fetch, thinking, m);
        req.sampling = extra_body.map(|e| SamplingOptions {
            extra_body: e.as_object().cloned(),
            ..Default::default()
        });
        run(provider("openai-chat").as_ref(), req).await.unwrap();
        bodies.lock().unwrap().clone()
    }
    let llama = || model_with(json!({ "server": "llamacpp" }));
    let off = json!({ "enable_thinking": false });
    let none = Some("none");
    assert_eq!(
        sent(ThinkingLevel::Off, llama(), &[none], None).await,
        [json!(["none", off])]
    );
    // A refused effort is retried with the next one; the kwarg stays on each try.
    assert_eq!(
        sent(ThinkingLevel::Off, llama(), &[Some("low")], None).await,
        [json!(["none", off]), json!(["minimal", off]), json!(["low", off])]
    );
    assert_eq!(
        sent(
            ThinkingLevel::Off,
            llama(),
            &[none],
            Some(json!({ "chat_template_kwargs": { "foo": 1 } }))
        )
        .await,
        [json!(["none", { "enable_thinking": false, "foo": 1 }])]
    );
    assert_eq!(
        sent(
            ThinkingLevel::Off,
            llama(),
            &[none],
            Some(json!({ "chat_template_kwargs": { "enable_thinking": true } }))
        )
        .await,
        [json!(["none", { "enable_thinking": true }])]
    );
    // Only for off, only on llama.cpp, and only when an effort is sent at all.
    assert_eq!(
        sent(ThinkingLevel::High, llama(), &[Some("high")], None).await,
        [json!(["high", null])]
    );
    assert_eq!(
        sent(
            ThinkingLevel::Off,
            model_with(json!({ "server": "ollama" })),
            &[none],
            None
        )
        .await,
        [json!(["none", null])]
    );
    assert_eq!(
        sent(ThinkingLevel::Off, fresh(), &[none], None).await,
        [json!(["none", null])]
    );
    assert_eq!(
        sent(
            ThinkingLevel::Off,
            model_with(json!({ "server": "llamacpp", "reasoning": false })),
            &[None],
            None
        )
        .await,
        [json!([null, null])]
    );
}
