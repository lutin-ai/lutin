//! Chat workflow UI plugin.
//!
//! Compiled into the crate's `cdylib` artifact and dlopen'd by
//! `lutin-desktop`. The factory `create_workflow` is the only `extern`
//! symbol; everything else hangs off the `Workflow` trait object it
//! returns.
//!
//! One UI surface: `SessionUi` (Main pane) — scrollback, composer,
//! persona indicator. Project-level chrome (sidebar header, top-bar
//! label, "+ New" button) lives in desktop now; this cdylib is only
//! invoked once a session opens.
//!
//! State ownership follows message-passing-over-shared-state: the
//! session pump tokio task is the sole writer of session state,
//! publishing immutable `Arc<SessionSnapshot>` values via
//! `tokio::sync::watch`. UI render borrows the latest snapshot
//! synchronously and clones the `Arc` (cheap). UI → pump intents
//! (submit, cancel) flow through an `mpsc::UnboundedSender<SessionIntent>`
//! so the pump can mutate its owned state and republish.

use std::sync::Arc;

use egui::{Color32, RichText, ScrollArea, TextEdit};
use lutin_protocol::{Frame, decode as frame_decode, encode as frame_encode};
use lutin_workflow_ui::{
    SessionCtx, SessionEndpoint, Transport, Workflow, WorkflowSessionUi,
};
use tokio::sync::{mpsc, watch};

use crate::{
    ChatEvent, ChatOk, ChatRequest, ChatResponse, FinishReason, HistoricalMessage, HistoricalRole,
    SessionState, decode as chat_decode, encode as chat_encode,
};

// ─── Domain types ────────────────────────────────────────────────────

#[derive(Clone)]
struct Message {
    role: Role,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// Per-session state. Authored by the session pump; published as an
/// immutable `Arc<SessionSnapshot>` to the UI.
///
/// `completed` holds finished user/assistant turns. The currently
/// streaming assistant turn (if any) is held separately in
/// `Turn::Streaming { buf }`, so "trailing-empty-assistant means
/// streaming" is no longer encoded implicitly. (See
/// `make-illegal-states-unrepresentable.md`.)
#[derive(Clone, Default)]
struct SessionSnapshot {
    persona: Option<String>,
    completed: Vec<Message>,
    turn: Turn,
}

#[derive(Clone, Default)]
enum Turn {
    #[default]
    Idle,
    Streaming {
        buf: String,
    },
    Errored(String),
}

// ─── Intents (UI → pump) ─────────────────────────────────────────────

enum SessionIntent {
    /// Optimistically append a user message and mark the session
    /// streaming, then forward `SendMessage` over the transport.
    Submit { text: String },
    /// Forward `Cancel` over the transport.
    Cancel,
}

// ─── Workflow root ───────────────────────────────────────────────────

/// Top-level workflow object. Holds no shared mutable state — each
/// `open_session` call hands its `Transport` to a fresh pump that owns
/// the session's state.
#[derive(Default)]
pub struct ChatWorkflow {}

impl Workflow for ChatWorkflow {
    fn open_session(
        &self,
        endpoint: SessionEndpoint,
        transport: Transport,
    ) -> Box<dyn WorkflowSessionUi> {
        Box::new(SessionUi::new(endpoint, transport))
    }
}

// ─── Session-scoped UI ───────────────────────────────────────────────

struct SessionUi {
    endpoint: SessionEndpoint,
    composer: String,
    rx: watch::Receiver<Arc<SessionSnapshot>>,
    intents: mpsc::UnboundedSender<SessionIntent>,
}

impl SessionUi {
    fn new(endpoint: SessionEndpoint, transport: Transport) -> Self {
        let (snap_tx, snap_rx) = watch::channel(Arc::new(SessionSnapshot::default()));
        let (intent_tx, intent_rx) = mpsc::unbounded_channel::<SessionIntent>();
        spawn_session_pump(transport, snap_tx, intent_rx);
        Self {
            endpoint,
            composer: String::new(),
            rx: snap_rx,
            intents: intent_tx,
        }
    }

    fn submit(&mut self) {
        let text = std::mem::take(&mut self.composer);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let _ = self.intents.send(SessionIntent::Submit {
            text: trimmed.to_string(),
        });
    }

    fn cancel(&self) {
        let _ = self.intents.send(SessionIntent::Cancel);
    }
}

impl WorkflowSessionUi for SessionUi {
    fn render(&mut self, _ctx: SessionCtx<'_>, ui: &mut egui::Ui) {
        let snap: Arc<SessionSnapshot> = self.rx.borrow().clone();

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("session {}", self.endpoint.session)).strong());
            if let Some(persona) = &snap.persona {
                ui.label(RichText::new(format!("· {persona}")).weak());
            }
            if matches!(snap.turn, Turn::Streaming { .. }) {
                ui.label(RichText::new("· streaming…").italics());
                if ui.button("Cancel").clicked() {
                    self.cancel();
                }
            }
        });
        if let Turn::Errored(err) = &snap.turn {
            ui.colored_label(Color32::from_rgb(220, 80, 80), err);
        }
        ui.separator();

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for msg in &snap.completed {
                    render_message(ui, msg.role, &msg.text);
                }
                if let Turn::Streaming { buf } = &snap.turn {
                    render_message(ui, Role::Assistant, buf);
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            let resp = ui.add(
                TextEdit::multiline(&mut self.composer)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("Send a message…"),
            );
            let submit_with_enter = resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            if submit_with_enter {
                self.submit();
            }
            if ui.button("Send").clicked() {
                self.submit();
            }
        });
    }
}

fn render_message(ui: &mut egui::Ui, role: Role, text: &str) {
    let prefix = match role {
        Role::User => "you",
        Role::Assistant => "assistant",
    };
    ui.label(RichText::new(prefix).weak().small());
    ui.label(text);
    ui.add_space(8.0);
}

fn spawn_session_pump(
    mut transport: Transport,
    tx: watch::Sender<Arc<SessionSnapshot>>,
    mut intents: mpsc::UnboundedReceiver<SessionIntent>,
) {
    // Route the pump through chrome's `Spawner` rather than
    // `tokio::spawn` / `Handle::spawn`. The cdylib statically links
    // its own copy of tokio with separate statics, and calling tokio
    // runtime APIs here corrupts those (state-dependent UB — first
    // session works, second segfaults). The Spawner's `tokio::spawn`
    // call is compiled into chrome and runs against desktop's tokio
    // statics. See the `lutin-workflow-ui` crate doc.
    let spawner = transport.spawner.clone();
    spawner.spawn(Box::pin(async move {
        let mut state = SessionSnapshot::default();
        let counter = std::sync::atomic::AtomicU64::new(1);
        let send = transport.send.clone();

        // Subscribe so the engine starts streaming events to us.
        send_chat_request(&send, &counter, &ChatRequest::Subscribe);

        loop {
            let mut changed = false;
            tokio::select! {
                maybe_bytes = transport.recv.recv() => {
                    let Some(bytes) = maybe_bytes else { return };
                    let frame = match frame_decode(&bytes) {
                        Ok(f) => f,
                        Err(err) => {
                            tracing::warn!(?err, "malformed frame from session transport");
                            continue;
                        }
                    };
                    match frame {
                        Frame::Broadcast { body } => match chat_decode::<ChatEvent>(&body) {
                            Ok(ev) => changed = apply_chat_event(&mut state, ev),
                            Err(err) => {
                                tracing::warn!(?err, "malformed ChatEvent broadcast");
                            }
                        },
                        Frame::Payload { body, .. } => match chat_decode::<ChatResponse>(&body) {
                            Ok(resp) => changed = apply_chat_response(&mut state, resp),
                            Err(err) => {
                                tracing::warn!(?err, "malformed ChatResponse payload");
                            }
                        },
                        _ => {}
                    }
                }
                maybe_intent = intents.recv() => {
                    let Some(intent) = maybe_intent else { return };
                    match intent {
                        SessionIntent::Submit { text } => {
                            // Optimistic local append: user turn. Try to
                            // forward the request first so we can mark
                            // the session `Errored` instead of
                            // `Streaming` when the transport is gone —
                            // otherwise the UI would hang on a spinner
                            // that no event will ever clear.
                            state.completed.push(Message {
                                role: Role::User,
                                text: text.clone(),
                            });
                            let sent = try_send_chat_request(
                                &send,
                                &counter,
                                &ChatRequest::SendMessage { text },
                            );
                            state.turn = if sent {
                                Turn::Streaming { buf: String::new() }
                            } else {
                                Turn::Errored("transport closed".into())
                            };
                            changed = true;
                        }
                        SessionIntent::Cancel => {
                            send_chat_request(&send, &counter, &ChatRequest::Cancel);
                        }
                    }
                }
            }
            if changed && tx.send(Arc::new(state.clone())).is_err() {
                return; // UI dropped its receiver
            }
        }
    }));
}

/// Pure mutator over `&mut SessionSnapshot`. Returns true iff the
/// snapshot changed in a way the UI cares about.
fn apply_chat_event(state: &mut SessionSnapshot, ev: ChatEvent) -> bool {
    match ev {
        ChatEvent::Delta(text) => {
            // If we receive a Delta without a streaming turn (e.g. a
            // server-initiated turn or an out-of-band reply), promote
            // `Idle`/`Errored` to `Streaming` rather than dropping it.
            if !matches!(state.turn, Turn::Streaming { .. }) {
                state.turn = Turn::Streaming { buf: String::new() };
            }
            if let Turn::Streaming { buf } = &mut state.turn {
                buf.push_str(&text);
            }
            true
        }
        ChatEvent::Reasoning(_) => false,
        ChatEvent::ToolCallStarted { .. } | ChatEvent::ToolCallCompleted { .. } => false,
        ChatEvent::MessageFinished { reason, .. } => {
            // Move any streaming buffer into `completed`.
            if let Turn::Streaming { buf } = std::mem::replace(&mut state.turn, Turn::Idle) {
                if !buf.is_empty() {
                    state.completed.push(Message {
                        role: Role::Assistant,
                        text: buf,
                    });
                }
            }
            if let FinishReason::Failed(msg) = reason {
                state.turn = Turn::Errored(msg);
            }
            true
        }
        ChatEvent::StateChanged(s) => {
            state.persona = s.persona;
            true
        }
    }
}

fn apply_chat_response(state: &mut SessionSnapshot, resp: ChatResponse) -> bool {
    // `Subscribed`, `State`, and `StateUpdated` are three different
    // response shapes that all carry the current `SessionState`; we
    // funnel them through `set_persona` so the sync logic stays in one
    // place.
    fn set_persona(state: &mut SessionSnapshot, s: SessionState) -> bool {
        state.persona = s.persona;
        true
    }
    match resp {
        Ok(ChatOk::Subscribed { state: s, history }) => {
            // Seed scrollback from the persisted transcript. Subscribe
            // happens once per session-open, so this replaces (rather
            // than appends to) `completed` — late-joiners and
            // session-reopens both land on the same starting state.
            state.completed = history
                .into_iter()
                .map(|HistoricalMessage { role, text }| Message {
                    role: match role {
                        HistoricalRole::User => Role::User,
                        HistoricalRole::Assistant => Role::Assistant,
                    },
                    text,
                })
                .collect();
            set_persona(state, s)
        }
        Ok(ChatOk::State(s)) => set_persona(state, s),
        Ok(ChatOk::StateUpdated { state: s }) => set_persona(state, s),
        Ok(ChatOk::MessageQueued { .. }) | Ok(ChatOk::Cancelled) => false,
        Err(e) => {
            // Drop any in-flight buffer; the engine won't deliver
            // `MessageFinished` for a request it rejected.
            if let Turn::Streaming { buf } = std::mem::replace(&mut state.turn, Turn::Idle) {
                if !buf.is_empty() {
                    state.completed.push(Message {
                        role: Role::Assistant,
                        text: buf,
                    });
                }
            }
            state.turn = Turn::Errored(e.to_string());
            true
        }
    }
}

fn send_chat_request(
    send: &mpsc::UnboundedSender<Vec<u8>>,
    counter: &std::sync::atomic::AtomicU64,
    req: &ChatRequest,
) {
    let _ = try_send_chat_request(send, counter, req);
}

/// Like `send_chat_request` but returns `false` if the underlying
/// transport channel is closed (or the request couldn't be encoded).
/// Used by the `Submit` path so the pump can surface a transport-down
/// state to the UI instead of getting stuck on a streaming spinner.
fn try_send_chat_request(
    send: &mpsc::UnboundedSender<Vec<u8>>,
    counter: &std::sync::atomic::AtomicU64,
    req: &ChatRequest,
) -> bool {
    let body = match chat_encode(req) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(?err, "chat request encode failed");
            return false;
        }
    };
    let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bytes = match frame_encode(&Frame::Payload {
        request_id: id,
        body,
    }) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(?err, "frame encode failed");
            return false;
        }
    };
    if let Err(err) = send.send(bytes) {
        tracing::warn!(?err, "chat transport closed; request dropped");
        return false;
    }
    true
}

// ─── cdylib entry ────────────────────────────────────────────────────

/// Symbol name: `create_workflow`. Resolved by the desktop chrome via
/// `libloading::Library::get(b"create_workflow")`.
#[unsafe(no_mangle)]
pub extern "Rust" fn create_workflow() -> Box<dyn Workflow> {
    let probe = lutin_workflow_ui::typeid_probe();
    eprintln!("[chat-cdylib] typeid_probe = {probe:?}");
    Box::new(ChatWorkflow::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatError, FinishReason, SessionState, TurnId};

    #[test]
    fn delta_appends_to_streaming_buf() {
        let mut s = SessionSnapshot::default();
        assert!(apply_chat_event(&mut s, ChatEvent::Delta("hel".into())));
        assert!(apply_chat_event(&mut s, ChatEvent::Delta("lo".into())));
        match &s.turn {
            Turn::Streaming { buf } => assert_eq!(buf, "hello"),
            _ => panic!("expected streaming"),
        }
        assert!(s.completed.is_empty());
    }

    #[test]
    fn finish_moves_buf_to_completed() {
        let mut s = SessionSnapshot::default();
        apply_chat_event(&mut s, ChatEvent::Delta("done".into()));
        apply_chat_event(
            &mut s,
            ChatEvent::MessageFinished {
                turn_id: TurnId(1),
                reason: FinishReason::Completed,
            },
        );
        assert!(matches!(s.turn, Turn::Idle));
        assert_eq!(s.completed.len(), 1);
        assert_eq!(s.completed[0].text, "done");
    }

    #[test]
    fn finish_failed_sets_errored() {
        let mut s = SessionSnapshot::default();
        apply_chat_event(&mut s, ChatEvent::Delta("partial".into()));
        apply_chat_event(
            &mut s,
            ChatEvent::MessageFinished {
                turn_id: TurnId(1),
                reason: FinishReason::Failed("boom".into()),
            },
        );
        // Partial assistant text is preserved before the error banner.
        assert_eq!(s.completed.len(), 1);
        match &s.turn {
            Turn::Errored(msg) => assert_eq!(msg, "boom"),
            _ => panic!("expected errored"),
        }
    }

    #[test]
    fn state_changed_updates_persona() {
        let mut s = SessionSnapshot::default();
        let st = SessionState {
            persona: Some("alice".into()),
            model_override: None,
        };
        assert!(apply_chat_event(&mut s, ChatEvent::StateChanged(st)));
        assert_eq!(s.persona.as_deref(), Some("alice"));
    }

    #[test]
    fn subscribed_seeds_completed_from_history() {
        let mut s = SessionSnapshot::default();
        // Existing scrollback is replaced — Subscribe fires once per
        // session-open, so a late-joiner reconciles fully against the
        // engine's view rather than appending duplicates.
        s.completed.push(Message {
            role: Role::User,
            text: "stale".into(),
        });
        let resp: ChatResponse = Ok(ChatOk::Subscribed {
            state: SessionState {
                persona: Some("bob".into()),
                model_override: None,
            },
            history: vec![
                HistoricalMessage {
                    role: HistoricalRole::User,
                    text: "hi".into(),
                },
                HistoricalMessage {
                    role: HistoricalRole::Assistant,
                    text: "hello".into(),
                },
            ],
        });
        assert!(apply_chat_response(&mut s, resp));
        assert_eq!(s.completed.len(), 2);
        assert_eq!(s.completed[0].text, "hi");
        assert!(matches!(s.completed[0].role, Role::User));
        assert_eq!(s.completed[1].text, "hello");
        assert!(matches!(s.completed[1].role, Role::Assistant));
        assert_eq!(s.persona.as_deref(), Some("bob"));
    }

    #[test]
    fn err_response_recovers_streaming_buf() {
        let mut s = SessionSnapshot::default();
        apply_chat_event(&mut s, ChatEvent::Delta("hi".into()));
        let changed = apply_chat_response(&mut s, Err(ChatError::NoTurnInFlight));
        assert!(changed);
        assert_eq!(s.completed.len(), 1);
        assert!(matches!(s.turn, Turn::Errored(_)));
    }
}
