//! Control-panel server. WS endpoint, CP-orchestrated session list,
//! request dispatch, broadcast fan-out. Holds the control-panel
//! signing key.

pub mod defaults;
mod downloads;
mod registry;
pub mod session_index;
pub mod session_summary;
pub mod sessions;
mod settings_io;
pub mod transcribe;
pub mod transcription_streams;
pub mod tts;
pub mod tts_streams;
pub mod workflow_images;

use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, SigningKey, VerifyingKey, verify};
use lutin_control_protocol::{
    self as cp, ApiError, DisplayName, Event, MonoPcm16k, ProjectInfo, Request, Response,
    ResponseOk, SessionId, Slug, TranscriptionStreamId, TtsBackend, TtsLimit, TtsSpeed,
    TtsStreamId, WhisperConfig, WorkflowId,
};
use lutin_tts::TtsEvent;
use std::sync::Arc;
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use std::path::{Path, PathBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

const CHANNEL_BUF: usize = 64;

/// Per-`SpeakTts` byte cap. The model's context window silently
/// truncates oversize inputs; rejecting at the wire boundary turns
/// the failure into a hard error a workflow can catch instead of
/// half-spoken sentences. 4096 bytes ≈ ~1.5 KB English over the 2K
/// token Orpheus context with headroom to spare.
const MAX_TEXT_LEN: usize = 4096;

/// Server-side project record. CP owns the per-project signing key
/// in-memory; `start_session` / `open_session` mint tokens against it
/// without re-reading disk.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub info: ProjectInfo,
    pub signing: SigningKey,
}

/// Where to find per-project state. Lives in the supervisor task.
#[derive(Clone)]
pub struct SpawnConfig {
    /// Parent dir of all per-project trees.
    pub projects_root: PathBuf,
    /// Global `.lutin/` directory.
    pub global_config_dir: PathBuf,
}

enum Command {
    ListProjects {
        reply: oneshot::Sender<Response>,
    },
    CreateProject {
        slug: Slug,
        display_name: DisplayName,
        reply: oneshot::Sender<Response>,
    },
    DeleteProject {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
    ListWorkflows {
        reply: oneshot::Sender<Response>,
    },
    ListSessions {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
    StartSession {
        slug: Slug,
        workflow: WorkflowId,
        reply: oneshot::Sender<Response>,
    },
    StopSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    ResumeSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    DeleteSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    OpenSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    GetWorkflowBundle {
        id: WorkflowId,
        reply: oneshot::Sender<Response>,
    },
    ListProviders {
        reply: oneshot::Sender<Response>,
    },
    SetProviders {
        providers: Vec<lutin_control_protocol::ProviderConfig>,
        reply: oneshot::Sender<Response>,
    },
    GetWebSearch {
        reply: oneshot::Sender<Response>,
    },
    SetWebSearch {
        settings: lutin_control_protocol::WebSearchSettings,
        reply: oneshot::Sender<Response>,
    },
}

#[derive(Clone)]
pub struct AppState {
    pub issuer: VerifyingKey,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
    /// Streaming-transcription registry. Lives outside the supervisor
    /// task because chunk appends are independent of project state and
    /// shouldn't serialise behind every other CP command — and because
    /// `FinishTranscription` runs whisper for several seconds, which
    /// would block the whole supervisor if routed through it.
    transcription: transcription_streams::TranscriptionRegistry,
    /// Loaded TTS backends (one `TtsService` per model identity). Lazy
    /// — first `EnsureTtsBackend` triggers the download/load.
    tts_backends: tts::TtsBackends,
    /// Per-stream TTS sessions. Open count is process-wide; the
    /// service `Arc` inside each entry keeps a backend alive as long
    /// as any stream points at it.
    tts_streams: tts_streams::TtsStreamRegistry,
}

pub struct Supervisor {
    pub state: AppState,
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

impl Supervisor {
    pub fn spawn(signing: SigningKey, config: SpawnConfig) -> Self {
        let issuer = signing.verifying_key();
        let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_BUF);
        let (ev_tx, _) = broadcast::channel(CHANNEL_BUF);
        let (sd_tx, sd_rx) = oneshot::channel();
        // CP-global whisper context. Process-wide singleton so a
        // second connection (or a re-press during finish) reuses the
        // already-loaded model instead of rebuilding it.
        transcribe::install_log_callback();
        let transcriber = Arc::new(transcribe::WhisperTranscriber::new(
            config.global_config_dir.clone(),
        ));
        let transcription = transcription_streams::TranscriptionRegistry::new(transcriber);

        // Single sink for every loaded TTS backend; CP fans events
        // out onto the broadcast as they arrive. Unbounded because
        // the producers are real-time audio synthesisers — applying
        // back-pressure to them would stall the model thread; we'd
        // rather drop a slow consumer than the producer.
        let (tts_sink_tx, tts_sink_rx) = mpsc::unbounded_channel::<TtsEvent>();
        let tts_backends = tts::TtsBackends::new(config.global_config_dir.clone(), tts_sink_tx);
        let tts_streams = tts_streams::TtsStreamRegistry::new();
        tokio::spawn(tts_sink_pump(tts_sink_rx, ev_tx.clone()));

        let join = tokio::spawn(supervisor(cmd_rx, ev_tx.clone(), sd_rx, config));
        let state = AppState {
            issuer,
            commands: cmd_tx,
            events: ev_tx,
            transcription,
            tts_backends,
            tts_streams,
        };
        Self {
            state,
            join,
            shutdown: sd_tx,
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        if let Err(e) = self.join.await {
            warn!(error = %e, "supervisor task did not exit cleanly");
        }
    }
}

impl AppState {
    async fn dispatch(&self, req: Request) -> Response {
        // Handle transcription RPCs directly without going through
        // the supervisor — each connection has its own task, and
        // routing inference through a serialised command queue would
        // make a single PTT block every other CP request.
        match req {
            Request::OpenTranscription { config } => {
                return self.handle_open_transcription(config);
            }
            Request::TranscribeChunk { stream_id, samples } => {
                return self.handle_transcribe_chunk(stream_id, samples);
            }
            Request::FinishTranscription { stream_id } => {
                return self.handle_finish_transcription(stream_id).await;
            }
            Request::CancelTranscription { stream_id } => {
                self.transcription.cancel(stream_id);
                return Response::Ok(ResponseOk::TranscriptionCancelled);
            }
            Request::EnsureTtsBackend { backend } => {
                return self.handle_ensure_tts_backend(&backend).await;
            }
            Request::OpenTtsStream { backend } => {
                return self.handle_open_tts_stream(backend);
            }
            Request::SpeakTts {
                stream_id,
                text,
                speed,
            } => {
                return self.handle_speak_tts(stream_id, &text, speed);
            }
            Request::CancelTts { stream_id } => {
                return self.handle_cancel_tts(stream_id);
            }
            Request::CloseTtsStream { stream_id } => {
                return self.handle_close_tts_stream(stream_id);
            }
            _ => {}
        }
        let (reply, rx) = oneshot::channel();
        let cmd = match req {
            Request::ListProjects => Command::ListProjects { reply },
            Request::CreateProject { slug, display_name } => Command::CreateProject {
                slug,
                display_name,
                reply,
            },
            Request::DeleteProject { slug } => Command::DeleteProject { slug, reply },
            Request::ListWorkflows => Command::ListWorkflows { reply },
            Request::ListSessions { slug } => Command::ListSessions { slug, reply },
            Request::StartSession { slug, workflow } => Command::StartSession {
                slug,
                workflow,
                reply,
            },
            Request::StopSession { slug, session } => Command::StopSession {
                slug,
                session,
                reply,
            },
            Request::ResumeSession { slug, session } => Command::ResumeSession {
                slug,
                session,
                reply,
            },
            Request::DeleteSession { slug, session } => Command::DeleteSession {
                slug,
                session,
                reply,
            },
            Request::OpenSession { slug, session } => Command::OpenSession {
                slug,
                session,
                reply,
            },
            Request::GetWorkflowBundle { id } => Command::GetWorkflowBundle { id, reply },
            Request::ListProviders => Command::ListProviders { reply },
            Request::SetProviders { providers } => Command::SetProviders { providers, reply },
            Request::GetWebSearch => Command::GetWebSearch { reply },
            Request::SetWebSearch { settings } => Command::SetWebSearch { settings, reply },
            Request::OpenTranscription { .. }
            | Request::TranscribeChunk { .. }
            | Request::FinishTranscription { .. }
            | Request::CancelTranscription { .. }
            | Request::EnsureTtsBackend { .. }
            | Request::OpenTtsStream { .. }
            | Request::SpeakTts { .. }
            | Request::CancelTts { .. }
            | Request::CloseTtsStream { .. } => {
                unreachable!("transcription/tts requests are handled before this match");
            }
        };
        if self.commands.send(cmd).await.is_err() {
            return Response::Err(ApiError::Supervisor("supervisor stopped".into()));
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Response::Err(ApiError::Supervisor("supervisor dropped reply".into())),
        }
    }

    fn handle_open_transcription(&self, config: WhisperConfig) -> Response {
        let model = config.model;
        let stream_id = match self.transcription.open(config) {
            Ok(id) => id,
            Err(limit) => return Response::Err(ApiError::TranscriptionLimit(limit)),
        };
        // Kick off model warmup so by the time the user finishes
        // talking the context is loaded. Failures are non-fatal —
        // `FinishTranscription` re-runs `ensure_ctx` and surfaces any
        // real error there.
        let transcriber = self.transcription.transcriber().clone();
        tokio::spawn(async move {
            if let Err(e) = transcriber.warmup(model).await {
                warn!(error = %e, "whisper warmup after OpenTranscription failed");
            }
        });
        Response::Ok(ResponseOk::TranscriptionOpened { stream_id })
    }

    fn handle_transcribe_chunk(
        &self,
        stream_id: TranscriptionStreamId,
        samples: MonoPcm16k,
    ) -> Response {
        match self.transcription.append(stream_id, samples.as_slice()) {
            Ok(transcription_streams::AppendOutcome::Appended) => {
                Response::Ok(ResponseOk::ChunkAccepted)
            }
            Ok(transcription_streams::AppendOutcome::StreamNotFound) => {
                Response::Err(ApiError::TranscriptionStreamNotFound(stream_id))
            }
            Err(limit) => Response::Err(ApiError::TranscriptionLimit(limit)),
        }
    }

    async fn handle_finish_transcription(&self, stream_id: TranscriptionStreamId) -> Response {
        let Some(stream) = self.transcription.take(stream_id) else {
            return Response::Err(ApiError::TranscriptionStreamNotFound(stream_id));
        };
        match self
            .transcription
            .transcriber()
            .transcribe(stream.samples, &stream.config)
            .await
        {
            Ok(text) => Response::Ok(ResponseOk::Transcription { text }),
            Err(e) => {
                // Distinguish model-availability errors from inference
                // errors so the desktop can surface different messages
                // and decide whether retrying makes sense. The error
                // chain from `ensure_model` runs through
                // `download_streaming` (reqwest, fs, size mismatch);
                // anything below the inference call is a model issue.
                let chain = format!("{e:#}");
                let looks_like_model_issue = e.is::<reqwest::Error>()
                    || e.is::<std::io::Error>()
                    || chain.contains("download")
                    || chain.contains("size mismatch")
                    || chain.contains("load whisper model");
                if looks_like_model_issue {
                    Response::Err(ApiError::WhisperModelUnavailable(chain))
                } else {
                    Response::Err(ApiError::WhisperInference(chain))
                }
            }
        }
    }

    async fn handle_ensure_tts_backend(&self, backend: &TtsBackend) -> Response {
        match self.tts_backends.ensure(backend, &self.events).await {
            Ok(()) => Response::Ok(ResponseOk::TtsBackendReady),
            Err(e) => Response::Err(ApiError::TtsBackendUnavailable(format!("{e:#}"))),
        }
    }

    fn handle_open_tts_stream(&self, backend: TtsBackend) -> Response {
        let Some(service) = self.tts_backends.lookup(&backend) else {
            return Response::Err(ApiError::TtsBackendNotReady);
        };
        match self.tts_streams.open(backend, service) {
            Ok(stream_id) => Response::Ok(ResponseOk::TtsStreamOpened { stream_id }),
            Err(limit) => Response::Err(ApiError::TtsLimit(limit)),
        }
    }

    fn handle_speak_tts(&self, stream_id: TtsStreamId, text: &str, speed: TtsSpeed) -> Response {
        if text.len() > MAX_TEXT_LEN {
            return Response::Err(ApiError::TtsLimit(TtsLimit::TextTooLong {
                got: text.len(),
                max: MAX_TEXT_LEN,
            }));
        }
        let Some((service, backend)) = self.tts_streams.lookup(stream_id) else {
            return Response::Err(ApiError::TtsStreamNotFound(stream_id));
        };
        let TtsBackend::Orpheus { voice, .. } = backend;
        if !service.speak(tts::to_worker_id(stream_id), text, tts::voice_token(voice), speed.as_f32()) {
            return Response::Err(ApiError::TtsSynthesis(
                "tts service rejected speak request".into(),
            ));
        }
        Response::Ok(ResponseOk::TtsSpeechQueued)
    }

    fn handle_cancel_tts(&self, stream_id: TtsStreamId) -> Response {
        if let Some((service, _)) = self.tts_streams.lookup(stream_id) {
            service.cancel(tts::to_worker_id(stream_id));
        }
        Response::Ok(ResponseOk::TtsCancelled)
    }

    fn handle_close_tts_stream(&self, stream_id: TtsStreamId) -> Response {
        if let Some(stream) = self.tts_streams.take(stream_id) {
            stream.service.close(tts::to_worker_id(stream_id));
        }
        Response::Ok(ResponseOk::TtsStreamClosed)
    }
}

/// Drain the process-wide TTS sink and fan events out onto the
/// broadcast. Wire `TtsStreamId` is just the producer-side `StreamId`
/// narrowed to `u32` — slice 2 locked in the single id space, so the
/// cast back is loss-free.
async fn tts_sink_pump(
    mut rx: mpsc::UnboundedReceiver<TtsEvent>,
    events: broadcast::Sender<Event>,
) {
    while let Some(ev) = rx.recv().await {
        let frame = match ev {
            TtsEvent::Audio { stream_id, chunk } => match tts::from_worker_id(stream_id) {
                Some(wire) => Event::TtsAudio {
                    stream_id: wire,
                    chunk,
                },
                None => {
                    warn!(?stream_id, "tts audio for out-of-range worker id; dropping");
                    continue;
                }
            },
            TtsEvent::Finished { stream_id } => match tts::from_worker_id(stream_id) {
                Some(wire) => Event::TtsFinished { stream_id: wire },
                None => {
                    warn!(?stream_id, "tts finished for out-of-range worker id; dropping");
                    continue;
                }
            },
        };
        // Broadcast `send` only fails when there are zero receivers;
        // that's a transient state during startup / reconnects, not
        // an error. Drop on the floor — the next event lands once a
        // client subscribes.
        let _ = events.send(frame);
    }
}

async fn supervisor(
    mut rx: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    mut shutdown: oneshot::Receiver<()>,
    config: SpawnConfig,
) {
    let mut projects: Vec<ProjectRecord> = match registry::load(&config.projects_root) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to load project registry; starting empty");
            Vec::new()
        }
    };
    let mut session_registry: sessions::SessionRegistry = Vec::new();

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            cmd = rx.recv() => {
                match cmd {
                    Some(c) => handle_command(c, &mut projects, &mut session_registry, &config, &events).await,
                    None => break,
                }
            }
        }
    }

    sessions::stop_all(&mut session_registry).await;
}

async fn handle_command(
    cmd: Command,
    projects: &mut Vec<ProjectRecord>,
    session_registry: &mut sessions::SessionRegistry,
    config: &SpawnConfig,
    events: &broadcast::Sender<Event>,
) {
    match cmd {
        Command::ListProjects { reply } => {
            let infos: Vec<ProjectInfo> = projects.iter().map(|r| r.info.clone()).collect();
            let _ = reply.send(Response::Ok(ResponseOk::Projects(infos)));
        }
        Command::CreateProject {
            slug,
            display_name,
            reply,
        } => {
            if projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::AlreadyExists(slug)));
                return;
            }
            let signing = match init_project_storage(&config.projects_root, &slug) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Supervisor(format!(
                        "init project storage for {}: {e}",
                        slug.as_str()
                    ))));
                    return;
                }
            };
            let info = ProjectInfo { slug, display_name };
            projects.push(ProjectRecord {
                info: info.clone(),
                signing,
            });
            if let Err(e) = registry::save(&config.projects_root, projects) {
                warn!(error = %e, "failed to persist project registry after create");
            }
            let _ = reply.send(Response::Ok(ResponseOk::Created(info.clone())));
            let _ = events.send(Event::ProjectCreated(info));
        }
        Command::DeleteProject { slug, reply } => {
            let before = projects.len();
            projects.retain(|p| p.info.slug != slug);
            if projects.len() == before {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            sessions::stop_all_for_slug(session_registry, &slug).await;
            let lutin_dir = config.projects_root.join(slug.as_str()).join(".lutin");
            if let Err(e) = tokio::fs::remove_dir_all(&lutin_dir).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                warn!(
                    slug = %slug.as_str(),
                    path = %lutin_dir.display(),
                    error = %e,
                    "failed to wipe .lutin dir on delete; manual cleanup required"
                );
            }
            if let Err(e) = registry::save(&config.projects_root, projects) {
                warn!(error = %e, "failed to persist project registry after delete");
            }
            let _ = reply.send(Response::Ok(ResponseOk::Deleted));
            let _ = events.send(Event::ProjectDeleted { slug });
        }
        Command::ListWorkflows { reply } => {
            let workflows = sessions::list_workflows();
            let _ = reply.send(Response::Ok(ResponseOk::Workflows(workflows)));
        }
        Command::GetWorkflowBundle { id, reply } => {
            // Detach: `docker run cat` is a multi-MB read and we don't
            // want it serialising with other commands on the supervisor
            // task. The spawned task replies on `reply` directly.
            tokio::task::spawn_blocking(move || {
                let response = fetch_workflow_bundle(&id);
                let _ = reply.send(response);
            });
        }
        Command::ListSessions { slug, reply } => {
            if !projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            let infos = sessions::list_sessions(session_registry, &config.projects_root, &slug);
            let _ = reply.send(Response::Ok(ResponseOk::Sessions(infos)));
        }
        Command::StartSession {
            slug,
            workflow,
            reply,
        } => {
            let Some(record) = projects.iter().find(|p| p.info.slug == slug) else {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            };
            match sessions::start_session(
                session_registry,
                &slug,
                &workflow,
                &record.signing,
                &config.projects_root,
                &config.global_config_dir,
            )
            .await
            {
                Ok((running_session, endpoint)) => {
                    let info = running_session.info.clone();
                    let _ = events.send(Event::SessionStarted {
                        slug: slug.clone(),
                        info: info.clone(),
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionStarted {
                        info,
                        endpoint,
                    }));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::StopSession {
            slug,
            session,
            reply,
        } => {
            match sessions::stop_session(session_registry, &slug, &session).await {
                Ok(()) => {
                    let _ = events.send(Event::SessionEnded {
                        slug: slug.clone(),
                        session,
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionStopped));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::ResumeSession {
            slug,
            session,
            reply,
        } => {
            let Some(record) = projects.iter().find(|p| p.info.slug == slug) else {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            };
            match sessions::resume_session(
                session_registry,
                &slug,
                &session,
                &record.signing,
                &config.projects_root,
                &config.global_config_dir,
            )
            .await
            {
                Ok((running_session, endpoint)) => {
                    let info = running_session.info.clone();
                    let _ = events.send(Event::SessionStarted {
                        slug: slug.clone(),
                        info: info.clone(),
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionResumed {
                        info,
                        endpoint,
                    }));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::DeleteSession {
            slug,
            session,
            reply,
        } => {
            match sessions::delete_session(
                session_registry,
                &slug,
                &session,
                &config.projects_root,
            )
            .await
            {
                Ok(()) => {
                    let _ = events.send(Event::SessionEnded {
                        slug: slug.clone(),
                        session,
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionDeleted));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::OpenSession {
            slug,
            session,
            reply,
        } => {
            let Some(record) = projects.iter().find(|p| p.info.slug == slug) else {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            };
            match sessions::open_session(session_registry, &slug, &session, &record.signing) {
                Ok(endpoint) => {
                    let _ = reply.send(Response::Ok(ResponseOk::SessionOpened(endpoint)));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::ListProviders { reply } => {
            match settings_io::read_providers(&config.global_config_dir) {
                Ok(providers) => {
                    let _ = reply.send(Response::Ok(ResponseOk::Providers(providers)));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Settings(e)));
                }
            }
        }
        Command::SetProviders { providers, reply } => {
            match settings_io::write_providers(&config.global_config_dir, &providers) {
                Ok(()) => {
                    let _ = reply.send(Response::Ok(ResponseOk::ProvidersSaved));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Settings(e)));
                }
            }
        }
        Command::GetWebSearch { reply } => {
            match settings_io::read_web_search(&config.global_config_dir) {
                Ok(settings) => {
                    let _ = reply.send(Response::Ok(ResponseOk::WebSearch(settings)));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Settings(e)));
                }
            }
        }
        Command::SetWebSearch { settings, reply } => {
            match settings_io::write_web_search(&config.global_config_dir, &settings) {
                Ok(()) => {
                    let _ = reply.send(Response::Ok(ResponseOk::WebSearchSaved));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Settings(e)));
                }
            }
        }
    }
}

/// Find the image for a workflow id and ship its plugin-bundle tarball
/// back as a `WorkflowBundle` response. Runs on a blocking thread;
/// ships the tarball verbatim — desktop unpacks.
fn fetch_workflow_bundle(id: &WorkflowId) -> Response {
    let images = workflow_images::list_installed();
    let Some(inst) = images.iter().find(|i| i.id == id.as_str()) else {
        return Response::Err(ApiError::WorkflowNotFound(id.clone()));
    };
    match workflow_images::read_bundle_bytes(&inst.image) {
        Ok((digest, bytes)) => Response::Ok(ResponseOk::WorkflowBundle {
            id: id.clone(),
            digest,
            bytes,
        }),
        Err(e) => Response::Err(ApiError::Supervisor(format!(
            "read bundle for {}: {e}",
            id.as_str()
        ))),
    }
}

/// Eagerly create the per-project on-disk layout and mint the project's
/// signing keypair if absent. Returns the loaded key so CP can hold it
/// in memory rather than re-reading disk on every session op.
fn init_project_storage(projects_root: &Path, slug: &Slug) -> std::io::Result<SigningKey> {
    let lutin_dir = projects_root.join(slug.as_str()).join(".lutin");
    std::fs::create_dir_all(&lutin_dir)?;
    let keypair_path = lutin_dir.join("keypair");
    lutin_keypair::load_or_create_keypair(&keypair_path).map_err(std::io::Error::other)
}

pub async fn run(listener: TcpListener, state: AppState) -> anyhow::Result<()> {
    loop {
        let (sock, peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(sock, state).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

async fn serve_conn(sock: TcpStream, state: AppState) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(sock).await?;
    let (mut tx, mut rx) = ws.split();

    let Some(msg) = rx.next().await else {
        return Ok(());
    };
    let bytes = match msg? {
        Message::Binary(b) => b,
        _ => anyhow::bail!("expected binary hello"),
    };
    let frame = decode(&bytes)?;
    let Frame::Hello {
        protocol_version,
        token,
    } = frame
    else {
        anyhow::bail!("expected Hello");
    };
    if protocol_version != PROTOCOL_VERSION {
        let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
            reason: format!(
                "protocol version mismatch: server={PROTOCOL_VERSION} client={protocol_version}"
            ),
        }))?;
        tx.send(Message::Binary(nack.into())).await?;
        return Ok(());
    }
    match verify(&token, &state.issuer) {
        Ok(claims) if matches!(claims.scope, Scope::ControlPanel) => {}
        Ok(_) => {
            let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
                reason: "scope must be ControlPanel".into(),
            }))?;
            tx.send(Message::Binary(nack.into())).await?;
            return Ok(());
        }
        Err(e) => {
            let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
                reason: format!("auth: {e}"),
            }))?;
            tx.send(Message::Binary(nack.into())).await?;
            return Ok(());
        }
    }
    let ack = encode(&Frame::HelloAck(HandshakeResult::Accepted))?;
    tx.send(Message::Binary(ack.into())).await?;

    let mut events = state.events.subscribe();

    loop {
        tokio::select! {
            biased;

            ev = events.recv() => match ev {
                Ok(e) => {
                    let body = cp::encode(&e)?;
                    let frame = encode(&Frame::Broadcast { body })?;
                    if tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(n, "client lagged events; dropping connection to force resync");
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },

            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let bytes = match msg? {
                    Message::Binary(b) => b,
                    Message::Close(_) => break,
                    Message::Ping(p) => {
                        tx.send(Message::Pong(p)).await?;
                        continue;
                    }
                    _ => continue,
                };
                let frame = decode(&bytes)?;
                match frame {
                    Frame::Payload { request_id, body } => {
                        let req = cp::decode::<Request>(&body)?;
                        let resp = state.dispatch(req).await;
                        let body = cp::encode(&resp)?;
                        let out = encode(&Frame::Payload { request_id, body })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Ping { nonce } => {
                        let out = encode(&Frame::Pong { nonce })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Close { .. } => break,
                    frame => {
                        warn!(?frame, "unexpected frame from client");
                    }
                }
            }
        }
    }
    Ok(())
}
