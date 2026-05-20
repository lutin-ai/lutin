//! Per-turn persona/settings/sandbox resolution and the [`BuildArgs`]
//! plumbing the SDK needs to instantiate or refresh an `Agent`.
//!
//! [`ResolvedArgs`] is the owned bundle the SDK borrows from; built
//! fresh every turn so out-of-band edits to `state.toml`, persona TOML,
//! or settings TOML take effect on the next turn.

use std::path::PathBuf;

use lutin_entities::Persona;
use lutin_settings::Settings;
use lutin_workflow_sdk::agent::{BuildArgs, BuildError};
use lutin_workflow_sdk::prompt::PromptExtras;
use principled::{ChatError, load_state};
use tokio::sync::mpsc;

use crate::agents;
use crate::review;
use crate::runner::RunnerCtx;
use crate::tools;

pub(crate) const DEFAULT_PERSONA: &str = "assistant";

/// Owned bundle of inputs the SDK's `BuildArgs` borrows from.
/// Re-resolved per turn so out-of-band edits to `state.toml`,
/// persona TOML, or settings TOML take effect on the next turn.
pub(crate) struct ResolvedArgs {
    pub(crate) persona: Persona,
    pub(crate) settings: Settings,
    pub(crate) sandbox_root: PathBuf,
    pub(crate) model_override: Option<String>,
    /// Resolved per-session reviewer fan-out concurrency. Already
    /// defaulted + clamped via `SessionState::review_concurrency`.
    pub(crate) review_concurrency: usize,
}

impl ResolvedArgs {
    /// Bind the resolved inputs to the SDK's build interface. Sub-agent
    /// tools are constructed fresh per call (each one closes over a
    /// clone of the registry sender from `ctx`); the persona's filter
    /// then drops them for non-orchestrator personas — see
    /// `tools::agent` for the gating story.
    pub(crate) fn as_build_args<'a>(&'a self, ctx: &RunnerCtx) -> BuildArgs<'a> {
        self.as_build_args_with(ctx, PromptExtras::default(), None, None)
    }

    /// `rewind_tx` is `Some` only when the caller is the principled
    /// review path that owns the channel — currently `run_turn`. When
    /// `None` (sub-agent builds, startup pre-turn refresh) the
    /// `abort_step` tool is omitted: there's nothing to rewind into,
    /// so exposing the tool would be a footgun for the model.
    pub(crate) fn as_build_args_with<'a>(
        &'a self,
        ctx: &RunnerCtx,
        prompt_extras: PromptExtras,
        owner_id: Option<agents::AgentId>,
        rewind_tx: Option<mpsc::UnboundedSender<review::RewindSignal>>,
    ) -> BuildArgs<'a> {
        let mut extra_tools = tools::make_subagent_tools(
            ctx.agent_registry.clone(),
            ctx.resolver.clone(),
            owner_id,
        );
        if let Some(tx) = rewind_tx {
            extra_tools.push(tools::make_abort_step_tool(tx));
        }
        BuildArgs {
            persona: &self.persona,
            settings: &self.settings,
            sandbox_root: self.sandbox_root.clone(),
            model_override: self.model_override.clone(),
            extra_tools,
            prompt_extras,
            disable_streaming: true,
        }
    }
}

/// Resolve the chat-specific inputs the SDK needs from on-disk state.
/// Translates SDK-agnostic errors (file IO, persona-not-found) back to
/// the chat protocol's typed variants.
///
/// `persona_override` lets a caller skip the disk read by handing in
/// an already-loaded `Persona` (sub-agent spawn path — the tool
/// boundary validated and loaded it once). `None` reads the session's
/// configured persona from disk; this is the main-session path that
/// honours out-of-band edits.
pub(crate) fn resolve_args(
    ctx: &RunnerCtx,
    persona_override: Option<Persona>,
) -> Result<ResolvedArgs, ChatError> {
    let session_state = load_state(&ctx.state_dir)
        .map_err(|e| ChatError::Internal(format!("load state: {e}")))?;

    let persona = match persona_override {
        Some(p) => p,
        None => {
            let name = session_state.persona.as_deref().unwrap_or(DEFAULT_PERSONA);
            Persona::load(&ctx.resolver, name).map_err(|e| match e {
                lutin_entities::EntityError::NotFound { name, .. } => {
                    ChatError::PersonaNotFound(name)
                }
                other => ChatError::Internal(format!("load persona: {other}")),
            })?
        }
    };
    let settings = Settings::load(&ctx.resolver)
        .map_err(|e| ChatError::Internal(format!("load settings: {e}")))?;

    // Sandbox root: the project workspace itself, not `.lutin/`. Tools
    // jail filesystem access here so the agent can read/edit user code.
    let sandbox_root = ctx
        .project_config_dir
        .parent()
        .ok_or_else(|| {
            ChatError::Internal(format!(
                "project_config_dir has no parent: {}",
                ctx.project_config_dir.display()
            ))
        })?
        .to_path_buf();

    let review_concurrency = session_state.review_concurrency();
    Ok(ResolvedArgs {
        persona,
        settings,
        sandbox_root,
        model_override: session_state.model_override,
        review_concurrency,
    })
}

pub(crate) fn map_build_error(e: BuildError) -> ChatError {
    match e {
        BuildError::ProviderNotFound(n) => ChatError::ProviderNotFound(n),
        BuildError::ProviderMisconfigured { name, reason } => {
            ChatError::ProviderMisconfigured { name, reason }
        }
        BuildError::ProviderUnsupported(s) => ChatError::ProviderUnsupported(s),
        BuildError::PersonaMissingProvider(_)
        | BuildError::PersonaMissingModel(_)
        | BuildError::Toolbox(_) => ChatError::Internal(e.to_string()),
    }
}
