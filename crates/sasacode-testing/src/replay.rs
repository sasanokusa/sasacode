//! Test support: record a provider's replies and replay them, so loops and plugins run end to end
//! without a model (`packages/testing/src/index.ts`). Not part of the core.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use sasacode_ai::{
    AssistantContent, AssistantMessage, JsonObject, Provider, Request, StopReason, StreamEvent, TextContent,
    ThinkingContent, ToolCall,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{utf16_cuts, utf16_len};

/// What the recorded request was: enough to tell recordings apart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedRequest {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub message_count: usize,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    TextDelta,
    ThinkingDelta,
    ToolcallDelta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    #[serde(rename = "type")]
    pub kind: DeltaKind,
    pub index: usize,
    pub delta: String,
}

/// Recording format: one JSON line per request, holding the final assistant message plus the deltas
/// that produced it. Replaying needs no network, so loops and plugins can be tested end to end
/// without an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub request: RecordedRequest,
    #[serde(default)]
    pub deltas: Vec<Delta>,
    #[serde(with = "assistant_with_role")]
    pub message: AssistantMessage,
}

/// The message as the TS wrote it, `{"role":"assistant",...}`; reading ignores the role.
mod assistant_with_role {
    use sasacode_ai::AssistantMessage;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize)]
    #[serde(tag = "role", rename_all = "snake_case")]
    enum Tagged<'a> {
        Assistant(&'a AssistantMessage),
    }

    pub fn serialize<S: Serializer>(m: &AssistantMessage, s: S) -> Result<S::Ok, S::Error> {
        Tagged::Assistant(m).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<AssistantMessage, D::Error> {
        AssistantMessage::deserialize(d)
    }
}

/// One scripted response: a recording, or a bare assistant message for hand-written scripts.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayItem {
    Recording(Recording),
    Message(AssistantMessage),
}

impl From<Recording> for ReplayItem {
    fn from(r: Recording) -> Self {
        ReplayItem::Recording(r)
    }
}

impl From<AssistantMessage> for ReplayItem {
    fn from(m: AssistantMessage) -> Self {
        ReplayItem::Message(m)
    }
}

impl ReplayItem {
    /// One line of a recording file: a bare assistant message has a `role`, a recording does not.
    pub fn parse(line: &str) -> anyhow::Result<ReplayItem> {
        let value: Value = serde_json::from_str(line)?;
        Ok(if value.get("role").is_some() {
            ReplayItem::Message(serde_json::from_value(value)?)
        } else {
            ReplayItem::Recording(serde_json::from_value(value)?)
        })
    }

    fn into_recording(self) -> Recording {
        match self {
            ReplayItem::Recording(r) => r,
            ReplayItem::Message(message) => Recording {
                request: RecordedRequest::default(),
                deltas: Vec::new(),
                message,
            },
        }
    }
}

/// Wraps `inner` and appends a [`Recording`] line to `file` for each reply it finishes.
pub struct RecordingProvider {
    inner: Arc<dyn Provider>,
    file: PathBuf,
}

impl RecordingProvider {
    pub fn new(inner: Arc<dyn Provider>, file: impl Into<PathBuf>) -> Self {
        RecordingProvider {
            inner,
            file: file.into(),
        }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn api(&self) -> &str {
        self.inner.api()
    }

    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        let request = RecordedRequest {
            model: req.model.id.clone(),
            message_count: req.messages.len(),
            tools: req.tools.iter().map(|t| t.name.clone()).collect(),
        };
        let file = self.file.clone();
        self.inner
            .stream(req)
            .scan((Vec::<Delta>::new(), false), move |(deltas, failed), item| {
                if *failed {
                    return futures::future::ready(None);
                }
                if let Ok(ev) = &item {
                    match ev {
                        StreamEvent::TextDelta { index, delta, .. } => deltas.push(Delta {
                            kind: DeltaKind::TextDelta,
                            index: *index,
                            delta: delta.clone(),
                        }),
                        StreamEvent::ThinkingDelta { index, delta, .. } => deltas.push(Delta {
                            kind: DeltaKind::ThinkingDelta,
                            index: *index,
                            delta: delta.clone(),
                        }),
                        StreamEvent::ToolcallDelta { index, delta, .. } => deltas.push(Delta {
                            kind: DeltaKind::ToolcallDelta,
                            index: *index,
                            delta: delta.clone(),
                        }),
                        StreamEvent::Done { message } => {
                            let rec = Recording {
                                request: request.clone(),
                                deltas: std::mem::take(deltas),
                                message: message.clone(),
                            };
                            if let Err(e) = append_line(&file, &rec) {
                                *failed = true;
                                return futures::future::ready(Some(Err(e)));
                            }
                        }
                        _ => {}
                    }
                }
                futures::future::ready(Some(item))
            })
            .boxed()
    }
}

fn append_line(file: &Path, rec: &Recording) -> anyhow::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .with_context(|| format!("could not open {}", file.display()))?;
    f.write_all(format!("{}\n", serde_json::to_string(rec)?).as_bytes())?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    /// The api name the provider answers to (default "replay").
    pub api: Option<String>,
    /// For responses without recorded deltas: stream text, thinking and tool-call JSON in chunks of
    /// this many characters (UTF-16 units, as the TS counted; default: one delta per block).
    pub chunk_size: Option<usize>,
}

impl ReplayOptions {
    pub fn api(api: impl Into<String>) -> Self {
        ReplayOptions {
            api: Some(api.into()),
            ..ReplayOptions::default()
        }
    }

    pub fn chunk_size(size: usize) -> Self {
        ReplayOptions {
            chunk_size: Some(size),
            ..ReplayOptions::default()
        }
    }
}

struct ReplayState {
    items: VecDeque<ReplayItem>,
    requests: Vec<Request>,
}

fn lock(m: &Mutex<ReplayState>) -> MutexGuard<'_, ReplayState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Replays recordings in order, one per request: the recorded deltas in the order they arrived
/// (blocks can interleave), otherwise each block in turn split by `chunk_size`. Also accepts bare
/// assistant messages for hand-written scripts. An abort is honoured between deltas, like a real
/// stream. Shares its state between clones of the `Arc`, so a test can register it and still read
/// [`requests`](Self::requests).
pub struct ReplayProvider {
    api: String,
    chunk_size: Option<usize>,
    state: Arc<Mutex<ReplayState>>,
}

impl ReplayProvider {
    pub fn new<I, T>(source: I, options: ReplayOptions) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<ReplayItem>,
    {
        ReplayProvider {
            api: options.api.unwrap_or_else(|| "replay".to_string()),
            chunk_size: options.chunk_size,
            state: Arc::new(Mutex::new(ReplayState {
                items: source.into_iter().map(Into::into).collect(),
                requests: Vec::new(),
            })),
        }
    }

    /// Reads a recording file: one recording or bare assistant message per non-blank line.
    pub fn from_file(path: impl AsRef<Path>, options: ReplayOptions) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
        let items = text
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .map(ReplayItem::parse)
            .collect::<anyhow::Result<Vec<_>>>()
            .with_context(|| format!("{} is not a recording", path.display()))?;
        Ok(Self::new(items, options))
    }

    /// The requests it was sent, in order (each with a fresh signal: the one it ran with is not kept).
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.state).requests.clone()
    }
}

#[async_trait]
impl Provider for ReplayProvider {
    fn api(&self) -> &str {
        &self.api
    }

    fn stream(&self, req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        // Nothing happens until the stream is first polled, like the TS async generator.
        let run = ReplayRun {
            begin: Some((self.state.clone(), req)),
            chunk_size: self.chunk_size,
            play: None,
            ended: false,
        };
        futures::stream::iter(run).boxed()
    }
}

/// One replayed reply, stepped each time the stream is polled. The consumer handles each event
/// before it asks for the next, which is where the TS awaited, so an abort it saw lands in time.
struct ReplayRun {
    begin: Option<(Arc<Mutex<ReplayState>>, Request)>,
    chunk_size: Option<usize>,
    play: Option<Playback>,
    ended: bool,
}

impl Iterator for ReplayRun {
    type Item = anyhow::Result<StreamEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        if let Some((state, req)) = self.begin.take() {
            let item = {
                let mut state = lock(&state);
                state.requests.push(Request {
                    signal: CancellationToken::new(),
                    ..req.clone()
                });
                state.items.pop_front()
            };
            let Some(item) = item else {
                self.ended = true;
                return Some(Err(anyhow::anyhow!("replay: no more recorded responses")));
            };
            let play = Playback::new(item.into_recording(), self.chunk_size, req.signal);
            let start = StreamEvent::Start {
                partial: play.partial(),
            };
            self.play = Some(play);
            return Some(Ok(start));
        }
        let event = self.play.as_mut()?.step();
        if matches!(event, StreamEvent::Done { .. }) {
            self.ended = true;
        }
        Some(Ok(event))
    }
}

struct Playback {
    signal: CancellationToken,
    /// The final message.
    message: AssistantMessage,
    /// The partial's content by block index. Blocks fill in as their first delta arrives; a hole
    /// (a later block started first) is left out of the partial, where JS kept a sparse array.
    slots: Vec<Option<AssistantContent>>,
    complete: HashSet<usize>,
    deltas: Vec<Delta>,
    next: usize,
    remaining: HashMap<usize, usize>,
    /// Started, not yet streamed.
    current: Option<Delta>,
    /// Streamed, not yet counted.
    counted: Option<usize>,
}

impl Playback {
    fn new(rec: Recording, chunk_size: Option<usize>, signal: CancellationToken) -> Self {
        let message = rec.message;
        // Blocks without recorded deltas are streamed whole (or in chunks), in block order, after the rest.
        let recorded: HashSet<usize> = rec.deltas.iter().map(|d| d.index).collect();
        let mut deltas = rec.deltas;
        for (index, block) in message.content.iter().enumerate() {
            if recorded.contains(&index) {
                continue;
            }
            let text = match block {
                AssistantContent::Text(t) => t.text.clone(),
                AssistantContent::Thinking(t) => t.thinking.clone(),
                AssistantContent::ToolCall(c) => c
                    .raw_input
                    .clone()
                    .unwrap_or_else(|| sasacode_ai::json::stringify(&Value::Object(c.input.clone()))),
            };
            for delta in chunks(&text, chunk_size) {
                deltas.push(Delta {
                    kind: kind_of(block),
                    index,
                    delta,
                });
            }
        }
        let mut remaining = HashMap::new();
        for d in &deltas {
            *remaining.entry(d.index).or_insert(0usize) += 1;
        }
        Playback {
            signal,
            slots: vec![None; message.content.len()],
            message,
            complete: HashSet::new(),
            deltas,
            next: 0,
            remaining,
            current: None,
            counted: None,
        }
    }

    /// The reply so far: the final message's fields with the content streamed until now.
    fn partial(&self) -> Arc<AssistantMessage> {
        Arc::new(self.with_content(self.slots.iter().flatten().cloned().collect()))
    }

    fn with_content(&self, content: Vec<AssistantContent>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: self.message.api.clone(),
            provider: self.message.provider.clone(),
            model: self.message.model.clone(),
            usage: self.message.usage.clone(),
            stop_reason: self.message.stop_reason,
            timestamp: self.message.timestamp,
            replay_scope: self.message.replay_scope.clone(),
            error_message: self.message.error_message.clone(),
        }
    }

    fn step(&mut self) -> StreamEvent {
        loop {
            if let Some(index) = self.counted.take() {
                let left = self.remaining.get(&index).copied().unwrap_or(1).saturating_sub(1);
                self.remaining.insert(index, left);
                if left == 0 {
                    // Complete: the block as recorded (signature, parsed input).
                    self.slots[index] = Some(self.message.content[index].clone());
                    self.complete.insert(index);
                }
            }
            if let Some(d) = self.current.take() {
                if self.signal.is_cancelled() {
                    // Like a real stream: finished blocks and the text so far stay; a half-streamed call does not.
                    let content = self
                        .slots
                        .iter()
                        .enumerate()
                        .filter_map(|(i, c)| match c {
                            Some(AssistantContent::ToolCall(_)) if !self.complete.contains(&i) => None,
                            c => c.clone(),
                        })
                        .collect();
                    let mut message = self.with_content(content);
                    message.stop_reason = StopReason::Aborted;
                    return StreamEvent::Done { message };
                }
                match &mut self.slots[d.index] {
                    Some(AssistantContent::Text(t)) => t.text.push_str(&d.delta),
                    Some(AssistantContent::Thinking(t)) => t.thinking.push_str(&d.delta),
                    _ => {}
                }
                self.counted = Some(d.index);
                let partial = self.partial();
                let (index, delta) = (d.index, d.delta);
                return match kind_of(&self.message.content[index]) {
                    DeltaKind::TextDelta => StreamEvent::TextDelta { index, delta, partial },
                    DeltaKind::ThinkingDelta => StreamEvent::ThinkingDelta { index, delta, partial },
                    DeltaKind::ToolcallDelta => StreamEvent::ToolcallDelta { index, delta, partial },
                };
            }
            let Some(d) = self.deltas.get(self.next).cloned() else {
                return StreamEvent::Done {
                    message: self.message.clone(),
                };
            };
            self.next += 1;
            let index = d.index;
            let Some(block) = self.message.content.get(index) else {
                continue;
            };
            self.current = Some(d);
            if self.slots[index].is_none() {
                match block {
                    AssistantContent::ToolCall(c) => {
                        let (id, name) = (c.id.clone(), c.name.clone());
                        self.slots[index] = Some(AssistantContent::ToolCall(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input: JsonObject::new(),
                            raw_input: None,
                        }));
                        return StreamEvent::ToolcallStart {
                            index,
                            id,
                            name,
                            partial: self.partial(),
                        };
                    }
                    AssistantContent::Text(_) => {
                        self.slots[index] = Some(AssistantContent::Text(TextContent {
                            text: String::new(),
                            phase: None,
                        }))
                    }
                    AssistantContent::Thinking(_) => {
                        self.slots[index] = Some(AssistantContent::Thinking(ThinkingContent {
                            thinking: String::new(),
                            signature: None,
                            redacted: None,
                            reasoning_field: None,
                        }))
                    }
                }
            }
        }
    }
}

fn kind_of(block: &AssistantContent) -> DeltaKind {
    match block {
        AssistantContent::Text(_) => DeltaKind::TextDelta,
        AssistantContent::Thinking(_) => DeltaKind::ThinkingDelta,
        AssistantContent::ToolCall(_) => DeltaKind::ToolcallDelta,
    }
}

/// `s` in pieces of `size` UTF-16 units (whole when no size is set or it fits).
fn chunks(s: &str, size: Option<usize>) -> Vec<String> {
    let len = utf16_len(s);
    let Some(size) = size.filter(|&n| n > 0 && len > n) else {
        return vec![s.to_string()];
    };
    let cuts: Vec<usize> = (1..).map(|k| k * size).take_while(|&c| c < len).collect();
    let mut out = Vec::new();
    let mut start = 0;
    for end in utf16_cuts(s, &cuts).into_iter().chain([s.len()]) {
        if end > start {
            out.push(s[start..end].to_string());
            start = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use sasacode_ai::{ModelInfo, Usage};
    use serde_json::json;

    use super::*;

    fn model() -> ModelInfo {
        ModelInfo {
            id: "m".into(),
            provider: "t".into(),
            api: "replay".into(),
            base_url: None,
            context_window: 100_000,
            max_output: 1000,
            price: None,
            reasoning: None,
            images: None,
            headers: None,
            server: None,
        }
    }

    fn req(signal: CancellationToken) -> Request {
        Request {
            model: model(),
            api_key: None,
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking: None,
            max_tokens: None,
            max_retries: None,
            sampling: None,
            signal,
            fetch: None,
        }
    }

    fn call(id: &str, name: &str, input: Value) -> AssistantContent {
        let Value::Object(input) = input else {
            panic!("input must be an object")
        };
        AssistantContent::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            raw_input: None,
        })
    }

    fn msg() -> AssistantMessage {
        reply(vec![
            AssistantContent::text("hello world"),
            call("1", "read", json!({ "path": "a.txt" })),
        ])
    }

    fn reply(content: Vec<AssistantContent>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "replay".into(),
            provider: "t".into(),
            model: "m".into(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            timestamp: 0,
            replay_scope: None,
            error_message: None,
        }
    }

    async fn collect(p: &dyn Provider, signal: CancellationToken) -> Vec<StreamEvent> {
        p.stream(req(signal))
            .map(|e| e.expect("no stream error"))
            .collect()
            .await
    }

    fn deltas(events: &[StreamEvent], kind: &str) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match (kind, e) {
                ("text_delta", StreamEvent::TextDelta { delta, .. })
                | ("thinking_delta", StreamEvent::ThinkingDelta { delta, .. })
                | ("toolcall_delta", StreamEvent::ToolcallDelta { delta, .. }) => Some(delta.clone()),
                _ => None,
            })
            .collect()
    }

    fn done(events: &[StreamEvent]) -> &AssistantMessage {
        match events.last() {
            Some(StreamEvent::Done { message }) => message,
            other => panic!("the last event is not done: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recorded_deltas_are_replayed_as_they_came() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rec.jsonl");
        let inner = Arc::new(ReplayProvider::new([msg()], ReplayOptions::chunk_size(3)));
        collect(&RecordingProvider::new(inner, &file), CancellationToken::new()).await;
        let replay = ReplayProvider::from_file(&file, ReplayOptions::default()).unwrap();
        let events = collect(&replay, CancellationToken::new()).await;
        assert_eq!(deltas(&events, "text_delta"), vec!["hel", "lo ", "wor", "ld"]);
        assert_eq!(deltas(&events, "toolcall_delta").concat(), r#"{"path":"a.txt"}"#);
        assert_eq!(done(&events), &msg());
    }

    #[tokio::test]
    async fn an_abort_between_deltas_ends_with_the_text_so_far_and_no_half_tool_call() {
        let ac = CancellationToken::new();
        let provider = ReplayProvider::new([msg()], ReplayOptions::chunk_size(2));
        let mut stream = provider.stream(req(ac.clone()));
        let mut events = Vec::new();
        while let Some(e) = stream.next().await {
            events.push(e.unwrap());
            if matches!(events.last(), Some(StreamEvent::TextDelta { .. })) && deltas(&events, "text_delta").len() == 2
            {
                ac.cancel();
            }
        }
        let done = done(&events);
        assert_eq!(done.stop_reason, StopReason::Aborted);
        assert_eq!(done.content, vec![AssistantContent::text("hell")]);
    }

    #[tokio::test]
    async fn interleaved_blocks_are_replayed_in_the_order_their_deltas_arrived() {
        let two = reply(vec![
            call("1", "read", json!({ "path": "a" })),
            call("2", "read", json!({ "path": "b" })),
        ]);
        let d = |index, delta: &str| Delta {
            kind: DeltaKind::ToolcallDelta,
            index,
            delta: delta.into(),
        };
        let rec = Recording {
            request: RecordedRequest {
                model: "m".into(),
                message_count: 0,
                tools: Vec::new(),
            },
            deltas: vec![
                d(0, r#"{"path":"#),
                d(1, r#"{"path":"#),
                d(0, r#""a"}"#),
                d(1, r#""b"}"#),
            ],
            message: two.clone(),
        };
        let events = collect(
            &ReplayProvider::new([rec], ReplayOptions::default()),
            CancellationToken::new(),
        )
        .await;
        let order: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolcallDelta { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(order, vec![0, 1, 0, 1]);
        assert_eq!(done(&events), &two);
    }

    // Rust-only: what the port had to decide.

    #[tokio::test]
    async fn requests_are_kept_and_running_out_is_an_error() {
        let provider = ReplayProvider::new([reply(vec![AssistantContent::text("a")])], ReplayOptions::api("x"));
        assert_eq!(provider.api(), "x");
        collect(&provider, CancellationToken::new()).await;
        let mut stream = provider.stream(req(CancellationToken::new()));
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.to_string(), "replay: no more recorded responses");
        assert!(stream.next().await.is_none());
        assert_eq!(provider.requests().len(), 2);
    }

    #[test]
    fn chunks_count_utf16_units() {
        assert_eq!(chunks("hello", None), vec!["hello"]);
        assert_eq!(chunks("hello", Some(0)), vec!["hello"]);
        assert_eq!(chunks("hello", Some(5)), vec!["hello"]);
        assert_eq!(chunks("こんにちは世界", Some(3)), vec!["こんに", "ちは世", "界"]);
        // An emoji is two units and stays whole.
        assert_eq!(chunks("a😀b", Some(1)), vec!["a", "😀", "b"]);
    }

    #[test]
    fn recordings_carry_the_role_and_bare_messages_are_recognised() {
        let rec = Recording {
            request: RecordedRequest::default(),
            deltas: Vec::new(),
            message: msg(),
        };
        let line = serde_json::to_string(&rec).unwrap();
        assert!(line.contains(r#""message":{"role":"assistant","content":"#), "{line}");
        assert_eq!(ReplayItem::parse(&line).unwrap(), ReplayItem::Recording(rec));
        let bare = serde_json::to_string(&sasacode_ai::Message::Assistant(msg())).unwrap();
        assert_eq!(ReplayItem::parse(&bare).unwrap(), ReplayItem::Message(msg()));
    }
}
