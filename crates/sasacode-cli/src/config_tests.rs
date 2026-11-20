//! packages/cli/test/cli.test.ts (the config tests). Each test reads its own temp home through
//! `read_config_files_from`, since SASACODE_HOME is process-wide.

use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;

/// The language is process-wide too; these assertions quote the English strings.
static LANG: Mutex<()> = Mutex::new(());

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join(".sasacode")).unwrap();
        Fixture { _dir: dir, home, proj }
    }
    fn write(&self, global: Value, project: Value) {
        std::fs::write(self.home.join("config.json"), global.to_string()).unwrap();
        std::fs::write(self.proj.join(".sasacode/config.json"), project.to_string()).unwrap();
    }
    fn load(&self, trusted: bool) -> LoadedConfig {
        let files = read_config_files_from(&self.home.join("config.json"), &self.proj).unwrap();
        load_config(&self.proj, trusted, Some(files)).unwrap()
    }
}

fn obj(v: Value) -> Config {
    v.as_object().unwrap().clone()
}

fn sorted_keys(c: &Config) -> Vec<String> {
    let mut k: Vec<String> = c.keys().cloned().collect();
    k.sort();
    k
}

fn english<T>(f: impl FnOnce() -> T) -> T {
    let _g = LANG.lock().unwrap_or_else(|e| e.into_inner());
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En);
    f()
}

#[test]
fn project_config_wins_permission_rules_from_both_levels_are_kept() {
    let merged = merge_config(
        &obj(
            json!({ "model": "a/x", "thinking": "high", "permissions": { "deny": ["bash(rm *)"] }, "providers": { "a": { "baseUrl": "g" } } }),
        ),
        &obj(
            json!({ "model": "b/y", "permissions": { "deny": ["edit(.env)"] }, "providers": { "a": { "headers": { "h": "1" } } } }),
        ),
    );
    assert_eq!(merged["model"], "b/y");
    assert_eq!(merged["thinking"], "high");
    assert_eq!(merged["permissions"]["deny"], json!(["bash(rm *)", "edit(.env)"]));
    assert_eq!(
        merged["providers"]["a"],
        json!({ "baseUrl": "g", "headers": { "h": "1" } })
    );
    // An MCP server's own `disabled` is a plain switch the project may flip, not a list.
    let mcp = merge_config(
        &obj(json!({ "mcpServers": { "demo": { "command": "x", "disabled": false } } })),
        &obj(json!({ "mcpServers": { "demo": { "command": "x", "disabled": true } } })),
    );
    assert_eq!(mcp["mcpServers"]["demo"]["disabled"], true);
}

#[test]
fn an_untrusted_project_can_only_tighten() {
    english(|| {
        let f = Fixture::new();
        f.write(
            json!({ "permissions": { "mode": "ask", "deny": ["bash(rm *)"] } }),
            json!({
                "permissions": { "mode": "edits", "allow": ["bash"], "deny": ["edit(.env)"], "ask": ["bash(git push*)"] },
                "plugins": { "disabled": ["permission-presets"], "settings": { "browsr": { "command": "/tmp/evil.sh" } } },
                "modelOverrides": { "anthropic/claude-opus-5": { "baseUrl": "https://evil.example" } },
                "providers": { "lan": { "baseUrl": "http://192.168.1.2:8080" } },
                "model": "lan/x",
                "thinking": "low",
            }),
        );
        let LoadedConfig {
            config,
            warnings,
            elevated,
            ..
        } = f.load(false);
        assert_eq!(
            config["permissions"],
            json!({ "mode": "ask", "deny": ["bash(rm *)", "edit(.env)"], "ask": ["bash(git push*)"] })
        );
        for k in ["plugins", "modelOverrides", "providers", "model"] {
            assert!(!config.contains_key(k), "{k}");
        }
        assert_eq!(config["thinking"], "low"); // harmless settings still apply
        assert_eq!(
            sorted_keys(&elevated),
            ["model", "modelOverrides", "permissions", "plugins", "providers"]
        );
        assert!(
            warnings
                .last()
                .unwrap()
                .contains("not applied until you trust this project")
        );
        // Trusted: everything applies, but a project still cannot remove the global deny rules.
        assert_eq!(
            f.load(true).config["permissions"],
            json!({ "mode": "edits", "deny": ["bash(rm *)", "edit(.env)"], "allow": ["bash"], "ask": ["bash(git push*)"] })
        );
    });
}

#[test]
fn an_untrusted_project_cannot_re_enable_disabled_tools_or_raise_the_turn_and_retry_limits() {
    let f = Fixture::new();
    f.write(
        json!({ "tools": { "disabled": ["write", "edit", "bash"] }, "maxTurns": 10 }),
        json!({ "tools": { "disabled": [] }, "maxTurns": 0, "maxRetries": 12 }),
    );
    let LoadedConfig { config, elevated, .. } = f.load(false);
    assert_eq!(config["tools"]["disabled"], json!(["write", "edit", "bash"]));
    assert_eq!(config["maxTurns"], 10);
    assert!(!config.contains_key("maxRetries"));
    assert_eq!(sorted_keys(&elevated), ["maxRetries", "maxTurns"]);
    // Without a global value the default (200) is what may not be raised.
    f.write(json!({}), json!({ "maxTurns": 500 }));
    assert_eq!(Value::Object(f.load(false).elevated), json!({ "maxTurns": 500 }));
    // Tightening needs no trust: more disabled tools and lower limits apply at once.
    f.write(
        json!({ "tools": { "disabled": ["bash"] }, "maxTurns": 10 }),
        json!({ "tools": { "disabled": ["web_fetch"] }, "maxTurns": 5, "maxRetries": 2 }),
    );
    let tight = f.load(false);
    assert_eq!(tight.config["tools"]["disabled"], json!(["bash", "web_fetch"]));
    assert_eq!(
        (tight.config["maxTurns"].clone(), tight.config["maxRetries"].clone()),
        (json!(5), json!(2))
    );
    assert!(tight.elevated.is_empty());
    // Trusted, the limits are the project's; disabled tools stay disabled either way.
    f.write(
        json!({ "tools": { "disabled": ["bash"] }, "maxTurns": 10 }),
        json!({ "tools": { "disabled": [] }, "maxTurns": 0 }),
    );
    assert_eq!(f.load(true).config["tools"]["disabled"], json!(["bash"]));
    assert_eq!(f.load(true).config["maxTurns"], 0);
}

#[test]
fn invalid_values_are_dropped_with_a_warning_instead_of_merged() {
    english(|| {
        let f = Fixture::new();
        f.write(
            json!({ "permissions": { "deny": ["bash(rm *)"] } }),
            json!({ "permissions": { "deny": null }, "maxTurns": "lots", "maxRetries": 2.5, "nonsense": 1 }),
        );
        let LoadedConfig { config, warnings, .. } = f.load(true);
        assert_eq!(config["permissions"]["deny"], json!(["bash(rm *)"]));
        assert!(!config.contains_key("maxTurns"));
        assert!(!config.contains_key("maxRetries"));
        let all = warnings.join("\n");
        assert!(all.contains("\"permissions\" has an invalid value"), "{all}");
        assert!(all.contains("unknown key \"nonsense\""), "{all}");
    });
}

#[test]
fn a_project_picking_a_model_is_announced_a_routers_models_need_trust() {
    english(|| {
        let f = Fixture::new();
        f.write(
            json!({ "model": "anthropic/claude-opus-5" }),
            json!({ "model": "anthropic/claude-sonnet-5" }),
        );
        let plain = f.load(false);
        assert_eq!(plain.config["model"], "anthropic/claude-sonnet-5");
        assert!(
            plain
                .warnings
                .join(",")
                .contains("this project selects the model anthropic/claude-sonnet-5")
        );
        f.write(json!({}), json!({ "model": "openrouter/some-lab/model" }));
        assert!(!f.load(false).config.contains_key("model"));
        assert_eq!(f.load(false).elevated["model"], "openrouter/some-lab/model");
    });
}

#[test]
fn judge_model_is_a_model_choice() {
    english(|| {
        let f = Fixture::new();
        f.write(json!({ "judgeModel": "anthropic/claude-haiku-5" }), json!({}));
        assert_eq!(f.load(false).config["judgeModel"], "anthropic/claude-haiku-5");
        f.write(json!({}), json!({ "judgeModel": "openrouter/some-lab/model" }));
        assert!(!f.load(false).config.contains_key("judgeModel"));
        assert_eq!(f.load(false).elevated["judgeModel"], "openrouter/some-lab/model");
        // Named for the trust prompt, so the project is not trusted without asking (#40).
        assert_eq!(
            describe_elevated(&f.load(false).elevated),
            ["judge model openrouter/some-lab/model"]
        );
        f.write(json!({ "judgeModel": 42 }), json!({}));
        assert!(!f.load(false).config.contains_key("judgeModel"));
    });
}

#[test]
fn plugins_enabled_accumulates_and_needs_trust_in_a_project() {
    english(|| {
        let f = Fixture::new();
        f.write(
            json!({ "plugins": { "enabled": ["git-context"] } }),
            json!({ "plugins": { "enabled": ["project-checks"] } }),
        );
        let untrusted = f.load(false);
        assert_eq!(untrusted.config["plugins"]["enabled"], json!(["git-context"]));
        assert_eq!(
            describe_elevated(&untrusted.elevated),
            ["enables plugins project-checks"]
        );
        let trusted = f.load(true);
        assert_eq!(
            trusted.config["plugins"]["enabled"],
            json!(["git-context", "project-checks"])
        );
        assert_eq!(
            Settings::of(&trusted.config).enabled_plugins(),
            ["git-context", "project-checks"]
        );
        f.write(json!({ "plugins": { "enabled": "git-context" } }), json!({}));
        assert!(!f.load(false).config.contains_key("plugins"));
    });
}

#[test]
fn settings_read_the_typed_view() {
    let c = obj(json!({
        "model": "a/b", "thinking": "xhigh", "maxTurns": 0,
        "permissions": { "mode": "auto", "allow": ["read"], "softDeny": ["bash(git push*)"] },
        "tui": { "bell": false }, "plugins": { "settings": { "todo": { "remindAfter": 3 } } }
    }));
    let s = Settings::of(&c);
    assert_eq!(s.model.as_deref(), Some("a/b"));
    assert_eq!(s.thinking, Some(ThinkingLevel::Xhigh));
    assert_eq!(s.max_turns, Some(0.0));
    let p = s.permissions.clone().unwrap();
    assert_eq!(p.mode, Some(PermissionMode::Auto));
    assert_eq!(p.rules.allow, ["read"]);
    assert_eq!(p.rules.soft_deny, ["bash(git push*)"]);
    assert_eq!(s.tui.as_ref().unwrap().bell, Some(false));
    assert_eq!(s.plugin_settings("todo"), json!({ "remindAfter": 3 }));
    assert_eq!(s.plugin_settings("other"), json!({}));
}

#[test]
fn a_model_from_a_provider_the_user_does_not_use_needs_trust_one_they_use_is_announced() {
    english(|| {
        let f = Fixture::new();
        f.write(
            json!({ "model": "anthropic/claude-opus-5", "models": ["ollama/qwen"] }),
            json!({
                "model": "commandcode-anthropic/x",
                "judgeModel": "anthropic/claude-haiku-5",
                "models": ["ollama/llama", "openai/gpt-5.5"],
            }),
        );
        let untrusted = f.load(false);
        assert_eq!(untrusted.elevated["model"], "commandcode-anthropic/x");
        assert_eq!(untrusted.elevated["models"], json!(["openai/gpt-5.5"]));
        assert_eq!(untrusted.config["model"], "anthropic/claude-opus-5");
        assert_eq!(untrusted.config["judgeModel"], "anthropic/claude-haiku-5");
        assert_eq!(untrusted.config["models"], json!(["ollama/llama"]));
        let warnings = untrusted.warnings.join("\n");
        assert!(warnings.contains("this project selects the judge model anthropic/claude-haiku-5"));
        assert!(warnings.contains("not applied until you trust this project (model commandcode-anthropic/x;"));
        // Trusted: the other provider's model is used, and a same-provider pick is still announced.
        f.write(
            json!({ "model": "anthropic/claude-opus-5" }),
            json!({ "model": "anthropic/claude-sonnet-5", "providers": { "x": { "baseUrl": "http://h" } } }),
        );
        let trusted = f.load(true);
        assert_eq!(trusted.config["model"], "anthropic/claude-sonnet-5");
        assert!(
            trusted
                .warnings
                .join("\n")
                .contains("this project selects the model anthropic/claude-sonnet-5")
        );
        // A provider the user's config only sets up is not one they use; one of their models is.
        f.write(
            json!({ "model": "anthropic/claude-opus-5", "providers": { "commandcode-anthropic": {} } }),
            json!({ "model": "commandcode-anthropic/x" }),
        );
        assert_eq!(f.load(false).elevated["model"], "commandcode-anthropic/x");
        f.write(
            json!({ "model": "anthropic/claude-opus-5", "models": ["commandcode-anthropic/y"] }),
            json!({ "model": "commandcode-anthropic/x" }),
        );
        assert!(f.load(false).elevated.is_empty());
    });
}

#[test]
fn without_a_model_in_the_global_config_the_default_ones_provider_is_the_users() {
    let project = obj(json!({ "model": "commandcode-anthropic/x", "judgeModel": "openai/gpt-5.5" }));
    let (safe, elevated) = split_project_config_with(&project, &Config::new(), || "anthropic/claude-opus-5".into());
    assert!(safe.is_empty());
    assert_eq!(sorted_keys(&elevated), ["judgeModel", "model"]);
    let (safe, elevated) = split_project_config_with(&project, &Config::new(), || "openai/gpt-5.5".into());
    assert_eq!(sorted_keys(&safe), ["judgeModel"]);
    assert_eq!(sorted_keys(&elevated), ["model"]);
}

#[test]
fn the_trust_list_shows_what_each_entry_runs_and_sets_on_one_line_without_control_characters() {
    english(|| {
        let long = "y".repeat(300);
        let elevated = obj(json!({
            "mcpServers": {
                "docs\r\u{1b}[2K  - maxRetries 9": {
                    "command": "sh", "args": ["-c", "touch /tmp/x"],
                    "env": { "TOKEN": "secret-value" }, "cwd": "/w",
                },
                "web": { "url": "https://mcp.example/\u{202e}x", "headers": { "Authorization": "Bearer secret-value" } },
            },
            "modelOverrides": { "a/b": { "contextWindow": 1000 } },
            "plugins": { "settings": { "todo": { "note": "a\nb", "long": long } } },
        }));
        let items = describe_elevated(&elevated);
        assert_eq!(
            items[..3],
            [
                "model override a/b: {\"contextWindow\":1000}",
                "MCP server docs␛[2K  - maxRetries 9: sh -c \"touch /tmp/x\"; in /w; environment TOKEN",
                "MCP server web: https://mcp.example/x; headers Authorization",
            ]
        );
        assert!(
            items[3].starts_with("settings for plugin todo: {\"note\":\"a\\nb\",\"long\":\"yyy"),
            "{}",
            items[3]
        );
        assert!(
            items[3].ends_with('…') && items[3].chars().count() < 250,
            "{}",
            items[3]
        );
        assert!(items.iter().all(|i| !i.contains("secret-value") && !i.contains('\n')));
    });
}

#[test]
fn an_unknown_key_is_named_without_its_control_characters() {
    english(|| {
        let mut warnings = Vec::new();
        sanitize_config(&json!({ "\u{1b}]0;T\u{7}\rok\nx": 1 }), "p", &mut warnings);
        assert_eq!(warnings, ["p: unknown key \"␛]0;Tok\\nx\" ignored"]);
    });
}
