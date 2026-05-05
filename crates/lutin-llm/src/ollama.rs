//! Ollama LLM provider.
//!
//! Ollama exposes an OpenAI-compatible API at `/v1/chat/completions` (since
//! v0.1.24) alongside its native API. We use the OpenAI-compatible endpoint
//! for chat completions and streaming, and the native `/api/tags` endpoint
//! for model listing (richer metadata than `/v1/models`).

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;

use super::openai_compat::{self, ApiRequest, OpenAiCompatQuirks, StreamMode};
use super::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, ModelInfo,
};

const QUIRKS: OpenAiCompatQuirks = OpenAiCompatQuirks {
    include_reasoning: false,
    include_response_format: false,
    include_ignore_providers: false,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Configuration for the Ollama provider.
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Base URL of the Ollama server (default: `http://localhost:11434`).
    pub base_url: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
        }
    }
}

/// Ollama LLM provider.
pub struct OllamaProvider {
    config: OllamaConfig,
    client: Client,
}

// --- Native `/api/tags` response types ---

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    /// Model name (e.g. "llama3.1:8b").
    #[serde(alias = "model")]
    name: String,
    /// Model details — may be absent on older Ollama versions.
    details: Option<TagModelDetails>,
}

#[derive(Deserialize)]
struct TagModelDetails {
    /// Parameter size string (e.g. "8B").
    parameter_size: Option<String>,
}

impl OllamaProvider {
    pub fn new(mut config: OllamaConfig, client: Client) -> Self {
        // Strip trailing slash so URL joins don't produce double-slashes.
        while config.base_url.ends_with('/') {
            config.base_url.pop();
        }
        Self { config, client }
    }

    fn completions_url(&self) -> String {
        format!("{}/v1/chat/completions", self.config.base_url)
    }

    fn tags_url(&self) -> String {
        format!("{}/api/tags", self.config.base_url)
    }

    fn build_body(request: CompletionRequest, mode: StreamMode) -> ApiRequest {
        openai_compat::build_request(request, mode, &QUIRKS)
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = Self::build_body(request, StreamMode::Buffered);

        let resp = self
            .client
            .post(self.completions_url())
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            log::error!("Ollama complete error {status}");
            return Err(openai_compat::error_from_response(resp).await);
        }

        let api_resp: openai_compat::ApiResponse =
            openai_compat::decode_json(resp, "Ollama complete").await?;
        openai_compat::parse_completion_response(api_resp)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream, LlmError> {
        let body = Self::build_body(request, StreamMode::Stream);

        if log::log_enabled!(log::Level::Debug) {
            if let Ok(json) = serde_json::to_string(&body) {
                log::debug!("Ollama stream request: {json}");
            }
        }

        let resp = self
            .client
            .post(self.completions_url())
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            log::error!("Ollama stream error {status}");
            return Err(openai_compat::error_from_response(resp).await);
        }

        Ok(openai_compat::sse_event_stream(resp))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let resp = self.client.get(self.tags_url()).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let tags: TagsResponse = openai_compat::decode_json(resp, "Ollama models").await?;
        Ok(tags
            .models
            .into_iter()
            .map(|m| {
                let display = m
                    .details
                    .as_ref()
                    .and_then(|d| d.parameter_size.as_deref())
                    .map(|size| format!("{} ({size})", m.name))
                    .unwrap_or_else(|| m.name.clone());
                ModelInfo {
                    id: crate::ids::ModelId::from(m.name),
                    name: display,
                    context_length: None,
                }
            })
            .collect())
    }
}
