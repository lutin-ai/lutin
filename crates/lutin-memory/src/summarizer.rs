use async_trait::async_trait;

use crate::event::{ChatSummary, EntitySummary, EventMeta, NewEvent};

#[async_trait]
pub trait Summarizer: Send + Sync + 'static {
    async fn summarize_event(&self, event: &NewEvent) -> Result<EventMeta, SummarizeError>;

    async fn summarize_chat(
        &self,
        prev_title: Option<&str>,
        prev_summary: Option<&str>,
        new_events: &[EventDigest],
    ) -> Result<ChatSummary, SummarizeError>;

    async fn summarize_entity(
        &self,
        name: &str,
        kind: Option<&str>,
        prev_summary: Option<&str>,
        new_mentions: &[EventDigest],
    ) -> Result<EntitySummary, SummarizeError>;
}

#[derive(Debug, Clone)]
pub struct EventDigest {
    pub timestamp: i64,
    pub event_type: String,
    pub summary: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SummarizeError {
    #[error("summarizer error: {0}")]
    Other(String),
}
