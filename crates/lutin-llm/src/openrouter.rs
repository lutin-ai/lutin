use async_trait::async_trait;
use reqwest::Client;

use super::openai_compat::{self, ApiRequest, OpenAiCompatQuirks, StreamMode};
use super::{
    CompletionRequest, CompletionResponse, EventStream, LlmError, LlmProvider, ModelInfo,
};

const QUIRKS: OpenAiCompatQuirks = OpenAiCompatQuirks {
    include_reasoning: true,
    include_response_format: true,
    include_ignore_providers: true,
    // OpenRouter rejects unknown vLLM-style kwargs; keep off here.
    // `thinking_enabled` already flows through the `reasoning` object.
    include_chat_template_thinking_kwarg: false,
};

const BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Configuration for the OpenRouter provider.
#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    pub api_key: String,
    pub app_name: Option<String>,
    pub app_url: Option<String>,
}

/// OpenRouter LLM provider.
pub struct OpenRouterProvider {
    config: OpenRouterConfig,
    client: Client,
}

impl OpenRouterProvider {
    pub fn new(config: OpenRouterConfig, client: Client) -> Self {
        Self { config, client }
    }

    fn build_headers(&self) -> Result<reqwest::header::HeaderMap, LlmError> {
        let mut headers = reqwest::header::HeaderMap::new();
        let auth = format!("Bearer {}", self.config.api_key);
        headers.insert(
            reqwest::header::AUTHORIZATION,
            auth.parse().map_err(|e| {
                LlmError::Other(format!("invalid authorization header value: {e}"))
            })?,
        );
        if let Some(ref app) = self.config.app_name {
            headers.insert(
                "X-Title",
                app.parse().map_err(|e| {
                    LlmError::Other(format!("invalid X-Title header value: {e}"))
                })?,
            );
        }
        if let Some(ref url) = self.config.app_url {
            headers.insert(
                "HTTP-Referer",
                url.parse().map_err(|e| {
                    LlmError::Other(format!("invalid HTTP-Referer header value: {e}"))
                })?,
            );
        }
        Ok(headers)
    }

    /// Send a request, mapping 429 responses to [`LlmError::RateLimited`].
    /// Retries are handled at the session layer so user cancellation and
    /// status broadcasts can interleave with the wait.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, LlmError> {
        let resp = request.send().await?;

        if resp.status().as_u16() != 429 {
            return Ok(resp);
        }

        Err(openai_compat::error_from_response(resp).await)
    }

    fn build_body(&self, request: CompletionRequest, mode: StreamMode) -> ApiRequest {
        openai_compat::build_request(request, mode, &QUIRKS)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let body = self.build_body(request, StreamMode::Buffered);

        let initial_req = self
            .client
            .post(format!("{BASE_URL}/chat/completions"))
            .headers(self.build_headers()?)
            .json(&body);

        let resp = self.send(initial_req).await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            log::error!("OpenRouter complete error {status}: {text}");
            return Err(openai_compat::parse_error_body(status.as_u16(), &text));
        }

        let api_resp: openai_compat::ApiResponse =
            openai_compat::decode_json(resp, "OpenRouter complete").await?;
        openai_compat::parse_completion_response(api_resp)
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream, LlmError> {
        let body = self.build_body(request, StreamMode::Stream);

        if log::log_enabled!(log::Level::Debug) {
            if let Ok(json) = serde_json::to_string(&body) {
                log::debug!("OpenRouter stream request: {json}");
            }
        }

        let initial_req = self
            .client
            .post(format!("{BASE_URL}/chat/completions"))
            .headers(self.build_headers()?)
            .json(&body);

        let resp = self.send(initial_req).await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            log::error!("OpenRouter stream error {status}: {text}");
            return Err(openai_compat::parse_error_body(status.as_u16(), &text));
        }

        Ok(openai_compat::sse_event_stream(resp))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        let resp = self
            .client
            .get(format!("{BASE_URL}/models"))
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
            openai_compat::decode_json(resp, "OpenRouter models").await?;
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

