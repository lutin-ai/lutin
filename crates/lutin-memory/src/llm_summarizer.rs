//! Concrete [`Summarizer`] backed by per-step LLM providers.
//!
//! Each summarization step (event / chat / entity) gets its own
//! `(provider, model)` pair, so callers can mix backends — e.g. a cheap
//! fast model for per-event summaries, a stronger model for rolling chat
//! summaries, etc.

use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{
    CompletionRequest, Extensions, LlmProvider, Message, ModelId, Reasoning, ReasoningEffort,
    ResponseFormat,
};
use serde::Deserialize;

use crate::event::{ChatSummary, EntityRef, EntitySummary, EventMeta, NewEvent};
use crate::summarizer::{EventDigest, SummarizeError, Summarizer};

/// One LLM endpoint: provider + model.
#[derive(Clone)]
pub struct LlmStep {
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelId,
    pub max_tokens: Option<u32>,
}

impl LlmStep {
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<ModelId>) -> Self {
        Self {
            provider,
            model: model.into(),
            max_tokens: Some(1024),
        }
    }
}

/// [`Summarizer`] that routes each step to a distinct LLM endpoint.
pub struct LlmSummarizer {
    pub event: LlmStep,
    pub chat: LlmStep,
    pub entity: LlmStep,
}

impl LlmSummarizer {
    /// Use the same endpoint for all three steps.
    pub fn uniform(step: LlmStep) -> Self {
        Self {
            event: step.clone(),
            chat: step.clone(),
            entity: step,
        }
    }
}

#[async_trait]
impl Summarizer for LlmSummarizer {
    async fn summarize_event(&self, event: &NewEvent) -> Result<EventMeta, SummarizeError> {
        let system = "You summarize a single message or event. Output STRICT JSON: \
            {\"summary\": string (<=200 chars), \"topics\": string[], \"entities\": [{\"name\": string, \"kind\": string|null}]}. \
            Topics are short kebab-case tags. Entities are people, files, projects, places mentioned.";
        let user = format!(
            "event_type: {}\nsource: {}\ntimestamp: {}\ncontent:\n{}",
            event.event_type.as_str(),
            event.source.as_deref().unwrap_or(""),
            event.timestamp,
            event.content
        );
        let raw = run_json(&self.event, system, &user).await?;
        let parsed: EventMetaJson = serde_json::from_str(&raw)
            .map_err(|e| SummarizeError::Other(format!("event meta parse: {e}; raw={raw}")))?;
        Ok(EventMeta {
            summary: parsed.summary,
            topics: parsed.topics,
            entities: parsed
                .entities
                .into_iter()
                .map(|e| EntityRef {
                    name: e.name,
                    kind: e.kind,
                })
                .collect(),
        })
    }

    async fn summarize_chat(
        &self,
        prev_title: Option<&str>,
        prev_summary: Option<&str>,
        new_events: &[EventDigest],
    ) -> Result<ChatSummary, SummarizeError> {
        let system = "You maintain a rolling summary of a chat. Update the prior title and summary \
            using the new events. Keep the summary <=400 chars, factual, no fluff. \
            Output STRICT JSON: {\"title\": string (<=60 chars), \"summary\": string}.";
        let mut user = String::new();
        user.push_str(&format!("prev_title: {}\n", prev_title.unwrap_or("(none)")));
        user.push_str(&format!(
            "prev_summary: {}\n\nnew_events:\n",
            prev_summary.unwrap_or("(none)")
        ));
        for d in new_events {
            user.push_str(&format!("- [{}] {}: {}\n", d.timestamp, d.event_type, d.summary));
        }
        let raw = run_json(&self.chat, system, &user).await?;
        let parsed: ChatSummaryJson = serde_json::from_str(&raw)
            .map_err(|e| SummarizeError::Other(format!("chat summary parse: {e}; raw={raw}")))?;
        Ok(ChatSummary {
            title: parsed.title,
            summary: parsed.summary,
        })
    }

    async fn summarize_entity(
        &self,
        name: &str,
        kind: Option<&str>,
        prev_summary: Option<&str>,
        new_mentions: &[EventDigest],
    ) -> Result<EntitySummary, SummarizeError> {
        let system = "You maintain a rolling profile of an entity (person, file, project, etc) \
            across events that mention it. Update the prior summary using the new mentions. \
            Keep summary <=400 chars, factual. Output STRICT JSON: {\"summary\": string}.";
        let mut user = String::new();
        user.push_str(&format!("name: {name}\nkind: {}\n", kind.unwrap_or("(none)")));
        user.push_str(&format!(
            "prev_summary: {}\n\nnew_mentions:\n",
            prev_summary.unwrap_or("(none)")
        ));
        for d in new_mentions {
            user.push_str(&format!("- [{}] {}: {}\n", d.timestamp, d.event_type, d.summary));
        }
        let raw = run_json(&self.entity, system, &user).await?;
        let parsed: EntitySummaryJson = serde_json::from_str(&raw)
            .map_err(|e| SummarizeError::Other(format!("entity summary parse: {e}; raw={raw}")))?;
        Ok(EntitySummary {
            summary: parsed.summary,
        })
    }
}

async fn run_json(step: &LlmStep, system: &str, user: &str) -> Result<String, SummarizeError> {
    let req = CompletionRequest {
        model: step.model.clone(),
        messages: vec![
            Message::System(system.to_string()),
            Message::User(user.to_string()),
        ],
        tools: Vec::new(),
        temperature: Some(0.2),
        max_tokens: step.max_tokens,
        thinking_enabled: false,
        presence_penalty: None,
        extensions: Extensions {
            reasoning: Some(Reasoning {
                effort: ReasoningEffort::Low,
                max_tokens: None,
            }),
            response_format: Some(ResponseFormat::JsonObject),
            ignore_providers: Vec::new(),
        },
    };
    let resp = step
        .provider
        .complete(req)
        .await
        .map_err(|e| SummarizeError::Other(format!("{e}")))?;
    Ok(resp.text)
}

#[derive(Deserialize)]
struct EventMetaJson {
    summary: String,
    #[serde(default)]
    topics: Vec<String>,
    #[serde(default)]
    entities: Vec<EntityRefJson>,
}

#[derive(Deserialize)]
struct EntityRefJson {
    name: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct ChatSummaryJson {
    title: String,
    summary: String,
}

#[derive(Deserialize)]
struct EntitySummaryJson {
    summary: String,
}
