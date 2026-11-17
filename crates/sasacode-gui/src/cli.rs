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
}

/// What was learnt; each part is there once its answer came.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub models: Option<Vec<Model>>,
    pub commands: Option<Vec<Command>>,
    pub skills: Option<Vec<Skill>>,
    pub plugins: Option<Vec<Plugin>>,
    pub trust: Option<Trust>,
}

impl Listing {
    /// What `other` learnt, over what was known.
    pub fn merge(&mut self, other: Listing) {
        if other.models.is_some() {
            self.models = other.models;
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
            let (commands, skills, plugins, trust) = tokio::join!(
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
                ask(program, cwd, vec!["trust", "--json"], lang),
            );
            Listing {
                commands: parsed(commands),
                skills: parsed(skills),
                plugins: parsed(plugins),
                trust: parsed(trust),
                ..Listing::default()
            }
        }
    };
    let models = {
        let (program, cwd) = (program.to_owned(), cwd.to_owned());
        async move {
            Listing {
                models: parsed(ask(program, cwd, vec!["list", "models", "--json"], lang).await),
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
                    // Its plugins, commands and skills are used from now on.
                    self.relist_cli()
                }
                Err(error) => {
                    self.toast = Some(error);
                    Task::none()
                }
            },
            Msg::TrustOnce => {
                self.trust_once = true;
                Task::none()
            }
            other => return Err(other),
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
}
