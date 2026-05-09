#[cfg(feature = "memory-agent")]
pub mod agent;
pub mod event;
#[cfg(feature = "llm-summarizer")]
pub mod llm_summarizer;
pub mod python;
pub mod schema;
pub mod store;
pub mod summarizer;

use std::path::Path;
use std::sync::Arc;

pub use event::{
    ChatId, ChatSummary, EntityRef, EntitySummary, Event, EventId, EventMeta, EventType, NewEvent,
    Status,
};
pub use store::Store;
pub use summarizer::{EventDigest, SummarizeError, Summarizer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("python: {0}")]
    Python(String),
    #[error("summarize: {0}")]
    Summarize(#[from] SummarizeError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Config {
    pub summary_every_n_events: i64,
    pub summary_every_n_mentions: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            summary_every_n_events: 10,
            summary_every_n_mentions: 10,
        }
    }
}

pub struct Memory {
    store: Arc<Store>,
    summarizer: Arc<dyn Summarizer>,
    config: Config,
}

impl Memory {
    pub fn open(
        path: &Path,
        config: Config,
        summarizer: Arc<dyn Summarizer>,
    ) -> Result<Self> {
        Ok(Self {
            store: Arc::new(Store::open(path)?),
            summarizer,
            config,
        })
    }

    pub fn open_in_memory(
        config: Config,
        summarizer: Arc<dyn Summarizer>,
    ) -> Result<Self> {
        Ok(Self {
            store: Arc::new(Store::open_in_memory()?),
            summarizer,
            config,
        })
    }

    /// Write a raw event row. Sync, fast, no LLM calls.
    pub fn insert(&self, event: NewEvent) -> Result<EventId> {
        let (id, _) = self.store.insert_event(&event)?;
        Ok(id)
    }

    /// Run the summarization cascade for `id`:
    ///   1. summarize the event itself
    ///   2. if its chat crosses `summary_every_n_events` → roll up the chat
    ///   3. for each entity touched: summarize if cold or threshold hit
    ///
    /// Idempotent — re-calling on a `ready` event is a no-op.
    pub async fn summarize(&self, id: EventId) -> Result<()> {
        let Some((ev, chat_id)) = self.store.fetch_for_summarize(id)? else {
            return Ok(());
        };

        let meta = match self.summarizer.summarize_event(&ev).await {
            Ok(m) => m,
            Err(e) => {
                let _ = self.store.mark_event_failed(id);
                return Err(e.into());
            }
        };
        let entity_ids = self.store.apply_event_meta(id, chat_id, &meta)?;

        if let Some(cid) = chat_id {
            if self
                .store
                .chat_due_for_summary(cid, self.config.summary_every_n_events)?
            {
                let (title, summary, digs) = self.store.chat_context(cid)?;
                let s = self
                    .summarizer
                    .summarize_chat(title.as_deref(), summary.as_deref(), &digs)
                    .await?;
                self.store.apply_chat_summary(cid, &s)?;
            }
        }

        for eid in entity_ids {
            if self
                .store
                .entity_due_for_summary(eid, self.config.summary_every_n_mentions)?
            {
                let (name, kind, prev, digs) = self.store.entity_context(eid)?;
                let s = self
                    .summarizer
                    .summarize_entity(&name, kind.as_deref(), prev.as_deref(), &digs)
                    .await?;
                self.store.apply_entity_summary(eid, &s)?;
            }
        }

        Ok(())
    }

    pub fn get(&self, id: EventId) -> Result<Option<Event>> {
        self.store.get_event(id)
    }

    pub fn query_sql(
        &self,
        sql: &str,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        self.store.query_sql(sql)
    }

    pub fn run_python(&self, code: &str) -> Result<String> {
        python::run_script(self.store.clone(), code)
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }
}
