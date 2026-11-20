//! What a run in a folder would offer, asked without starting one (`packages/cli/src/listings.ts`):
//! `sasacode list models|commands|skills [--json]`, `sasacode trust [--yes] [--json]`, and the
//! plugins `sasacode plugin list --json` reports. The desktop GUI asks these before it starts a run,
//! so the JSON keys are its contract (sasacode-gui's cli.rs).

use std::io::Write;
use std::path::Path;

use indexmap::IndexSet;
use serde::Serialize;
use serde_json::Value;

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{ModelInfo, fmt_tokens};
use sasacode_host::{current_lang, t};
use sasacode_skills::{discover_skills_in, materialize_builtin_skills, project_skills_dir};

use crate::args::{boolean, parse_args};
use crate::catalog::ModelChoice;
use crate::config::{Settings, load_config, read_config_files_from, sasacode_home};
use crate::setup::{PluginChoice, SetupOptions, setup, skill_dirs, skills_options};
use crate::trust::{ProjectTrust, assess_files, is_trusted_in, save_trust_in};

/// One of the TUI's own slash commands (`core_commands` in sasacode-tui's app.rs). They run only in
/// the TUI: a headless run (`--slash`) does not have them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiCommand {
    pub name: &'static str,
    /// As the TUI writes it, in Japanese: the text `t` translates.
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
}

const fn tui_command(name: &'static str, description: &'static str, argument_hint: Option<&'static str>) -> TuiCommand {
    TuiCommand {
        name,
        description,
        argument_hint,
    }
}

/// The TUI's own commands, in its order. A test keeps this in step with app.rs; the effort and
/// permission hints are the thinking levels and the permission modes joined with `|`.
pub const TUI_COMMANDS: &[TuiCommand] = &[
    tui_command("help", "コマンドとキー操作を表示", None),
    tui_command(
        "model",
        "モデルを切り替える（プロバイダーから取得した一覧）",
        Some("<provider/model> | --refresh"),
    ),
    tui_command(
        "effort",
        "推論の深さ（thinking）を切り替える",
        Some("off|low|medium|high|xhigh|max"),
    ),
    tui_command("copy", "直前の応答をクリップボードにコピー", None),
    tui_command("resume", "過去のセッションを再開", None),
    tui_command("clear", "新しいセッションを始める", None),
    tui_command("permission", "権限モードを切り替える", Some("edits|ask|agent|auto")),
    tui_command("fork", "過去のメッセージから会話を分岐する", None),
    tui_command("session", "このセッションの ID と保存先", None),
    tui_command("language", "表示言語を切り替える（日本語 / English）", Some("ja|en")),
    tui_command("exit", "終了する", None),
    tui_command("quit", "終了する（/exit と同じ）", None),
];

/// The owner the TUI registers its own commands under.
const TUI_OWNER: &str = "core-commands";

/// One model `-m` could take. A size or server nobody reported is left out of the JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedModel {
    /// `provider/model`.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// The model's maximum, when the size it is loaded with is not known yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context: Option<u64>,
    /// The local server it runs on ("ollama", "lmstudio" or "llamacpp").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Only on the model a run would start with.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
}

/// One slash command, with who registers it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedCommand {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    pub owner: String,
    /// Whether it runs with `--slash`; the TUI's own commands do not.
    pub headless: bool,
}

/// One skill: a folder with a SKILL.md.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListedSkill {
    pub name: String,
    pub description: String,
    /// The SKILL.md.
    pub path: String,
}

/// One plugin and whether a run here loads it. 0.20 loads only the plugins it ships, so each is
/// `bundled` and none has a version, a folder or a load error to report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListedPlugin {
    pub name: String,
    pub source: &'static str,
    pub enabled: bool,
}

impl ListedPlugin {
    fn bundled(name: &str, enabled: bool) -> Self {
        ListedPlugin {
            name: name.to_string(),
            source: "bundled",
            enabled,
        }
    }
}

/// A server the project's config adds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrustEndpoint {
    pub name: String,
    /// Its `baseUrl`; null when it has none.
    pub url: Option<String>,
}

/// `sasacode trust --json`: whether the folder is trusted and what trusting it turns on.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStatus {
    pub cwd: String,
    pub trusted: bool,
    /// Whether the project's config has anything that needs trust.
    pub needed: bool,
    /// Always true: with no project plugins (ADR-002) there is no plugin code that could not be
    /// read, so a saved decision always holds.
    pub complete: bool,
    /// One line per thing trust turns on.
    pub items: Vec<String>,
    /// Always empty: the project plugins whose code the decision covered are no longer loaded.
    pub plugins: Vec<Value>,
    pub mcp_servers: Vec<String>,
    pub endpoints: Vec<TrustEndpoint>,
    /// The allow rules the project adds.
    pub allow: Vec<String>,
    /// The permission mode the project asks for, when it is looser than the user's.
    pub mode: Option<String>,
}

/// What `sasacode list` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Models,
    Commands,
    Skills,
}

/// `args`: what follows `list`. Exactly one of models, commands or skills, and `--json`.
fn list_args(args: &[String]) -> anyhow::Result<(What, bool)> {
    let parsed = parse_args(args, &[boolean("json", None)]).map_err(anyhow::Error::msg)?;
    let what = match parsed.positionals.as_slice() {
        [w] if w == "models" => What::Models,
        [w] if w == "commands" => What::Commands,
        [w] if w == "skills" => What::Skills,
        _ => anyhow::bail!(t!("usage: sasacode list models|commands|skills [--json]")),
    };
    Ok((what, parsed.flag("json")))
}

/// `sasacode list models|commands|skills [--json]`; `args` is what follows `list`.
pub async fn list_command(args: &[String], cwd: &Path) -> anyhow::Result<i32> {
    let (what, json) = list_args(args)?;
    let home = sasacode_home();
    // Stdout is locked only to print: the lock must not be held across an await.
    match what {
        What::Models => {
            let models = model_listing(cwd).await?;
            print_models(&mut std::io::stdout().lock(), &models, json)?;
        }
        What::Commands => {
            let commands = command_listing_in(&home, cwd).await?;
            print_commands(&mut std::io::stdout().lock(), &commands, json)?;
        }
        What::Skills => print_skills(&mut std::io::stdout().lock(), &skill_listing_in(&home, cwd)?, json)?,
    }
    Ok(0)
}

fn print_json<T: Serialize>(out: &mut dyn Write, value: &T) -> anyhow::Result<()> {
    writeln!(out, "{}", serde_json::to_string(value)?)?;
    Ok(())
}

fn print_models(out: &mut dyn Write, models: &[ListedModel], json: bool) -> anyhow::Result<()> {
    if json {
        return print_json(out, &models);
    }
    for m in models {
        let size = match (m.context_window, m.max_context) {
            (Some(window), _) => format!("{} ctx", fmt_tokens(window)),
            (None, Some(max)) => t!("up to {size}", size = fmt_tokens(max)),
            (None, None) => String::new(),
        };
        let current = if m.current { t!("現在") } else { String::new() };
        let parts: Vec<&str> = [m.id.as_str(), &size, &current]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        writeln!(out, "{}", parts.join("  "))?;
    }
    Ok(())
}

fn print_commands(out: &mut dyn Write, commands: &[ListedCommand], json: bool) -> anyhow::Result<()> {
    if json {
        return print_json(out, &commands);
    }
    for c in commands {
        let hint = c.argument_hint.as_ref().map(|h| format!(" {h}")).unwrap_or_default();
        let tui_only = if c.headless {
            String::new()
        } else {
            format!(", {}", t!("TUI only"))
        };
        writeln!(out, "/{}{hint}  {}  [{}{tui_only}]", c.name, c.description, c.owner)?;
    }
    writeln!(
        out,
        "{}",
        t!("(the bundled plugins' commands are listed by a run: the `commands` line of --control stdio)")
    )?;
    Ok(())
}

fn print_skills(out: &mut dyn Write, skills: &[ListedSkill], json: bool) -> anyhow::Result<()> {
    if json {
        return print_json(out, &skills);
    }
    for s in skills {
        writeln!(out, "{} — {}\n  {}", s.name, s.description, s.path)?;
    }
    Ok(())
}

/// The config a run here would use: the project's elevated part only once the project is trusted,
/// as main decides for a run nobody is asked about.
fn run_settings(home: &Path, cwd: &Path) -> anyhow::Result<Settings> {
    let project = assess_files(cwd, read_config_files_from(&home.join("config.json"), cwd)?);
    let trusted = is_trusted_in(home, &cwd.to_string_lossy(), &project);
    Ok(Settings::of(&load_config(cwd, trusted, Some(project.files))?.config))
}

/// The models the TUI's /model offers: the harness set up as for a run (no session saved), its
/// warnings on stderr, then [`models_of`].
async fn model_listing(cwd: &Path) -> anyhow::Result<Vec<ListedModel>> {
    let home = sasacode_home();
    let project = assess_files(cwd, read_config_files_from(&home.join("config.json"), cwd)?);
    let cwd = cwd.to_string_lossy().into_owned();
    let trusted = is_trusted_in(&home, &cwd, &project);
    let mut opts = SetupOptions::new(cwd);
    opts.no_session = true;
    opts.trust_project = trusted;
    opts.preloaded = Some((project.files, project.warnings));
    let h = setup(opts).await?;
    let warn = |w: String| eprintln!("{}", t!("warning: {warning}", warning = w));
    for w in h.warnings() {
        warn(w);
    }
    let current = h.agent.model().spec();
    let configured = h.settings.models.clone().unwrap_or_default();
    // The catalog keeps a provider that cannot be listed out of the list, as /model does, so this
    // never fails as a whole.
    let listed = h.list_models(false).await;
    Ok(models_of(&current, &configured, Ok(listed), &warn))
}

/// The current model first, then the config's `models`, then what the providers list, each once.
/// A listing that failed is a warning, and the rest is still listed.
fn models_of(
    current: &str,
    configured: &[String],
    listed: anyhow::Result<Vec<ModelChoice>>,
    warn: &dyn Fn(String),
) -> Vec<ListedModel> {
    let listed = listed.unwrap_or_else(|e| {
        warn(format!("{e:#}"));
        Vec::new()
    });
    let ids: IndexSet<&str> = std::iter::once(current)
        .chain(configured.iter().map(String::as_str))
        .chain(listed.iter().map(|m| m.spec.as_str()))
        .collect();
    ids.into_iter()
        .map(|id| {
            // A spec listed twice is described by its last listing, as a JS Map built from the list.
            let m = listed.iter().rev().find(|m| m.spec == id);
            ListedModel {
                id: id.to_string(),
                // Zero is no size, as a falsy number was.
                context_window: m.and_then(|m| m.context_window).filter(|n| *n != 0),
                max_context: m.and_then(|m| m.max_context).filter(|n| *n != 0),
                server: m.and_then(|m| m.server.clone()).filter(|s| !s.is_empty()),
                current: id == current,
            }
        })
        .collect()
}

/// The skills a run here offers: the built-in ones, the user's and the project's; none when
/// `plugins.disabled` turns Skills off.
fn skill_listing_in(home: &Path, cwd: &Path) -> anyhow::Result<Vec<ListedSkill>> {
    if !PluginChoice::of(&run_settings(home, cwd)?).skills() {
        return Ok(Vec::new());
    }
    let mut dirs = skill_dirs(home, cwd);
    // First, as the skills plugin puts them, so a user or project skill of the same name wins.
    if let Some(builtin) = skills_options(home).builtin_dir
        && materialize_builtin_skills(&builtin).is_ok()
    {
        dirs.insert(0, builtin);
    }
    let project = project_skills_dir(cwd, &home.join("skills"));
    Ok(discover_skills_in(&dirs, project.as_deref())?
        .into_iter()
        .map(|s| ListedSkill {
            name: s.name,
            description: s.description,
            path: s.path.to_string_lossy().into_owned(),
        })
        .collect())
}

/// The TUI's own commands, in the current language, then the skills plugin's. The other bundled
/// plugins' commands are not here: a run lists them on the `commands` line of `--control stdio`.
async fn command_listing_in(home: &Path, cwd: &Path) -> anyhow::Result<Vec<ListedCommand>> {
    let mut out: Vec<ListedCommand> = TUI_COMMANDS
        .iter()
        .map(|c| ListedCommand {
            name: c.name.to_string(),
            description: t(c.description, &[]),
            argument_hint: c.argument_hint.map(str::to_string),
            owner: TUI_OWNER.to_string(),
            headless: false,
        })
        .collect();
    if PluginChoice::of(&run_settings(home, cwd)?).skills() {
        out.extend(skill_commands(home, cwd).await);
    }
    Ok(out)
}

/// What the skills plugin registers, so the names, hints and descriptions are its own: the plugin
/// is loaded by itself, on a plugin host whose agent is never run and has no model to call.
async fn skill_commands(home: &Path, cwd: &Path) -> Vec<ListedCommand> {
    let cwd_text = cwd.to_string_lossy().into_owned();
    let model = ModelInfo {
        id: String::new(),
        provider: String::new(),
        api: String::new(),
        base_url: None,
        context_window: 0,
        max_output: 0,
        price: None,
        reasoning: None,
        images: None,
        headers: None,
        server: None,
    };
    let host = PluginHost::new(PluginHostOptions {
        agent: Agent::new(AgentOptions::new(model, cwd_text.clone(), "")),
        cwd: cwd_text,
        settings: None,
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: current_lang(),
    });
    let plugin = sasacode_skills::create_skills_plugin(skill_dirs(home, cwd), skills_options(home));
    host.load("skills", plugin).await;
    host.commands()
        .into_iter()
        .map(|c| ListedCommand {
            name: c.command.name,
            description: c.command.description,
            argument_hint: c.command.argument_hint.filter(|h| !h.is_empty()),
            owner: c.owner,
            headless: true,
        })
        .collect()
}

/// The plugins 0.20 ships, in load order (then skills and MCP, which load last), each enabled as a
/// run here would load it: openai-codex at setup, free-search on its own or joined to MCP.
pub fn plugin_listing(cwd: &Path) -> anyhow::Result<Vec<ListedPlugin>> {
    plugin_listing_in(&sasacode_home(), cwd)
}

fn plugin_listing_in(home: &Path, cwd: &Path) -> anyhow::Result<Vec<ListedPlugin>> {
    let choice = PluginChoice::of(&run_settings(home, cwd)?);
    let mut out: Vec<ListedPlugin> = sasacode_bundled::PLUGINS
        .iter()
        .map(|p| ListedPlugin::bundled(p.name, choice.loads(p)))
        .collect();
    out.push(ListedPlugin::bundled("skills", choice.skills()));
    out.push(ListedPlugin::bundled("mcp", choice.mcp()));
    Ok(out)
}

/// `sasacode trust [--yes|-y] [--json]`; `args` is what follows `trust`. With `--yes` the project
/// is trusted; the exit code is 1 only when it still is not.
pub fn trust_command(args: &[String], cwd: &Path) -> anyhow::Result<i32> {
    trust_command_in(args, cwd, &sasacode_home(), &mut std::io::stdout().lock())
}

fn trust_command_in(args: &[String], cwd: &Path, home: &Path, out: &mut dyn Write) -> anyhow::Result<i32> {
    let parsed = parse_args(args, &[boolean("yes", Some('y')), boolean("json", None)]).map_err(anyhow::Error::msg)?;
    if !parsed.positionals.is_empty() {
        anyhow::bail!(t!("usage: sasacode trust [--yes] [--json]"));
    }
    let yes = parsed.flag("yes");
    let cwd_text = cwd.to_string_lossy().into_owned();
    let project = assess_files(cwd, read_config_files_from(&home.join("config.json"), cwd)?);
    if yes && !project.items.is_empty() && !is_trusted_in(home, &cwd_text, &project) {
        save_trust_in(home, &cwd_text, &project)?;
    }
    let status = trust_status(home, cwd, &project)?;
    if parsed.flag("json") {
        print_json(out, &status)?;
    } else if !status.needed {
        writeln!(
            out,
            "{}",
            t!("{cwd}: nothing in this project needs trust", cwd = cwd_text)
        )?;
    } else {
        let line = if status.trusted {
            t!("{cwd} is trusted", cwd = cwd_text)
        } else {
            t!("{cwd} is not trusted", cwd = cwd_text)
        };
        writeln!(out, "{line}")?;
        writeln!(out, "{}", t!("This project wants to use:"))?;
        for item in &status.items {
            writeln!(out, "  - {item}")?;
        }
        if !status.trusted {
            writeln!(out, "{}", t!("trust it with: sasacode trust --yes"))?;
        }
    }
    Ok(if yes && !status.trusted { 1 } else { 0 })
}

fn trust_status(home: &Path, cwd: &Path, project: &ProjectTrust) -> anyhow::Result<TrustStatus> {
    let cwd_text = cwd.to_string_lossy().into_owned();
    let elevated = load_config(cwd, false, Some(project.files.clone()))?.elevated;
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let permissions = elevated.get("permissions");
    Ok(TrustStatus {
        trusted: is_trusted_in(home, &cwd_text, project),
        needed: !project.items.is_empty(),
        complete: true,
        items: project.items.clone(),
        plugins: Vec::new(),
        mcp_servers: elevated
            .get("mcpServers")
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        endpoints: elevated
            .get("providers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, p)| TrustEndpoint {
                name: name.clone(),
                url: p.get("baseUrl").and_then(Value::as_str).map(str::to_string),
            })
            .collect(),
        allow: strings(permissions.and_then(|p| p.get("allow"))),
        mode: permissions
            .and_then(|p| p.get("mode"))
            .and_then(Value::as_str)
            .map(str::to_string),
        cwd: cwd_text,
    })
}

#[cfg(test)]
#[path = "listings_tests.rs"]
mod tests;
