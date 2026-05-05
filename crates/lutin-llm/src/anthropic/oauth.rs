//! PKCE login and token-exchange flow for Anthropic subscription OAuth.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use std::sync::Arc;

use super::store::{CredBackend, Credentials, OAuthCredentialStore};
use crate::LlmError;

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
pub const SCOPES: &str = "org:create_api_key user:profile user:inference \
     user:sessions:claude_code user:mcp_servers user:file_upload";

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

pub struct PendingLogin {
    verifier: String,
    state: String,
}

impl PendingLogin {
    pub fn state(&self) -> &str {
        &self.state
    }

    pub async fn complete(self, pasted_code: &str) -> Result<OAuthCredentialStore, LlmError> {
        self.complete_with(
            pasted_code,
            Arc::new(super::store::KeyringBackend::new()) as Arc<dyn CredBackend>,
        )
        .await
    }

    /// Complete the exchange and persist to `backend` (e.g. an encrypted file
    /// when running inside a container).
    pub async fn complete_with(
        self,
        pasted_code: &str,
        backend: Arc<dyn CredBackend>,
    ) -> Result<OAuthCredentialStore, LlmError> {
        let cleaned = pasted_code
            .split(['#', '&'])
            .next()
            .unwrap_or(pasted_code)
            .trim();
        if cleaned.is_empty() {
            return Err(LlmError::Other("empty authorization code".into()));
        }
        let http = reqwest::Client::new();
        let tr = exchange_code(&http, cleaned, &self.verifier, &self.state).await?;
        let creds = Credentials::from_token_response(tr, None)?;
        OAuthCredentialStore::store_with(backend, creds)
    }
}

/// Begin a PKCE login. Returns the pending state and the URL to open in the
/// user's browser. The caller must display the URL (or `open::that(&url)`),
/// then collect the pasted code and call [`PendingLogin::complete`].
pub fn begin_login() -> Result<(PendingLogin, String), LlmError> {
    // `rand::random()` routes to `ThreadRng`, which is a CSPRNG seeded from
    // the OS. Sufficient for PKCE verifiers and CSRF state.
    let verifier_bytes: [u8; 32] = rand::random();
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);

    let state_bytes: [u8; 16] = rand::random();
    let state: String = state_bytes.iter().map(|b| format!("{:02x}", b)).collect();

    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let url = url::Url::parse_with_params(
        AUTH_URL,
        &[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ],
    )
    .map_err(|e| LlmError::Other(e.to_string()))?;

    // `url::Url::parse_with_params` encodes spaces in query values as `+`
    // (form-urlencoded style). Anthropic's authorize endpoint expects RFC
    // 3986 query encoding — spaces in `scope` must be `%20`. Base64url
    // verifiers never contain `+`, and `state` is hex, so this replace is
    // safe to apply globally.
    let url: String = String::from(url).replace('+', "%20");
    Ok((PendingLogin { verifier, state }, url))
}

/// Full interactive CLI login: opens the browser, reads the pasted code from
/// stdin, stores credentials.
pub async fn login_interactive() -> Result<OAuthCredentialStore, LlmError> {
    let (pending, auth_url) = begin_login()?;
    if open::that(&auth_url).is_err() {
        println!("Open this URL in your browser to authenticate:\n  {auth_url}");
    } else {
        println!("Opened browser. If nothing appeared, use this URL:\n  {auth_url}");
    }
    print!("Paste the authorization code: ");
    use std::io::Write;
    std::io::stdout()
        .flush()
        .map_err(|e| LlmError::Other(e.to_string()))?;
    let mut code = String::new();
    std::io::stdin()
        .read_line(&mut code)
        .map_err(|e| LlmError::Other(e.to_string()))?;
    pending.complete(code.trim()).await
}

async fn exchange_code(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
    state: &str,
) -> Result<TokenResponse, LlmError> {
    post_token(
        http,
        serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
            "state": state,
        }),
    )
    .await
}

pub async fn refresh_tokens(
    http: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenResponse, LlmError> {
    post_token(
        http,
        serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }),
    )
    .await
}

async fn post_token(
    http: &reqwest::Client,
    body: serde_json::Value,
) -> Result<TokenResponse, LlmError> {
    // JSON body with an `axios/*` UA — the token endpoint filters on both;
    // a form body or a naked request is 400'd.
    let resp = http
        .post(TOKEN_URL)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("User-Agent", "axios/1.13.6")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    if status.is_success() {
        return serde_json::from_str(&body).map_err(LlmError::Json);
    }
    if status.as_u16() == 429 {
        return Err(LlmError::RateLimited {
            message: body,
            retry_after: None,
        });
    }
    let err: TokenError = serde_json::from_str(&body).unwrap_or(TokenError {
        error: None,
        error_description: Some(body.clone()),
    });
    let msg = err
        .error_description
        .or(err.error)
        .unwrap_or_else(|| body.clone());
    Err(LlmError::Other(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fragment_and_ampersand_from_pasted_code() {
        let a = "abc123#state=xyz";
        let b = "abc123&state=xyz";
        let c = "  abc123  ";
        let clean = |s: &str| {
            s.split(['#', '&'])
                .next()
                .unwrap_or(s)
                .trim()
                .to_string()
        };
        assert_eq!(clean(a), "abc123");
        assert_eq!(clean(b), "abc123");
        assert_eq!(clean(c), "abc123");
    }

    #[test]
    fn begin_login_produces_valid_url() {
        let (_, url) = begin_login().unwrap();
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code=true"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference"));
        assert!(url.contains("user%3Asessions%3Aclaude_code"));
        assert!(url.contains("user%3Amcp_servers"));
        assert!(url.contains("user%3Afile_upload"));
    }
}
