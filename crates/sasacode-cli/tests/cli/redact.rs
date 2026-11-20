//! What sasacode hides in its records (`[REDACTED]`) goes beyond the key in use: every configured
//! provider's key, the OpenAI Codex login, the MCP servers' header and environment values, and what
//! is in `~/.sasacode/.env`. The jsonl output and the session log both show them hidden.

use serde_json::{Value, json};

use sasacode_testing::harness::parse_lines;
use sasacode_testing::{FakeServer, Reply};

use crate::support::{Bin, fake_config, provider};

const OTHER_PROVIDER_KEY: &str = "other-provider-key-redact-0123456789";
const DOTENV_VALUE: &str = "dotenv-only-value-redact-0123456789";
const CODEX_ACCESS: &str = "codex-access-token-redact-0123456789";
const CODEX_REFRESH: &str = "codex-refresh-token-redact-0123456789";
const MCP_TOKEN: &str = "mcp-header-token-redact-0123456789";
const MCP_ENV: &str = "mcp-env-literal-redact-0123456789";
/// Seven characters: too short to hide, so ordinary text with it stays whole.
const SHORT: &str = "tok-7ch";

fn session_logs(home: &std::path::Path) -> String {
    let mut all = String::new();
    for dir in std::fs::read_dir(home.join("sessions")).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            all.push_str(&std::fs::read_to_string(f.path()).unwrap_or_default());
        }
    }
    all
}

#[test]
fn secrets_other_than_the_key_in_use_are_hidden_in_the_jsonl_output_and_in_the_session_log() {
    let said = [
        OTHER_PROVIDER_KEY,
        DOTENV_VALUE,
        CODEX_ACCESS,
        CODEX_REFRESH,
        MCP_TOKEN,
        MCP_ENV,
        SHORT,
    ]
    .join(" | ");
    let server = FakeServer::start(vec![Reply::text(format!("echo {said}"))]).expect("the fake server");
    let mut providers = fake_config(server.url(), json!({}))["providers"].clone();
    providers["second"] = provider("openai-chat", server.url());
    providers["second"]["apiKeyEnv"] = json!("SASACODE_TEST_SECOND_KEY");
    let config = fake_config(
        server.url(),
        json!({
            "providers": providers,
            // Never started: hiding what it would be given does not depend on it running.
            "mcpServers": {
                "docs": {
                    "url": "http://127.0.0.1:9/mcp",
                    "headers": { "Authorization": "Bearer ${SASACODE_TEST_MCP_TOKEN}", "X-Short": SHORT },
                    "disabled": true,
                },
                "tool": { "command": "none", "env": { "LITERAL": MCP_ENV }, "disabled": true },
            },
        }),
    );
    let bin = Bin::new(&config);
    std::fs::write(
        bin.sb.home.join(".env"),
        format!("SASACODE_TEST_DOTENV={DOTENV_VALUE}\n"),
    )
    .unwrap();
    std::fs::write(
        bin.sb.home.join("codex-auth.json"),
        json!({ "access": CODEX_ACCESS, "refresh": CODEX_REFRESH }).to_string(),
    )
    .unwrap();

    let r = bin.run_with(
        ["-p", "say it", "--output", "jsonl"],
        None,
        &[
            ("SASACODE_TEST_SECOND_KEY", OTHER_PROVIDER_KEY),
            ("SASACODE_TEST_MCP_TOKEN", MCP_TOKEN),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let events: Vec<Value> = parse_lines(&r.stdout);
    let answer = events
        .iter()
        .rev()
        .find(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant")
        .expect("the answer")
        .to_string();
    let log = session_logs(&bin.sb.home);
    for (where_, text) in [("jsonl", &r.stdout), ("session log", &log)] {
        for secret in [
            OTHER_PROVIDER_KEY,
            DOTENV_VALUE,
            CODEX_ACCESS,
            CODEX_REFRESH,
            MCP_TOKEN,
            MCP_ENV,
        ] {
            assert!(!text.contains(secret), "{secret} is in the {where_}:\n{text}");
        }
        assert!(text.contains("[REDACTED]"), "{where_}");
        assert!(text.contains(SHORT), "a short value is ordinary text: {where_}");
    }
    assert!(answer.contains("[REDACTED] | [REDACTED]"), "{answer}");
}
