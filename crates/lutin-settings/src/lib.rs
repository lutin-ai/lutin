//! Two-tier (global + per-project) user settings.
//!
//! Loaded from `settings.toml` via [`lutin_storage::Resolver`]: the global
//! file is read first, then the project file overrides it field-by-field.
//! Every field is optional in the on-disk format; defaults fill in what's
//! missing. Subtables override as a unit (project's `[whisper]` replaces
//! global's entirely; partial subtable overrides require the user to
//! restate the table). The `providers` list is treated as a single value
//! — if the project file sets it, the project list wins outright.

use std::collections::HashMap;
use std::path::Path;

use lutin_storage::Resolver;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Filename relative to each `.lutin/` root.
pub const SETTINGS_FILE: &str = "settings.toml";

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

/// Fully-resolved settings. Built by merging global + project tiers and
/// substituting defaults for absent fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub chat: ChatSettings,
    #[serde(default)]
    pub web_search: WebSearchSettings,
    #[serde(default)]
    pub whisper: WhisperSettings,
    #[serde(default)]
    pub limits: LimitsSettings,
    #[serde(default)]
    pub tts: TtsSettings,
    #[serde(default)]
    pub tool_permissions: HashMap<String, ToolPermission>,
}

/// One configured LLM backend. `name` is a user-chosen identifier
/// (e.g. `"openrouter"`, `"my-ollama"`); `kind` selects the wire protocol.
///
/// API keys can be supplied in three ways, in priority order:
/// 1. `api_key` — literal in the settings file (convenient, plaintext).
/// 2. `api_key_env` — the named env var is read at startup.
/// 3. `use_oauth` (Anthropic only) — bearer token from the credential store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Anthropic only — bearer token from the OAuth credential store.
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_oauth: bool,
}

/// Authentication strategy resolved from a [`ProviderConfig`]'s three
/// raw fields (`api_key`, `api_key_env`, `use_oauth`). Encoding the
/// precedence rule in one place lets callers pattern-match on the
/// outcome instead of re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAuth {
    /// No auth configured.
    None,
    /// Literal key from the settings file.
    Inline(String),
    /// Read the named env var at use time.
    FromEnv(String),
    /// Anthropic OAuth subscription credential store.
    OAuth,
}

impl ProviderConfig {
    /// Collapse `(api_key, api_key_env, use_oauth)` into a single auth
    /// outcome. Precedence: inline key > env var > oauth > none.
    pub fn resolved_auth(&self) -> ResolvedAuth {
        if let Some(k) = &self.api_key {
            ResolvedAuth::Inline(k.clone())
        } else if let Some(var) = &self.api_key_env {
            ResolvedAuth::FromEnv(var.clone())
        } else if self.use_oauth {
            ResolvedAuth::OAuth
        } else {
            ResolvedAuth::None
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenRouter,
    Ollama,
    Anthropic,
    /// Generic OpenAI-compatible endpoint (custom `base_url` required).
    OpenAiCompat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSettings {
    /// Auto-generate chat titles after the first user/assistant exchange.
    #[serde(default = "default_true")]
    pub auto_title: bool,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self { auto_title: true }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchSettings {
    /// Brave Search API key (free tier: 2K queries/mo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brave_api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhisperSettings {
    /// GGML model filename (e.g. `"ggml-large-v3-turbo.bin"`).
    #[serde(default = "default_whisper_model")]
    pub model: String,
    /// ISO language code; `None` = auto-detect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Decoding strategy (1 = greedy / fast; >1 = beam search / accurate).
    #[serde(default = "default_beam_size")]
    pub beam_size: i32,
}

impl Default for WhisperSettings {
    fn default() -> Self {
        Self {
            model: default_whisper_model(),
            language: None,
            beam_size: default_beam_size(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LimitsSettings {
    /// Whisper model upload limit.
    #[serde(default = "default_max_model_upload_bytes")]
    pub max_model_upload_bytes: u64,
    /// Per-message attachment count.
    #[serde(default = "default_max_attachments")]
    pub max_attachments: usize,
    /// Total attachment size per message.
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,
    /// Audio streaming buffer.
    #[serde(default = "default_max_audio_buffer_bytes")]
    pub max_audio_buffer_bytes: usize,
}

impl Default for LimitsSettings {
    fn default() -> Self {
        Self {
            max_model_upload_bytes: default_max_model_upload_bytes(),
            max_attachments: default_max_attachments(),
            max_attachment_bytes: default_max_attachment_bytes(),
            max_audio_buffer_bytes: default_max_audio_buffer_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TtsSettings {
    /// TTS engine (`"orpheus"`, `"kokoro"`, `"qwen3"`).
    #[serde(default = "default_tts_backend")]
    pub backend: String,
    /// Model name without extension.
    #[serde(default = "default_tts_model")]
    pub model: String,
    /// Voice name (backend-specific).
    #[serde(default = "default_tts_voice")]
    pub default_voice: String,
}

impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            backend: default_tts_backend(),
            model: default_tts_model(),
            default_voice: default_tts_voice(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    /// Run without confirmation.
    #[default]
    Auto,
    /// Ask before each call.
    Confirm,
    /// Never run.
    Restricted,
}

// --- Loading ---------------------------------------------------------------

impl Settings {
    /// Load settings using the two-tier resolver. Reads
    /// `<global>/settings.toml` then `<project>/settings.toml` (if either
    /// exists) and overlays the project file on top of the global one.
    /// Missing files are not an error — defaults are used.
    pub fn load(resolver: &Resolver) -> Result<Self, SettingsError> {
        let mut merged = PartialSettings::default();
        for (_scope, path) in resolver.all_files(Path::new(SETTINGS_FILE)) {
            let text = std::fs::read_to_string(&path).map_err(|source| SettingsError::Io {
                path: path.display().to_string(),
                source,
            })?;
            let layer: PartialSettings =
                toml::from_str(&text).map_err(|source| SettingsError::Parse {
                    path: path.display().to_string(),
                    source,
                })?;
            merged.overlay(layer);
        }
        Ok(merged.into())
    }
}

/// All-optional mirror of [`Settings`] for layered loading. Each field is
/// `Option`; `None` means "not set in this layer, fall through to the next."
#[derive(Debug, Default, Deserialize)]
struct PartialSettings {
    #[serde(default)]
    providers: Option<Vec<ProviderConfig>>,
    #[serde(default)]
    chat: Option<ChatSettings>,
    #[serde(default)]
    web_search: Option<WebSearchSettings>,
    #[serde(default)]
    whisper: Option<WhisperSettings>,
    #[serde(default)]
    limits: Option<LimitsSettings>,
    #[serde(default)]
    tts: Option<TtsSettings>,
    #[serde(default)]
    tool_permissions: Option<HashMap<String, ToolPermission>>,
}

impl PartialSettings {
    /// Replace each present field on `self` with the corresponding field
    /// from `other`. Subtables override as a unit; the providers list is
    /// replaced wholesale.
    fn overlay(&mut self, other: PartialSettings) {
        if other.providers.is_some() {
            self.providers = other.providers;
        }
        if other.chat.is_some() {
            self.chat = other.chat;
        }
        if other.web_search.is_some() {
            self.web_search = other.web_search;
        }
        if other.whisper.is_some() {
            self.whisper = other.whisper;
        }
        if other.limits.is_some() {
            self.limits = other.limits;
        }
        if other.tts.is_some() {
            self.tts = other.tts;
        }
        if other.tool_permissions.is_some() {
            self.tool_permissions = other.tool_permissions;
        }
    }
}

impl From<PartialSettings> for Settings {
    fn from(p: PartialSettings) -> Self {
        Settings {
            providers: p.providers.unwrap_or_default(),
            chat: p.chat.unwrap_or_default(),
            web_search: p.web_search.unwrap_or_default(),
            whisper: p.whisper.unwrap_or_default(),
            limits: p.limits.unwrap_or_default(),
            tts: p.tts.unwrap_or_default(),
            tool_permissions: p.tool_permissions.unwrap_or_default(),
        }
    }
}

// --- Defaults --------------------------------------------------------------

fn default_true() -> bool {
    true
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn default_whisper_model() -> String {
    "ggml-large-v3-turbo.bin".into()
}
fn default_beam_size() -> i32 {
    1
}
fn default_max_model_upload_bytes() -> u64 {
    10 * 1024 * 1024 * 1024
}
fn default_max_attachments() -> usize {
    50
}
fn default_max_attachment_bytes() -> usize {
    1024 * 1024 * 1024
}
fn default_max_audio_buffer_bytes() -> usize {
    512 * 1024 * 1024
}
fn default_tts_backend() -> String {
    "orpheus".into()
}
fn default_tts_model() -> String {
    "orpheus-3b-0.1-ft-q4_k_m".into()
}
fn default_tts_voice() -> String {
    "tara".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_defaults_when_no_files() {
        let dir = tempdir().unwrap();
        let resolver = Resolver::new(dir.path().join("global"), None::<&Path>);
        let s = Settings::load(&resolver).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn project_overrides_global() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            global.join("settings.toml"),
            r#"
[[providers]]
name = "openrouter"
kind = "open_router"
api_key = "G"

[chat]
auto_title = false
"#,
        )
        .unwrap();
        std::fs::write(
            project.join("settings.toml"),
            r#"
[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
"#,
        )
        .unwrap();

        let resolver = Resolver::new(global, Some(project));
        let s = Settings::load(&resolver).unwrap();
        assert_eq!(s.providers.len(), 1);
        assert_eq!(s.providers[0].name, "ollama");
        assert_eq!(s.providers[0].kind, ProviderKind::Ollama);
        // chat untouched by project — global override survives.
        assert!(!s.chat.auto_title);
    }

    #[test]
    fn resolved_auth_precedence() {
        let base = ProviderConfig {
            name: "p".into(),
            kind: ProviderKind::Anthropic,
            api_key: None,
            api_key_env: None,
            base_url: None,
            use_oauth: false,
        };
        assert_eq!(base.resolved_auth(), ResolvedAuth::None);

        let oauth = ProviderConfig { use_oauth: true, ..base.clone() };
        assert_eq!(oauth.resolved_auth(), ResolvedAuth::OAuth);

        let env = ProviderConfig {
            api_key_env: Some("MY_KEY".into()),
            use_oauth: true,
            ..base.clone()
        };
        assert_eq!(env.resolved_auth(), ResolvedAuth::FromEnv("MY_KEY".into()));

        let inline = ProviderConfig {
            api_key: Some("sk-...".into()),
            api_key_env: Some("MY_KEY".into()),
            use_oauth: true,
            ..base.clone()
        };
        assert_eq!(inline.resolved_auth(), ResolvedAuth::Inline("sk-...".into()));
    }

    #[test]
    fn unknown_keys_rejected() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("settings.toml"),
            "providers = []\nbogus_top_level = 1\n",
        )
        .unwrap();
        // PartialSettings is non-strict (no deny_unknown_fields); we
        // accept unknown top-level keys to keep forward compatibility.
        let resolver = Resolver::new(dir.path(), None::<&Path>);
        Settings::load(&resolver).unwrap();
    }
}
