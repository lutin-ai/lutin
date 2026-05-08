//! Build a configured [`Agent`] from a persona + settings.
//!
//! The chat workflow used to inline this logic; pulling it here means
//! every workflow gets the same provider construction, sampling-params
//! plumbing, and tool wiring. Workflows still own their own per-session
//! state shape and the agent runner loop — this module only assembles
//! the agent itself, not the surrounding lifecycle.

use std::path::PathBuf;
use std::sync::Arc;

use lutin_agent_sdk::{
    slide_window_by_user_turns, Agent, AgentConfig, LoopConfig, SamplingParams, Tool, ToolPolicy,
    Toolbox,
};
use lutin_entities::{Persona, ToolFilterMode};
use lutin_llm::anthropic::{AnthropicAuth, AnthropicProvider, OAuthCredentialStore};
use lutin_llm::ollama::{OllamaConfig, OllamaProvider};
use lutin_llm::openai_compat_provider::{OpenAiCompatConfig, OpenAiCompatProvider};
use lutin_llm::openrouter::{OpenRouterConfig, OpenRouterProvider};
use lutin_llm::{LlmProvider, ModelId};
use lutin_settings::{ProviderConfig, ProviderKind, ResolvedAuth, Settings};
use lutin_tools::{FilterMode, ReadState, ToolContext};
use thiserror::Error;

use crate::prompt::{self, PromptContext, PromptExtras};

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("provider not configured: {0}")]
    ProviderNotFound(String),
    #[error("provider '{name}' misconfigured: {reason}")]
    ProviderMisconfigured { name: String, reason: String },
    #[error("provider kind unsupported: {0}")]
    ProviderUnsupported(String),
    #[error("persona '{0}' has no provider configured")]
    PersonaMissingProvider(String),
    #[error("persona '{0}' has no model and no override is set")]
    PersonaMissingModel(String),
    #[error("toolbox: {0}")]
    Toolbox(String),
}

/// Inputs for [`build_agent`]. Borrowed where the call site already
/// owns the value, owned where the helper needs to take it apart.
pub struct BuildArgs<'a> {
    pub persona: &'a Persona,
    pub settings: &'a Settings,
    /// Sandbox root for the standard tool set. Filesystem tools jail
    /// every path below this directory; `shell` runs commands here.
    pub sandbox_root: PathBuf,
    /// Per-session model override; when `Some`, beats `persona.model`.
    pub model_override: Option<String>,
    /// Tools to splice in alongside the standard toolset. Workflow
    /// hooks live here — e.g. the chat workflow's sub-agent registry
    /// tools (`spawn_agent`, `get_agent`, `stop_agent`). Like the
    /// default tools they go through the persona's `tool_filter_list`
    /// filter, so a persona that doesn't whitelist them simply doesn't
    /// see them. Pass `Vec::new()` if not needed.
    pub extra_tools: Vec<Box<dyn Tool>>,
    /// Workflow-supplied chat state for `%placeholder%` substitution
    /// in the persona's system prompt. The SDK fills `persona:*` and
    /// `cwd` automatically; everything else (message_count, attached
    /// agents, latest response, variables, …) the workflow provides.
    /// Defaults to empty — placeholders for unset fields collapse to
    /// empty strings, so personas without placeholders don't notice.
    pub prompt_extras: PromptExtras,
}

/// Assemble an [`Agent`] ready to run one round-loop. Workflows that
/// keep an agent across turns should call this once at startup, then
/// [`refresh_agent`] before each turn to pick up persona/settings
/// edits. Workflows that build an agent per turn can call this every
/// time — the cost is a `reqwest::Client` and a few `Box`es.
pub fn build_agent(args: BuildArgs<'_>) -> Result<Agent, BuildError> {
    let (config, toolbox) = build_inputs(args)?;
    let mut agent = Agent::new(config);
    // try_set_tools only fails if the agent is mid-run; we just built it.
    agent
        .try_set_tools(toolbox)
        .map_err(|_| BuildError::Toolbox("agent unexpectedly busy".into()))?;
    Ok(agent)
}

/// Re-derive the agent's config + toolbox from the current persona +
/// settings and apply them in place. Use this between turns when the
/// agent is held across the workflow's lifetime: the agent's
/// `messages` survive (no duplication), provider/model/sampling/
/// system-prompt/tools all refresh, so out-of-band TOML edits take
/// effect on the next turn.
///
/// Fails with [`BuildError::Toolbox`] if the agent is mid-run; callers
/// should only call this when they know the agent is idle.
pub fn refresh_agent(agent: &mut Agent, args: BuildArgs<'_>) -> Result<(), BuildError> {
    let (config, toolbox) = build_inputs(args)?;
    agent
        .update_config(|c| *c = config)
        .map_err(|_| BuildError::Toolbox("agent busy during refresh".into()))?;
    agent
        .try_set_tools(toolbox)
        .map_err(|_| BuildError::Toolbox("agent busy during tool refresh".into()))?;
    Ok(())
}

/// Resolve a persona + settings into the (config, toolbox) pair used
/// to construct or refresh an agent. Internal seam between
/// [`build_agent`] and [`refresh_agent`] so they can't drift.
fn build_inputs(args: BuildArgs<'_>) -> Result<(AgentConfig, Toolbox), BuildError> {
    let BuildArgs {
        persona,
        settings,
        sandbox_root,
        model_override,
        extra_tools,
        prompt_extras,
    } = args;

    let provider_name = persona
        .provider
        .as_deref()
        .ok_or_else(|| BuildError::PersonaMissingProvider(persona.name.clone()))?;
    let provider_cfg = settings
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| BuildError::ProviderNotFound(provider_name.to_string()))?;
    let provider = build_provider(provider_cfg)?;

    let model = model_override
        .or_else(|| persona.model.clone())
        .ok_or_else(|| BuildError::PersonaMissingModel(persona.name.clone()))?;

    // ReasoningParams isn't re-exported from lutin-agent-sdk, so the
    // effort/max-tokens knobs on Persona aren't plumbed through yet —
    // only `thinking_enabled` is.
    let sampling = SamplingParams {
        temperature: persona.temperature,
        thinking_enabled: persona.thinking_enabled,
        penalties: persona
            .presence_penalty
            .map(|presence| lutin_agent_sdk::PenaltyParams {
                presence,
                frequency: 0.0,
            }),
        ..SamplingParams::default()
    };

    let tool_ctx = Arc::new(ToolContext {
        root: sandbox_root.clone(),
        env: Arc::from([]),
        http: reqwest::Client::new(),
        read_state: Arc::new(ReadState::new(sandbox_root.clone())),
    });
    let mode = match persona.tool_filter_mode {
        ToolFilterMode::Whitelist => FilterMode::Whitelist,
        ToolFilterMode::Blacklist => FilterMode::Blacklist,
    };
    let mut tools = lutin_tools::default_tools(
        Arc::clone(&tool_ctx),
        settings.web_search.brave_api_key.clone(),
    );
    tools.extend(extra_tools);
    let tools = lutin_tools::filter_by_name(tools, mode, &persona.tool_filter_list);
    let toolbox = Toolbox::new(tools).map_err(|e| BuildError::Toolbox(e.to_string()))?;

    let mut loop_config = LoopConfig::default();
    if let Some(n) = persona.sliding_window_messages {
        let n = n as usize;
        loop_config.message_projector = Some(Arc::new(move |msgs: &[lutin_llm::Message]| {
            slide_window_by_user_turns(msgs, n)
        }));
    }

    let cwd = sandbox_root.to_string_lossy().to_string();
    let prompt_ctx = PromptContext::from_parts(persona, &cwd, &prompt_extras);
    let system = prompt::resolve(&persona.system_prompt, &prompt_ctx);

    let config = AgentConfig {
        provider,
        model: ModelId::new(model),
        sampling,
        system,
        tool_policy: ToolPolicy::default(),
        loop_config,
    };
    Ok((config, toolbox))
}

/// Construct a provider from one [`ProviderConfig`] entry. Resolves the
/// `(api_key, api_key_env, use_oauth)` tri-state once at the boundary.
pub fn build_provider(cfg: &ProviderConfig) -> Result<Arc<dyn LlmProvider>, BuildError> {
    let auth = cfg.resolved_auth();
    let resolve_key = |required_hint: &str| -> Result<String, BuildError> {
        match &auth {
            ResolvedAuth::Inline(k) => Ok(k.clone()),
            ResolvedAuth::FromEnv(var) => std::env::var(var).map_err(|_| {
                BuildError::ProviderMisconfigured {
                    name: cfg.name.clone(),
                    reason: format!("env var {var} unset"),
                }
            }),
            ResolvedAuth::OAuth | ResolvedAuth::None => {
                Err(BuildError::ProviderMisconfigured {
                    name: cfg.name.clone(),
                    reason: required_hint.to_string(),
                })
            }
        }
    };
    let http = reqwest::Client::new();
    Ok(match cfg.kind {
        ProviderKind::OpenRouter => {
            let api_key = resolve_key("openrouter requires api_key or api_key_env")?;
            Arc::new(OpenRouterProvider::new(
                OpenRouterConfig {
                    api_key,
                    app_name: None,
                    app_url: None,
                },
                http,
            ))
        }
        ProviderKind::Ollama => {
            let base_url = cfg
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".into());
            Arc::new(OllamaProvider::new(OllamaConfig { base_url }, http))
        }
        ProviderKind::Anthropic => {
            let auth = match &auth {
                ResolvedAuth::OAuth => {
                    let store = OAuthCredentialStore::load().map_err(|e| {
                        BuildError::ProviderMisconfigured {
                            name: cfg.name.clone(),
                            reason: format!("oauth load failed: {e}"),
                        }
                    })?;
                    AnthropicAuth::OAuthSubscription(store)
                }
                ResolvedAuth::Inline(k) => AnthropicAuth::ApiKey(k.clone()),
                ResolvedAuth::FromEnv(var) => {
                    let key = std::env::var(var).map_err(|_| {
                        BuildError::ProviderMisconfigured {
                            name: cfg.name.clone(),
                            reason: format!("env var {var} unset"),
                        }
                    })?;
                    AnthropicAuth::ApiKey(key)
                }
                ResolvedAuth::None => {
                    return Err(BuildError::ProviderMisconfigured {
                        name: cfg.name.clone(),
                        reason: "anthropic requires api_key, api_key_env, or use_oauth".into(),
                    });
                }
            };
            Arc::new(AnthropicProvider::new(auth))
        }
        ProviderKind::OpenAiCompat => {
            let base_url = cfg.base_url.clone().ok_or_else(|| {
                BuildError::ProviderMisconfigured {
                    name: cfg.name.clone(),
                    reason: "openai_compat requires base_url".into(),
                }
            })?;
            // Inline key, env-var key, or no auth — OAuth doesn't apply here.
            let api_key = match &auth {
                ResolvedAuth::Inline(k) => Some(k.clone()),
                ResolvedAuth::FromEnv(var) => {
                    Some(std::env::var(var).map_err(|_| BuildError::ProviderMisconfigured {
                        name: cfg.name.clone(),
                        reason: format!("env var {var} unset"),
                    })?)
                }
                ResolvedAuth::None => None,
                ResolvedAuth::OAuth => {
                    return Err(BuildError::ProviderMisconfigured {
                        name: cfg.name.clone(),
                        reason: "openai_compat does not support use_oauth".into(),
                    });
                }
            };
            Arc::new(OpenAiCompatProvider::new(
                OpenAiCompatConfig { base_url, api_key },
                http,
            ))
        }
    })
}
