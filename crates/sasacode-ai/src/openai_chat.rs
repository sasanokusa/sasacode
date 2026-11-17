//! OpenAI Chat Completions adapter (`packages/ai/src/openai-chat.ts`). Being ported.

use futures::stream::BoxStream;

use crate::provider::{Provider, Request, StreamEvent};

pub struct OpenAiChatProvider;

#[async_trait::async_trait]
impl Provider for OpenAiChatProvider {
    fn api(&self) -> &str {
        "openai-chat"
    }
    fn stream(&self, _req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        Box::pin(futures::stream::once(async { Err(anyhow::anyhow!("openai-chat adapter not ported yet")) }))
    }
}
