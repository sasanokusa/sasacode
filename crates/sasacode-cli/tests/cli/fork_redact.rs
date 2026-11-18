//! packages/cli/test/fork-redact.test.ts. /fork writes the kept conversation into a new log before
//! any prompt: the keys the old log redacted must stay redacted (#103).

use serde_json::json;

use sasacode_cli::setup::{SetupOptions, setup};

use crate::support::{child_with, provider, sse, sse_answer};
use crate::test_server::{Res, TestServer};

const KEY: &str = "sk-test-0123456789abcdefSECRET";

#[tokio::test(flavor = "multi_thread")]
async fn a_forked_log_redacts_the_key_the_original_log_redacted_103() {
    let Some(t) = child_with(&[("SASACODE_TEST_FORK_KEY", KEY)]) else {
        return;
    };
    let server = TestServer::start(|req| match req.path.as_str() {
        "/v1/models" => {
            Res::json(json!({ "object": "list", "data": [{ "id": "m", "object": "model", "owned_by": "x" }] }))
        }
        "/v1/chat/completions" => sse(sse_answer("m", "ok")),
        _ => Res::text(404, ""),
    });
    let mut fake = provider("openai-chat", &format!("{}/v1", server.url()));
    fake["apiKeyEnv"] = json!("SASACODE_TEST_FORK_KEY");
    t.config(&json!({ "providers": { "fake": fake }, "model": "fake/m", "plugins": { "disabled": ["openai-codex"] } }));
    let h = setup(SetupOptions::new(t.cwd())).await.expect("setup");
    h.agent
        .prompt_text(&format!("why is this rejected? FAKE_PROVIDER_KEY={KEY}"))
        .await
        .expect("the run");
    let original = std::fs::read_to_string(h.agent.session().expect("a session").path()).unwrap();
    assert!(!original.contains(KEY));
    h.fork(h.agent.messages().len()).await.expect("the fork");
    let forked = h.agent.session().expect("the forked session").path().to_path_buf();
    let logs = std::fs::read_dir(forked.parent().unwrap()).unwrap().count();
    assert_eq!(logs, 2);
    let log = std::fs::read_to_string(&forked).unwrap();
    assert!(log.contains("[REDACTED]"), "{log}");
    assert!(!log.contains(KEY));
    h.shutdown().await;
}
