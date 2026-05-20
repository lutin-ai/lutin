//! Per-stream state for `OpenTranscription` / `TranscribeChunk` /
//! `FinishTranscription`. Splits by backend:
//!
//! - Whisper streams buffer raw PCM in i16 and run one-shot decode at
//!   `Finish` time. `append` is a vector extend under the mutex.
//! - Parakeet streams run [`lutin_stt::SttStream`] on a dedicated
//!   blocking thread: `append` sends each chunk into an mpsc, the
//!   thread pushes through `ParakeetUnified::transcribe_chunk`, and
//!   text deltas are broadcast as `Event::TranscriptionPartial`. The
//!   thread exits on `Finish` (drains right context via `flush`) or
//!   on `Cancel` (channel closes, no reply expected).
//!
//! `Vec<Stream>` rather than `HashMap<StreamId, Stream>` per the
//! "prefer Vec" guideline — at most a handful of streams are live at
//! once, and `swap_remove` keeps `take()` O(1) regardless. `next_id`
//! is monotonic per CP boot.

use std::sync::Arc;
use std::sync::Mutex;

use lutin_control_protocol::{
    Event, ParakeetConfig, SttConfig, TranscriptionLimit, TranscriptionStreamId, WhisperConfig,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::warn;

use crate::transcribe::{SttFailure, SttManager};

/// Per-connection cap on simultaneously-open streams. Process-wide
/// enforcement (the registry is shared across connections) — a more
/// accurate per-connection bound would track owners explicitly, but
/// this is enough to stop a runaway client from exhausting CP memory.
pub const MAX_OPEN_STREAMS: usize = 32;

/// Per-chunk sample cap. 16 kHz × 10 s = 160_000 samples; the desktop
/// emits chunks at the cpal callback period (typically <100 ms), so
/// hitting this means a buggy or hostile client. Returning the
/// configured max in the error lets the desktop surface a useful
/// message instead of a generic failure.
pub const MAX_CHUNK_SAMPLES: usize = 160_000;

enum Stream {
    Buffered(BufferedStream),
    Streaming(StreamingStream),
}

impl Stream {
    fn id(&self) -> TranscriptionStreamId {
        match self {
            Stream::Buffered(s) => s.id,
            Stream::Streaming(s) => s.id,
        }
    }
}

struct BufferedStream {
    id: TranscriptionStreamId,
    config: WhisperConfig,
    samples: Vec<i16>,
}

struct StreamingStream {
    id: TranscriptionStreamId,
    cmd_tx: mpsc::UnboundedSender<StreamingCmd>,
}

enum StreamingCmd {
    Chunk(Vec<i16>),
    Finish(oneshot::Sender<Result<String, SttFailure>>),
}

struct Inner {
    next_id: u32,
    streams: Vec<Stream>,
}

#[derive(Clone)]
pub struct TranscriptionRegistry {
    inner: Arc<Mutex<Inner>>,
    manager: Arc<SttManager>,
    events: broadcast::Sender<Event>,
}

impl TranscriptionRegistry {
    pub fn new(manager: Arc<SttManager>, events: broadcast::Sender<Event>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                next_id: 1,
                streams: Vec::new(),
            })),
            manager,
            events,
        }
    }

    pub fn manager(&self) -> &Arc<SttManager> {
        &self.manager
    }

    /// Allocate a stream id and (for parakeet) spawn the per-stream
    /// worker. Fails with `TooManyStreams` if the process-wide open
    /// count would exceed `MAX_OPEN_STREAMS`. Parakeet's model load
    /// runs inside the worker task — `open` itself doesn't block on
    /// it, so the desktop can start sending chunks immediately and
    /// they queue in the mpsc until the load resolves.
    pub fn open(&self, config: SttConfig) -> Result<TranscriptionStreamId, TranscriptionLimit> {
        let id = {
            let mut inner = self.inner.lock().expect("transcription registry poisoned");
            if inner.streams.len() >= MAX_OPEN_STREAMS {
                return Err(TranscriptionLimit::TooManyStreams {
                    max: MAX_OPEN_STREAMS,
                });
            }
            let id = TranscriptionStreamId(inner.next_id);
            // `wrapping_add` keeps the counter monotonic past u32::MAX
            // (practically unreachable: would need 4B PTT presses
            // without a CP restart). The `max(1)` keeps id 0 available
            // as a future "unset" sentinel if needed.
            inner.next_id = inner.next_id.wrapping_add(1).max(1);
            match config {
                SttConfig::Whisper(cfg) => {
                    inner.streams.push(Stream::Buffered(BufferedStream {
                        id,
                        config: cfg,
                        samples: Vec::new(),
                    }));
                }
                SttConfig::Parakeet(cfg) => {
                    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                    inner
                        .streams
                        .push(Stream::Streaming(StreamingStream { id, cmd_tx }));
                    spawn_parakeet_worker(
                        id,
                        cfg,
                        cmd_rx,
                        self.manager.clone(),
                        self.events.clone(),
                    );
                }
            }
            id
        };
        Ok(id)
    }

    /// Append i16 samples. For whisper, extends the in-memory buffer.
    /// For parakeet, forwards the chunk to the worker task — the
    /// worker decodes asynchronously and broadcasts any text delta.
    pub fn append(
        &self,
        id: TranscriptionStreamId,
        samples: &[i16],
    ) -> Result<AppendOutcome, TranscriptionLimit> {
        if samples.len() > MAX_CHUNK_SAMPLES {
            return Err(TranscriptionLimit::ChunkTooLarge {
                got: samples.len(),
                max: MAX_CHUNK_SAMPLES,
            });
        }
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        let Some(stream) = inner.streams.iter_mut().find(|s| s.id() == id) else {
            return Ok(AppendOutcome::StreamNotFound);
        };
        match stream {
            Stream::Buffered(b) => b.samples.extend_from_slice(samples),
            Stream::Streaming(s) => {
                // `send` only fails when the worker has exited (load
                // error or panic). Treat as a silent drop — the next
                // `Finish` will surface the real error via the
                // oneshot channel (or `StreamNotFound` if cancelled).
                let _ = s.cmd_tx.send(StreamingCmd::Chunk(samples.to_vec()));
            }
        }
        Ok(AppendOutcome::Appended)
    }

    /// Remove the stream and start its finish path. Returns the
    /// outcome so the caller can drive whichever decode shape the
    /// backend uses without the registry needing async access.
    pub fn take(&self, id: TranscriptionStreamId) -> FinishOutcome {
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        let Some(pos) = inner.streams.iter().position(|s| s.id() == id) else {
            return FinishOutcome::NotFound;
        };
        match inner.streams.swap_remove(pos) {
            Stream::Buffered(b) => FinishOutcome::Whisper {
                samples: b.samples,
                config: b.config,
            },
            Stream::Streaming(s) => {
                let (reply_tx, reply_rx) = oneshot::channel();
                // If the worker already exited (load failure, panic)
                // the send fails; the caller's `await reply_rx` will
                // resolve with `Err(RecvError)` which we map to a
                // generic inference error.
                if s.cmd_tx.send(StreamingCmd::Finish(reply_tx)).is_err() {
                    warn!(?id, "parakeet worker already gone at finish");
                }
                FinishOutcome::Parakeet { reply: reply_rx }
            }
        }
    }

    /// Idempotent cancel. For parakeet, drops the command sender —
    /// the worker observes channel close and exits without emitting
    /// a final partial.
    pub fn cancel(&self, id: TranscriptionStreamId) -> bool {
        let mut inner = self.inner.lock().expect("transcription registry poisoned");
        let Some(pos) = inner.streams.iter().position(|s| s.id() == id) else {
            return false;
        };
        inner.streams.swap_remove(pos);
        true
    }
}

/// Spawn the per-stream parakeet worker. Two-stage: an async task
/// awaits the model load (which itself does `spawn_blocking` for the
/// ORT session create), then the loaded stream + receiver hand off
/// to a blocking thread that drives the encoder/decoder in a tight
/// loop. Chunks that arrive during model load queue in the mpsc.
fn spawn_parakeet_worker(
    stream_id: TranscriptionStreamId,
    cfg: ParakeetConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<StreamingCmd>,
    manager: Arc<SttManager>,
    events: broadcast::Sender<Event>,
) {
    tokio::spawn(async move {
        let stt = match manager.open_parakeet_stream(&cfg).await {
            Ok(s) => s,
            Err(e) => {
                warn!(?stream_id, error = ?e, "parakeet stream load failed");
                // Drain pending chunks so the sender doesn't see a
                // closed channel; reply with the load error on
                // whichever `Finish` shows up.
                while let Some(cmd) = cmd_rx.recv().await {
                    if let StreamingCmd::Finish(reply) = cmd {
                        let _ = reply.send(Err(load_failure_clone(&e)));
                        break;
                    }
                }
                return;
            }
        };
        let blocking = tokio::task::spawn_blocking(move || {
            run_parakeet_stream_loop(stream_id, stt, cmd_rx, events);
        });
        if let Err(e) = blocking.await {
            warn!(?stream_id, error = %e, "parakeet stream worker panicked");
        }
    });
}

fn run_parakeet_stream_loop(
    stream_id: TranscriptionStreamId,
    mut stt: Box<dyn lutin_stt::SttStream>,
    mut cmd_rx: mpsc::UnboundedReceiver<StreamingCmd>,
    events: broadcast::Sender<Event>,
) {
    while let Some(cmd) = cmd_rx.blocking_recv() {
        match cmd {
            StreamingCmd::Chunk(pcm) => match stt.push(pcm) {
                Ok(delta) if !delta.is_empty() => {
                    // Broadcast `send` only fails when there are no
                    // receivers — a transient state during startup or
                    // a client reconnect. Drop on the floor; the
                    // final `Transcription` reply still carries the
                    // full text.
                    let _ = events.send(Event::TranscriptionPartial {
                        stream_id,
                        text_delta: delta,
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(?stream_id, error = ?e, "parakeet chunk decode failed");
                    // Keep looping — subsequent chunks may still
                    // decode; on Finish we'll surface the next error.
                }
            },
            StreamingCmd::Finish(reply) => {
                let final_text = stt
                    .finish()
                    .map_err(|e| SttFailure::Inference(anyhow::anyhow!(e)));
                let _ = reply.send(final_text);
                return;
            }
        }
    }
}

/// `SttFailure` doesn't implement `Clone` (wraps `anyhow::Error`).
/// When the load fails and multiple `Finish` requests could in
/// principle arrive, we synthesise a fresh error from the original's
/// `Display`. In practice only one Finish ever arrives per stream so
/// the loop above breaks after the first.
fn load_failure_clone(original: &SttFailure) -> SttFailure {
    match original {
        SttFailure::ModelUnavailable(e) => {
            SttFailure::ModelUnavailable(anyhow::anyhow!("{e:#}"))
        }
        SttFailure::Inference(e) => SttFailure::Inference(anyhow::anyhow!("{e:#}")),
    }
}

/// What `take` returns. Whisper hands back the raw buffer for one-
/// shot decode at the dispatch layer; parakeet hands back a oneshot
/// receiver fed by the per-stream worker thread.
pub enum FinishOutcome {
    Whisper {
        samples: Vec<i16>,
        config: WhisperConfig,
    },
    Parakeet {
        reply: oneshot::Receiver<Result<String, SttFailure>>,
    },
    NotFound,
}

/// Result of `append` apart from the `ChunkTooLarge` failure mode.
/// Splitting it keeps the principled-errors split: `TranscriptionLimit`
/// is the wire boundary failure (typed enum); "stream not found" is
/// a normal control-flow signal (caller turns it into the matching
/// wire `ApiError::TranscriptionStreamNotFound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    StreamNotFound,
}
