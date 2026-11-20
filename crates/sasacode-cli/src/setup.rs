//! Everything a run needs, put together once (`packages/cli/src/setup.ts`): config, providers,
//! the agent, the plugin host, the session, and the model catalog. The TUI and the headless mode
//! both drive a [`Harness`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::{PERMISSION_MODES, PermissionPolicy};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionEntry, SessionFile, restore, session_dir};
use sasacode_agent::system_prompt::build_system_prompt;
use sasacode_ai::{Message, ModelInfo, ProviderConfig, ThinkingLevel};
use sasacode_host::{ModelOverride, builtin_providers, current_lang, find_session, known_model, list_sessions, t};
use sasacode_plugin_api::hook;
use sasacode_plugin_api::{Lang, SessionEndEvent, SessionEndReason, SessionStartEvent, UiBridge};

use crate::catalog::{GetKey, ModelCatalog, ModelChoice, Providers, untold};
use crate::config::{Config, ConfigFiles, DEFAULT_MAX_TURNS, Settings, load_config, sasacode_home};
use crate::json::{read_json, write_json};
use crate::keys::resolve_api_key;

pub struct SetupOptions {
    pub cwd: String,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub thinking: Option<String>,
    pub max_turns: Option<u64>,
    /// "last" = most recent session in cwd, otherwise a session id or path.
    pub resume: Option<String>,
    pub no_session: bool,
    /// Text added to the system prompt after `config.instructions` (--append-system-prompt-file).
    pub append_system_prompt: Option<String>,
    /// The project is trusted: the elevated part of its config is used.
    pub trust_project: bool,
    /// Config files already read for the trust check (see `trust::assess_project`).
    pub preloaded: Option<(ConfigFiles, Vec<String>)>,
}

impl SetupOptions {
    pub fn new(cwd: impl Into<String>) -> Self {
        SetupOptions {
            cwd: cwd.into(),
            model: None,
            permission: None,
            thinking: None,
            max_turns: None,
            resume: None,
            no_session: false,
            append_system_prompt: None,
            trust_project: false,
            preloaded: None,
        }
    }
}

/// --append-system-prompt-file: the file's text as UTF-8; a file that cannot be read is an error,
/// not a silent skip.
pub fn read_prompt_file(path: &str) -> anyhow::Result<String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        // Worded as Node words it (`ENOENT: no such file or directory, open 'x'`), as before.
        .map_err(|e| {
            let error = sasacode_tools::util::fs_error(&e, "open", path);
            anyhow::anyhow!(t!("failed to read {path}: {error}", path = path, error = error))
        })
}

/// Which plugins a run loads, as the config says: `plugins.enabled`, `plugins.disabled`, and the
/// jev-guard setting from when it was a separate plugin. `load_plugins` and the openai-codex load in
/// `setup` decide with it, and `sasacode plugin list` reports the same answer.
pub struct PluginChoice {
    enabled: Vec<String>,
    disabled: Vec<String>,
    /// A config that turned jev-guard on through its own setting keeps it on (ADR-011).
    jev_on: bool,
    /// When `slim-profile` is active, omit heavy optional plugins and skills unless explicitly enabled.
    slim_on: bool,
    slim_drop_skills: bool,
}

impl PluginChoice {
    pub fn of(settings: &Settings) -> Self {
        let enabled = settings.enabled_plugins();
        let disabled = settings.disabled_plugins();
        let slim_on = enabled.iter().any(|e| e == "slim-profile") && !disabled.iter().any(|d| d == "slim-profile");
        let slim_drop_skills = slim_on
            && settings
                .plugin_settings("slim-profile")
                .get("dropSkills")
                .and_then(Value::as_bool)
                != Some(false);
        PluginChoice {
            enabled,
            disabled,
            jev_on: settings
                .plugin_settings("jev-guard")
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            slim_on,
            slim_drop_skills,
        }
    }

    fn disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|d| d == name)
    }

    /// Whether the shipped plugin `p` loads.
    pub fn loads(&self, p: &sasacode_bundled::Bundled) -> bool {
        if self.slim_on
            && sasacode_bundled::slim_profile::SLIM_DROPPED_PLUGINS.contains(&p.name)
            && !self.enabled.iter().any(|e| e == p.name)
        {
            return false;
        }
        sasacode_bundled::wanted(p, &self.enabled, &self.disabled)
            || (p.name == "jev-guard" && self.jev_on && !self.disabled("jev-guard"))
    }

    /// Skills load unless `plugins.disabled` names them (or `slim-profile` drops them).
    pub fn skills(&self) -> bool {
        !self.disabled("skills") && !(self.slim_drop_skills && !self.enabled.iter().any(|e| e == "skills"))
    }

    /// MCP loads unless `plugins.disabled` names it.
    pub fn mcp(&self) -> bool {
        !self.disabled("mcp")
    }
}

/// The skills folders a run reads: the user's, then the project's (which wins for a skill of the
/// same name). The built-in skills come before both, written out where [`skills_options`] says.
pub fn skill_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    vec![home.join("skills"), cwd.join(".sasacode").join("skills")]
}

/// Where the skills plugin writes the built-in skills out.
pub fn skills_options(home: &Path) -> sasacode_skills::SkillsOptions {
    sasacode_skills::SkillsOptions {
        builtin_dir: Some(home.join("builtin-skills")),
    }
}

type Resolve = Arc<dyn Fn(&str) -> anyhow::Result<ModelInfo> + Send + Sync>;

pub struct Harness {
    pub agent: Agent,
    pub host: Arc<PluginHost>,
    pub config: Config,
    pub settings: Settings,
    pub warnings: Mutex<Vec<String>>,
    pub providers: Providers,
    pub sessions_dir: PathBuf,
    pub history_path: PathBuf,
    /// What the agent permission mode's judge decides (`--control stdio` reports it as `judged`).
    pub judged: sasacode_bundled::agent_judge::Verdicts,
    resolve: Resolve,
    catalog: ModelCatalog,
    opts: SetupOptions,
    home: PathBuf,
    session_entries: Mutex<Vec<SessionEntry>>,
    loading: tokio::sync::OnceCell<()>,
}

/// The first provider whose API key is set; with no key at all but a ChatGPT login, that plan.
pub(crate) fn default_model(providers: &IndexMap<String, ProviderConfig>, codex: bool) -> String {
    let any_key = providers.values().any(|p| {
        p.api_key_env
            .as_deref()
            .and_then(|v| std::env::var(v).ok())
            .is_some_and(|v| !v.trim().is_empty())
    });
    if !any_key && codex && sasacode_bundled::openai_codex::read_codex_auth().is_some() {
        let id = providers
            .get("openai-codex")
            .and_then(|p| p.default_model.clone())
            .unwrap_or_else(|| "gpt-5.5".into());
        return format!("openai-codex/{id}");
    }
    sasacode_host::default_model_for(providers)
}

/// `{ ...a, ...b }` for provider entries kept as JSON (a URL-only entry has no `api` yet).
fn merge_provider(base: Option<&ProviderConfig>, over: &Value) -> ProviderConfig {
    let mut v = base
        .and_then(|b| serde_json::to_value(b).ok())
        .unwrap_or_else(|| json!({}));
    if let (Value::Object(a), Value::Object(b)) = (&mut v, over) {
        for (k, x) in b {
            a.insert(k.clone(), x.clone());
        }
        // Detected from baseUrl by resolve_endpoints when missing.
        a.entry("api").or_insert_with(|| json!(""));
    }
    serde_json::from_value(v).unwrap_or_default()
}

/// Whether the user has a key or a login for provider `name`: its apiKeyEnv set, a key the keychain
/// already gave this process, the ChatGPT login (openai-codex) or an `ant auth login` profile (anthropic).
fn has_credentials(name: &str, cfg: Option<&ProviderConfig>) -> bool {
    let env = cfg
        .and_then(|c| c.api_key_env.as_deref())
        .and_then(|v| std::env::var(v).ok())
        .is_some_and(|v| !v.trim().is_empty());
    env || crate::keys::cached_key(name)
        || (name == "openai-codex" && sasacode_bundled::openai_codex::read_codex_auth().is_some())
        || (name == "anthropic"
            && crate::anthropic_profile::load(&crate::anthropic_profile::Env::process()).is_ok_and(|p| p.is_some()))
}

/// [`Resolve`] for a model the model itself names (a subagent's `model`): only on a provider the run
/// already uses (`picked`: its model, models, judgeModel and the models plugin settings name) or one
/// the user has a key or login for, so the conversation never goes to a provider the user did not
/// choose. Checked before any request; the error says which providers can be used.
pub(crate) fn limited_resolve(resolve: Resolve, providers: Providers, picked: Vec<String>) -> Resolve {
    Arc::new(move |spec: &str| {
        let list = providers.read().unwrap_or_else(|e| e.into_inner()).clone();
        let usable = |name: &str| picked.iter().any(|p| p == name) || has_credentials(name, list.get(name));
        let name = crate::config::provider_of(spec);
        if !usable(name) {
            let names: Vec<&str> = list.keys().map(String::as_str).filter(|n| usable(n)).collect();
            anyhow::bail!(
                "\"{spec}\": {name} is not a provider this session uses or has a key for; use a model from one of: {}",
                names.join(", ")
            );
        }
        resolve(spec)
    })
}

fn get_key(providers: &Providers) -> GetKey {
    let providers = providers.clone();
    Arc::new(move |p: String| {
        let cfg = providers.read().unwrap_or_else(|e| e.into_inner()).get(&p).cloned();
        async move { resolve_api_key(&p, cfg.as_ref()).await }.boxed()
    })
}

/// A server whose launch (`command`, or `url`) the project's own config writes runs in the project
/// (the user trusted it); one that only the global config launches starts in a folder of its own
/// under the sasacode home, whatever else the project sets on it (`disabled`, `timeout`, ...). The
/// two files are merged by now, so this reads the project's entries.
fn project_servers_start_in_project(config: &mut Config, project: &Config) {
    let Some(Value::Object(merged)) = config.get_mut("mcpServers") else {
        return;
    };
    let launched = |s: &Value| s.get("command").is_some() || s.get("url").is_some();
    let names = project
        .get("mcpServers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten();
    for (name, _) in names.filter(|(_, own)| launched(own)) {
        if let Some(Value::Object(server)) = merged.get_mut(name)
            && !server.contains_key("cwd")
        {
            server.insert("cwd".into(), sasacode_mcp::PROJECT_CWD.into());
        }
    }
}

pub async fn setup(mut opts: SetupOptions) -> anyhow::Result<Arc<Harness>> {
    // Decided before setup (asked on the terminal, --trust-project, or remembered).
    let trusted = opts.trust_project;
    let (files, extra) = match opts.preloaded.take() {
        Some((f, w)) => (Some(f), w),
        None => (None, Vec::new()),
    };
    let loaded = load_config(Path::new(&opts.cwd), trusted, files)?;
    let mut config = loaded.config;
    if trusted {
        project_servers_start_in_project(&mut config, &loaded.elevated);
    }
    let settings = Settings::of(&config);
    let mut warnings = loaded.warnings;
    warnings.extend(extra);

    let mut providers: IndexMap<String, ProviderConfig> = builtin_providers().clone();
    if let Some(custom) = &settings.providers {
        for (name, p) in custom {
            let merged = merge_provider(providers.get(name), p);
            providers.insert(name.clone(), merged);
        }
    }
    let providers: Providers = Arc::new(RwLock::new(providers));
    // Custom endpoints configured with only a URL: detect OpenAI- or Anthropic-style (cached).
    {
        let mut list = providers.read().unwrap_or_else(|e| e.into_inner()).clone();
        crate::endpoints::resolve_endpoints(&mut list, get_key(&providers), &mut warnings).await;
        *providers.write().unwrap_or_else(|e| e.into_inner()) = list;
    }

    // What the Models API reported fills in context windows; explicit modelOverrides in config win.
    let catalog_cell: Arc<OnceLock<ModelCatalog>> = Arc::new(OnceLock::new());
    let config_overrides: IndexMap<String, ModelOverride> = settings
        .model_overrides
        .iter()
        .flatten()
        .filter_map(|(k, v)| {
            serde_json::from_value::<ModelOverride>(v.clone())
                .ok()
                .map(|o| (k.clone(), o))
        })
        .collect();
    let resolve: Resolve = {
        let (providers, catalog_cell) = (providers.clone(), catalog_cell.clone());
        Arc::new(move |spec: &str| {
            let mut merged: IndexMap<String, ModelOverride> =
                catalog_cell.get().map(|c| c.discovered()).unwrap_or_default();
            for (k, v) in &config_overrides {
                let m = merged.get(k).map(|d| d.merged(v)).unwrap_or_else(|| v.clone());
                merged.insert(k.clone(), m);
            }
            let list = providers.read().unwrap_or_else(|e| e.into_inner());
            sasacode_host::resolve_model(spec, &list, &merged)
        })
    };

    let permissions = settings.permissions.clone().unwrap_or_default();
    let mode_name = opts
        .permission
        .clone()
        .or_else(|| permissions.mode.map(|m| m.as_str().to_string()))
        .unwrap_or_else(|| "edits".into());
    let Some(mode) = PERMISSION_MODES.iter().copied().find(|m| m.as_str() == mode_name) else {
        let modes = PERMISSION_MODES
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(t!(
            "unknown permission mode \"{mode}\" ({modes})",
            mode = mode_name,
            modes = modes
        ));
    };
    let home = sasacode_home();
    let sessions_dir = session_dir(&home, &opts.cwd);
    // Sessions, input history, saved outputs and keys live here: private to the user, whatever the umask.
    sasacode_plugin_api::create_private_dir(&home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(&home)
            && m.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
        }
    }

    // Resolved now so a typo fails at startup, not at the first judged call.
    if let Some(judge) = &settings.judge_model {
        resolve(judge)?;
    }
    let disabled: Vec<String> = settings.disabled_plugins();
    let model_spec = match opts.model.clone().or_else(|| settings.model.clone()) {
        Some(m) => m,
        None => default_model(
            &providers.read().unwrap_or_else(|e| e.into_inner()),
            !disabled.iter().any(|d| d == "openai-codex"),
        ),
    };
    let thinking_name = opts
        .thinking
        .clone()
        .or_else(|| config.get("thinking").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "high".into());
    let thinking: ThinkingLevel = serde_json::from_value(json!(thinking_name)).map_err(|_| {
        anyhow::anyhow!(t!(
            "unknown thinking level \"{level}\" ({levels})",
            level = thinking_name,
            levels = "off, low, medium, high, xhigh, max"
        ))
    })?;
    let mut agent_opts = AgentOptions::new(
        resolve(&model_spec)?,
        opts.cwd.clone(),
        build_system_prompt(
            &opts.cwd,
            &[settings.instructions.clone(), opts.append_system_prompt.clone()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>(),
        ),
    );
    agent_opts.thinking = Some(thinking);
    agent_opts.permissions = Some(Arc::new(PermissionPolicy::new(mode, &permissions.rules)?));
    let key = get_key(&providers);
    agent_opts.get_api_key = Some(key.clone());
    let picked: Vec<String> = [Some(model_spec.clone()), settings.judge_model.clone()]
        .into_iter()
        .flatten()
        .chain(settings.models.clone().unwrap_or_default())
        .chain(settings.plugins.iter().flat_map(|p| {
            let models = p
                .settings
                .values()
                .filter_map(|s| s.get("model")?.as_str().map(str::to_string));
            models.collect::<Vec<_>>()
        }))
        .map(|s| crate::config::provider_of(&s).to_string())
        .collect();
    agent_opts.resolve_model = Some({
        let resolve = limited_resolve(resolve.clone(), providers.clone(), picked);
        Arc::new(move |s: &str| resolve(s))
    });
    let max_turns = opts
        .max_turns
        .or_else(|| settings.max_turns.map(|n| n as u64))
        .unwrap_or(DEFAULT_MAX_TURNS);
    agent_opts.max_turns = Some(max_turns.min(u32::MAX as u64) as u32);
    agent_opts.max_retries = settings.max_retries.map(|n| n as u32);
    agent_opts.tool_search = settings.tool_search_config();
    let agent = Agent::new(agent_opts);

    let catalog = ModelCatalog::new(providers.clone(), key.clone(), {
        let agent = agent.clone();
        Arc::new(move || agent.model().provider)
    });
    let _ = catalog_cell.set(catalog.clone());

    turn_end_listing(&agent, &providers, &catalog, &resolve, &settings);

    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd: opts.cwd.clone(),
        // judgeModel is the agent-judge plugin's model.
        settings: Some({
            let settings = settings.clone();
            Arc::new(move |name: &str| {
                let own = settings.plugin_settings(name);
                match (&settings.judge_model, name) {
                    (Some(judge), "agent-judge") => {
                        let mut out = Map::new();
                        out.insert("model".into(), json!(judge));
                        if let Value::Object(o) = own {
                            out.extend(o);
                        }
                        Value::Object(out)
                    }
                    _ => own,
                }
            })
        }),
        register_provider: Some({
            let providers = providers.clone();
            Arc::new(move |name: &str, cfg: ProviderConfig, imp| {
                if let Some(imp) = imp {
                    sasacode_ai::register_api(&cfg.api, imp);
                }
                providers
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(name.to_string(), cfg);
                Ok(())
            })
        }),
        disabled_tools: settings.disabled_tools(),
        lang: current_lang(),
    });
    // Built-in tools go through the same API as everything else (P2); they load first.
    host.load(sasacode_tools::PLUGIN_NAME, |api| sasacode_tools::plugin(api).boxed())
        .await;
    // Loaded now, not with the other plugins: /model lists its models at startup.
    if let Some(codex) = sasacode_bundled::find("openai-codex")
        && PluginChoice::of(&settings).loads(codex)
    {
        host.load(codex.name, codex.plugin).await;
    }

    let history_path = home.join("history.jsonl");
    let h = Arc::new(Harness {
        agent,
        host,
        config,
        settings,
        warnings: Mutex::new(warnings),
        providers,
        sessions_dir,
        history_path,
        judged: Default::default(),
        resolve,
        catalog,
        opts,
        home,
        session_entries: Mutex::new(Vec::new()),
        loading: tokio::sync::OnceCell::new(),
    });

    if let Some(reference) = h.opts.resume.clone() {
        let list = list_sessions(&h.sessions_dir, Some(&h.opts.cwd));
        let target = if reference == "last" {
            list.first().cloned()
        } else {
            list.into_iter()
                .find(|s| s.id == reference || s.path.to_string_lossy() == reference || s.id.starts_with(&reference))
        };
        let Some(target) = target else {
            if reference == "last" {
                anyhow::bail!(t!("no previous session in this directory"));
            }
            // The id alone is often all a script keeps: when the session lives in another
            // directory, name it, so the run can be continued from there instead of "not found".
            return Err(match find_session(&h.home.join("sessions"), &reference) {
                Some(elsewhere) => anyhow::anyhow!(t!(
                    "session {id} belongs to {cwd}: run sasacode from there",
                    id = reference,
                    cwd = elsewhere.cwd
                )),
                None => anyhow::anyhow!(t!("session not found: {id}", id = reference)),
            });
        };
        h.open_session(&target.path, h.opts.model.is_some())?;
    } else {
        h.create_session()?;
    }
    Ok(h)
}

/// A model Ollama (without num_ctx) or LM Studio has not loaded yet is listed with its maximum
/// only; once it has answered, the server says what it was loaded with. Asked at the end of its
/// turn and before the plugins' turn_end hooks (registered later), so that turn's compaction
/// threshold already uses the real size (#61). While the size stays unknown (the server was down
/// when listed, the refresh failed, it reported none) it is asked again at the next turns' ends,
/// three times in all. A contextWindow in modelOverrides is the user's: nothing is asked for it.
/// A custom server not told apart may be LM Studio that was down when listed (#59). One that has
/// listed the model, and is still not told apart, is not. One that has not is listed again once:
/// it has just answered the turn, so it is up, and one still not told apart then is not asked
/// again. Telling a server apart is the provider's, so it is counted by the provider's name.
fn turn_end_listing(
    agent: &Agent,
    providers: &Providers,
    catalog: &ModelCatalog,
    resolve: &Resolve,
    settings: &Settings,
) {
    let asked: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let (agent2, providers, catalog, resolve) = (agent.clone(), providers.clone(), catalog.clone(), resolve.clone());
    let overrides = settings.model_overrides.clone().unwrap_or_default();
    agent.hooks().on::<hook::TurnEnd, _, _>("sasacode", move |_e, _ctx| {
        let (agent, providers, catalog, resolve, asked, overrides) = (
            agent2.clone(),
            providers.clone(),
            catalog.clone(),
            resolve.clone(),
            asked.clone(),
            overrides.clone(),
        );
        async move {
            let model = agent.model();
            let spec = model.spec();
            let name = model.provider.clone();
            let p = providers.read().unwrap_or_else(|e| e.into_inner()).get(&name).cloned();
            let is_untold = untold(&name, p.as_ref());
            let key = if is_untold { name.clone() } else { spec.clone() };
            let n = asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .copied()
                .unwrap_or(0);
            let local = p
                .as_ref()
                .is_some_and(|p| p.ollama == Some(true) || p.lmstudio == Some(true));
            let user_size = overrides
                .get(&spec)
                .and_then(|o| o.get("contextWindow"))
                .is_some_and(|v| {
                    // A JS truthy number: 0 and null do not count.
                    v.as_f64().is_some_and(|n| n != 0.0)
                });
            if !(local || is_untold) || n >= 3 || user_size {
                return Ok(None);
            }
            let listed = catalog.entry(&spec, false).await;
            let mut done = listed.as_ref().is_some_and(|l| l.context_window.is_some_and(|c| c > 0))
                || (listed.is_some() && is_untold);
            if !done {
                done = catalog
                    .entry(&spec, true)
                    .await
                    .is_some_and(|l| l.context_window.is_some_and(|c| c > 0))
                    || is_untold;
            }
            asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, if done { 3 } else { n + 1 });
            adopt(&agent, &catalog, &resolve);
            Ok(None)
        }
    });
}

/// The current model may just have learned its real context window, or which server it runs on
/// (#59).
fn adopt(agent: &Agent, catalog: &ModelCatalog, resolve: &Resolve) {
    let current = agent.model();
    let spec = current.spec();
    if let Ok(fresh) = resolve(&spec)
        && (catalog.has_discovered(&spec) || fresh.server != current.server)
    {
        let _ = agent.set_model(fresh);
    }
}

impl Harness {
    pub fn resolve(&self, spec: &str) -> anyhow::Result<ModelInfo> {
        (self.resolve)(spec)
    }

    pub fn warnings(&self) -> Vec<String> {
        self.warnings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn warn(&self, w: String) {
        self.warnings.lock().unwrap_or_else(|e| e.into_inner()).push(w);
    }

    fn current(&self) -> String {
        self.agent.model().spec()
    }

    /// Models offered by the providers you have keys for (fetched once, then cached).
    pub async fn list_models(&self, refresh: bool) -> Vec<ModelChoice> {
        let models = self.catalog.list(refresh).await;
        adopt(&self.agent, &self.catalog, &self.resolve);
        models
    }

    /// What the current model's provider alone lists for it (cached); the model takes in the size
    /// it reports.
    pub async fn current_listing(&self) -> Option<ModelChoice> {
        let listed = self.catalog.entry(&self.current(), false).await;
        adopt(&self.agent, &self.catalog, &self.resolve);
        listed
    }

    /// -m: fails when the provider lists its models and `spec` is not one of them.
    pub async fn check_model(&self, spec: &str) -> anyhow::Result<()> {
        // Names sasacode or the config already knows are taken as they are (aliases are not always listed).
        let id = spec.split_once('/').map(|(_, id)| id).unwrap_or(spec);
        if known_model(id).is_some() || self.settings.model_override(spec).is_some() {
            return Ok(());
        }
        let Some(r) = self.catalog.offers(spec).await else {
            return Ok(());
        };
        if r.ok {
            return Ok(());
        }
        let provider = spec.split_once('/').map(|(p, _)| p).unwrap_or("");
        if r.similar.is_empty() {
            anyhow::bail!(t!(
                "{provider} has no model \"{spec}\" (the TUI's /model lists what it offers)",
                provider = provider,
                spec = spec
            ));
        }
        let similar = r
            .similar
            .iter()
            .map(|s| format!("{provider}/{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(t!(
            "{provider} has no model \"{spec}\". Did you mean: {similar}",
            provider = provider,
            spec = spec,
            similar = similar
        ));
    }

    async fn start_session(&self, resumed: bool) {
        let entries = self.session_entries.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.host.set_session_entries(&entries);
        let session_id = self.agent.session().map(|s| s.id().to_string());
        self.agent
            .hooks()
            .emit::<hook::SessionStart>(SessionStartEvent { session_id, resumed })
            .await;
    }

    async fn end_session(&self, reason: SessionEndReason) {
        let session_id = self.agent.session().map(|s| s.id().to_string());
        self.agent
            .hooks()
            .emit::<hook::SessionEnd>(SessionEndEvent {
                session_id,
                reason: Some(reason),
            })
            .await;
    }

    /// `keep_model`: -m was given on the command line, and beats the model the session ended with.
    fn open_session(&self, path: &Path, keep_model: bool) -> anyhow::Result<()> {
        let secrets = self.agent.session().map(|s| s.secrets()).unwrap_or_default();
        let (file, entries) = SessionFile::open(path, secrets)?;
        let state = restore(&entries);
        self.agent.reset(state.messages.clone());
        // --no-session: the old conversation is read, and nothing more is written to it.
        self.agent
            .set_session(if self.opts.no_session { None } else { Some(file.clone()) });
        // Record the repair, so restoring again (after more turns are appended) gives the same history.
        if state.repaired && !self.opts.no_session {
            file.append(&SessionEntry::Replace {
                messages: state.messages,
            })?;
        }
        *self.session_entries.lock().unwrap_or_else(|e| e.into_inner()) = entries;
        if let Some(model) = state.model
            && !keep_model
        {
            match self.resolve(&model) {
                Ok(m) => {
                    let _ = self.agent.set_model(m);
                }
                Err(e) => self.warn(t!(
                    "session model {model} unavailable: {error}",
                    model = model,
                    error = format!("{e:#}")
                )),
            }
        }
        Ok(())
    }

    fn create_session(&self) -> anyhow::Result<()> {
        // The keys the old log redacted carry over: a fork writes the old conversation before any
        // prompt registers them (#103).
        let secrets = self.agent.session().map(|s| s.secrets()).unwrap_or_default();
        self.agent.reset(Vec::new());
        let session = if self.opts.no_session {
            None
        } else {
            Some(SessionFile::create(&self.sessions_dir, &self.opts.cwd, secrets)?)
        };
        self.agent.set_session(session);
        self.session_entries.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(())
    }

    /// Load the bundled plugins, MCP and Skills, then start the session. Safe to call more than
    /// once: the first call does the work. `ui`: None is the headless one.
    pub async fn load_plugins(&self, ui: Option<Arc<dyn UiBridge>>) {
        self.loading
            .get_or_init(|| async move {
                self.host.set_ui(ui);
                let choice = PluginChoice::of(&self.settings);
                let mcp_off = !choice.mcp();
                let mut free_search = false;
                for p in sasacode_bundled::PLUGINS {
                    // Loaded by setup, before the session.
                    if p.name == "openai-codex" {
                        continue;
                    }
                    let on = choice.loads(p);
                    if on && p.name == "free-search" && !mcp_off {
                        // Its server joins the mcp plugin's below, under one `/mcp`.
                        self.host.load(p.name, sasacode_bundled::free_search::joined).await;
                        free_search = true;
                    } else if on && p.name == "agent-judge" {
                        let judge = sasacode_bundled::agent_judge::with_verdicts(self.judged.clone());
                        self.host.load(p.name, judge).await;
                    } else if on {
                        self.host.load(p.name, p.plugin).await;
                    }
                }
                if choice.skills() {
                    let dirs = skill_dirs(&self.home, Path::new(&self.opts.cwd));
                    self.host
                        .load(
                            "skills",
                            sasacode_skills::create_skills_plugin(dirs, skills_options(&self.home)),
                        )
                        .await;
                }
                if !mcp_off {
                    // Project MCP servers are only in `config` when the project is trusted.
                    let mut servers = sasacode_mcp::McpServerConfig::servers_from_json(
                        self.config.get("mcpServers").unwrap_or(&Value::Null),
                    );
                    if free_search {
                        for (name, server) in sasacode_bundled::free_search::servers() {
                            servers.entry(name).or_insert(server);
                        }
                    }
                    self.host.load("mcp", sasacode_mcp::create_mcp_plugin(servers)).await;
                }
                self.start_session(self.opts.resume.is_some()).await;
            })
            .await;
    }

    /// Replace the agent's conversation with a stored session.
    pub async fn load_session(&self, path: &Path) -> anyhow::Result<()> {
        self.end_session(SessionEndReason::Switch).await;
        self.open_session(path, false)?;
        self.start_session(true).await;
        Ok(())
    }

    pub async fn new_session(&self) -> anyhow::Result<()> {
        self.end_session(SessionEndReason::Switch).await;
        self.create_session()?;
        self.start_session(false).await;
        Ok(())
    }

    /// New session holding the conversation up to (not including) message `index`.
    pub async fn fork(&self, index: usize) -> anyhow::Result<()> {
        let kept: Vec<Message> = self.agent.messages().into_iter().take(index).collect();
        self.end_session(SessionEndReason::Switch).await;
        self.create_session()?;
        // And the model's key, as a prompt would register it (the old log may have had none yet).
        if let Some(session) = self.agent.session() {
            let provider = self.agent.model().provider;
            let cfg = self
                .providers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&provider)
                .cloned();
            let key = resolve_api_key(&provider, cfg.as_ref()).await;
            session.add_secret(key.as_deref());
            for m in &kept {
                session.append(&SessionEntry::Message { message: m.clone() })?;
            }
        }
        self.agent.reset(kept);
        self.start_session(false).await;
        Ok(())
    }

    /// Everything said in the session loaded, also what a compaction summarized for the model (to
    /// show it).
    pub fn transcript(&self) -> Vec<Message> {
        let entries = self.session_entries.lock().unwrap_or_else(|e| e.into_inner());
        let compacted = entries.iter().any(|e| match e {
            SessionEntry::Replace { messages } => messages.first().is_some_and(|m| m.text().starts_with("[Summary of")),
            _ => false,
        });
        if !compacted {
            // Not compacted: the history as restored (a torn tail repaired).
            return self.agent.messages();
        }
        entries
            .iter()
            .filter_map(|e| match e {
                SessionEntry::Message { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// /language: the `lang` setting in the global config.json. A config.json that cannot be read
    /// is left alone.
    pub fn save_lang(&self, lang: Lang) -> anyhow::Result<()> {
        let path = sasacode_home().join("config.json");
        let mut config = read_json(&path)?;
        if let Value::Object(o) = &mut config {
            o.insert("lang".into(), serde_json::to_value(lang)?);
        }
        write_json(&path, &config)?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.end_session(SessionEndReason::Exit).await;
    }

    pub fn cwd(&self) -> &str {
        &self.opts.cwd
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
