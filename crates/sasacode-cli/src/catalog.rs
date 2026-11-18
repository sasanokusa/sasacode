//! The models the configured providers offer, fetched from their Models APIs (`packages/cli/src/catalog.ts`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use indexmap::IndexMap;

use sasacode_ai::{ProviderConfig, get_provider};
use sasacode_host::{DEFAULT_CONTEXT_WINDOW, ModelOverride, builtin_providers, known_model};

use crate::list_models::{Deadline, ListedModel, api_endpoint, list_models, lmstudio_list};

/// The API key of a provider, by name (`resolveApiKey`).
pub type GetKey = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;
/// The providers, shared with setup: the catalog marks one it found to be LM Studio, and plugins add
/// providers after the catalog exists (the TS shared one object).
pub type Providers = Arc<RwLock<IndexMap<String, ProviderConfig>>>;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelChoice {
    /// "<provider>/<model>"
    pub spec: String,
    /// Effective context window: from the provider, else sasacode's table of known models.
    pub context_window: Option<u64>,
    /// The model's maximum, shown when the effective size is unknown (Ollama without num_ctx, LM Studio before loading).
    pub max_context: Option<u64>,
    /// The local server it runs on ("ollama", "lmstudio" or "llamacpp"): why the size may be unknown,
    /// and what to do about it.
    pub server: Option<String>,
}

/// Whether a provider lists a model (`offers`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Offers {
    pub ok: bool,
    pub similar: Vec<String>,
}

/// Where a provider with no base URL lives.
fn default_host(api: &str) -> Option<&'static str> {
    match api {
        "anthropic" => Some("api.anthropic.com"),
        "openai-chat" | "openai-responses" => Some("api.openai.com"),
        _ => None,
    }
}

/// Where the service lives: host and path. OpenAI-style base URLs end in /v1 and Anthropic-style
/// ones do not (its client adds it), so /v1 is left off: ".../provider/v1" and ".../provider" are one
/// service, while two servers behind one proxy (".../vllm/v1", ".../tabby/v1") are two (#105).
fn service(p: &ProviderConfig) -> String {
    let base = match p.base_url.as_deref() {
        Some(b) if !b.is_empty() => b,
        _ => return default_host(&p.api).map_or_else(|| p.api.clone(), str::to_string),
    };
    match url::Url::parse(base) {
        Ok(url) => {
            let host = match (url.host_str(), url.port()) {
                (Some(h), Some(port)) => format!("{h}:{port}"),
                (Some(h), None) => h.to_string(),
                (None, _) => String::new(),
            };
            let path = url.path().trim_end_matches('/');
            format!("{host}{}", path.strip_suffix("/v1").unwrap_or(path))
        }
        Err(_) => base.to_string(),
    }
}

/// Providers that reach the same service with the same key (openai / openai-chat, the three
/// commandcode entries) are one group: listed once, and each model goes to the member whose API
/// format it answers on.
fn groups(providers: &IndexMap<String, ProviderConfig>) -> Vec<Vec<String>> {
    let mut by_key: IndexMap<String, Vec<String>> = IndexMap::new();
    for (name, p) in providers {
        let k = format!("{}@{}", p.api_key_env.as_deref().unwrap_or(""), service(p));
        by_key.entry(k).or_default().push(name.clone());
    }
    by_key.into_values().collect()
}

/// A custom OpenAI-style server not told apart. LM Studio set up before sasacode knew it is one
/// (`endpoint add` wrote no flag, a URL-only entry was cached without one) until it is listed (#59).
pub fn untold(name: &str, p: Option<&ProviderConfig>) -> bool {
    p.is_some_and(|p| {
        p.api == "openai-chat"
            && p.base_url.as_deref().is_some_and(|b| !b.is_empty())
            && !builtin_providers().contains_key(name)
            && p.ollama != Some(true)
            && p.llamacpp != Some(true)
            && p.lmstudio != Some(true)
    })
}

/// `spec.slice(0, spec.indexOf("/"))`: without a "/", JS drops the last UTF-16 unit; a name that
/// leaves a lone surrogate matches no provider.
fn provider_of(spec: &str) -> Option<&str> {
    match spec.find('/') {
        Some(i) => Some(&spec[..i]),
        None => {
            let last = spec.chars().next_back()?;
            (last.len_utf16() == 1).then(|| &spec[..spec.len() - last.len_utf8()])
        }
    }
}

/// `spec.slice(spec.indexOf("/") + 1)`.
fn model_of(spec: &str) -> &str {
    spec.find('/').map_or(spec, |i| &spec[i + 1..])
}

type Listing = Shared<BoxFuture<'static, Vec<ModelChoice>>>;

struct Inner {
    providers: Providers,
    get_key: GetKey,
    /// Also asked without a key (it may authenticate another way, e.g. an `ant auth login` profile).
    current_provider: Arc<dyn Fn() -> String + Send + Sync>,
    /// Per spec, what the provider told us (context window, max output, price).
    discovered: Mutex<IndexMap<String, ModelOverride>>,
    /// Each group's listing, asked once (again on refresh). The listing itself is shared, so callers
    /// at the same time wait for one request.
    lists: Mutex<HashMap<String, Listing>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Models the configured providers offer, fetched from their Models API (GET /v1/models).
#[derive(Clone)]
pub struct ModelCatalog {
    inner: Arc<Inner>,
}

impl ModelCatalog {
    pub fn new(providers: Providers, get_key: GetKey, current_provider: Arc<dyn Fn() -> String + Send + Sync>) -> Self {
        ModelCatalog {
            inner: Arc::new(Inner {
                providers,
                get_key,
                current_provider,
                discovered: Mutex::new(IndexMap::new()),
                lists: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// What the providers told about their models so far (TS `discovered`), as a snapshot.
    pub fn discovered(&self) -> IndexMap<String, ModelOverride> {
        lock(&self.inner.discovered).clone()
    }

    pub fn has_discovered(&self, spec: &str) -> bool {
        lock(&self.inner.discovered).contains_key(spec)
    }

    fn snapshot(&self) -> IndexMap<String, ProviderConfig> {
        self.inner.providers.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Cached after the first call; `refresh` fetches again.
    pub async fn list(&self, refresh: bool) -> Vec<ModelChoice> {
        let listings: Vec<Listing> = groups(&self.snapshot())
            .iter()
            .map(|g| self.list_group(g, refresh))
            .collect();
        futures::future::join_all(listings)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    /// What the provider of `spec` lists for it; `refresh` asks that provider again, and no other.
    pub async fn entry(&self, spec: &str, refresh: bool) -> Option<ModelChoice> {
        let group = self.group_of(spec)?;
        self.list_group(&group, refresh)
            .await
            .into_iter()
            .find(|m| m.spec == spec)
    }

    fn group_of(&self, spec: &str) -> Option<Vec<String>> {
        let provider = provider_of(spec)?;
        groups(&self.snapshot())
            .into_iter()
            .find(|g| g.iter().any(|n| n == provider))
    }

    fn list_group(&self, members: &[String], refresh: bool) -> Listing {
        let key = members.join(" ");
        let mut lists = lock(&self.inner.lists);
        let last = lists.get(&key).cloned();
        if !refresh && let Some(l) = &last {
            return l.clone();
        }
        // A refresh that fails keeps what was listed before: the server's models stay in /model.
        let inner = self.inner.clone();
        let members = members.to_vec();
        let listed = async move {
            match inner.fetch_group(&members).await {
                Ok(models) => models,
                Err(_) => match last {
                    Some(l) => l.await,
                    None => Vec::new(),
                },
            }
        }
        .boxed()
        .shared();
        lists.insert(key, listed.clone());
        // Run to the end whoever waits for it, like the TS promise: a caller that stops waiting must
        // not leave a half-run listing (its deadline running out) in the cache.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(listed.clone());
        }
        listed
    }

    /// Whether the provider of `spec` lists that model, and if not, the listed names most like it.
    /// None when it cannot be told: no Models API, offline, or an empty list.
    pub async fn offers(&self, spec: &str) -> Option<Offers> {
        let id = model_of(spec);
        let listed = match self.group_of(spec) {
            Some(g) => self.list_group(&g, false).await,
            None => Vec::new(),
        };
        if listed.is_empty() {
            return None;
        }
        let ids: Vec<String> = listed.iter().map(|m| model_of(&m.spec).to_string()).collect();
        Some(if ids.iter().any(|i| i == id) {
            Offers {
                ok: true,
                similar: Vec::new(),
            }
        } else {
            Offers {
                ok: false,
                similar: similar(id, &ids),
            }
        })
    }
}

impl Inner {
    async fn fetch_group(&self, members: &[String]) -> anyhow::Result<Vec<ModelChoice>> {
        let primary = members
            .first()
            .ok_or_else(|| anyhow::anyhow!("empty provider group"))?
            .clone();
        let provider = |name: &str| {
            self.providers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)
                .cloned()
        };
        let mut p = provider(&primary).ok_or_else(|| anyhow::anyhow!("no provider {primary}"))?;
        let key = (self.get_key)(primary.clone()).await;
        let keyed = key.as_deref().is_some_and(|k| !k.is_empty());
        if p.api_key_env.as_deref().is_some_and(|e| !e.is_empty())
            && !keyed
            && !members.contains(&(self.current_provider)())
        {
            return Ok(Vec::new());
        }
        // A provider implementation (e.g. from a plugin) may know its own models.
        let implementation = get_provider(&p.api).ok();
        let deadline = Deadline::after(Duration::from_millis(8000));
        let signal = deadline.token.clone();
        // Asked when listed, not at startup: a server that is down delays nothing, and LM Studio with no
        // model downloaded when it was detected is still recognised later. The flag stays on the
        // provider for the session, so the turn_end refresh asks it too. The list that told it apart
        // is the one the sizes come from.
        let native = if untold(&primary, Some(&p)) {
            lmstudio_list(&p, key.as_deref(), &signal).await
        } else {
            None
        };
        if native.is_some() {
            p.lmstudio = Some(true);
            if let Some(shared) = self
                .providers
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&primary)
            {
                shared.lmstudio = Some(true);
            }
        }
        let own = match &implementation {
            Some(i) => i.list_models(&p, key.as_deref(), signal.clone()).await,
            None => None,
        };
        let models: Vec<ListedModel> = match own {
            Some(listed) => listed?.into_iter().map(ListedModel::from).collect(),
            None => list_models(&p, key.as_deref(), &signal, native).await?,
        };
        let server = if p.ollama == Some(true) {
            Some("ollama")
        } else if p.lmstudio == Some(true) {
            Some("lmstudio")
        } else if p.llamacpp == Some(true) {
            Some("llamacpp")
        } else {
            None
        };
        let apis: Vec<(String, Option<String>)> = members
            .iter()
            .map(|n| (n.clone(), provider(n).map(|p| p.api)))
            .collect();
        let mut out = Vec::new();
        for m in models {
            let target = match &m.endpoints {
                Some(endpoints) => apis
                    .iter()
                    .find(|(_, api)| {
                        let wanted = api.as_deref().and_then(api_endpoint).unwrap_or("");
                        endpoints.iter().any(|e| e == wanted)
                    })
                    .map(|(n, _)| n.clone()),
                None => Some(primary.clone()),
            };
            let Some(target) = target else { continue }; // e.g. an endpoint sasacode does not speak
            let spec = format!("{target}/{}", m.id);
            let known = known_model(&m.id).map(|k| k.context_window);
            let mut info = ModelOverride::default();
            {
                let mut discovered = lock(&self.discovered);
                if let Some(cw) = m.context_window.filter(|c| *c != 0) {
                    info.context_window = Some(cw);
                }
                // Only the maximum known (not loaded yet): no request gets more, so one below the default is
                // planned with, though the size stays unknown. A size learned before is the better guess.
                else if let Some(max) = m.max_context.filter(|c| *c != 0)
                    && max < known.unwrap_or(DEFAULT_CONTEXT_WINDOW)
                    && !discovered
                        .get(&spec)
                        .is_some_and(|d| d.context_window.is_some_and(|c| c != 0))
                {
                    info.context_window = Some(max);
                }
                if let Some(mo) = m.max_output.filter(|c| *c != 0) {
                    info.max_output = Some(mo);
                }
                // A listing that reports an all-zero price is saying "unknown", not free: keep it from
                // shadowing a price the built-in table knows.
                if let Some(price) = m.price.as_ref().filter(|p| p.input > 0.0 || p.output > 0.0) {
                    info.price = Some(price.clone());
                }
                if info != ModelOverride::default() {
                    discovered.insert(spec.clone(), info);
                }
            }
            out.push(ModelChoice {
                spec,
                context_window: m.context_window.or(known),
                max_context: m.max_context,
                server: server.map(str::to_string),
            });
        }
        drop(deadline);
        Ok(out)
    }
}

/// Listed names sharing the most words with `id` (a typo'd "deepseek-v4-flsh" still finds "deepseek-v4-flash").
fn similar(id: &str, ids: &[String]) -> Vec<String> {
    let lower = id.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.'))
        .filter(|w| w.len() > 1)
        .collect();
    let mut scored: Vec<(&String, usize)> = ids
        .iter()
        .map(|c| {
            let c_lower = c.to_lowercase();
            (c, words.iter().filter(|w| c_lower.contains(**w)).count())
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.1));
    scored.into_iter().take(5).map(|(c, _)| c.clone()).collect()
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
