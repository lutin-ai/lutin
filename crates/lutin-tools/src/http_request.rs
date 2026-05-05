use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lutin_llm::{ToolCall, ToolDefinition, ToolName, ToolParameter};
use reqwest::Method;
use serde::Deserialize;

use crate::context::ToolContext;
use crate::outcome::ToolOutput;

const MAX_RESPONSE_BYTES: usize = 1_048_576;
const MAX_CONTENT_CHARS: usize = 8000;

pub struct HttpRequest {
    ctx: Arc<ToolContext>,
}

impl HttpRequest {
    pub fn new(ctx: Arc<ToolContext>) -> Self {
        Self { ctx }
    }
}

pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: ToolName::new("http_request"),
        description: "HTTP request to any URL. Optional readable-text extraction from HTML.".into(),
        parameters: vec![
            ToolParameter { name: "url".into(), r#type: "string".into(), description: "The URL to request.".into(), required: true },
            ToolParameter { name: "method".into(), r#type: "string".into(), description: "HTTP method: GET, POST, PUT, DELETE, PATCH, HEAD.".into(), required: false },
            ToolParameter { name: "headers".into(), r#type: "string".into(), description: "JSON object string of headers, e.g. {\"Authorization\": \"Bearer xxx\"}.".into(), required: false },
            ToolParameter { name: "body".into(), r#type: "string".into(), description: "Request body string.".into(), required: false },
            ToolParameter { name: "extract_text".into(), r#type: "boolean".into(), description: "When true and response is HTML, strip tags and return readable text.".into(), required: false },
            ToolParameter { name: "page".into(), r#type: "integer".into(), description: "Page number for paginated content (starts at 1). Long pages are split into ~8000 character chunks.".into(), required: false },
            ToolParameter { name: "timeout".into(), r#type: "integer".into(), description: "Request timeout in seconds (clamped 1-120).".into(), required: false },
        ],
    }
}

#[derive(Deserialize)]
struct Input {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default)]
    headers: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default = "default_extract_text")]
    extract_text: bool,
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_timeout")]
    timeout: u64,
}

fn default_method() -> String {
    "GET".into()
}
fn default_extract_text() -> bool {
    true
}
fn default_page() -> u32 {
    1
}
fn default_timeout() -> u64 {
    30
}

fn parse_method(s: &str) -> Result<Method, String> {
    match s.to_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "DELETE" => Ok(Method::DELETE),
        "PATCH" => Ok(Method::PATCH),
        "HEAD" => Ok(Method::HEAD),
        other => Err(format!("invalid HTTP method: {other}")),
    }
}

#[async_trait]
impl crate::Tool for HttpRequest {
    fn definition(&self) -> ToolDefinition {
        definition()
    }

    async fn call(&self, _ctx: &crate::ToolCallContext, call: ToolCall) -> crate::ToolResult {
        let out = self.run(call.arguments).await;
        out.into_outcome(call.id)
    }
}

impl HttpRequest {
    async fn run(&self, args: serde_json::Value) -> ToolOutput {
        let input: Input = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(format!("invalid input: {e}")),
        };

        let method = match parse_method(&input.method) {
            Ok(m) => m,
            Err(e) => return ToolOutput::err(e),
        };

        if reqwest::Url::parse(&input.url).is_err() {
            return ToolOutput::err(format!("invalid URL: {}", input.url));
        }

        let page = input.page.max(1) as usize;
        let timeout_secs = input.timeout.clamp(1, 120);

        let mut req = self
            .ctx
            .http
            .request(method, &input.url)
            .timeout(Duration::from_secs(timeout_secs));

        if let Some(ref headers_json) = input.headers {
            let headers_map: HashMap<String, String> = match serde_json::from_str(headers_json) {
                Ok(m) => m,
                Err(e) => return ToolOutput::err(format!("invalid headers JSON: {e}")),
            };
            for (key, value) in &headers_map {
                req = req.header(key.as_str(), value.as_str());
            }
        }

        if let Some(ref body) = input.body {
            req = req.body(body.clone());
        }

        let response = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    return ToolOutput::err(format!("request timed out after {timeout_secs}s"));
                }
                let mut msg = format!("request failed: {e}");
                let mut source = std::error::Error::source(&e);
                while let Some(cause) = source {
                    let _ = write!(msg, " — {cause}");
                    source = cause.source();
                }
                return ToolOutput::err(msg);
            }
        };

        let status_code = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("Unknown")
            .to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                if e.is_timeout() {
                    return ToolOutput::err(format!("request timed out after {timeout_secs}s"));
                }
                return ToolOutput::err(format!("failed to read response body: {e}"));
            }
        };

        let truncated = body_bytes.len() > MAX_RESPONSE_BYTES;
        let body_bytes = if truncated {
            &body_bytes[..MAX_RESPONSE_BYTES]
        } else {
            &body_bytes[..]
        };
        let raw_len = body_bytes.len();

        let output_body = if is_text_content_type(&content_type) {
            let body_text = String::from_utf8_lossy(body_bytes);
            let extracted = if input.extract_text && content_type.contains("text/html") {
                html_to_markdown(&body_text, Some(&input.url))
            } else {
                body_text.into_owned()
            };
            paginate(&extracted, page)
        } else {
            format!("[binary response omitted: content-type={content_type}, {raw_len} bytes]")
        };

        let content_length = output_body.len();
        let truncation_note = if truncated {
            "\n\n[Response truncated at 1 MB]"
        } else {
            ""
        };

        ToolOutput::ok(format!(
            "HTTP {status_code} {status_text}\nContent-Type: {content_type}\nContent-Length: {content_length}\n\n{output_body}{truncation_note}"
        ))
    }
}

fn is_text_content_type(content_type: &str) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct.starts_with("text/") {
        return true;
    }
    matches!(
        ct.as_str(),
        "application/json"
            | "application/ld+json"
            | "application/xml"
            | "application/xhtml+xml"
            | "application/javascript"
            | "application/ecmascript"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/x-yaml"
            | "application/yaml"
            | "application/x-www-form-urlencoded"
            | "unknown"
    ) || ct.ends_with("+json")
        || ct.ends_with("+xml")
}

fn html_to_markdown(html: &str, url: Option<&str>) -> String {
    use dom_smoothie::{Config, Readability};

    let cfg = Config {
        max_elements_to_parse: 0,
        ..Default::default()
    };

    let article = Readability::new(html, url, Some(cfg))
        .and_then(|mut r| r.parse())
        .ok();

    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript", "iframe", "svg"])
        .build();

    match article {
        Some(a) => {
            let body = converter
                .convert(&a.content)
                .unwrap_or_else(|_| a.text_content.to_string());
            let mut out = String::with_capacity(body.len() + 128);
            if !a.title.is_empty() {
                let _ = writeln!(out, "# {}\n", a.title);
            }
            if let Some(byline) = a.byline.as_ref().filter(|s| !s.is_empty()) {
                let _ = writeln!(out, "_{byline}_\n");
            }
            if let Some(excerpt) = a.excerpt.as_ref().filter(|s| !s.is_empty()) {
                let _ = writeln!(out, "> {excerpt}\n");
            }
            out.push_str(body.trim());
            out
        }
        None => converter
            .convert(html)
            .unwrap_or_else(|_| html.to_string())
            .trim()
            .to_string(),
    }
}

fn paginate(text: &str, page: usize) -> String {
    if text.chars().count() <= MAX_CONTENT_CHARS {
        return text.to_string();
    }

    let mut pages: Vec<&str> = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text[start..]
            .char_indices()
            .nth(MAX_CONTENT_CHARS)
            .map(|(i, _)| start + i)
            .unwrap_or(text.len());
        let search_start = text[start..end]
            .char_indices()
            .rev()
            .nth(200)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let break_at = text[start..end]
            .rfind('\n')
            .filter(|&pos| pos >= search_start)
            .map(|pos| start + pos + 1)
            .unwrap_or(end);
        pages.push(&text[start..break_at]);
        start = break_at;
    }

    let total = pages.len().max(1);
    let idx = (page - 1).min(total - 1);
    let content = pages[idx];

    if total > 1 {
        format!("[Page {page} of {total}]\n\n{content}")
    } else {
        content.to_string()
    }
}
