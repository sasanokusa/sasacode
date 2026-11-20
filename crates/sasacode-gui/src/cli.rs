//! What a run here could use, asked of the CLI without starting one (`sasacode list … --json`,
//! `plugin list --json`, `trust --json`): the models, the slash commands and skills, the plugins,
//! and whether the folder is trusted. An older CLI that knows none of these says nothing here.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use iced::Task;
use serde::Deserialize;

use crate::app::{App, Msg};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// `provider/model`, as `-m` takes it.
    pub id: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub argument_hint: Option<String>,
    #[serde(default)]
    pub owner: String,
    /// Runs with `--slash`; the TUI's own commands do not.
    #[serde(default = "yes")]
    pub headless: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Plugin {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// `bundled`, `global` or `project`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trust {
    pub trusted: bool,
    /// Whether the project's config adds anything that needs trust.
    pub needed: bool,
    /// False when the plugins' code could not all be read: a saved decision does not hold, and
    /// each run must be told (`--trust-project`).
    pub complete: bool,
    /// One line per thing trust enables.
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// What each MCP server the folder adds starts, or connects to. These and the rest below are
    /// said by a CLI from 0.20.1; an older one says none of them.
    #[serde(default)]
    pub mcp_server_details: Vec<TrustServer>,
    /// The model, judge model and models the folder picks from providers not in use.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// As the folder's config writes them.
    #[serde(default)]
    pub model_overrides: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub plugin_settings: serde_json::Map<String, serde_json::Value>,
}

/// An MCP server a folder's config adds: what it starts, and the names (never the values) of what
/// it is given.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct TrustServer {
    pub name: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<String>,
    pub headers: Vec<String>,
}

/// How long a setting's value is shown before it is cut; a command, an address or a model's name
/// never is.
const VALUE_MAX: usize = 160;

/// A word of a command line as a shell would need it written: quoted when it holds a space, a
/// quote or nothing.
fn shell_word(word: &str) -> String {
    let word = crate::plain::plain_line(word);
    if !word.is_empty() && !word.contains(|c: char| c.is_whitespace() || "'\"\\$`".contains(c)) {
        return word;
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// `value` under `path`, one line per string, number or flag in it: a string named as a command, an
/// address, a model or a path in full, any other cut when long.
fn leaves(path: &str, value: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    let whole = ["command", "cmd", "url", "model", "path", "program", "args", "bin"];
    match value {
        Value::Object(all) if !all.is_empty() => {
            for (k, v) in all {
                leaves(&format!("{path}.{}", crate::plain::plain_line(k)), v, out);
            }
        }
        Value::String(s) => {
            let key = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
            let shown = if whole.iter().any(|w| key.contains(w)) {
                crate::plain::plain_line(s)
            } else {
                crate::plain::plain_clipped(s, VALUE_MAX)
            };
            out.push(format!("{path} = {shown}"));
        }
        Value::Array(all) if all.iter().all(|v| v.is_string()) => {
            let words: Vec<String> = all.iter().filter_map(|v| v.as_str()).map(shell_word).collect();
            out.push(format!("{path} = [{}]", words.join(", ")));
        }
        other => out.push(format!(
            "{path} = {}",
            crate::plain::plain_clipped(&other.to_string(), VALUE_MAX)
        )),
    }
}

impl Trust {
    /// What trusting the folder turns on, beyond its items: one line each, with no control
    /// character and no character that would reorder it. A line under another starts with spaces.
    pub fn details(&self, w: &crate::words::Words) -> Vec<String> {
        use crate::plain::plain_line;
        let names = |all: &[String]| all.iter().map(|s| plain_line(s)).collect::<Vec<_>>().join(", ");
        let mut out = Vec::new();
        for s in &self.mcp_server_details {
            out.push(crate::i18n::fill(w.trust_mcp, "name", plain_line(&s.name)));
            if let Some(command) = &s.command {
                let line: Vec<String> = std::iter::once(command).chain(&s.args).map(|a| shell_word(a)).collect();
                out.push(format!("  {}{}", w.trust_runs, line.join(" ")));
            }
            if let Some(url) = &s.url {
                out.push(format!("  {}{}", w.trust_url, plain_line(url)));
            }
            if let Some(cwd) = &s.cwd {
                out.push(format!("  {}{}", w.trust_cwd, plain_line(cwd)));
            }
            if !s.env.is_empty() {
                out.push(format!("  {}{}", w.trust_env, names(&s.env)));
            }
            if !s.headers.is_empty() {
                out.push(format!("  {}{}", w.trust_headers, names(&s.headers)));
            }
        }
        if let Some(model) = &self.model {
            out.push(format!("{}{}", w.trust_model, plain_line(model)));
        }
        if let Some(judge) = &self.judge_model {
            out.push(format!("{}{}", w.trust_judge, plain_line(judge)));
        }
        if !self.models.is_empty() {
            out.push(format!("{}{}", w.trust_models, names(&self.models)));
        }
        for (head, all) in [
            (w.trust_overrides, &self.model_overrides),
            (w.trust_plugin_settings, &self.plugin_settings),
        ] {
            let mut lines = Vec::new();
            for (k, v) in all {
                leaves(&plain_line(k), v, &mut lines);
            }
            if !lines.is_empty() {
                out.push(head.trim_end().to_owned());
                out.extend(lines.into_iter().map(|l| format!("  {l}")));
            }
        }
        out
    }
}

/// What was learnt; each part is there once its answer came.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub models: Option<Vec<Model>>,
    pub commands: Option<Vec<Command>>,
    pub skills: Option<Vec<Skill>>,
    pub plugins: Option<Vec<Plugin>>,
    pub trust: Option<Trust>,
    /// `sasacode --version`.
    pub version: Option<String>,
}

impl Listing {
    /// What `other` learnt, over what was known.
    pub fn merge(&mut self, other: Listing) {
        // A CLI that did not answer lists none: that does not undo a list already had.
        if let Some(models) = other.models
            && (!models.is_empty() || self.models.is_none())
        {
            self.models = Some(models);
        }
        if other.commands.is_some() {
            self.commands = other.commands;
        }
        if other.skills.is_some() {
            self.skills = other.skills;
        }
        if other.plugins.is_some() {
            self.plugins = other.plugins;
        }
        if other.trust.is_some() {
            self.trust = other.trust;
        }
        if other.version.is_some() {
            self.version = other.version;
        }
    }

    /// Whether the folder has something that is not used until it is trusted.
    pub fn untrusted(&self) -> Option<&Trust> {
        self.trust.as_ref().filter(|t| t.needed && !t.trusted)
    }
}

/// What the CLI wrote to stdout for `args`, run in `cwd`; its stderr when it failed.
pub async fn ask(
    program: PathBuf,
    cwd: PathBuf,
    args: Vec<&'static str>,
    lang: Option<String>,
) -> Result<String, String> {
    let mut c = tokio::process::Command::new(&program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = c
        .args(&args)
        .envs(lang.iter().map(|l| ("SASACODE_LANG", l)))
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Parsed, or nothing: a CLI that does not know the command is not an error worth a word.
fn parsed<T: serde::de::DeserializeOwned>(answer: Result<String, String>) -> Option<T> {
    serde_json::from_str(answer.ok()?.trim()).ok()
}

/// Asks for all of it. The models come on their own: listing them may wait on servers.
pub fn listing(program: &Path, cwd: &Path, lang: Option<String>) -> Task<Msg> {
    let quick = {
        let (program, cwd, lang) = (program.to_owned(), cwd.to_owned(), lang.clone());
        async move {
            let (commands, skills, plugins, trust, version) = tokio::join!(
                ask(
                    program.clone(),
                    cwd.clone(),
                    vec!["list", "commands", "--json"],
                    lang.clone()
                ),
                ask(
                    program.clone(),
                    cwd.clone(),
                    vec!["list", "skills", "--json"],
                    lang.clone()
                ),
                ask(
                    program.clone(),
                    cwd.clone(),
                    vec!["plugin", "list", "--json"],
                    lang.clone()
                ),
                ask(program.clone(), cwd.clone(), vec!["trust", "--json"], lang.clone()),
                ask(program, cwd, vec!["--version"], lang),
            );
            Listing {
                commands: parsed(commands),
                skills: parsed(skills),
                plugins: parsed(plugins),
                trust: parsed(trust),
                version: version
                    .ok()
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty() && !v.contains('\n')),
                ..Listing::default()
            }
        }
    };
    let models = {
        let (program, cwd) = (program.to_owned(), cwd.to_owned());
        async move {
            Listing {
                // Asked and not answered is none listed: the models picker stops waiting for them.
                models: Some(
                    parsed(ask(program, cwd, vec!["list", "models", "--json"], lang).await).unwrap_or_default(),
                ),
                ..Listing::default()
            }
        }
    };
    Task::batch([
        Task::perform(quick, |l| Msg::Listed(Box::new(l))),
        Task::perform(models, |l| Msg::Listed(Box::new(l))),
    ])
}

impl App {
    /// Asks the CLI again what a run in this folder can use.
    pub(crate) fn relist_cli(&mut self) -> Task<Msg> {
        listing(&self.program, &self.cwd, self.prefs.more.lang.clone())
    }

    /// The slash commands a message can be: the plugins' as the newest run listed them, then
    /// the skills' and the others the CLI listed, each once; never the TUI's own.
    pub(crate) fn slash_commands(&self) -> Vec<Command> {
        let mut all: Vec<Command> = self
            .plugin_commands
            .iter()
            .map(|c| Command {
                name: c.name.clone(),
                description: c.description.clone(),
                argument_hint: c.argument_hint.clone(),
                owner: c.owner.clone(),
                headless: true,
            })
            .collect();
        for c in self.listing.commands.iter().flatten() {
            if c.headless && !all.iter().any(|a| a.name == c.name) {
                all.push(c.clone());
            }
        }
        all
    }

    /// The listing's messages; any other comes back.
    pub(crate) fn update_cli(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::Listed(listing) => {
                self.listing.merge(*listing);
                Task::none()
            }
            Msg::Relist => self.relist_cli(),
            Msg::TrustFolder => {
                let (program, cwd, lang) = (self.program.clone(), self.cwd.clone(), self.prefs.more.lang.clone());
                Task::perform(
                    async move { ask(program, cwd, vec!["trust", "--yes", "--json"], lang).await },
                    Msg::Trusted,
                )
            }
            Msg::Trusted(answer) => match answer {
                Ok(text) => {
                    if let Ok(trust) = serde_json::from_str::<Trust>(text.trim()) {
                        self.listing.trust = Some(trust);
                    }
                    // Its plugins, commands and skills are used from now on, and git is run here.
                    Task::batch([self.relist_cli(), self.know_git_here()])
                }
                Err(error) => {
                    self.toast = Some(error);
                    Task::none()
                }
            },
            Msg::TrustOnce => {
                let before = self.git_allowed();
                self.trust_once = true;
                self.git_now_allowed(before)
            }
            other => return self.update_nav(other),
        };
        Ok(task)
    }
}

/// The share of the context window at which the compaction plugin summarises the conversation,
/// as the global config and then the folder's set it (0.8 unless set); none when it is turned off.
pub fn compaction_point(home: Option<&Path>, cwd: &Path) -> Option<f64> {
    let read =
        |p: PathBuf| -> Option<serde_json::Value> { serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok() };
    let configs = [
        home.map(|h| h.join("config.json")),
        Some(cwd.join(".sasacode").join("config.json")),
    ];
    let mut point = 0.8;
    for config in configs.into_iter().flatten().filter_map(read) {
        let off = config
            .pointer("/plugins/disabled")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|d| d.iter().any(|x| x == "compaction"));
        if off {
            return None;
        }
        if let Some(at) = config
            .pointer("/plugins/settings/compaction/threshold")
            .and_then(serde_json::Value::as_f64)
        {
            point = at;
        }
    }
    Some(point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_cli_lists_is_read_and_merged() {
        let models: Vec<Model> = serde_json::from_str(
            r#"[{"id":"a/m","contextWindow":128000,"current":true},{"id":"b/n","server":"ollama"}]"#,
        )
        .unwrap();
        assert!(models[0].current && models[0].context_window == Some(128000) && !models[1].current);
        let commands: Vec<Command> = serde_json::from_str(
            r#"[{"name":"help","description":"h","owner":"core-commands","headless":false},{"name":"review","description":"r","argumentHint":"[path]","owner":"skills","headless":true}]"#,
        )
        .unwrap();
        assert_eq!(commands[1].argument_hint.as_deref(), Some("[path]"));
        let trust: Trust = serde_json::from_str(
            r#"{"cwd":"/p","trusted":false,"needed":true,"complete":true,"items":["plugin x"],"plugins":["x"],"mcpServers":["db"],"endpoints":[],"allow":[],"mode":null}"#,
        )
        .unwrap();
        let mut listing = Listing {
            trust: Some(trust),
            ..Listing::default()
        };
        listing.merge(Listing {
            models: Some(models),
            ..Listing::default()
        });
        assert!(listing.models.is_some() && listing.untrusted().is_some());
        // Asked again and not answered, the models already listed stay.
        listing.merge(Listing {
            models: Some(Vec::new()),
            ..Listing::default()
        });
        assert!(!listing.models.as_ref().unwrap().is_empty());
        assert_eq!(listing.untrusted().unwrap().mcp_servers, ["db"]);
    }

    #[test]
    fn the_compaction_point_is_read_from_the_configs_the_folders_last() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-compact-{}", std::process::id()));
        let (home, cwd) = (dir.join("home"), dir.join("proj"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(cwd.join(".sasacode")).unwrap();
        assert_eq!(compaction_point(Some(&home), &cwd), Some(0.8));
        let settings = r#"{"plugins":{"settings":{"compaction":{"threshold":0.6}}}}"#;
        std::fs::write(home.join("config.json"), settings).unwrap();
        assert_eq!(compaction_point(Some(&home), &cwd), Some(0.6));
        std::fs::write(
            cwd.join(".sasacode").join("config.json"),
            r#"{"plugins":{"disabled":["compaction"]}}"#,
        )
        .unwrap();
        assert_eq!(compaction_point(Some(&home), &cwd), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_trust_card_says_what_each_item_starts_and_is_given_with_no_control_character() {
        let trust: Trust = serde_json::from_str(
            r#"{"trusted":false,"needed":true,"complete":true,"items":["MCP server db"],"mcpServers":["db"],
                "mcpServerDetails":[{"name":"d\u202eb","command":"npx","args":["-y","db-server","/a b"],"url":null,
                  "cwd":"/srv","env":["DB_TOKEN"],"headers":[]},
                  {"name":"api","command":null,"args":[],"url":"https://example.invalid/mcp","cwd":null,"env":[],"headers":["Authorization"]}],
                "model":"far/away-model-with-a-long-name","judgeModel":null,"models":["far/a","far/b"],
                "modelOverrides":{"far/a":{"contextWindow":1000}},
                "pluginSettings":{"syntax-check":{"externalCheck":"/opt/check --all","note":"x\u001b[2Ky"}}}"#,
        )
        .unwrap();
        let lines = trust.details(&crate::words::JA);
        assert_eq!(
            lines,
            [
                "MCPサーバー「db」",
                "  起動するもの：npx -y db-server '/a b'",
                "  作業フォルダ：/srv",
                "  環境変数（名前だけ）：DB_TOKEN",
                "MCPサーバー「api」",
                "  接続先：https://example.invalid/mcp",
                "  ヘッダー（名前だけ）：Authorization",
                "モデル：far/away-model-with-a-long-name",
                "モデルの一覧：far/a, far/b",
                "modelOverrides：",
                "  far/a.contextWindow = 1000",
                "プラグインの設定：",
                "  syntax-check.externalCheck = /opt/check --all",
                "  syntax-check.note = x␛[2Ky",
            ]
        );
        // A long value is cut, but never a command, an address or a model's name.
        let long = "z".repeat(400);
        let mut out = Vec::new();
        leaves("p", &serde_json::json!({ "note": long, "command": long }), &mut out);
        let (note, command) = (&out[0], &out[1]);
        assert!(
            note.chars().count() < 200 && note.ends_with('…') && command.ends_with(&long),
            "{out:?}"
        );
        // What an older CLI says has none of it.
        let old: Trust =
            serde_json::from_str(r#"{"trusted":false,"needed":true,"complete":true,"items":["x"]}"#).unwrap();
        assert!(old.details(&crate::words::EN).is_empty());
    }
}
