//! Anthropic Messages API adapter (`packages/ai/src/anthropic.ts`). Being ported.

use futures::stream::BoxStream;

use crate::provider::{Provider, Request, StreamEvent};

pub struct AnthropicProvider;

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn api(&self) -> &str {
        "anthropic"
    }
    fn stream(&self, _req: Request) -> BoxStream<'static, anyhow::Result<StreamEvent>> {
        Box::pin(futures::stream::once(async { Err(anyhow::anyhow!("anthropic adapter not ported yet")) }))
    }
}
