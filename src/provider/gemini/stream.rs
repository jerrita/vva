use async_trait::async_trait;

use super::parse::GeminiAccumulator;
use crate::provider::ProviderError;
use crate::provider::http::SseEventHandler;

#[async_trait]
impl SseEventHandler for GeminiAccumulator {
    async fn on_event(&mut self, json: serde_json::Value) -> Result<(), ProviderError> {
        self.merge_response(&json)?;
        self.flush_events().await;
        Ok(())
    }
}
