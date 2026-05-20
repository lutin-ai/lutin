//! Singleton agent runner. One in-flight turn at a time; concurrent
//! `Send` requests are dropped on the floor (the UI should disable the
//! composer while busy). Each turn (re)builds the provider + toolbox
//! from the on-disk persona + settings so TOML edits take effect on
//! the next turn.

use std::path::PathBuf;
use std::sync::Arc;

use lutin_entities::Persona;
use lutin_llm::Message;
use lutin_settings::Settings;
use lutin_storage::Resolver;
use lutin_workflow_sdk::agent::{BuildArgs, BuildError, build_inputs};
use lutin_workflow_sdk::prompt::PromptExtras;
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::principle::PRINCIPLES;
use crate::runtime;
use crate::serve::load_session;
use crate::store;
use crate::types::{Agent, TurnOutcome};
use crate::wire::{ChatEvent, FinishReason, TurnId};

const DEFAULT_PERSONA: &str = "coder";

pub enum AgentCmd {
    Send { text: String, turn: TurnId },
    Cancel,
}

#[derive(Clone)]
pub struct RunnerCtx {
    pub state_dir: PathBuf,
    pub project_config_dir: PathBuf,
    pub resolver: Arc<Resolver>,
    pub events: broadcast::Sender<ChatEvent>,
}

pub async fn run_agent_loop(ctx: RunnerCtx, mut rx: mpsc::UnboundedReceiver<AgentCmd>) {
    loop {
        let Some(cmd) = rx.recv().await else { return };
        let (text, turn) = match cmd {
            AgentCmd::Send { text, turn } => (text, turn),
            AgentCmd::Cancel => continue,
        };

        let ctx_task = ctx.clone();
        let mut task = tokio::spawn(async move { handle_send(&ctx_task, text, turn).await });

        loop {
            tokio::select! {
                res = &mut task => {
                    if let Err(e) = res
                        && !e.is_cancelled()
                    {
                        warn!(error = %e, "reviewed turn task panicked");
                    }
                    break;
                }
                next = rx.recv() => {
                    match next {
                        Some(AgentCmd::Cancel) => {
                            task.abort();
                            let _ = ctx.events.send(ChatEvent::TurnFinished {
                                turn_id: turn,
                                reason: FinishReason::Cancelled,
                            });
                            let _ = (&mut task).await;
                            break;
                        }
                        Some(AgentCmd::Send { .. }) => {
                            // Singleton runner; UI should gate sends.
                        }
                        None => {
                            task.abort();
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn handle_send(ctx: &RunnerCtx, text: String, turn: TurnId) {
    let id = format!("u-{}", turn.0);
    let _ = ctx
        .events
        .send(ChatEvent::UserMessageAppended { id, text: text.clone() });

    let result = run_turn(ctx, text).await;
    let reason = match result {
        Ok(()) => FinishReason::Completed,
        Err(message) => {
            warn!(error = %message, "reviewed turn failed");
            FinishReason::Failed { message }
        }
    };
    let _ = ctx.events.send(ChatEvent::TurnFinished { turn_id: turn, reason });
}

async fn run_turn(ctx: &RunnerCtx, user_text: String) -> Result<(), String> {
    let mut agent = build_agent(ctx).map_err(|e| format!("build agent: {e}"))?;
    agent.messages.push(Message::User(user_text));

    let outcome = runtime::run_turn(&mut agent, &PRINCIPLES)
        .await
        .map_err(|e| format!("run_turn: {e}"))?;
    if let Err(e) = store::save(&agent) {
        warn!(error = %e, "reviewed: persist state failed");
    }
    match outcome {
        TurnOutcome::Yield { reply: _ } => Ok(()),
    }
}

fn build_agent(ctx: &RunnerCtx) -> Result<Agent, String> {
    let session = load_session(&ctx.state_dir).map_err(|e| format!("load session: {e}"))?;
    let persona_name = session.persona.as_deref().unwrap_or(DEFAULT_PERSONA);
    let persona: Persona = Persona::load(&ctx.resolver, persona_name)
        .map_err(|e| format!("load persona `{persona_name}`: {e}"))?;
    let settings = Settings::load(&ctx.resolver).map_err(|e| format!("load settings: {e}"))?;

    let sandbox_root = ctx
        .project_config_dir
        .parent()
        .ok_or_else(|| {
            format!(
                "project_config_dir has no parent: {}",
                ctx.project_config_dir.display()
            )
        })?
        .to_path_buf();

    let args = BuildArgs {
        persona: &persona,
        settings: &settings,
        sandbox_root,
        model_override: None,
        extra_tools: Vec::new(),
        prompt_extras: PromptExtras::default(),
        disable_streaming: true,
    };
    let (config, toolbox) = build_inputs(args).map_err(map_build_error)?;

    let saved = store::load(&ctx.state_dir).map_err(|e| format!("load state: {e}"))?;
    let mut messages = match saved {
        Some(s) => s.messages,
        None => Vec::new(),
    };
    if messages.is_empty() && !config.system.is_empty() {
        messages.push(Message::System(config.system.clone()));
    }

    // Match scratchpad: Qwen-family models loop with greedy decoding,
    // so default sampling knobs nudge them off it.
    let temperature = persona.temperature.or(Some(0.6));
    let presence_penalty = persona.presence_penalty.or(Some(1.5));

    Ok(Agent {
        persona: persona.name.clone(),
        provider: config.provider,
        model: config.model,
        temperature,
        presence_penalty,
        messages,
        toolbox,
        state_dir: ctx.state_dir.clone(),
        resolver: ctx.resolver.clone(),
        events: ctx.events.clone(),
    })
}

fn map_build_error(e: BuildError) -> String {
    match e {
        BuildError::ProviderNotFound(n) => format!("provider not configured: {n}"),
        BuildError::ProviderMisconfigured { name, reason } => {
            format!("provider {name} misconfigured: {reason}")
        }
        BuildError::ProviderUnsupported(s) => format!("provider unsupported: {s}"),
        BuildError::PersonaMissingProvider(p) => format!("persona {p} has no provider"),
        BuildError::PersonaMissingModel(p) => format!("persona {p} has no model"),
        BuildError::Toolbox(s) => format!("toolbox: {s}"),
    }
}
