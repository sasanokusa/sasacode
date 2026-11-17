//! packages/bundled/test/jev-backends.test.ts. `fetch`, `process.env` and the keychain come in
//! through `Deps`, so nothing here reaches the network, the environment or the real keychain.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_ai::Fetch;
use sasacode_bundled::jev_guard::backends::{
    BackendSettings, Completer, Deps, NoulCriteria, Question, Questions, detect_service, make_backend, parse_vercel,
};
use sasacode_plugin_api::CompleteOptions;

fn questions() -> Questions {
    let mut q = Questions::new();
    q.insert(
        "verdict".into(),
        Question::Choice {
            instructions: "How?".into(),
            criteria: [("allow", "a"), ("ask", "b"), ("deny", "c")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        },
    );
    q.insert(
        "risky".into(),
        Question::Noul {
            instructions: "Risky?".into(),
            criteria: Some(NoulCriteria {
                yes: "yes".into(),
                no: "no".into(),
            }),
        },
    );
    q
}

#[derive(Debug, Clone)]
struct Sent {
    url: String,
    headers: HashMap<String, String>,
    body: Value,
}

type Reply = Arc<dyn Fn(usize) -> anyhow::Result<http::Response<String>> + Send + Sync>;

/// `globalThis.fetch` replaced: records what was sent and answers with `reply(call index)`.
struct Capture {
    sent: Arc<Mutex<Vec<Sent>>>,
    reply: Reply,
}

impl Fetch for Capture {
    fn send(&self, req: reqwest::Request) -> BoxFuture<'static, anyhow::Result<reqwest::Response>> {
        let body = req
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let n = {
            let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            sent.push(Sent {
                url: req.url().to_string(),
                headers,
                body,
            });
            sent.len() - 1
        };
        let res = (self.reply)(n);
        Box::pin(async move { Ok(reqwest::Response::from(res?)) })
    }
}

fn response(status: u16, headers: &[(&str, String)], body: &str) -> anyhow::Result<http::Response<String>> {
    let mut b = http::Response::builder().status(status);
    for (k, v) in headers {
        b = b.header(*k, v.as_str());
    }
    Ok(b.body(body.to_string())?)
}

struct Env {
    vars: Arc<Mutex<HashMap<String, String>>>,
    sent: Arc<Mutex<Vec<Sent>>>,
}

impl Env {
    fn new() -> Env {
        Env {
            vars: Arc::default(),
            sent: Arc::default(),
        }
    }
    fn set(&self, k: &str, v: &str) {
        self.vars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(k.into(), v.into());
    }
    fn unset(&self, k: &str) {
        self.vars.lock().unwrap_or_else(|e| e.into_inner()).remove(k);
    }
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Deps answering with `reply`; a fresh record of what was sent.
    fn deps(&self, reply: Reply) -> Deps {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let vars = self.vars.clone();
        Deps {
            fetch: Arc::new(Capture {
                sent: self.sent.clone(),
                reply,
            }),
            env: Arc::new(move |k| vars.lock().unwrap_or_else(|e| e.into_inner()).get(k).cloned()),
            keychain: Arc::new(|_| Box::pin(async { None })),
        }
    }
    /// `capture(reply, status)`.
    fn capture(&self, reply: Value, status: u16) -> Deps {
        let text = reply.to_string();
        self.deps(Arc::new(move |_| response(status, &[], &text)))
    }
}

/// The plugin api is never used by the Jev services.
fn no_complete() -> Completer {
    Arc::new(|_| Box::pin(async { anyhow::bail!("not used") }))
}

fn native() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "verdict": { "type": "choice", "choice": "ask", "probabilities": { "allow": 0.2, "ask": 0.7, "deny": 0.1 }, "confidence": 0.8 },
            "risky": { "type": "noul", "noul": 0.6 },
        },
    })
}

fn settings() -> BackendSettings {
    BackendSettings::default()
}

#[tokio::test]
async fn native_dialect_command_code_typesafe_and_openrouter_differ_only_in_url_model_and_key() {
    let env = Env::new();
    let cases = [
        (
            "commandcode",
            "CMD_API_KEY",
            "https://api.commandcode.ai/provider/v1/systemone",
            "typesafe/jev",
        ),
        (
            "typesafe",
            "TYPESAFE_API_KEY",
            "https://api.typesafe.ai/v1/systemone",
            "jev-latest",
        ),
        (
            "openrouter",
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/alpha/decisions",
            "~typesafe/jev-latest",
        ),
    ];
    for (name, var, url, model) in cases {
        env.set(var, &format!("key-{name}"));
        let deps = env.capture(native(), 200);
        let raw = make_backend(no_complete(), name, &settings(), &deps)
            .unwrap()
            .ask(&json!({ "call": "x" }), &questions(), &CancellationToken::new())
            .await
            .unwrap();
        let sent = env.sent();
        assert_eq!(sent[0].url, url);
        assert_eq!(sent[0].headers["authorization"], format!("Bearer key-{name}"));
        assert_eq!(
            sent[0].body,
            json!({ "model": model, "state": { "call": "x" }, "questions": questions() })
        );
        assert_eq!(raw.choice["verdict"].choice, "ask");
        assert_eq!(
            raw.choice["verdict"].probabilities,
            json!({ "allow": 0.2, "ask": 0.7, "deny": 0.1 })
        );
        assert_eq!(raw.choice["verdict"].confidence, 0.8);
        assert_eq!(raw.choice.len(), 1);
        assert_eq!(raw.noul.iter().collect::<Vec<_>>(), vec![(&"risky".to_string(), &0.6)]);
    }
}

#[tokio::test]
async fn vercel_dialect_model_in_a_header_noul_as_boolean_probability_confidence_out_of_band() {
    let env = Env::new();
    env.set("AI_GATEWAY_API_KEY", "gw");
    let deps = env.capture(
        json!({
            "answers": {
                "verdict": { "type": "choice", "choice": "allow", "probabilities": { "allow": 0.9, "ask": 0.1, "deny": 0 } },
                "risky": { "type": "boolean", "probability": 0.05 },
            },
            "providerMetadata": { "typesafe": { "confidence": { "verdict": 0.85 } } },
        }),
        200,
    );
    let raw = make_backend(no_complete(), "vercel", &settings(), &deps)
        .unwrap()
        .ask(&json!({ "call": "x" }), &questions(), &CancellationToken::new())
        .await
        .unwrap();
    let sent = env.sent();
    assert_eq!(sent[0].url, "https://ai-gateway.vercel.sh/v4/ai/evaluation-model");
    assert_eq!(sent[0].headers["ai-model-id"], "typesafe-ai/jev");
    assert_eq!(sent[0].body.get("model"), None);
    assert_eq!(sent[0].body["questions"]["risky"]["type"], "boolean");
    assert_eq!(raw.choice["verdict"].confidence, 0.85);
    assert_eq!(raw.noul["risky"], 0.05);
    // Without reported calibration, a high probability cannot stand in for confidence.
    let est = parse_vercel(
        &json!({ "answers": { "verdict": { "choice": "ask", "probabilities": { "allow": 0.3, "ask": 0.6, "deny": 0.1 } }, "risky": { "probability": 1 } } }),
        &questions(),
    )
    .unwrap();
    assert_eq!(est.choice["verdict"].confidence, 0.0);
}

#[tokio::test]
async fn settings_override_model_endpoint_and_key_variable_a_missing_key_is_an_error_not_a_guess() {
    let env = Env::new();
    env.set("MY_JEV_KEY", "mine");
    let deps = env.capture(native(), 200);
    let custom = BackendSettings {
        model: Some("jev-1.13.0".into()),
        endpoint: Some("https://proxy.example/v1/systemone".into()),
        api_key_env: Some("MY_JEV_KEY".into()),
        ..BackendSettings::default()
    };
    make_backend(no_complete(), "typesafe", &custom, &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap();
    let sent = env.sent();
    assert_eq!(sent[0].url, "https://proxy.example/v1/systemone");
    assert_eq!(sent[0].headers["authorization"], "Bearer mine");
    assert_eq!(sent[0].body["model"], "jev-1.13.0");
    env.unset("MY_JEV_KEY");
    let keyed = BackendSettings {
        api_key_env: Some("MY_JEV_KEY".into()),
        ..BackendSettings::default()
    };
    let e = make_backend(no_complete(), "typesafe", &keyed, &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("MY_JEV_KEY"), "{e}");
    let e = make_backend(no_complete(), "nope", &settings(), &deps).err().unwrap();
    assert!(e.to_string().contains("unknown backend"), "{e}");
}

#[tokio::test]
async fn errors_http_failures_and_malformed_answers_are_reported_5xx_retried_once() {
    let env = Env::new();
    env.set("TYPESAFE_API_KEY", "k");
    let deps = env.deps(Arc::new(|_| response(529, &[], "overloaded")));
    let e = make_backend(no_complete(), "typesafe", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("HTTP 529"), "{e}");
    assert_eq!(env.sent().len(), 2);
    let deps = env.capture(json!({ "answers": { "verdict": native()["answers"]["verdict"] } }), 200);
    let e = make_backend(no_complete(), "typesafe", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no answer for risky"), "{e}");
}

#[tokio::test]
async fn auto_detection_picks_command_code_then_typesafe_openrouter_and_vercel_only_when_named() {
    let env = Env::new();
    for k in [
        "CMD_API_KEY",
        "TYPESAFE_API_KEY",
        "OPENROUTER_API_KEY",
        "AI_GATEWAY_API_KEY",
    ] {
        env.set(k, "");
    }
    env.set("OPENROUTER_API_KEY", "or");
    env.set("TYPESAFE_API_KEY", "ts");
    let deps = env.capture(json!({}), 200);
    assert_eq!(detect_service(&deps).await, Some("typesafe"));
    env.set("CMD_API_KEY", "cc");
    assert_eq!(detect_service(&deps).await, Some("commandcode"));
}

#[tokio::test]
async fn chat_backend_any_sasacode_model_answers_the_same_questions_as_json() {
    let env = Env::new();
    let deps = env.capture(json!({}), 200);
    type Asked = Arc<Mutex<Vec<(Option<String>, String)>>>;
    let asked: Asked = Arc::default();
    let rec = asked.clone();
    let chat: Completer = Arc::new(move |o: CompleteOptions| {
        let text = match &o.messages[0] {
            sasacode_ai::Message::User(u) => u.content[0].as_text().unwrap_or("").to_string(),
            _ => String::new(),
        };
        rec.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((o.model.clone(), text));
        Box::pin(async {
            Ok(
                "Sure:\n```json\n{\"verdict\": \"deny\", \"verdict_confidence\": 0.9, \"risky\": true}\n```"
                    .to_string(),
            )
        })
    });
    let model = BackendSettings {
        model: Some("ollama/gemma4:e4b".into()),
        ..BackendSettings::default()
    };
    let raw = make_backend(chat, "chat", &model, &deps)
        .unwrap()
        .ask(
            &json!({ "call": { "command": "rm -rf ~" } }),
            &questions(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked[0].0.as_deref(), Some("ollama/gemma4:e4b"));
    assert!(
        asked[0].1.contains("\"verdict\": one of \"allow\", \"ask\", \"deny\""),
        "{}",
        asked[0].1
    );
    assert!(asked[0].1.contains("rm -rf ~"));
    let v = &raw.choice["verdict"];
    assert_eq!(v.choice, "deny");
    assert_eq!(v.probabilities["deny"], 0.9);
    assert!((v.probabilities["allow"].as_f64().unwrap() - 0.05).abs() < 0.005);
    assert_eq!(raw.noul["risky"], 1.0);

    let bad: Completer = Arc::new(|_| Box::pin(async { Ok("{\"verdict\": \"maybe\"}".to_string()) }));
    let e = make_backend(bad, "chat", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("chose \"maybe\""), "{e}");
    let none: Completer = Arc::new(|_| Box::pin(async { Ok("I cannot help".to_string()) }));
    let e = make_backend(none, "chat", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no JSON"), "{e}");
}

#[tokio::test]
async fn jev_retries_honor_retry_after_and_cancellation_rather_than_hammering_a_busy_service() {
    let env = Env::new();
    env.set("TYPESAFE_API_KEY", "k");
    let deps = env.deps(Arc::new(|_| response(429, &[("retry-after", "1".into())], "busy")));
    let signal = CancellationToken::new();
    let started = Instant::now();
    let s = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.cancel();
    });
    let e = make_backend(no_complete(), "typesafe", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &signal)
        .await
        .unwrap_err();
    // The TS rejected with the signal's reason ("judge deadline"); a CancellationToken carries none.
    assert_eq!(e.to_string(), "aborted");
    assert_eq!(env.sent().len(), 1);
    assert!(started.elapsed() < Duration::from_millis(500));

    let busy_then_ok: Reply = Arc::new(|n| {
        if n == 0 {
            response(529, &[("retry-after", "0".into())], "busy")
        } else {
            response(200, &[], &native().to_string())
        }
    });
    let deps = env.deps(busy_then_ok);
    let raw = make_backend(no_complete(), "typesafe", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(raw.choice["verdict"].confidence, 0.8);
    assert_eq!(env.sent().len(), 2);
}

#[tokio::test]
async fn jev_does_not_retry_validation_errors_or_wait_beyond_its_retry_budget() {
    let env = Env::new();
    env.set("TYPESAFE_API_KEY", "k");
    for status in [400, 401, 403, 422] {
        let deps = env.capture(json!({ "error": "validation" }), status);
        let e = make_backend(no_complete(), "typesafe", &settings(), &deps)
            .unwrap()
            .ask(&json!({}), &questions(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(e.to_string().contains(&format!("HTTP {status}")), "{e}");
        assert_eq!(env.sent().len(), 1);
    }
    let later = (chrono::Utc::now() + chrono::Duration::seconds(60))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let deps = env.deps(Arc::new(move |_| {
        response(503, &[("retry-after", later.clone())], "busy")
    }));
    let e = make_backend(no_complete(), "typesafe", &settings(), &deps)
        .unwrap()
        .ask(&json!({}), &questions(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("retry deferred"), "{e}");
    assert_eq!(env.sent().len(), 1);
}
