//! Backend-agnostic worker pool. Stream-scoped: each request carries a
//! `StreamId` (opaque `u64`); the sender thread emits `TtsEvent`s to
//! the configured `sink` so audio for one stream stays ordered, and
//! `Finished` fires only when that stream's queue drains.
//!
//! Mirrors the legacy engine's pool architecture, with two changes:
//! (1) routing is by stream id rather than `chat_id`+`connection_id`,
//! and (2) output is a generic `mpsc::UnboundedSender<TtsEvent>`
//! instead of an engine-specific server handle. CP owns the receiver
//! and translates events onto the wire.

use std::collections::HashMap;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};

use tokio::sync::{mpsc, watch};

use crate::backend::TtsBackendFactory;
use crate::TtsError;

/// Default worker count. Two workers cover the common case (one
/// running, one warm) without doubling GPU memory beyond what a 3B
/// Orpheus model needs.
pub const DEFAULT_WORKER_COUNT: usize = 2;

/// Opaque, caller-allocated stream identifier. The crate never mints
/// these — CP allocates and maps to its protocol id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub u64);

/// Output event. PCM is 24 kHz mono i16 LE bytes (the contract Orpheus
/// + SNAC produce; future backends should match or document a
/// different sample rate at their boundary).
#[derive(Debug, Clone)]
pub enum TtsEvent {
    Audio { stream_id: StreamId, chunk: Vec<u8> },
    /// Sent when the stream's outstanding sentence queue drains. Not
    /// the same as "stream closed" — a later `speak` on the same
    /// `StreamId` simply opens a new run.
    Finished { stream_id: StreamId },
}

struct Request {
    stream_id: StreamId,
    text: String,
    voice: String,
    speed: f32,
    cancel_rx: watch::Receiver<bool>,
}

struct Job {
    text: String,
    voice: String,
    speed: f32,
    cancel_rx: watch::Receiver<bool>,
    audio_tx: std_mpsc::SyncSender<Vec<u8>>,
}

struct SentenceSlot {
    stream_id: StreamId,
    audio_rx: std_mpsc::Receiver<Vec<u8>>,
}

/// Handle to a backend-specific TTS pool. One service per loaded
/// backend; CP keeps a registry keyed on `TtsBackend`.
pub struct TtsService {
    request_tx: mpsc::UnboundedSender<Request>,
    cancel_tokens: Mutex<HashMap<StreamId, watch::Sender<bool>>>,
}

impl TtsService {
    /// Spawn the worker pool. Must be called from a blocking-tolerant
    /// context: the factory moves to a background thread and runs
    /// model init there.
    pub fn new(
        factory: Box<dyn TtsBackendFactory>,
        sink: mpsc::UnboundedSender<TtsEvent>,
        worker_count: usize,
    ) -> Result<Self, TtsError> {
        let (request_tx, request_rx) = mpsc::unbounded_channel();

        std::thread::Builder::new()
            .name("tts-pool".into())
            .spawn(move || {
                run_pool(factory, request_rx, sink, worker_count);
            })
            .map_err(TtsError::Io)?;

        Ok(Self {
            request_tx,
            cancel_tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Queue a sentence for synthesis. Non-blocking. `speed` is
    /// clamped to 0.5..=2.0; markdown is stripped from `text` before
    /// the model sees it.
    pub fn speak(&self, stream_id: StreamId, text: String, voice: String, speed: f32) {
        let speed = speed.clamp(0.5, 2.0);
        let clean_text = clean_for_speech(&text);
        if clean_text.is_empty() {
            return;
        }

        let cancel_rx = {
            let mut tokens = self.cancel_tokens.lock().expect("cancel tokens poisoned");
            let tx = tokens
                .entry(stream_id)
                .or_insert_with(|| watch::channel(false).0);
            // A stream that was previously cancelled gets a fresh
            // token so a follow-up `speak` isn't dead on arrival.
            if *tx.borrow() {
                let (new_tx, rx) = watch::channel(false);
                *tx = new_tx;
                rx
            } else {
                tx.subscribe()
            }
        };

        let _ = self.request_tx.send(Request {
            stream_id,
            text: clean_text,
            voice,
            speed,
            cancel_rx,
        });
    }

    /// Cancel pending + in-progress synthesis for a stream. The watch
    /// flag is set; workers exit early; the submitter drops queued
    /// requests for this stream. Idempotent — cancelling an unknown
    /// stream is a no-op.
    pub fn cancel(&self, stream_id: StreamId) {
        let tokens = self.cancel_tokens.lock().expect("cancel tokens poisoned");
        if let Some(tx) = tokens.get(&stream_id) {
            let _ = tx.send(true);
        }
    }

    /// Drop all state for a stream. Cancels first so anything in
    /// flight tears down, then removes the cancel token.
    pub fn close(&self, stream_id: StreamId) {
        let mut tokens = self.cancel_tokens.lock().expect("cancel tokens poisoned");
        if let Some(tx) = tokens.remove(&stream_id) {
            let _ = tx.send(true);
        }
    }
}

fn run_pool(
    factory: Box<dyn TtsBackendFactory>,
    mut request_rx: mpsc::UnboundedReceiver<Request>,
    sink: mpsc::UnboundedSender<TtsEvent>,
    worker_count: usize,
) {
    let (job_tx, job_rx) = std_mpsc::sync_channel::<Job>(worker_count.max(1) * 4);
    let job_rx = Arc::new(Mutex::new(job_rx));

    let (slot_tx, slot_rx) = std_mpsc::channel::<SentenceSlot>();

    std::thread::scope(|s| {
        // ----- Workers -----
        // A failed worker init must not crash the pool: a degraded
        // pool (fewer workers, even zero) beats panicking the whole
        // service and silently dropping every future request.
        for i in 0..worker_count {
            let job_rx = job_rx.clone();
            let mut worker = match factory.create_worker(i) {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!(worker = i, error = %e, "TTS worker failed to initialize");
                    continue;
                }
            };

            s.spawn(move || loop {
                let job = match job_rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => break,
                };
                if *job.cancel_rx.borrow() {
                    continue;
                }
                if let Err(e) = worker.generate(
                    &job.text,
                    &job.voice,
                    job.speed,
                    &job.cancel_rx,
                    &job.audio_tx,
                ) {
                    tracing::error!(worker = i, error = %e, "TTS worker error");
                }
            });
        }

        // ----- Sender (ordered delivery per stream) -----
        let sink_ref = sink.clone();
        s.spawn(move || sender_loop(slot_rx, sink_ref));

        // ----- Submitter (this thread) -----
        while let Some(req) = request_rx.blocking_recv() {
            if *req.cancel_rx.borrow() {
                continue;
            }

            let (audio_tx, audio_rx) = std_mpsc::sync_channel(16);

            let _ = slot_tx.send(SentenceSlot {
                stream_id: req.stream_id,
                audio_rx,
            });

            let _ = job_tx.send(Job {
                text: req.text,
                voice: req.voice,
                speed: req.speed,
                cancel_rx: req.cancel_rx,
                audio_tx,
            });
        }

        drop(job_tx);
        drop(slot_tx);
    });
}

/// Read sentence slots in order and forward audio to the sink.
///
/// `Finished` is suppressed between consecutive sentences for the same
/// stream (to avoid an audible gap on the client) and only fires when
/// no follow-up sentence is immediately queued.
fn sender_loop(
    slot_rx: std_mpsc::Receiver<SentenceSlot>,
    sink: mpsc::UnboundedSender<TtsEvent>,
) {
    let Ok(mut slot) = slot_rx.recv() else {
        return;
    };

    loop {
        let stream_id = slot.stream_id;

        while let Ok(chunk) = slot.audio_rx.recv() {
            if sink
                .send(TtsEvent::Audio { stream_id, chunk })
                .is_err()
            {
                return;
            }
        }

        match slot_rx.try_recv() {
            Ok(next) if next.stream_id == stream_id => {
                slot = next;
            }
            Ok(next) => {
                let _ = sink.send(TtsEvent::Finished { stream_id });
                slot = next;
            }
            Err(_) => {
                let _ = sink.send(TtsEvent::Finished { stream_id });
                match slot_rx.recv() {
                    Ok(next) => slot = next,
                    Err(_) => break,
                }
            }
        }
    }
}

/// Strip markdown so the model doesn't pronounce formatting
/// characters literally ("asterisk asterisk bold"). Verbatim from the
/// legacy engine.
pub(crate) fn clean_for_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match c {
            '[' => {
                let mut link_text = String::new();
                let mut found_close = false;
                for (_, ch) in chars.by_ref() {
                    if ch == ']' {
                        found_close = true;
                        break;
                    }
                    link_text.push(ch);
                }
                if found_close {
                    if chars.peek().map(|(_, ch)| *ch) == Some('(') {
                        chars.next();
                        for (_, ch) in chars.by_ref() {
                            if ch == ')' {
                                break;
                            }
                        }
                    }
                    out.push_str(&link_text);
                } else {
                    out.push('[');
                    out.push_str(&link_text);
                }
            }
            '*' | '_' => {
                while chars.peek().map(|(_, ch)| *ch) == Some(c) {
                    chars.next();
                }
            }
            '#' if is_line_start(text, i) => {
                while chars.peek().map(|(_, ch)| *ch) == Some('#') {
                    chars.next();
                }
                if chars.peek().map(|(_, ch)| *ch) == Some(' ') {
                    chars.next();
                }
            }
            '`' => {
                while chars.peek().map(|(_, ch)| *ch) == Some('`') {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }

    let mut result = String::with_capacity(out.len());
    let mut prev_space = false;
    for c in out.chars() {
        if c == ' ' {
            if !prev_space {
                result.push(' ');
            }
            prev_space = true;
        } else {
            prev_space = false;
            result.push(c);
        }
    }

    result.trim().to_string()
}

fn is_line_start(text: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    text.as_bytes().get(i - 1) == Some(&b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_markdown() {
        assert_eq!(clean_for_speech("**bold** and *italic*"), "bold and italic");
        assert_eq!(clean_for_speech("# Heading"), "Heading");
        assert_eq!(clean_for_speech("see [here](https://x)"), "see here");
        assert_eq!(clean_for_speech("`code` runs"), "code runs");
    }
}
