//! Generic OpenAI-compatible LLM provider.
//!
//! Hits any endpoint that speaks the OpenAI `/chat/completions` wire
//! format — DeepSeek, Together, Groq, Fireworks, vLLM, LM Studio, and
//! the like. The user supplies `base_url` (required) and an optional
//! bearer key; everything else is delegated to the shared
//! [`super::openai_compat`] helpers.

use async_trait::async_trait;
use reqwest::Client;

use super::openai_compat::{self, ApiRequest, OpenAiCompatQuirks, StreamMode};
use super::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, ModelInfo,
};

/// Conservative defaults: forward `response_format` (broadly supported),
/// skip OpenRouter-specific routing/reasoning extensions which most
/// generic endpoints reject as unknown fields.
///
/// `include_chat_template_thinking_kwarg` is on because this provider
/// targets vLLM-class endpoints, where `chat_template_kwargs.enable_thinking`
/// is the canonical knob for Qwen3.x and similar thinking models. Without
/// it, those models reason out loud in `content` until they hit
/// `max_tokens`, never reaching tool calls.
const QUIRKS: OpenAiCompatQuirks = OpenAiCompatQuirks {
    include_reasoning: false,
    include_response_format: true,
    include_ignore_providers: false,
    include_chat_template_thinking_kwarg: true,
};

#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Base URL up to (but not including) `/chat/completions`, e.g.
    /// `https://api.openai.com/v1` or `https://api.deepseek.com/v1`.
    pub base_url: String,
    /// Bearer token. Optional: some self-hosted endpoints (vLLM, LM
    /// Studio with auth disabled) accept unauthenticated requests.
    pub api_key: Option<String>,
}

pub struct OpenAiCompatProvider {
    config: OpenAiCompatConfig,
    client: Client,
}

impl OpenAiCompatProvider {
    pub fn new(mut config: OpenAiCompatConfig, client: Client) -> Self {
        while config.base_url.ends_with('/') {
            config.base_url.pop();
        }
        Self { config, client }
    }

    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url)
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.config.base_url)
    }

    fn build_headers(&self) -> Result<reqwest::header::HeaderMap, LlmError> {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(ref key) = self.config.api_key {
            let auth = format!("Bearer {key}");
            headers.insert(
                reqwest::header::AUTHORIZATION,
                auth.parse().map_err(|e| {
                    LlmError::Other(format!("invalid authorization header value: {e}"))
                })?,
            );
        }
        Ok(headers)
    }

    fn build_body(request: CompletionRequest, mode: StreamMode) -> ApiRequest {
        openai_compat::build_request(request, mode, &QUIRKS)
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = Self::build_body(request, StreamMode::Buffered);

        let resp = self
            .client
            .post(self.completions_url())
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            log::error!("OpenAI-compat complete error {status}");
            return Err(openai_compat::error_from_response(resp).await);
        }

        let api_resp: openai_compat::ApiResponse =
            openai_compat::decode_json(resp, "OpenAI-compat complete").await?;
        openai_compat::parse_completion_response(api_resp)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream, LlmError> {
        let body = Self::build_body(request, StreamMode::Stream);

        if log::log_enabled!(log::Level::Debug) {
            if let Ok(json) = serde_json::to_string(&body) {
                log::debug!("OpenAI-compat stream request: {json}");
            }
        }

        let resp = self
            .client
            .post(self.completions_url())
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            log::error!("OpenAI-compat stream error {status}");
            return Err(openai_compat::error_from_response(resp).await);
        }

        Ok(openai_compat::sse_event_stream(resp))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let resp = self
            .client
            .get(self.models_url())
            .headers(self.build_headers()?)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let models: openai_compat::ModelsResponse =
            openai_compat::decode_json(resp, "OpenAI-compat models").await?;
        Ok(models
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.name.unwrap_or_else(|| m.id.clone()),
                id: crate::ids::ModelId::from(m.id),
                context_length: m.context_length,
            })
            .collect())
    }
}
