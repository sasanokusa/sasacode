//! OpenAI Responses adapter (`packages/ai/src/openai-responses.ts`). Being ported.

use futures::stream::BoxStream;

use crate::provider::{Provider, Request, StreamEvent};

pub struct OpenAiResponsesProvider;

#[async_trait::async_trait]
impl Provider for OpenAiResponsesProvider {
    fn api(&self) -> &str {
        "openai-responses"
    }
    fn stream(&self, _req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        Box::pin(futures::stream::once(async { Err(anyhow::anyhow!("openai-responses adapter not ported yet")) }))
    }
}
