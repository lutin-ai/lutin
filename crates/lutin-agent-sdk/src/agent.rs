use std::sync::Arc;

use futures::Stream;
use lutin_llm::Message;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::{
    approval::{AllowAll, ApprovalPolicy},
    config::AgentConfig,
    error::{AgentBusy, AgentError},
    event::AgentEvent,
    loop_control::FinishReason,
    outcome::RunOutcome,
    run::{drive, DriveResult, RunInputs},
    tools::{NoTools, Toolbox},
};

enum AgentState {
    Idle,
    Running {
        handle: JoinHandle<DriveResult>,
        cancel: Option<oneshot::Sender<()>>,
    },
    Done(RunOutcome),
}

/// Stateful agent handle; owns provider + config; runs at most one round-loop at a time.
pub struct Agent {
    config: AgentConfig,
    messages: Vec<Message>,
    tools: Arc<Toolbox>,
    approval: Arc<dyn ApprovalPolicy>,
    state: AgentState,
}

impl Agent {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            messages: Vec::new(),
            tools: Arc::new(NoTools::toolbox()),
            approval: Arc::new(AllowAll),
            state: AgentState::Idle,
        }
    }

    #[inline]
    fn ensure_mutable(&self) -> Result<(), AgentBusy> {
        match &self.state {
            AgentState::Running { .. } => Err(AgentBusy),
            _ => Ok(()),
        }
    }

    /// Mutate the agent's [`AgentConfig`] in place. Fails with [`AgentBusy`] if a run is
    /// in flight. Replaces the family of single-field setters that previously lived here.
    pub fn update_config<F: FnOnce(&mut AgentConfig)>(
        &mut self,
        f: F,
    ) -> Result<(), AgentBusy> {
        self.ensure_mutable()?;
        f(&mut self.config);
        Ok(())
    }

    pub fn push_message(&mut self, m: Message) -> Result<(), AgentBusy> {
        self.ensure_mutable()?;
        self.messages.push(m);
        Ok(())
    }
    pub fn edit_messages(
        &mut self,
        f: impl FnOnce(&mut Vec<Message>),
    ) -> Result<(), AgentBusy> {
        self.ensure_mutable()?;
        f(&mut self.messages);
        Ok(())
    }
    pub fn try_set_tools(&mut self, t: Toolbox) -> Result<(), AgentBusy> {
        self.ensure_mutable()?;
        self.tools = Arc::new(t);
        Ok(())
    }
    pub fn try_set_approval(&mut self, a: Box<dyn ApprovalPolicy>) -> Result<(), AgentBusy> {
        self.ensure_mutable()?;
        self.approval = Arc::from(a);
        Ok(())
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, AgentState::Running { .. })
    }

    pub fn last_outcome(&self) -> Option<&RunOutcome> {
        match &self.state {
            AgentState::Done(o) => Some(o),
            _ => None,
        }
    }

    /// Spawn the round-loop in a background task.
    ///
    /// Returns a stream of [`AgentEvent`]s for the run, or [`AgentBusy`] if a run is already
    /// in-flight. The run owns the message history until [`Agent::join`] resolves; mutating
    /// methods return [`AgentBusy`] during that window.
    pub fn start(
        &mut self,
    ) -> Result<impl Stream<Item = AgentEvent> + Send + Unpin + 'static, AgentBusy> {
        if self.is_running() {
            return Err(AgentBusy);
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();

        let model_for_span = self.config.model.clone();
        let inputs = RunInputs {
            config: self.config.clone(),
            // AgentBusy blocks mutation during the run; move history into task, restore on join.
            messages: std::mem::take(&mut self.messages),
            tools: Arc::clone(&self.tools),
            approval: Arc::clone(&self.approval),
        };

        let tx_for_task = tx.clone();
        let span = tracing::info_span!("agent.run", model = %model_for_span);
        let handle = tokio::spawn(
            async move { drive(inputs, tx_for_task, cancel_rx).await }.instrument(span),
        );

        self.state = AgentState::Running { handle, cancel: Some(cancel_tx) };
        drop(tx);

        Ok(Box::pin(tokio_stream::wrappers::UnboundedReceiverStream::new(rx)))
    }

    /// Await the in-flight run and restore message history to the agent.
    ///
    /// On success the returned [`RunOutcome`] is cached (see [`Agent::last_outcome`]) and
    /// [`Agent::messages`] reflects the final transcript. Calling `join` when idle returns
    /// a default outcome; calling it again after completion returns a clone of the cached
    /// outcome. A panic in the driver task is surfaced as `FinishReason::Error`.
    pub async fn join(&mut self) -> RunOutcome {
        let state = std::mem::replace(&mut self.state, AgentState::Idle);
        match state {
            AgentState::Running { handle, .. } => match handle.await {
                Ok(DriveResult { outcome, messages }) => {
                    self.messages = messages;
                    self.state = AgentState::Done(outcome.clone());
                    outcome
                }
                Err(join_err) => {
                    let err = std::sync::Arc::new(AgentError::Internal(format!(
                        "task panicked: {join_err}"
                    )));
                    let outcome = RunOutcome {
                        last_assistant: None,
                        usage: lutin_llm::Usage::default(),
                        rounds: 0,
                        finish_reason: FinishReason::Error(err),
                    };
                    self.state = AgentState::Done(outcome.clone());
                    outcome
                }
            },
            AgentState::Done(o) => {
                let clone = o.clone();
                self.state = AgentState::Done(o);
                clone
            }
            AgentState::Idle => RunOutcome {
                last_assistant: None,
                usage: lutin_llm::Usage::default(),
                rounds: 0,
                finish_reason: FinishReason::Stopped,
            },
        }
    }

    /// Request cancellation of the in-flight run.
    ///
    /// Best-effort and non-blocking. No-op when the agent is idle, already finished, or when
    /// cancel has previously been sent. Completion is still observed via the event stream
    /// (terminal `AgentEvent::Finished(FinishReason::Cancelled)`) or by awaiting
    /// [`Agent::join`]. Safe to call multiple times.
    pub fn cancel(&mut self) {
        if let AgentState::Running { cancel, .. } = &mut self.state
            && let Some(tx) = cancel.take()
        {
            let _ = tx.send(());
        }
    }
}

