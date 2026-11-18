//! packages/cli/test/listings.test.ts (the home directory is passed in, not set in SASACODE_HOME)

use std::cell::RefCell;
use std::path::PathBuf;

use regex::Regex;
use serde_json::json;

use sasacode_agent::permission::PERMISSION_MODES;
use sasacode_ai::ThinkingLevel;

use super::*;

/// These assertions quote the English strings.
fn english() {
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En);
}

/// sasacode-gui's cli.rs shapes, copied (the GUI is not a dependency): what the GUI reads must
/// parse from what these commands write. Trust has no defaults there, so a missing key drops it.
/// Not every field is read here.
#[allow(dead_code)]
mod gui {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Model {
        pub id: String,
        #[serde(default)]
        pub context_window: Option<u64>,
        #[serde(default)]
        pub current: bool,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Command {
        pub name: String,
        #[serde(default)]
        pub description: String,
        #[serde(default)]
        pub argument_hint: Option<String>,
        #[serde(default)]
        pub owner: String,
        #[serde(default = "yes")]
        pub headless: bool,
    }

    fn yes() -> bool {
        true
    }

    #[derive(Debug, Deserialize)]
    pub struct Skill {
        pub name: String,
        #[serde(default)]
        pub description: String,
        #[serde(default)]
        pub path: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct Plugin {
        pub name: String,
        #[serde(default)]
        pub version: Option<String>,
        #[serde(default)]
        pub source: String,
        #[serde(default)]
        pub enabled: bool,
        #[serde(default)]
        pub error: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Trust {
        pub trusted: bool,
        pub needed: bool,
        pub complete: bool,
        #[serde(default)]
        pub items: Vec<String>,
        #[serde(default)]
        pub mcp_servers: Vec<String>,
    }
}

/// A fresh home and project.
struct Dirs {
    _root: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
}

fn dirs() -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let proj = root.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(proj.join(".sasacode")).unwrap();
    Dirs {
        _root: root,
        home,
        proj,
    }
}

impl Dirs {
    fn global(&self, config: Value) {
        std::fs::write(self.home.join("config.json"), config.to_string()).unwrap();
    }

    fn project(&self, config: Value) {
        std::fs::write(self.proj.join(".sasacode/config.json"), config.to_string()).unwrap();
    }

    fn cwd(&self) -> String {
        self.proj.to_string_lossy().into_owned()
    }

    /// `sasacode trust <args>`: the exit code and what it printed.
    fn trust(&self, args: &[&str]) -> (i32, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let mut out = Vec::new();
        let code = trust_command_in(&args, &self.proj, &self.home, &mut out).unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    fn trust_json(&self, args: &[&str]) -> (i32, Value) {
        let (code, out) = self.trust(args);
        assert!(serde_json::from_str::<gui::Trust>(&out).is_ok(), "{out}");
        (code, serde_json::from_str(&out).unwrap())
    }
}

fn skill(dir: &Path, folder: &str, name: &str, description: &str) {
    std::fs::create_dir_all(dir.join(folder)).unwrap();
    std::fs::write(
        dir.join(folder).join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nDo it.\n"),
    )
    .unwrap();
}

/// A user's skill and a project's.
fn with_skills(d: &Dirs) {
    skill(&d.home.join("skills"), "mine", "mine", "my own skill");
    skill(
        &d.proj.join(".sasacode/skills"),
        "theirs",
        "theirs",
        "the project's skill",
    );
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

// ── TUI commands ────────────────────────────────────────────────────

/// A command's name, description and argument hint.
type Described = (String, String, Option<String>);

/// Each command `core_commands` in sasacode-tui's app.rs returns, in its order. The `vec![…]` it
/// ends with holds `command(h, <name>, <description>, …)` calls and the names of commands made
/// above it, whose argument hints are set there.
fn tui_source_commands() -> Vec<Described> {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../sasacode-tui/src/app.rs")).unwrap();
    let start = src.find("fn core_commands(").expect("core_commands in app.rs");
    let body = &src[start..];
    let body = &body[..body.find("\n}\n").expect("the end of core_commands")];
    let call = r#"command\(\s*h,\s*"([^"]+)",\s*&t\(\s*"([^"]+)""#;
    let defined: Vec<(String, String, String)> = Regex::new(&format!(r"let mut (\w+) = {call}"))
        .unwrap()
        .captures_iter(body)
        .map(|c| (c[1].to_string(), c[2].to_string(), c[3].to_string()))
        .collect();
    // A literal, or the list the TUI joins with `|`.
    let hints: Vec<(String, String)> =
        Regex::new(r#"(\w+)\.argument_hint = Some\(\s*(?:"([^"]*)"|(ThinkingLevel::ALL|PERMISSION_MODES))"#)
            .unwrap()
            .captures_iter(body)
            .map(|c| {
                let hint = match (c.get(2), c.get(3).map(|m| m.as_str())) {
                    (Some(literal), _) => literal.as_str().to_string(),
                    (None, Some("ThinkingLevel::ALL")) => ThinkingLevel::ALL.map(|l| l.as_str()).join("|"),
                    (None, _) => PERMISSION_MODES.map(|m| m.as_str()).join("|"),
                };
                (c[1].to_string(), hint)
            })
            .collect();
    let list = &body[body.rfind("vec![").expect("the vec! of commands")..];
    let mut items: Vec<(usize, Described)> = Regex::new(call)
        .unwrap()
        .captures_iter(list)
        .map(|c| (c.get(0).unwrap().start(), (c[1].to_string(), c[2].to_string(), None)))
        .collect();
    for c in Regex::new(r"(?m)^\s*(\w+),\s*$").unwrap().captures_iter(list) {
        if let Some((var, name, description)) = defined.iter().find(|(v, ..)| *v == c[1]) {
            let hint = hints.iter().find(|(v, _)| v == var).map(|(_, h)| h.clone());
            items.push((c.get(0).unwrap().start(), (name.clone(), description.clone(), hint)));
        }
    }
    items.sort_by_key(|(at, _)| *at);
    items.into_iter().map(|(_, item)| item).collect()
}

#[test]
fn tui_commands_are_the_tuis_own_list_in_its_order() {
    let ours: Vec<Described> = TUI_COMMANDS
        .iter()
        .map(|c| {
            (
                c.name.to_string(),
                c.description.to_string(),
                c.argument_hint.map(str::to_string),
            )
        })
        .collect();
    let theirs = tui_source_commands();
    assert_eq!(theirs.len(), 12, "{theirs:?}");
    assert_eq!(ours, theirs);
}

#[test]
fn every_tui_command_description_has_an_english_form() {
    for c in TUI_COMMANDS {
        let en = sasacode_host::messages_en::TO_EN
            .iter()
            .find(|(ja, _)| *ja == c.description);
        assert!(en.is_some(), "{}", c.name);
    }
}

// ── models ──────────────────────────────────────────────────────────

fn choice(spec: &str, context_window: Option<u64>, max_context: Option<u64>, server: Option<&str>) -> ModelChoice {
    ModelChoice {
        spec: spec.into(),
        context_window,
        max_context,
        server: server.map(str::to_string),
    }
}

#[test]
fn models_are_the_current_one_then_the_configs_then_the_listed_ones_each_once() {
    let listed = vec![
        choice("p/a", Some(200_000), None, None),
        choice("p/b", None, Some(32_768), Some("ollama")),
        choice("p/cur", Some(128_000), None, None),
        // Zero and empty are nothing reported, as falsy values were.
        choice("p/z", Some(0), Some(0), Some("")),
    ];
    let configured = strings(&["x/conf", "p/a", "p/cur"]);
    let models = models_of("p/cur", &configured, Ok(listed), &|w| panic!("warned: {w}"));
    assert_eq!(
        serde_json::to_value(&models).unwrap(),
        json!([
            { "id": "p/cur", "contextWindow": 128_000, "current": true },
            { "id": "x/conf" },
            { "id": "p/a", "contextWindow": 200_000 },
            { "id": "p/b", "maxContext": 32_768, "server": "ollama" },
            { "id": "p/z" },
        ])
    );
    let json = serde_json::to_string(&models).unwrap();
    let parsed: Vec<gui::Model> = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed[0].id, "p/cur");
    assert!(parsed[0].current && !parsed[1].current);
    assert_eq!(parsed[2].context_window, Some(200_000));

    english();
    let mut out = Vec::new();
    print_models(&mut out, &models, false).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "p/cur  128k ctx  current\nx/conf\np/a  200k ctx\np/b  up to 32k\np/z\n"
    );
}

#[test]
fn a_listing_that_fails_is_a_warning_and_the_rest_is_still_listed() {
    let warnings = RefCell::new(Vec::new());
    let models = models_of(
        "p/cur",
        &strings(&["x/conf", "p/cur"]),
        Err(anyhow::anyhow!("offline")),
        &|w| warnings.borrow_mut().push(w),
    );
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["p/cur", "x/conf"]);
    assert_eq!(warnings.into_inner(), ["offline"]);
}

// ── skills and commands ─────────────────────────────────────────────

#[test]
fn skills_are_the_built_in_ones_the_users_and_the_projects() {
    let d = dirs();
    with_skills(&d);
    let skills = skill_listing_in(&d.home, &d.proj).unwrap();
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["tool-authoring", "mine", "theirs"]);
    assert!(d.home.join("builtin-skills/tool-authoring/SKILL.md").is_file());
    assert_eq!(
        serde_json::to_value(&skills[1]).unwrap(),
        json!({
            "name": "mine",
            "description": "my own skill",
            "path": d.home.join("skills/mine/SKILL.md").to_string_lossy(),
        })
    );
    let parsed: Vec<gui::Skill> = serde_json::from_str(&serde_json::to_string(&skills).unwrap()).unwrap();
    assert_eq!(
        parsed[2].path,
        d.proj.join(".sasacode/skills/theirs/SKILL.md").to_string_lossy()
    );
    assert_eq!(parsed[2].description, "the project's skill");
}

#[tokio::test]
async fn commands_are_the_tuis_own_then_the_skills_plugins() {
    english();
    let d = dirs();
    with_skills(&d);
    let commands = command_listing_in(&d.home, &d.proj).await.unwrap();
    assert_eq!(
        serde_json::to_string(&commands[0]).unwrap(),
        r#"{"name":"help","description":"show commands and keys","owner":"core-commands","headless":false}"#
    );
    let tui: Vec<&str> = commands
        .iter()
        .filter(|c| c.owner == "core-commands")
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(tui, TUI_COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>());
    assert!(commands[..TUI_COMMANDS.len()].iter().all(|c| !c.headless));
    let model = commands.iter().find(|c| c.name == "model").unwrap();
    assert_eq!(model.argument_hint.as_deref(), Some("<provider/model> | --refresh"));

    let skills: Vec<&str> = commands[TUI_COMMANDS.len()..].iter().map(|c| c.name.as_str()).collect();
    assert_eq!(skills, ["skill:tool-authoring", "skill:mine", "skill:theirs", "skills"]);
    let mine = commands.iter().find(|c| c.name == "skill:mine").unwrap();
    assert_eq!(
        serde_json::to_value(mine).unwrap(),
        json!({ "name": "skill:mine", "description": "my own skill", "argumentHint": "[task]", "owner": "skills", "headless": true })
    );
    let list = commands.iter().find(|c| c.name == "skills").unwrap();
    assert_eq!(
        serde_json::to_value(list).unwrap(),
        json!({ "name": "skills", "description": "list the available skills", "owner": "skills", "headless": true })
    );

    let parsed: Vec<gui::Command> = serde_json::from_str(&serde_json::to_string(&commands).unwrap()).unwrap();
    assert!(!parsed[0].headless && parsed.last().unwrap().headless);
    assert_eq!(parsed[1].argument_hint.as_deref(), Some("<provider/model> | --refresh"));

    let mut out = Vec::new();
    print_commands(&mut out, &[commands[0].clone(), mine.clone()], false).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "/help  show commands and keys  [core-commands, TUI only]\n\
         /skill:mine [task]  my own skill  [skills]\n\
         (the bundled plugins' commands are listed by a run: the `commands` line of --control stdio)\n"
    );
}

#[tokio::test]
async fn skills_turned_off_leave_no_skills_and_only_the_tuis_commands() {
    english();
    let d = dirs();
    with_skills(&d);
    d.global(json!({ "plugins": { "disabled": ["skills"] } }));
    assert_eq!(skill_listing_in(&d.home, &d.proj).unwrap(), Vec::new());
    let commands = command_listing_in(&d.home, &d.proj).await.unwrap();
    assert_eq!(commands.len(), TUI_COMMANDS.len());
    assert!(commands.iter().all(|c| c.owner == "core-commands"));
}

#[tokio::test]
async fn a_project_turns_skills_off_only_once_it_is_trusted() {
    let d = dirs();
    with_skills(&d);
    d.project(json!({ "plugins": { "disabled": ["skills"] } }));
    // plugins.* needs trust: a run here would still load the skills.
    assert_eq!(skill_listing_in(&d.home, &d.proj).unwrap().len(), 3);
    let project = assess_files(
        &d.proj,
        read_config_files_from(&d.home.join("config.json"), &d.proj).unwrap(),
    );
    save_trust_in(&d.home, &d.cwd(), &project).unwrap();
    assert_eq!(skill_listing_in(&d.home, &d.proj).unwrap(), Vec::new());
    let commands = command_listing_in(&d.home, &d.proj).await.unwrap();
    assert_eq!(commands.len(), TUI_COMMANDS.len());
}

// ── plugins ─────────────────────────────────────────────────────────

fn enabled(plugins: &[ListedPlugin], name: &str) -> bool {
    plugins
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} is not listed"))
        .enabled
}

#[test]
fn plugins_are_the_bundled_ones_enabled_as_a_run_loads_them() {
    let d = dirs();
    let plugins = plugin_listing_in(&d.home, &d.proj).unwrap();
    let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    let mut expected: Vec<&str> = sasacode_bundled::PLUGINS.iter().map(|p| p.name).collect();
    expected.extend(["skills", "mcp"]);
    assert_eq!(names, expected);
    for p in sasacode_bundled::PLUGINS {
        assert_eq!(enabled(&plugins, p.name), p.on_by_default, "{}", p.name);
    }
    assert!(enabled(&plugins, "skills") && enabled(&plugins, "mcp") && enabled(&plugins, "openai-codex"));
    assert_eq!(
        serde_json::to_value(&plugins[0]).unwrap(),
        json!({ "name": "tool-repair", "source": "bundled", "enabled": true })
    );
    let parsed: Vec<gui::Plugin> = serde_json::from_str(&serde_json::to_string(&plugins).unwrap()).unwrap();
    assert!(
        parsed
            .iter()
            .all(|p| p.source == "bundled" && p.version.is_none() && p.error.is_none())
    );

    d.global(json!({
        "plugins": {
            "disabled": ["todo", "mcp", "openai-codex"],
            "enabled": ["git-context", "free-search"],
            "settings": { "jev-guard": { "enabled": true } },
        },
    }));
    let plugins = plugin_listing_in(&d.home, &d.proj).unwrap();
    assert!(!enabled(&plugins, "todo"));
    assert!(!enabled(&plugins, "mcp"));
    assert!(!enabled(&plugins, "openai-codex"));
    // Off by default, on once plugins.enabled names it.
    assert!(enabled(&plugins, "git-context"));
    assert!(!enabled(&plugins, "ask-user"));
    // On by itself while MCP is off.
    assert!(enabled(&plugins, "free-search"));
    // The setting from when it was a separate plugin.
    assert!(enabled(&plugins, "jev-guard"));
    assert!(enabled(&plugins, "skills"));
    assert!(enabled(&plugins, "tool-repair"));
}

#[test]
fn a_project_changes_which_plugins_load_only_once_it_is_trusted() {
    let d = dirs();
    d.project(json!({ "plugins": { "disabled": ["todo"], "enabled": ["workflows"] } }));
    let plugins = plugin_listing_in(&d.home, &d.proj).unwrap();
    assert!(enabled(&plugins, "todo") && !enabled(&plugins, "workflows"));
    d.trust(&["--yes"]);
    let plugins = plugin_listing_in(&d.home, &d.proj).unwrap();
    assert!(!enabled(&plugins, "todo") && enabled(&plugins, "workflows"));
}

// ── trust ───────────────────────────────────────────────────────────

#[test]
fn trust_reports_what_the_project_needs_and_yes_trusts_it_until_it_changes() {
    english();
    let d = dirs();
    let config = |mode: &str| {
        json!({
            "mcpServers": { "fx": { "command": "true" } },
            "providers": {
                "local": { "baseUrl": "http://127.0.0.1:9/v1" },
                "bare": { "api": "openai-chat" },
            },
            "permissions": { "allow": ["bash"], "mode": mode },
        })
    };
    d.project(config("auto"));
    let items = [
        "endpoint local (http://127.0.0.1:9/v1)",
        "endpoint bare (openai-chat)",
        "MCP server fx",
        "allow rules bash",
        "permission mode auto",
    ];
    let (code, status) = d.trust_json(&["--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        status,
        json!({
            "cwd": d.cwd(),
            "trusted": false,
            "needed": true,
            "complete": true,
            "items": items,
            "plugins": [],
            "mcpServers": ["fx"],
            "endpoints": [
                { "name": "local", "url": "http://127.0.0.1:9/v1" },
                { "name": "bare", "url": null },
            ],
            "allow": ["bash"],
            "mode": "auto",
        })
    );
    let keys: Vec<&str> = status.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "cwd",
            "trusted",
            "needed",
            "complete",
            "items",
            "plugins",
            "mcpServers",
            "endpoints",
            "allow",
            "mode"
        ]
    );
    let (_, text) = d.trust(&[]);
    assert_eq!(
        text,
        format!(
            "{} is not trusted\nThis project wants to use:\n{}\ntrust it with: sasacode trust --yes\n",
            d.cwd(),
            items.map(|i| format!("  - {i}")).join("\n")
        )
    );

    let (code, status) = d.trust_json(&["--yes", "--json"]);
    assert_eq!((code, &status["trusted"]), (0, &json!(true)));
    let (code, text) = d.trust(&["-y"]);
    assert_eq!(code, 0);
    assert!(
        text.starts_with(&format!("{} is trusted\nThis project wants to use:\n", d.cwd())),
        "{text}"
    );
    assert!(!text.contains("trust it with"));

    // A change to what the project turns on is not trusted yet.
    d.project(config("agent"));
    let (code, status) = d.trust_json(&["--json"]);
    assert_eq!(
        (code, &status["trusted"], &status["mode"]),
        (0, &json!(false), &json!("agent"))
    );
}

#[test]
fn a_plain_folder_needs_no_trust() {
    english();
    let d = dirs();
    let (code, status) = d.trust_json(&["--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        status,
        json!({
            "cwd": d.cwd(),
            "trusted": true,
            "needed": false,
            "complete": true,
            "items": [],
            "plugins": [],
            "mcpServers": [],
            "endpoints": [],
            "allow": [],
            "mode": null,
        })
    );
    assert_eq!(
        d.trust(&["--yes"]),
        (0, format!("{}: nothing in this project needs trust\n", d.cwd()))
    );
    // --yes had nothing to save.
    assert!(!d.home.join("trust.json").exists());
}

#[test]
fn wrong_arguments_are_a_usage_error() {
    english();
    let d = dirs();
    let mut out = Vec::new();
    let e = trust_command_in(&strings(&["now"]), &d.proj, &d.home, &mut out).unwrap_err();
    assert_eq!(e.to_string(), "usage: sasacode trust [--yes] [--json]");
    assert!(trust_command_in(&strings(&["--force"]), &d.proj, &d.home, &mut out).is_err());

    assert_eq!(list_args(&strings(&["models"])).unwrap(), (What::Models, false));
    assert_eq!(
        list_args(&strings(&["--json", "commands"])).unwrap(),
        (What::Commands, true)
    );
    assert_eq!(
        list_args(&strings(&["skills", "--json"])).unwrap(),
        (What::Skills, true)
    );
    let wrongs: [&[&str]; 3] = [&[], &["plugins"], &["models", "skills"]];
    for wrong in wrongs {
        let e = list_args(&strings(wrong)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "usage: sasacode list models|commands|skills [--json]",
            "{wrong:?}"
        );
    }
    assert!(list_args(&strings(&["models", "--all"])).is_err());
}
