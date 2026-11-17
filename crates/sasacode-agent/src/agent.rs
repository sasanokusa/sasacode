//! The agent loop (`packages/agent/src/agent.ts`).
//!
//! Concurrency: an [`Agent`] is a cheap handle (`Arc`). Its state sits behind a std mutex that is
//! never held across an await, an event, a hook, a session write or a tool method. A run is a
//! spawned task; `running` holds its shared future. Starting a run (check, token, spawn, store) and
//! ending one (clear, check halted, take the queue, start the next) each happen in one critical
//! section, as they happened atomically on the JS event loop.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, StreamExt};
use indexmap::IndexMap;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use sasacode_ai::{
    AssistantContent, AssistantMessage, Message, ModelInfo, Request, SamplingOptions, StopReason, StreamEvent,
    ThinkingLevel, ToolCall, UserContent, UserMessage, get_provider, is_context_overflow, now_ms,
};
use sasacode_plugin_api::{
    CompleteOptions, DeltaKind, HookContext, HookName, HookRunner, InjectWhen, Spend, SpendSource, StopCause,
    Stopped, SubagentControl, SubagentOptions, SubagentResult, ToolRef, hook, spec_of, text,
};

use crate::events::{AgentEvent, EventBus};
use crate::permission::PermissionPolicy;
use crate::session::{SessionEntry, SessionFile};
use crate::tool_exec::{ApproveFn, ExecOptions, Repair, ToolRunContext, execute_tools, repair_calls};
use crate::tool_search::{ToolSearchConfig, loaded_from_history, make_tool_search_tool, should_defer};

pub use crate::tool_exec::{ApprovalAnswer, ApprovalDecision, ApprovalRequest};

pub type GetApiKey = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;
/// Turns "<provider>/<model>" into ModelInfo, for subagents and complete() with another model.
pub type ResolveModel = Arc<dyn Fn(&str) -> anyhow::Result<ModelInfo> + Send + Sync>;

#[derive(Clone)]
pub struct AgentOptions {
    pub model: ModelInfo,
    pub cwd: String,
    pub system_prompt: String,
    pub tools: Vec<ToolRef>,
    pub messages: Vec<Message>,
    pub thinking: Option<ThinkingLevel>,
    pub permissions: Option<Arc<PermissionPolicy>>,
    /// Asks the user about a tool call. Without one, calls that need approval are denied.
    pub approve: Option<ApproveFn>,
    pub get_api_key: Option<GetApiKey>,
    pub resolve_model: Option<ResolveModel>,
    pub session: Option<Arc<SessionFile>>,
    pub hooks: Option<HookRunner>,
    pub tool_search: Option<ToolSearchConfig>,
    /// Unlimited unless set (A7); 0 is unlimited too.
    pub max_turns: Option<u32>,
    pub max_retries: Option<u32>,
}

impl AgentOptions {
    pub fn new(model: ModelInfo, cwd: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        AgentOptions {
            model,
            cwd: cwd.into(),
            system_prompt: system_prompt.into(),
            tools: Vec::new(),
            messages: Vec::new(),
            thinking: None,
            permissions: None,
            approve: None,
            get_api_key: None,
            resolve_model: None,
            session: None,
            hooks: None,
            tool_search: None,
            max_turns: None,
            max_retries: None,
        }
    }
}

const INTERRUPTED_NOTE: &str = "[The user interrupted the previous response.]";
const IMAGE_OMITTED: &str = "[image omitted: this model does not accept images]";
static SUBAGENT_RUNS: AtomicU64 = AtomicU64::new(0);

/// Hooks that belong to the user's session, not to subagents.
const SESSION_HOOKS: [HookName; 7] = [
    HookName::SessionStart,
    HookName::SessionEnd,
    HookName::UserPrompt,
    HookName::TurnEnd,
    HookName::AgentEnd,
    HookName::ContextLimit,
    HookName::Usage,
];

/// JS `string.length`.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Images in a history sent to a model that cannot take them, replaced by a note.
pub fn without_images(messages: &[Message]) -> Vec<Message> {
    let strip = |content: &[UserContent]| -> Vec<UserContent> {
        content
            .iter()
            .map(|c| match c {
                UserContent::Image(_) => UserContent::text(IMAGE_OMITTED),
                other => other.clone(),
            })
            .collect()
    };
    let has_image = |content: &[UserContent]| content.iter().any(|c| matches!(c, UserContent::Image(_)));
    messages
        .iter()
        .map(|m| match m {
            Message::User(u) if has_image(&u.content) => {
                Message::User(UserMessage { content: strip(&u.content), ..u.clone() })
            }
            Message::Tool(t) if has_image(&t.content) => {
                let mut t = t.clone();
                t.content = strip(&t.content);
                Message::Tool(t)
            }
            other => other.clone(),
        })
        .collect()
}

/// `JSON.stringify(v, (k, v) => k === "data" && typeof v === "string" && v.length > 64 ? "" : v)`.
fn blank_data(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if k == "data"
                    && let Value::String(s) = x
                    && js_len(s) > 64
                {
                    *x = Value::String(String::new());
                } else {
                    blank_data(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(blank_data),
        _ => {}
    }
}

/// Characters a message stands for, an image as about 1,600 tokens rather than its base64.
pub fn content_chars(m: &Message) -> usize {
    let (mut v, images) = match m {
        Message::User(u) => (
            serde_json::to_value(&u.content),
            u.content.iter().filter(|c| matches!(c, UserContent::Image(_))).count(),
        ),
        Message::Tool(t) => (
            serde_json::to_value(&t.content),
            t.content.iter().filter(|c| matches!(c, UserContent::Image(_))).count(),
        ),
        Message::Assistant(a) => (serde_json::to_value(&a.content), 0),
    };
    let Ok(v) = v.as_mut() else { return 6400 * images };
    blank_data(v);
    js_len(&sasacode_ai::json::stringify(v)) + 6400 * images
}

/// max_tokens for the next reply: the model's maximum, unless less room is left in the window.
/// Servers such as vLLM reject a request whose input plus max_tokens exceeds the window, and that
/// error reads like a full context. The input is estimated from the last reported usage plus about
/// 4 characters a token for what came after it; None leaves the adapter's default. Usage reported
/// before `trust_from` describes a conversation since replaced and is not used.
pub fn reply_budget(model: &ModelInfo, messages: &[Message], fixed_chars: usize, trust_from: usize) -> Option<u64> {
    let mut used = 0u64;
    let mut from = 0usize;
    for i in (trust_from..messages.len()).rev() {
        if let Message::Assistant(a) = &messages[i]
            && a.usage.input + a.usage.output > 0
        {
            used = a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output;
            from = i + 1;
            break;
        }
    }
    // The system prompt and tool definitions are in the reported usage once there is one.
    let chars = if from > 0 { 0 } else { fixed_chars } + messages[from..].iter().map(content_chars).sum::<usize>();
    let estimate = ((used as f64 + chars as f64 / 4.0) * 1.1).ceil();
    let room = model.context_window as f64 - estimate;
    if room >= model.max_output as f64 {
        return None;
    }
    Some(room.max(1024.0) as u64)
}

/// Room left for the next response before the window counts as full. Proportional for small
/// windows: a local model with an 8K window and a 32K maxOutput would otherwise be "full" at once.
pub fn response_room(model: &ModelInfo) -> u64 {
    model.max_output.min(16_384).min((model.context_window as f64 * 0.15).floor() as u64)
}

type RunFuture = Shared<BoxFuture<'static, Result<StopCause, String>>>;
type HaltFuture = Shared<BoxFuture<'static, ()>>;

struct State {
    messages: Vec<Message>,
    model: ModelInfo,
    thinking: ThinkingLevel,
    cwd: String,
    system_prompt: String,
    permissions: Arc<PermissionPolicy>,
    session: Option<Arc<SessionFile>>,
    max_turns: Option<u32>,
    approve: Option<ApproveFn>,
    tool_search: ToolSearchConfig,
    tools: IndexMap<String, ToolRef>,
    loaded_tools: HashSet<String>,
    abort: Option<CancellationToken>,
    queued: Vec<Vec<UserContent>>,
    interrupted: bool,
    /// Set while halt() waits for the run to stop.
    halted: Option<HaltFuture>,
    running: Option<RunFuture>,
    /// Bumped by replace_messages, so usage figures from before a compaction are not trusted.
    generation: u64,
    /// Messages before this index came from replace_messages: their usage is not this conversation's.
    trust_from: usize,
}

struct Inner {
    events: EventBus,
    hooks: HookRunner,
    opts: AgentOptions,
    search_tool: ToolRef,
    is_subagent: bool,
    tasks: TaskTracker,
    state: Mutex<State>,
}

/// The agent. Clones share one conversation.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

impl Agent {
    pub fn new(opts: AgentOptions) -> Self {
        Self::build(opts, false)
    }

    fn build(mut opts: AgentOptions, is_subagent: bool) -> Self {
        let hooks = opts.hooks.take().unwrap_or_default();
        let events = EventBus::default();
        if !hooks.has_on_error() {
            let ev = events.clone();
            hooks.set_on_error(Arc::new(move |plugin, hook, error| {
                ev.emit(AgentEvent::PluginError {
                    plugin: plugin.to_string(),
                    hook: hook.as_str().to_string(),
                    error: format!("{error:#}"),
                })
            }));
        }
        let tools: IndexMap<String, ToolRef> =
            opts.tools.drain(..).map(|t| (t.name().to_string(), t)).collect();
        let state = State {
            messages: std::mem::take(&mut opts.messages),
            model: opts.model.clone(),
            thinking: opts.thinking.unwrap_or_default(),
            cwd: opts.cwd.clone(),
            system_prompt: opts.system_prompt.clone(),
            permissions: opts.permissions.clone().unwrap_or_default(),
            session: opts.session.clone(),
            max_turns: opts.max_turns,
            approve: opts.approve.clone(),
            tool_search: opts.tool_search.clone().unwrap_or_default(),
            tools,
            loaded_tools: HashSet::new(),
            abort: None,
            queued: Vec::new(),
            interrupted: false,
            halted: None,
            running: None,
            generation: 0,
            trust_from: 0,
        };
        let inner = Arc::new_cyclic(|weak: &Weak<Inner>| {
            // Weak: the agent owns the search tool, which must not own the agent.
            let (w1, w2) = (weak.clone(), weak.clone());
            let search_tool: ToolRef = Arc::new(make_tool_search_tool(
                move || w1.upgrade().map(|i| Agent { inner: i }.deferred_tools()).unwrap_or_default(),
                move |names| {
                    if let Some(i) = w2.upgrade() {
                        let mut st = i.state.lock().unwrap_or_else(|e| e.into_inner());
                        st.loaded_tools.extend(names);
                    }
                },
            ));
            Inner { events, hooks, opts, search_tool, is_subagent, tasks: TaskTracker::new(), state: Mutex::new(state) }
        });
        Agent { inner }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── accessors ─────────────────────────────────────────────────────

    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }
    pub fn hooks(&self) -> &HookRunner {
        &self.inner.hooks
    }
    pub fn messages(&self) -> Vec<Message> {
        self.st().messages.clone()
    }
    pub fn model(&self) -> ModelInfo {
        self.st().model.clone()
    }
    pub fn thinking(&self) -> ThinkingLevel {
        self.st().thinking
    }
    pub fn set_thinking(&self, t: ThinkingLevel) {
        self.st().thinking = t;
    }
    pub fn cwd(&self) -> String {
        self.st().cwd.clone()
    }
    pub fn set_cwd(&self, cwd: impl Into<String>) {
        self.st().cwd = cwd.into();
    }
    pub fn system_prompt(&self) -> String {
        self.st().system_prompt.clone()
    }
    pub fn set_system_prompt(&self, p: impl Into<String>) {
        self.st().system_prompt = p.into();
    }
    pub fn permissions(&self) -> Arc<PermissionPolicy> {
        self.st().permissions.clone()
    }
    pub fn set_permissions(&self, p: Arc<PermissionPolicy>) {
        self.st().permissions = p;
    }
    pub fn session(&self) -> Option<Arc<SessionFile>> {
        self.st().session.clone()
    }
    pub fn set_session(&self, s: Option<Arc<SessionFile>>) {
        self.st().session = s;
    }
    pub fn max_turns(&self) -> Option<u32> {
        self.st().max_turns
    }
    pub fn set_max_turns(&self, n: Option<u32>) {
        self.st().max_turns = n;
    }
    pub fn approve(&self) -> Option<ApproveFn> {
        self.st().approve.clone()
    }
    pub fn set_approve(&self, a: Option<ApproveFn>) {
        self.st().approve = a;
    }
    pub fn tool_search(&self) -> ToolSearchConfig {
        self.st().tool_search.clone()
    }
    pub fn set_tool_search(&self, c: ToolSearchConfig) {
        self.st().tool_search = c;
    }
    pub fn subagent(&self) -> bool {
        self.inner.is_subagent
    }
    pub fn is_running(&self) -> bool {
        self.st().running.is_some()
    }

    pub fn set_tool(&self, tool: ToolRef) {
        self.st().tools.insert(tool.name().to_string(), tool);
    }
    pub fn remove_tool(&self, name: &str) {
        self.st().tools.shift_remove(name);
    }
    pub fn get_tools(&self) -> Vec<ToolRef> {
        self.st().tools.values().cloned().collect()
    }

    pub fn set_model(&self, model: ModelInfo) -> anyhow::Result<()> {
        let spec = model.spec();
        let session = {
            let mut st = self.st();
            st.model = model;
            st.session.clone()
        };
        if let Some(s) = session {
            s.append(&SessionEntry::Model { model: spec })?;
        }
        Ok(())
    }

    /// Waits for fire-and-forget work (usage hooks, runs restarted by queued input) to finish.
    pub async fn wait_for_tasks(&self) {
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
        self.inner.tasks.reopen();
    }

    // ── conversation control ──────────────────────────────────────────

    /// Send user input. While a run is in progress it is queued and delivered at the next turn
    /// (FR-L05). `accepted` gets the content the user_prompt hooks made of it (None: a plugin
    /// handled it), the same blocks the user message will hold.
    pub async fn prompt(
        &self,
        input: Vec<UserContent>,
        accepted: Option<Box<dyn FnOnce(Option<Vec<UserContent>>) + Send>>,
    ) -> anyhow::Result<StopCause> {
        // Sent while a halt waits for the run to stop: it belongs after the halt, not to the
        // stopping run. (Input sent before the halt was queued for that run, and the halt drops it.)
        let halting = self.st().halted.clone();
        let mut content = input;
        let mut handled = false;
        let ev = sasacode_plugin_api::UserPromptEvent { content: content.clone() };
        self.inner
            .hooks
            .run::<hook::UserPrompt>(ev, |r, _, _| {
                if let Some(c) = r.content {
                    content = c;
                }
                if r.handled {
                    handled = true;
                }
                !handled
            })
            .await;
        if let Some(f) = accepted {
            f(if handled { None } else { Some(content.clone()) });
        }
        if handled {
            return Ok(StopCause::Done);
        }
        // Known before the first message is written: a key pasted into it is redacted too.
        let (provider, session) = {
            let st = self.st();
            (st.model.provider.clone(), st.session.clone())
        };
        if let Some(s) = session {
            let key = self.api_key(&provider).await;
            s.add_secret(key.as_deref());
        }
        if let Some(h) = halting {
            h.await;
        }
        let run = {
            let mut st = self.st();
            match st.running.clone() {
                Some(r) => {
                    st.queued.push(content);
                    r
                }
                None => self.start_locked(&mut st, vec![content]),
            }
        };
        run.await.map_err(anyhow::Error::msg)
    }

    pub async fn prompt_text(&self, input: &str) -> anyhow::Result<StopCause> {
        self.prompt(vec![text(input)], None).await
    }

    /// Continue without new input, e.g. after resuming a session.
    pub async fn continue_run(&self) -> anyhow::Result<StopCause> {
        let run = {
            let mut st = self.st();
            match st.running.clone() {
                Some(r) => r,
                None => self.start_locked(&mut st, Vec::new()),
            }
        };
        run.await.map_err(anyhow::Error::msg)
    }

    /// Add a user message from a plugin: Next waits for the next turn or prompt, Now also starts a run.
    pub fn inject(&self, content: Vec<UserContent>, when: InjectWhen) {
        let mut st = self.st();
        if let Some(h) = st.halted.clone() {
            drop(st);
            let me = self.clone();
            self.inner.tasks.spawn(async move {
                h.await;
                me.inject(content, when);
            });
            return;
        }
        if st.running.is_some() || when == InjectWhen::Next {
            st.queued.push(content);
        } else {
            // Spawned: it runs whether or not anyone awaits it.
            let _ = self.start_locked(&mut st, vec![content]);
        }
    }

    /// Swap the conversation (compaction, fork). Persisted as a replace entry.
    pub fn replace_messages(&self, messages: Vec<Message>) -> anyhow::Result<()> {
        let session = {
            let mut st = self.st();
            st.messages = messages.clone();
            st.generation += 1;
            st.trust_from = messages.len();
            st.session.clone()
        };
        if let Some(s) = session {
            s.append(&SessionEntry::Replace { messages })?;
        }
        self.inner.events.emit(AgentEvent::MessagesReplaced);
        Ok(())
    }

    /// Another conversation (new session, resume, fork): input queued for the old one and its
    /// interrupted note do not carry over.
    pub fn reset(&self, messages: Vec<Message>) {
        let mut st = self.st();
        st.messages = messages;
        st.queued.clear();
        st.interrupted = false;
        st.generation += 1;
        st.trust_from = 0;
    }

    pub fn abort(&self) {
        if let Some(t) = &self.st().abort {
            t.cancel();
        }
    }

    /// Abort and drop queued input, so nothing more runs on this conversation (before switching sessions).
    pub async fn halt(&self) {
        let h = {
            let mut st = self.st();
            st.queued.clear();
            if let Some(t) = &st.abort {
                t.cancel();
            }
            match st.halted.clone() {
                Some(h) => h,
                None => {
                    let me = self.clone();
                    let task = self.inner.tasks.spawn(async move {
                        me.wait_for_idle().await;
                        let mut st = me.st();
                        st.halted = None;
                        st.queued.clear();
                    });
                    let h: HaltFuture = async move {
                        let _ = task.await;
                    }
                    .boxed()
                    .shared();
                    st.halted = Some(h.clone());
                    h
                }
            }
        };
        h.await;
    }

    pub async fn wait_for_idle(&self) {
        loop {
            let r = self.st().running.clone();
            match r {
                Some(r) => {
                    let _ = r.await;
                }
                None => break,
            }
        }
    }

    /// Starts a run (the caller holds the lock and found none running). The task pushes `initial`
    /// as user messages before agent_start, as the TS did before calling start().
    fn start_locked(&self, st: &mut State, initial: Vec<Vec<UserContent>>) -> RunFuture {
        let token = CancellationToken::new();
        st.abort = Some(token.clone());
        let me = self.clone();
        let handle = self.inner.tasks.spawn(async move {
            for c in initial {
                if let Err(e) = me.push_user(c) {
                    me.end_run();
                    return Err(format!("{e:#}"));
                }
            }
            let cause = me.run(token).await;
            me.end_run();
            Ok(cause)
        });
        let fut: RunFuture = async move {
            match handle.await {
                Ok(r) => r,
                Err(e) => Err(format!("the run failed: {e}")),
            }
        }
        .boxed()
        .shared();
        st.running = Some(fut.clone());
        fut
    }

    /// The run is over: input that arrived as it was finishing starts the next one.
    fn end_run(&self) {
        let mut st = self.st();
        st.running = None;
        if st.halted.is_none() && !st.queued.is_empty() {
            let queued = std::mem::take(&mut st.queued);
            let _ = self.start_locked(&mut st, queued);
        }
    }

    fn push_user(&self, content: Vec<UserContent>) -> anyhow::Result<()> {
        let content = {
            let mut st = self.st();
            if st.interrupted {
                st.interrupted = false;
                let mut c = vec![text(INTERRUPTED_NOTE)];
                c.extend(content);
                c
            } else {
                content
            }
        };
        self.push_message(Message::User(UserMessage { content, timestamp: now_ms() }))
    }

    fn push_message(&self, m: Message) -> anyhow::Result<()> {
        let session = {
            let mut st = self.st();
            st.messages.push(m.clone());
            st.session.clone()
        };
        if let Some(s) = session {
            s.append(&SessionEntry::Message { message: m.clone() })?;
        }
        self.inner.events.emit(AgentEvent::MessageEnd { message: m });
        Ok(())
    }

    /// A response that is not in the conversation still cost tokens: logged, and (`emit`) reported
    /// to the UI and plugins.
    fn spend(&self, s: Spend, emit: bool) -> anyhow::Result<()> {
        if let Some(session) = self.session() {
            session.append(&SessionEntry::Usage(s.clone()))?;
        }
        if !emit {
            return Ok(());
        }
        self.inner.events.emit(AgentEvent::Usage {
            t: s.t,
            model: s.model.clone(),
            usage: s.usage.clone(),
            source: s.source,
        });
        let hooks = self.inner.hooks.clone();
        self.inner.tasks.spawn(async move { hooks.emit::<hook::Usage>(s).await });
        Ok(())
    }

    fn spent(&self, m: &AssistantMessage, source: SpendSource, emit: bool) -> anyhow::Result<()> {
        let s = Spend { t: m.timestamp, model: format!("{}/{}", m.provider, m.model), usage: m.usage.clone(), source };
        self.spend(s, emit)
    }

    /// Stores a reply. One with nothing to keep stays out of the conversation, but the UI counts it
    /// from message_end.
    fn keep(&self, m: AssistantMessage) -> anyhow::Result<()> {
        if !m.content.is_empty() {
            return self.push_message(Message::Assistant(m));
        }
        self.spent(&m, SpendSource::Discarded, false)?;
        self.inner.events.emit(AgentEvent::MessageEnd { message: Message::Assistant(m) });
        Ok(())
    }

    fn drain_queue(&self) -> anyhow::Result<bool> {
        let queued = std::mem::take(&mut self.st().queued);
        if queued.is_empty() {
            return Ok(false);
        }
        for c in queued {
            self.push_user(c)?;
        }
        Ok(true)
    }

    fn queue_text(&self, t: String) {
        self.st().queued.push(vec![text(t)]);
    }

    async fn run(&self, signal: CancellationToken) -> StopCause {
        self.inner.events.emit(AgentEvent::AgentStart);
        let mut cause = StopCause::Done;
        let mut stopped: Option<Stopped> = None;
        let mut error: Option<String> = None;
        if let Err(e) = self.turns(&signal, &mut cause, &mut stopped).await {
            let msg = format!("{e:#}");
            self.inner.events.emit(AgentEvent::Error { error: msg.clone() });
            error = Some(msg);
            cause = StopCause::Error;
        }
        self.st().abort = None;
        let ev = sasacode_plugin_api::AgentEndEvent { cause, stopped: stopped.clone(), error: error.clone() };
        self.inner
            .hooks
            .run::<hook::AgentEnd>(ev, |r, _, _| {
                if let Some(i) = r.inject.filter(|i| !i.is_empty()) {
                    self.queue_text(i);
                }
                true
            })
            .await;
        self.inner.events.emit(AgentEvent::AgentEnd { cause, stopped, error });
        cause
    }

    async fn turns(
        &self,
        signal: &CancellationToken,
        cause: &mut StopCause,
        stopped: &mut Option<Stopped>,
    ) -> anyhow::Result<()> {
        let mut turn: u32 = 0;
        let mut limit_retries: u32 = 0;
        loop {
            if let Some(max) = self.max_turns()
                && max > 0
                && turn >= max
            {
                *cause = StopCause::MaxTurns;
                return Ok(());
            }
            turn += 1;
            self.inner.events.emit(AgentEvent::TurnStart { turn });
            let (msg, repairs) = match self.respond(signal).await {
                Ok(r) => r,
                Err(e) if is_context_overflow(&e) => {
                    let retries = limit_retries;
                    limit_retries += 1;
                    if self.context_limit(-1, retries).await {
                        continue;
                    }
                    *cause = StopCause::ContextLimit;
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            let calls: Vec<ToolCall> = msg.tool_calls().cloned().collect();
            if msg.stop_reason == StopReason::Aborted {
                *cause = StopCause::Aborted;
                return Ok(());
            }
            if msg.stop_reason == StopReason::Refusal {
                *cause = StopCause::Refusal;
                return Ok(());
            }
            if !calls.is_empty() {
                let me = self.clone();
                execute_tools(
                    self.tool_context(),
                    calls.clone(),
                    ExecOptions {
                        truncated: msg.stop_reason == StopReason::MaxTokens,
                        signal: signal.clone(),
                        repairs,
                        on_result: Arc::new(move |r| me.push_message(Message::Tool(r))),
                        ancestors: Vec::new(),
                        parent_call_id: None,
                        on_tool_result: None,
                    },
                )
                .await?;
            }
            self.inner.events.emit(AgentEvent::TurnEnd { turn });
            if signal.is_cancelled() {
                self.st().interrupted = true;
                *cause = StopCause::Aborted;
                return Ok(());
            }
            let mut injected = false;
            let gen_before = self.st().generation;
            let ev = sasacode_plugin_api::TurnEndEvent { turn, message: msg.clone() };
            self.inner
                .hooks
                .run::<hook::TurnEnd>(ev, |r, _, plugin| {
                    if let Some(reason) = r.stop.filter(|s| !s.is_empty()) {
                        *stopped = Some(Stopped { plugin: plugin.to_string(), reason });
                        return false;
                    }
                    if let Some(i) = r.inject.filter(|i| !i.is_empty()) {
                        self.queue_text(i);
                        injected = true;
                    }
                    true
                })
                .await;
            if stopped.is_some() {
                *cause = StopCause::Stopped;
                return Ok(());
            }
            let used = msg.usage.input + msg.usage.cache_read + msg.usage.cache_write + msg.usage.output;
            let (generation, model) = {
                let st = self.st();
                (st.generation, st.model.clone())
            };
            if gen_before == generation && used as i64 >= model.context_window as i64 - response_room(&model) as i64 {
                let retries = limit_retries;
                limit_retries += 1;
                if self.context_limit(used as i64, retries).await {
                    continue;
                }
                *cause = StopCause::ContextLimit;
                return Ok(());
            }
            limit_retries = 0;
            if !self.drain_queue()? && calls.is_empty() && !injected {
                return Ok(());
            }
        }
    }

    /// Emits the event and lets plugins free up context. True when the loop should continue.
    async fn context_limit(&self, tokens: i64, retries: u32) -> bool {
        let context_window = self.st().model.context_window;
        self.inner.events.emit(AgentEvent::ContextLimit { tokens, context_window });
        if retries >= 2 {
            return false;
        }
        let mut retry = false;
        let ev = sasacode_plugin_api::ContextLimitEvent { tokens, context_window };
        self.inner
            .hooks
            .run::<hook::ContextLimit>(ev, |r, _, _| {
                if r.retry {
                    retry = true;
                }
                true
            })
            .await;
        retry
    }

    // ── requests ──────────────────────────────────────────────────────

    fn deferred_tools(&self) -> Vec<ToolRef> {
        self.get_tools().into_iter().filter(|t| !t.always_load()).collect()
    }

    /// Tools sent with a request: everything, or the always-loaded ones plus tool_search when deferring (5.4).
    pub fn request_tools(&self) -> Vec<ToolRef> {
        let all = self.get_tools();
        let (model, cfg) = {
            let st = self.st();
            (st.model.clone(), st.tool_search.clone())
        };
        let deferred: Vec<ToolRef> = all.iter().filter(|t| !t.always_load()).cloned().collect();
        if !should_defer(&deferred, &model, &cfg) {
            return all;
        }
        let mut loaded = {
            let st = self.st();
            let mut l = loaded_from_history(&st.messages);
            l.extend(st.loaded_tools.iter().cloned());
            l
        };
        loaded.shrink_to_fit();
        let mut out: Vec<ToolRef> =
            all.into_iter().filter(|t| t.always_load() || loaded.contains(t.name())).collect();
        out.push(self.inner.search_tool.clone());
        out
    }

    async fn api_key(&self, provider: &str) -> Option<String> {
        match &self.inner.opts.get_api_key {
            Some(f) => f(provider.to_string()).await,
            None => None,
        }
    }

    /// One model response, settled: streamed (stream_delta may stop it), passed through
    /// assistant_message (which may rewrite it, ask again, or add a follow-up), its tool calls
    /// repaired (tool_call_raw), and stored.
    async fn respond(
        &self,
        signal: &CancellationToken,
    ) -> anyhow::Result<(AssistantMessage, std::collections::HashMap<String, Repair>)> {
        let mut attempt = 0u32;
        let mut cut = 0u32;
        loop {
            let (mut message, stopped) = self.stream(signal).await?;
            // The connection dropped mid-reply: ask again (after 1, 2, 4 s) rather than take a fragment as the answer.
            if message.stop_reason == StopReason::Error && !signal.is_cancelled() {
                if cut >= 3 {
                    anyhow::bail!("{}", message.error_message.clone().unwrap_or_else(|| "the reply failed".into()));
                }
                self.spent(&message, SpendSource::Discarded, true)?;
                self.inner.events.emit(AgentEvent::MessageDiscarded { message });
                let wait = Duration::from_millis(1000 * 2u64.pow(cut));
                cut += 1;
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = signal.cancelled() => {}
                }
                continue;
            }
            let user_aborted = message.stop_reason == StopReason::Aborted;
            if user_aborted || stopped.is_some() {
                // Half-streamed tool calls never ran and unsigned thinking cannot be replayed; keep what was finished.
                message.content.retain(|c| match c {
                    AssistantContent::Text(_) => true,
                    AssistantContent::Thinking(t) => t.signature.as_deref().is_some_and(|s| !s.is_empty()),
                    AssistantContent::ToolCall(_) => false,
                });
            }
            // A refusal ends the run before its tool calls run: kept, they would be calls without
            // results, which every API rejects (#97).
            if message.stop_reason == StopReason::Refusal {
                message.content.retain(|c| !matches!(c, AssistantContent::ToolCall(_)));
            }
            if user_aborted {
                self.st().interrupted = true;
                self.keep(message.clone())?;
                return Ok((message, Default::default()));
            }
            let mut fin = message.clone();
            let mut retry = false;
            let mut inject: Option<String> = None;
            let ev = sasacode_plugin_api::AssistantMessageEvent { message, stopped };
            self.inner
                .hooks
                .run::<hook::AssistantMessage>(ev, |r, ev, _| {
                    if let Some(m) = r.message {
                        fin = m.clone();
                        ev.message = m;
                    }
                    if r.retry {
                        retry = true;
                    }
                    if let Some(i) = r.inject.filter(|i| !i.is_empty()) {
                        inject = Some(i);
                    }
                    true
                })
                .await;
            if retry && attempt < 2 && !signal.is_cancelled() {
                self.spent(&fin, SpendSource::Discarded, true)?;
                self.inner.events.emit(AgentEvent::MessageDiscarded { message: fin });
                attempt += 1;
                continue;
            }
            let repairs = repair_calls(&self.tool_context(), &mut fin).await?;
            self.keep(fin.clone())?;
            if let Some(i) = inject {
                self.queue_text(i);
            }
            return Ok((fin, repairs));
        }
    }

    /// Stream one response. A stream_delta handler can end it early; that is reported as `stopped`.
    async fn stream(&self, signal: &CancellationToken) -> anyhow::Result<(AssistantMessage, Option<Stopped>)> {
        let (model, mut system, thinking, session) = {
            let st = self.st();
            (st.model.clone(), st.system_prompt.clone(), st.thinking, st.session.clone())
        };
        let provider = get_provider(&model.api)?;
        let api_key = self.api_key(&model.provider).await;
        if let Some(s) = &session {
            s.add_secret(api_key.as_deref());
        }
        let hooks = &self.inner.hooks;
        hooks
            .run::<hook::SystemPrompt>(sasacode_plugin_api::SystemPromptEvent { prompt: system.clone() }, |r, ev, _| {
                if let Some(p) = r.prompt {
                    system = p.clone();
                    ev.prompt = p;
                }
                true
            })
            .await;
        let (mut messages, own_len, own_trust) = {
            let st = self.st();
            (st.messages.clone(), st.messages.len(), st.trust_from)
        };
        let mut sampling = SamplingOptions::default();
        let mut request_fetch = None;
        let ev = sasacode_plugin_api::BeforeRequestEvent {
            messages: messages.clone(),
            model: model.clone(),
            sampling: sampling.clone(),
            fetch: None,
        };
        hooks
            .run::<hook::BeforeRequest>(ev, |r, ev, _| {
                if let Some(m) = r.messages {
                    messages = m.clone();
                    ev.messages = m;
                }
                if let Some(s) = r.sampling {
                    sampling = sampling.merged(&s);
                    ev.sampling = sampling.clone();
                }
                if let Some(f) = r.fetch {
                    request_fetch = Some(f.clone());
                    ev.fetch = Some(f);
                }
                true
            })
            .await;

        // A model that takes no images gets a note instead: one image would fail every request after it.
        if model.images == Some(false) {
            messages = without_images(&messages);
        }
        let tools: Vec<sasacode_ai::ToolSpec> = self.request_tools().iter().map(|t| spec_of(t.as_ref())).collect();
        // Counted from the end: a before_request hook may have dropped messages at the front.
        let trust_from = messages.len().saturating_sub(own_len.saturating_sub(own_trust));
        let fixed = js_len(&system) + js_len(&sasacode_ai::json::stringify(&tools));
        let max_tokens = reply_budget(&model, &messages, fixed, trust_from);

        // The request has its own token so a plugin can stop it without it counting as a user abort.
        // A child of a cancelled token starts cancelled (#96).
        let request = signal.child_token();
        let watch_deltas = hooks.has(HookName::StreamDelta);
        let mut tool_json: std::collections::HashMap<usize, String> = Default::default();
        let mut stopped: Option<Stopped> = None;
        let mut fin: Option<AssistantMessage> = None;
        let mut stream = provider.stream(Request {
            model: model.clone(),
            api_key,
            system,
            messages,
            tools,
            thinking: Some(thinking),
            max_tokens,
            max_retries: self.inner.opts.max_retries,
            sampling: Some(sampling),
            signal: request.clone(),
            fetch: request_fetch,
        });
        while let Some(ev) = stream.next().await {
            let ev = ev?;
            match ev {
                StreamEvent::Start { partial } => self.inner.events.emit(AgentEvent::MessageStart { message: partial }),
                StreamEvent::Done { message } => fin = Some(message),
                other => {
                    // INFALLIBLE: every event but Done carries a partial message.
                    let partial = other.partial().cloned().expect("partial");
                    self.inner.events.emit(AgentEvent::MessageUpdate { message: partial.clone(), event: other.clone() });
                    let delta = match &other {
                        StreamEvent::TextDelta { index, delta, .. } => Some((DeltaKind::Text, *index, delta.clone())),
                        StreamEvent::ThinkingDelta { index, delta, .. } => {
                            Some((DeltaKind::Thinking, *index, delta.clone()))
                        }
                        StreamEvent::ToolcallDelta { index, delta, .. } => {
                            Some((DeltaKind::Toolcall, *index, delta.clone()))
                        }
                        _ => None,
                    };
                    if let Some((kind, index, delta)) = delta
                        && watch_deltas
                        && stopped.is_none()
                    {
                        if kind == DeltaKind::Toolcall {
                            tool_json.entry(index).or_default().push_str(&delta);
                        }
                        let text = match partial.content.get(index) {
                            Some(AssistantContent::Text(t)) => t.text.clone(),
                            Some(AssistantContent::Thinking(t)) => t.thinking.clone(),
                            _ => tool_json.get(&index).cloned().unwrap_or_default(),
                        };
                        let mut ev = sasacode_plugin_api::StreamDeltaEvent { kind, index, delta, text, message: partial };
                        hooks.run_sync::<hook::StreamDelta>(&mut ev, |r, _, owner| {
                            let Some(reason) = r.stop.filter(|s| !s.is_empty()) else { return true };
                            stopped = Some(Stopped { plugin: owner.to_string(), reason });
                            request.cancel();
                            false
                        });
                    }
                }
            }
        }
        let Some(mut fin) = fin else { anyhow::bail!("provider stream ended without a final message") };
        if stopped.is_some() && !signal.is_cancelled() {
            fin.stop_reason = StopReason::Stopped;
        } else {
            stopped = None;
        }
        Ok((fin, stopped))
    }

    fn find_tool(&self, name: &str) -> Option<ToolRef> {
        let found = self.st().tools.get(name).cloned();
        found.or_else(|| (name == self.inner.search_tool.name()).then(|| self.inner.search_tool.clone()))
    }

    fn tool_context(&self) -> Arc<ToolRunContext> {
        let st = self.st();
        let (find, all) = (self.clone(), self.clone());
        Arc::new(ToolRunContext {
            cwd: st.cwd.clone(),
            hooks: self.inner.hooks.clone(),
            events: self.inner.events.clone(),
            permissions: st.permissions.clone(),
            approve: st.approve.clone(),
            session: st.session.clone(),
            find_tool: Arc::new(move |n| find.find_tool(n)),
            all_tools: Arc::new(move || {
                let mut v = all.get_tools();
                v.push(all.inner.search_tool.clone());
                v
            }),
        })
    }

    // ── plugin services ───────────────────────────────────────────────

    fn resolve(&self, spec: &str) -> anyhow::Result<ModelInfo> {
        match &self.inner.opts.resolve_model {
            Some(f) => f(spec),
            None => anyhow::bail!("model resolution is not available"),
        }
    }

    /// One model call without tools, its max_tokens kept within the room left in that model's
    /// window (#60), estimated from what it sends: usage in o.messages was another request's.
    pub async fn complete(&self, o: CompleteOptions) -> anyhow::Result<String> {
        let model = match &o.model {
            Some(m) => self.resolve(m)?,
            None => self.model(),
        };
        let budget = reply_budget(&model, &o.messages, js_len(&o.system), o.messages.len()).unwrap_or(model.max_output);
        let max_tokens = o.max_tokens.unwrap_or(8192).min(budget);
        let mut stream = get_provider(&model.api)?.stream(Request {
            model: model.clone(),
            api_key: self.api_key(&model.provider).await,
            system: o.system,
            messages: o.messages,
            tools: Vec::new(),
            thinking: Some(ThinkingLevel::Low),
            max_tokens: Some(max_tokens),
            max_retries: self.inner.opts.max_retries,
            sampling: None,
            signal: o.signal.unwrap_or_default(),
            fetch: None,
        });
        let mut out = String::new();
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Done { message } = ev? {
                self.spent(&message, SpendSource::Side, true)?;
                if message.stop_reason == StopReason::Aborted {
                    anyhow::bail!("aborted");
                }
                // A stream cut before its end is a fragment: a compaction would replace the whole
                // history with it (#95).
                if message.stop_reason == StopReason::Error {
                    anyhow::bail!("{}", message.error_message.clone().unwrap_or_else(|| "the reply failed".into()));
                }
                out = message.text();
            }
        }
        Ok(out)
    }

    /// An independent loop sharing tools, permissions, approvals and tool hooks, with its own history.
    pub async fn run_subagent(&self, o: SubagentOptions) -> anyhow::Result<SubagentResult> {
        let exclude: HashSet<&String> = o.exclude_tools.iter().collect();
        let tools: Vec<ToolRef> = self
            .get_tools()
            .into_iter()
            .filter(|t| match &o.tools {
                Some(names) => names.iter().any(|n| n == t.name()),
                None => !exclude.contains(&t.name().to_string()),
            })
            .collect();
        let model = match &o.model {
            Some(m) => self.resolve(m)?,
            None => self.model(),
        };
        let n = SUBAGENT_RUNS.fetch_add(1, Ordering::Relaxed) + 1;
        let (system_prompt, cwd, thinking, permissions, approve, tool_search) = {
            let st = self.st();
            (
                o.system_prompt.clone().unwrap_or_else(|| st.system_prompt.clone()),
                st.cwd.clone(),
                st.thinking,
                st.permissions.clone(),
                st.approve.clone(),
                st.tool_search.clone(),
            )
        };
        let mut opts = self.inner.opts.clone();
        opts.model = model;
        opts.cwd = cwd;
        opts.system_prompt = system_prompt;
        opts.tools = tools;
        opts.messages = Vec::new();
        opts.thinking = Some(thinking);
        opts.permissions = Some(permissions);
        opts.approve = approve;
        opts.session = None;
        opts.hooks =
            Some(self.inner.hooks.without(&SESSION_HOOKS, HookContext { agent: Some(format!("sub-{n}")) }));
        opts.tool_search = Some(tool_search);
        let child = Agent::build(opts, true);

        let error: Arc<Mutex<Option<String>>> = Arc::default();
        let sub = {
            let parent = self.clone();
            let weak = Arc::downgrade(&child.inner);
            let (signal, on_progress, error) = (o.signal.clone(), o.on_progress.clone(), error.clone());
            child.events().on(move |e| match e {
                AgentEvent::ToolStart { call, summary, .. } => {
                    if let Some(f) = &on_progress {
                        f(format!("{}({summary})", call.name));
                    }
                }
                // Aborted before its run had a token to cancel (#125).
                AgentEvent::AgentStart => {
                    if signal.as_ref().is_some_and(|s| s.is_cancelled())
                        && let Some(i) = weak.upgrade()
                    {
                        Agent { inner: i }.abort();
                    }
                }
                AgentEvent::PluginError { .. } => parent.inner.events.emit(e.clone()),
                AgentEvent::Error { error: msg } => {
                    *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.clone());
                }
                // The child keeps no log: what it used is the parent's.
                AgentEvent::MessageEnd { message: Message::Assistant(m) } => {
                    let _ = parent.spent(m, SpendSource::Subagent, true);
                }
                AgentEvent::Usage { t, model, usage, .. } => {
                    let _ = parent.spend(
                        Spend { t: *t, model: model.clone(), usage: usage.clone(), source: SpendSource::Subagent },
                        true,
                    );
                }
                _ => {}
            })
        };
        let watcher = o.signal.clone().map(|s| {
            let weak = Arc::downgrade(&child.inner);
            tokio::spawn(async move {
                s.cancelled().await;
                if let Some(i) = weak.upgrade() {
                    Agent { inner: i }.abort();
                }
            })
        });
        if let Some(on_start) = &o.on_start {
            let (w1, w2) = (Arc::downgrade(&child.inner), Arc::downgrade(&child.inner));
            on_start(SubagentControl {
                send: Arc::new(move |t: String| {
                    if let Some(i) = w1.upgrade() {
                        Agent { inner: i }.inject(vec![text(t)], InjectWhen::Next);
                    }
                }),
                stop: Arc::new(move || {
                    if let Some(i) = w2.upgrade() {
                        Agent { inner: i }.abort();
                    }
                }),
            });
        }
        let result = child.prompt(vec![text(o.prompt.clone())], None).await;
        if let Some(w) = watcher {
            w.abort();
        }
        sub.off();
        let cause = result?;
        let messages = child.messages();
        // The last thing it said, even when it stopped in the middle of tool calls.
        let text = messages
            .iter()
            .rev()
            .map(|m| match m {
                Message::Assistant(a) => a.text().trim().to_string(),
                _ => String::new(),
            })
            .find(|t| !t.is_empty())
            .unwrap_or_default();
        let error = error.lock().unwrap_or_else(|e| e.into_inner()).take();
        Ok(SubagentResult { text, messages, cause, error })
    }
}
