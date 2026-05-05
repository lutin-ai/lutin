// TODO: swap `std::env::var` for a caller-provided key getter

use std::sync::Arc;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const DEFAULT_COUNT: u32 = 10;
const MIN_COUNT: u32 = 1;
const MAX_COUNT: u32 = 20;

/// Name of the env var consulted for the Brave Search API key.
pub const BRAVE_API_KEY_ENV: &str = "BRAVE_SEARCH_API_KEY";

pub struct WebSearch {
    ctx: Arc<ToolContext>,
    /// Optional override for the Brave API key. When `Some`, this key is used
    /// directly and the `BRAVE_SEARCH_API_KEY` env var is ignored. Lets
    /// embedders feed the key from their own secret store without mutating
    /// global process env.
    api_key: Option<String>,
}

impl WebSearch {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx, api_key: None }
    }

    /// Construct with an explicit API key. Bypasses the `BRAVE_SEARCH_API_KEY`
    /// env var lookup for this instance.
    pub fn with_api_key(ctx: Arc<ToolContext>, api_key: impl Into<String>) -> Self {
        Self {
            ctx,
            api_key: Some(api_key.into()),
        }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("web_search"),
        description: "Search the web using Brave Search API, returning structured results with titles, URLs, and descriptions.".into(),
        parameters: vec![
            ToolParameter { name: "query".into(), r#type: "string".into(), description: "The search query.".into(), required: true },
            ToolParameter { name: "count".into(), r#type: "integer".into(), description: "Number of results to return (1-20).".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    query: String,
    #[serde(default)]
    count: Option<u32>,
}

#[derive(Deserialize)]
struct BraveSearchResponse {
    #[serde(default)]
    web: Option<BraveWebResults>,
}

#[derive(Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

#[async_trait]
impl crate::Tool for WebSearch {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl WebSearch {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        if input.query.is_empty() {
            return ToolOutput::err("query must not be empty");
        }

        let count = input
            .count
            .unwrap_or(DEFAULT_COUNT)
            .clamp(MIN_COUNT, MAX_COUNT);

        let api_key = match self.api_key.clone() {
            Some(k) if !k.is_empty() => k,
            _ => match std::env::var(BRAVE_API_KEY_ENV) {
                Ok(k) if !k.is_empty() => k,
                _ => {
                    return ToolOutput::err(format!(
                        "Brave Search API key not configured. Set {BRAVE_API_KEY_ENV} in the environment."
                    ));
                }
            },
        };

        let mut url = match reqwest::Url::parse("https://api.search.brave.com/res/v1/web/search") {
            Ok(u) => u,
            Err(e) => return ToolOutput::err(format!("internal URL error: {e}")),
        };
        url.query_pairs_mut()
            .append_pair("q", &input.query)
            .append_pair("count", &count.to_string());

        let response = match self
            .ctx
            .http
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .header("X-Subscription-Token", &api_key)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("request failed: {e}")),
        };

        if !response.status().is_success() {
            return ToolOutput::err(format!(
                "Brave Search API returned status {}",
                response.status()
            ));
        }

        let body: BraveSearchResponse = match response.json().await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("failed to parse response: {e}")),
        };

        let results = body.web.map(|w| w.results).unwrap_or_default();

        if results.is_empty() {
            return ToolOutput::ok(format!("no results found for '{}'", input.query));
        }

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            output.push_str(&format!(
                "{}. {}\n   {}\n   {}",
                i + 1,
                result.title,
                result.url,
                result.description,
            ));
            if i < results.len() - 1 {
                output.push('\n');
            }
        }

        ToolOutput::ok(output)
    }
}
