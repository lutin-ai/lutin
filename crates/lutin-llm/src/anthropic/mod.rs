//! Anthropic Messages API provider with subscription-based OAuth auth.
//!
//! Two auth modes:
//! - `AnthropicAuth::ApiKey` — `x-api-key`, billed per token to a Console acct.
//! - `AnthropicAuth::OAuthSubscription` — bearer from a Claude.ai Pro/Max
//!   subscription obtained via browser PKCE. Requires the `oauth-2025-04-20`
//!   beta header and the Claude Code system-prompt preamble on every request.

pub mod billing;
pub mod messages;
pub mod oauth;
pub mod store;

pub use messages::{
    AnthropicProvider, ANTHROPIC_VERSION, API_URL, CLAUDE_CODE_PREAMBLE, OAUTH_BETA,
};
pub use oauth::{begin_login, login_interactive, PendingLogin, CLIENT_ID};
pub use store::{
    CredBackend, Credentials, EncryptedFileBackend, KeyringBackend, MemoryBackend,
    OAuthCredentialStore,
};

#[derive(Clone)]
pub enum AnthropicAuth {
    ApiKey(String),
    OAuthSubscription(OAuthCredentialStore),
}

impl AnthropicAuth {
    pub fn is_oauth(&self) -> bool {
        matches!(self, Self::OAuthSubscription(_))
    }
}
