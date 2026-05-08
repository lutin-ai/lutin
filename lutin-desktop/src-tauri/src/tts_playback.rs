//! TTS audio playback.
//!
//! CP synthesises 24 kHz mono i16 PCM and broadcasts it as
//! `Event::TtsAudio` chunks. This module owns the cpal output device,
//! decodes/resamples each chunk on the way in, and feeds the active
//! session's queue into the output callback. The audio bytes never
//! cross the Tauri JS boundary — playback stays Rust-side, so the
//! `Vec<u8>`-as-JSON-number-array IPC tax doesn't apply.
//!
//! Streams are bound to a session at `register()` time. Only streams
//! whose session matches the chrome's currently-active session are
//! drained by the output callback; queues for non-active streams are
//! held untouched. When the active session changes, queues belonging
//! to the previously-active session are dropped (held audio after a
//! context switch is worse than losing it) and any audio chunks that
//! arrive afterwards for those streams are also dropped on enqueue
//! with a single rate-limited warn.
//!
//! cpal's `Stream` is `!Send`, so the output stream lives on a
//! dedicated control thread (same pattern as `audio.rs`). The shared
//! state (`PlaybackState`) is reachable from the callback and from
//! the chrome's command handlers via an `Arc<Mutex<…>>`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use lutin_control_protocol::{SessionId, TTS_AUDIO_SAMPLE_RATE_HZ, TtsStreamId};
use tracing::{debug, error, info, warn};

/// Public handle held by `AppState`. Exposes the small command surface
/// the dispatch layer needs; the cpal output stream and its callback
/// live behind the shared state.
pub struct TtsPlayback {
    state: Arc<Mutex<PlaybackState>>,
    /// Output device sample rate; chunks are resampled to this on
    /// enqueue so the callback only needs to mix and copy. Atomic so
    /// `set_device` can publish the rebuilt stream's rate without
    /// reacquiring the playback-state mutex.
    device_rate: Arc<AtomicU32>,
    cmd_tx: mpsc::Sender<Cmd>,
}

enum Cmd {
    /// Rebuild on the named output device (or host default when
    /// `None`). The control thread drops the prior stream first;
    /// rebuild failures leave the thread without a stream and audio
    /// chunks become a no-op until the next `SetDevice` arrives.
    SetDevice(Option<String>),
}

struct PlaybackState {
    /// Per-stream queues. A `Vec` (linear scan) rather than a
    /// `HashMap`: the registry is capped at 32 streams CP-side and
    /// the output callback iterates them all every frame anyway, so
    /// hashing is pure overhead.
    streams: Vec<(TtsStreamId, StreamSlot)>,
    /// Currently-active session per `set_active_session`. `None` while
    /// no plugin iframe is mounted.
    active_session: Option<SessionId>,
    /// Throttle for the "dropped audio for non-active session" log.
    /// `None` until the first drop; never reset (the log line is
    /// debug-level, so a one-line-per-second cadence is plenty even
    /// across long inactive sessions).
    last_drop_warn: Option<Instant>,
}

struct StreamSlot {
    session: SessionId,
    /// Resampled samples at device rate, mono. The cpal callback pops
    /// from the front; chunks land at the back.
    queue: VecDeque<f32>,
    /// Linear-resampler carry-over. `pos` is the fractional source
    /// index where the next output sample starts — values < 1.0 hop
    /// across the chunk boundary using `last_sample` for the s0 of
    /// the first interpolation. Without this state, resampling each
    /// chunk independently would alias at chunk seams.
    resample_pos: f64,
    last_sample: f32,
    /// Playback speed multiplier; folded into the resample ratio so
    /// the existing 24 kHz → device-rate stage handles speed for
    /// free. Pitch shifts with speed (no time-stretch).
    speed: f32,
}

impl StreamSlot {
    fn new(session: SessionId) -> Self {
        Self {
            session,
            queue: VecDeque::new(),
            resample_pos: 0.0,
            last_sample: 0.0,
            speed: 1.0,
        }
    }

    fn reset(&mut self) {
        self.queue.clear();
        self.resample_pos = 0.0;
        self.last_sample = 0.0;
    }
}

impl TtsPlayback {
    /// Build the cpal output stream on a control thread and start it.
    /// Returns `Err` on no-device / unsupported-format setups; chrome
    /// logs and treats TTS as unavailable in that case.
    pub fn new(initial_device: Option<String>) -> Result<Self, PlaybackError> {
        let state = Arc::new(Mutex::new(PlaybackState {
            streams: Vec::new(),
            active_session: None,
            last_drop_warn: None,
        }));
        let device_rate = Arc::new(AtomicU32::new(0));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, PlaybackError>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let cb_state = state.clone();
        let rate_pub = device_rate.clone();
        std::thread::Builder::new()
            .name("tts-playback-ctrl".into())
            .spawn(move || {
                run_stream_thread(cb_state, ready_tx, cmd_rx, rate_pub, initial_device)
            })
            .map_err(|_| PlaybackError::ThreadInit)?;
        let initial_rate = ready_rx.recv().map_err(|_| PlaybackError::ThreadInit)??;
        device_rate.store(initial_rate, Ordering::Release);
        Ok(Self { state, device_rate, cmd_tx })
    }

    /// Rebuild the output stream on `device` (None ⇒ host default).
    /// Pending queues are cleared on the control thread because
    /// already-resampled samples target the *old* device rate; mixing
    /// them post-swap would briefly play at the wrong pitch.
    pub fn set_device(&self, device: Option<String>) {
        // Clear queues here too so the swap is visible to the next
        // enqueue immediately, even before the control thread picks
        // up the command.
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        for (_, slot) in s.streams.iter_mut() {
            slot.reset();
        }
        drop(s);
        let _ = self.cmd_tx.send(Cmd::SetDevice(device));
    }

    /// Bind `stream_id` to `session`. Subsequent `enqueue` calls for
    /// this id append to its queue (and play when `session` is
    /// active). Idempotent: re-registering with the same id replaces
    /// the prior binding, which would only happen if dispatch reused
    /// an id (it shouldn't — CP allocates).
    pub fn register(&self, stream_id: TtsStreamId, session: SessionId) {
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        let slot = StreamSlot::new(session);
        match s.streams.iter_mut().find(|(id, _)| *id == stream_id) {
            Some(entry) => entry.1 = slot,
            None => s.streams.push((stream_id, slot)),
        }
    }

    /// Push a CP-broadcast audio chunk (24 kHz mono i16 LE bytes) to
    /// the stream's queue, resampling to the device rate on the way
    /// in. Drops the chunk if the stream isn't registered (defensive:
    /// a broadcast can deliver events for streams owned by other
    /// clients in multi-desktop setups), or if the bound session
    /// isn't the active one (post-context-switch cleanup).
    pub fn enqueue(&self, stream_id: TtsStreamId, chunk: &[u8]) {
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        // Split borrow so we can touch `last_drop_warn` while holding
        // a mutable reference into `streams` for the active path.
        let PlaybackState {
            streams,
            active_session,
            last_drop_warn,
        } = &mut *s;
        let device_rate = self.device_rate.load(Ordering::Acquire);
        if device_rate == 0 {
            // No output stream is currently bound (post-failed
            // SetDevice). Drop the chunk rather than divide-by-zero
            // in the resampler.
            return;
        }
        let Some((_, slot)) = streams.iter_mut().find(|(id, _)| *id == stream_id) else {
            return;
        };
        if active_session.as_ref() != Some(&slot.session) {
            let now = Instant::now();
            let recent = last_drop_warn
                .is_some_and(|prev| now.duration_since(prev).as_millis() < 1000);
            if !recent {
                *last_drop_warn = Some(now);
                debug!(
                    ?stream_id,
                    bound = %slot.session.as_str(),
                    "tts audio for non-active session; dropping (rate-limited)",
                );
            }
            return;
        }
        let ratio = TTS_AUDIO_SAMPLE_RATE_HZ as f64 * slot.speed as f64
            / device_rate as f64;
        resample_into(chunk, slot, ratio);
    }

    /// Set the playback speed for a stream. Already-resampled samples
    /// in the queue keep playing at whatever speed they were enqueued
    /// with; only chunks arriving after this call use the new rate.
    pub fn set_speed(&self, stream_id: TtsStreamId, speed: f32) {
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        if let Some((_, slot)) = s.streams.iter_mut().find(|(id, _)| *id == stream_id) {
            slot.speed = speed;
        }
    }

    /// Drop `stream_id`'s queued samples synchronously. Called from
    /// `tts_cancel` *before* awaiting the CP `CancelTts` round-trip
    /// — without this, already-broadcast PCM continues to play after
    /// the user pressed stop.
    pub fn cancel(&self, stream_id: TtsStreamId) {
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        if let Some((_, slot)) = s.streams.iter_mut().find(|(id, _)| *id == stream_id) {
            slot.reset();
        }
    }

    /// Forget the stream entirely. Called from `tts_close_stream`
    /// after CP acks `CloseTtsStream`.
    pub fn unregister(&self, stream_id: TtsStreamId) {
        self.state
            .lock()
            .expect("tts_playback state poisoned")
            .streams
            .retain(|(id, _)| *id != stream_id);
    }

    /// Track which session is in front. When the active session
    /// changes, drop queues for streams bound to the *previous*
    /// active session — held audio after a context switch is worse
    /// than losing it. New streams (or streams bound to the new
    /// active session) are unaffected.
    pub fn set_active_session(&self, active: Option<&SessionId>) {
        let mut s = self.state.lock().expect("tts_playback state poisoned");
        if s.active_session.as_ref() == active {
            return;
        }
        let prev = s.active_session.take();
        s.active_session = active.cloned();
        if let Some(prev) = prev {
            for (_, slot) in s.streams.iter_mut() {
                if slot.session == prev {
                    slot.reset();
                }
            }
        }
    }
}

fn run_stream_thread(
    state: Arc<Mutex<PlaybackState>>,
    ready_tx: mpsc::Sender<Result<u32, PlaybackError>>,
    cmd_rx: mpsc::Receiver<Cmd>,
    device_rate: Arc<AtomicU32>,
    initial_device: Option<String>,
) {
    let mut stream = match build_and_start(state.clone(), initial_device.as_deref()) {
        Ok((s, rate)) => {
            let _ = ready_tx.send(Ok(rate));
            Some(s)
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    // Process device-swap commands. The cpal `Stream` is `!Send` and
    // must drop on this thread, so the swap happens here rather than
    // on the chrome side.
    for cmd in cmd_rx {
        match cmd {
            Cmd::SetDevice(name) => {
                stream.take(); // drop old stream first; releases the device
                device_rate.store(0, Ordering::Release);
                match build_and_start(state.clone(), name.as_deref()) {
                    Ok((s, rate)) => {
                        device_rate.store(rate, Ordering::Release);
                        stream = Some(s);
                    }
                    Err(e) => {
                        error!(error = %e, requested = ?name, "tts set_device failed");
                    }
                }
            }
        }
    }
}

fn build_and_start(
    state: Arc<Mutex<PlaybackState>>,
    requested: Option<&str>,
) -> Result<(cpal::Stream, u32), PlaybackError> {
    let (stream, rate) = build_stream(state, requested)?;
    stream.play().map_err(PlaybackError::Play)?;
    Ok((stream, rate))
}

/// Enumerate output device names. See `audio::list_input_devices` for
/// the same fallback rationale.
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in devices {
        if let Ok(desc) = d.description() {
            out.push(desc.name().to_owned());
        }
    }
    out
}

fn build_stream(
    state: Arc<Mutex<PlaybackState>>,
    requested: Option<&str>,
) -> Result<(cpal::Stream, u32), PlaybackError> {
    let host = cpal::default_host();
    let device = pick_output_device(&host, requested)?;
    let supported = device
        .supported_output_configs()
        .map_err(|e| PlaybackError::Config(e.to_string()))?
        .next()
        .ok_or(PlaybackError::NoDevice)?;
    // Prefer 24 kHz when the device's range covers it (skip the
    // resample altogether), otherwise take the device default.
    let config = if supported.min_sample_rate() <= TTS_AUDIO_SAMPLE_RATE_HZ
        && supported.max_sample_rate() >= TTS_AUDIO_SAMPLE_RATE_HZ
    {
        supported
            .with_sample_rate(TTS_AUDIO_SAMPLE_RATE_HZ)
            .config()
    } else {
        supported.with_max_sample_rate().config()
    };
    let sample_rate = config.sample_rate;
    let channels = config.channels as usize;
    info!(
        device = %device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "unknown".into()),
        rate = sample_rate,
        channels,
        "tts playback output opened",
    );
    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                fill_output(&state, data, channels);
            },
            |err| error!(error = %err, "tts playback stream error"),
            None,
        )
        .map_err(PlaybackError::Build)?;
    Ok((stream, sample_rate))
}

fn pick_output_device(
    host: &cpal::Host,
    requested: Option<&str>,
) -> Result<cpal::Device, PlaybackError> {
    if let Some(name) = requested {
        match host.output_devices() {
            Ok(mut iter) => {
                if let Some(d) =
                    iter.find(|d| d.description().ok().map(|x| x.name().to_owned()).as_deref() == Some(name))
                {
                    return Ok(d);
                }
                warn!(requested = name, "output device not found; falling back to default");
            }
            Err(e) => {
                warn!(error = %e, "output_devices enumeration failed; using default");
            }
        }
    }
    host.default_output_device().ok_or(PlaybackError::NoDevice)
}

/// Mix every active-session stream's queue into the output frame
/// buffer. Frames not covered by any stream are filled with silence;
/// frames partially covered are clamped to [-1, 1] after summation so
/// two simultaneous streams don't clip beyond legal float range.
fn fill_output(state: &Arc<Mutex<PlaybackState>>, data: &mut [f32], channels: usize) {
    data.fill(0.0);
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    let mut s = state.lock().expect("tts_playback state poisoned");
    let Some(active) = s.active_session.clone() else {
        return;
    };
    for (_, slot) in s.streams.iter_mut() {
        if slot.session != active {
            continue;
        }
        for f in 0..frames {
            let Some(sample) = slot.queue.pop_front() else {
                break;
            };
            let base = f * channels;
            for c in 0..channels {
                data[base + c] += sample;
            }
        }
    }
    for s in data.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
}

/// Decode `chunk` (i16 LE) and append samples to `slot.queue`,
/// resampled by `ratio` (= effective source rate / device rate).
/// Carries linear-resampler state across the chunk boundary so seams
/// don't alias.
///
/// Decoding is fused into the loop — no intermediate `Vec<f32>` —
/// because the audio path runs every chunk and one allocation per
/// chunk adds up.
fn resample_into(chunk: &[u8], slot: &mut StreamSlot, ratio: f64) {
    let n = chunk.len() / 2;
    if n == 0 {
        return;
    }

    // Identity fast path: decode straight into the queue.
    if (ratio - 1.0).abs() < 1e-9 {
        let mut last = 0.0_f32;
        for i in 0..n {
            let s = decode_sample(chunk, i);
            slot.queue.push_back(s);
            last = s;
        }
        slot.last_sample = last;
        return;
    }

    let mut pos = slot.resample_pos;
    // Stop strictly *before* the last source sample: the s1 of the
    // final interpolation needs an index < n, which only holds while
    // `pos < n - 1`. Anything past defers to the next chunk via
    // `resample_pos`.
    let upper = (n - 1) as f64;
    while pos < upper {
        let idx_f = pos.floor();
        let frac = (pos - idx_f) as f32;
        let idx = idx_f as i64;
        let s0 = if idx < 0 {
            slot.last_sample
        } else {
            decode_sample(chunk, idx as usize)
        };
        let s1 = decode_sample(chunk, (idx + 1) as usize);
        slot.queue.push_back(s0 + (s1 - s0) * frac);
        pos += ratio;
    }
    slot.resample_pos = pos - n as f64;
    slot.last_sample = decode_sample(chunk, n - 1);
}

#[inline]
fn decode_sample(chunk: &[u8], idx: usize) -> f32 {
    let i = idx * 2;
    i16::from_le_bytes([chunk[i], chunk[i + 1]]) as f32 / i16::MAX as f32
}

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("no default audio output device")]
    NoDevice,
    #[error("device config: {0}")]
    Config(String),
    #[error("build output stream: {0}")]
    Build(cpal::BuildStreamError),
    #[error("play output stream: {0}")]
    Play(cpal::PlayStreamError),
    #[error("playback control thread failed to initialize")]
    ThreadInit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn fresh_slot() -> StreamSlot {
        // Tests don't care about session binding; resample_into
        // operates on the slot in isolation.
        StreamSlot {
            session: SessionId::parse("00000000000000000000000000000000").unwrap(),
            queue: VecDeque::new(),
            resample_pos: 0.0,
            last_sample: 0.0,
            speed: 1.0,
        }
    }

    #[test]
    fn resample_identity_on_equal_rates() {
        let mut slot = fresh_slot();
        let chunk = pcm_bytes(&[0, 16384, -16384, 32767]);
        resample_into(&chunk, &mut slot, 1.0);
        assert_eq!(slot.queue.len(), 4);
        // Last sample carry should equal the last decoded value.
        let last = slot.queue.back().copied().unwrap();
        assert!((slot.last_sample - last).abs() < 1e-6);
    }

    #[test]
    fn resample_output_length_within_one_of_ratio() {
        // 24 kHz source, 48 kHz destination → ratio 0.5, expect ~2× out.
        let mut slot = fresh_slot();
        let n_in = 240usize;
        let samples: Vec<i16> = (0..n_in).map(|i| (i as i16) * 100).collect();
        let chunk = pcm_bytes(&samples);
        resample_into(&chunk, &mut slot, 0.5);
        let expected = (n_in as f64 / 0.5) as i64;
        let got = slot.queue.len() as i64;
        assert!(
            (got - expected).abs() <= 2,
            "expected ~{expected} samples, got {got}",
        );
    }

    #[test]
    fn resample_constant_input_has_no_seam_click() {
        // Feed two chunks of a constant DC signal across a rate change.
        // No interpolation should ever produce a value outside the
        // constant after the first sample.
        let mut slot = fresh_slot();
        let chunk = pcm_bytes(&vec![16_000_i16; 512]);
        resample_into(&chunk, &mut slot, 0.5);
        resample_into(&chunk, &mut slot, 0.5);
        // First output sample interpolates from `last_sample = 0` up
        // to the DC value, so skip it. After that the signal must be
        // ~constant.
        let mut iter = slot.queue.iter().copied();
        iter.next();
        let target = 16_000.0 / i16::MAX as f32;
        let max_dev = iter
            .map(|v| (v - target).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_dev < 1e-3, "seam click: max_dev={max_dev}");
    }

    #[test]
    fn resample_empty_chunk_is_noop() {
        let mut slot = fresh_slot();
        resample_into(&[], &mut slot, 0.5);
        resample_into(&[0u8], &mut slot, 0.5); // odd byte
        assert!(slot.queue.is_empty());
    }

    #[test]
    fn resample_speed_compresses_output_length() {
        // Speed 2.0 at matched rates → ratio 2.0 → ~half as many out
        // samples. Mirrors what `enqueue` computes when src == device
        // rate (the common 24 kHz device case).
        let mut slot = fresh_slot();
        let n_in = 240usize;
        let samples: Vec<i16> = (0..n_in).map(|i| (i as i16) * 100).collect();
        let chunk = pcm_bytes(&samples);
        resample_into(&chunk, &mut slot, 2.0);
        let expected = (n_in as f64 / 2.0) as i64;
        let got = slot.queue.len() as i64;
        assert!(
            (got - expected).abs() <= 2,
            "expected ~{expected} samples, got {got}",
        );
    }

    fn test_pb(state: Arc<Mutex<PlaybackState>>, rate: u32) -> TtsPlayback {
        let (cmd_tx, _cmd_rx) = mpsc::channel::<Cmd>();
        TtsPlayback {
            state,
            device_rate: Arc::new(AtomicU32::new(rate)),
            cmd_tx,
        }
    }

    #[test]
    fn set_active_session_clears_queues_for_previous_session() {
        // Build a playback state directly (no cpal). We test the
        // observable contract of `set_active_session` — queues for
        // the previously-active session reset, others left alone —
        // through the public method.
        let state = Arc::new(Mutex::new(PlaybackState {
            streams: Vec::new(),
            active_session: None,
            last_drop_warn: None,
        }));
        let pb = test_pb(state.clone(), 24_000);
        let s_a = SessionId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let s_b = SessionId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        pb.register(TtsStreamId(1), s_a.clone());
        pb.register(TtsStreamId(2), s_b.clone());
        pb.set_active_session(Some(&s_a));
        // Queue some samples on stream-1 (its session is active).
        let chunk = pcm_bytes(&vec![1234_i16; 64]);
        pb.enqueue(TtsStreamId(1), &chunk);
        // Sanity: stream-1 has data, stream-2 doesn't (session
        // wasn't active).
        let g = state.lock().unwrap();
        assert!(!g.streams.iter().find(|(i, _)| *i == TtsStreamId(1)).unwrap().1.queue.is_empty());
        assert!(g.streams.iter().find(|(i, _)| *i == TtsStreamId(2)).unwrap().1.queue.is_empty());
        drop(g);

        // Switch active to s_b: stream-1's queue must clear; stream-2
        // is bound to s_b (the new active) and stays empty (also OK).
        pb.set_active_session(Some(&s_b));
        let g = state.lock().unwrap();
        assert!(g.streams.iter().find(|(i, _)| *i == TtsStreamId(1)).unwrap().1.queue.is_empty());
    }

    #[test]
    fn enqueue_drops_for_unregistered_stream() {
        let state = Arc::new(Mutex::new(PlaybackState {
            streams: Vec::new(),
            active_session: None,
            last_drop_warn: None,
        }));
        let pb = test_pb(state.clone(), 24_000);
        pb.enqueue(TtsStreamId(99), &pcm_bytes(&[1, 2, 3, 4]));
        assert!(state.lock().unwrap().streams.is_empty());
    }

    #[test]
    fn set_speed_affects_output_length() {
        // 24 kHz device, 1.0× → identity (n samples in / n out).
        // Switch to 2.0× → next chunk yields ~n/2 out.
        let state = Arc::new(Mutex::new(PlaybackState {
            streams: Vec::new(),
            active_session: None,
            last_drop_warn: None,
        }));
        let pb = test_pb(state.clone(), 24_000);
        let s = SessionId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        pb.register(TtsStreamId(1), s.clone());
        pb.set_active_session(Some(&s));
        let chunk = pcm_bytes(&vec![1234_i16; 240]);

        pb.enqueue(TtsStreamId(1), &chunk);
        let len_1x = state
            .lock()
            .unwrap()
            .streams
            .iter()
            .find(|(i, _)| *i == TtsStreamId(1))
            .unwrap()
            .1
            .queue
            .len();
        assert_eq!(len_1x, 240);

        pb.cancel(TtsStreamId(1));
        pb.set_speed(TtsStreamId(1), 2.0);
        pb.enqueue(TtsStreamId(1), &chunk);
        let len_2x = state
            .lock()
            .unwrap()
            .streams
            .iter()
            .find(|(i, _)| *i == TtsStreamId(1))
            .unwrap()
            .1
            .queue
            .len();
        assert!(
            (len_2x as i64 - 120).abs() <= 2,
            "expected ~120 samples at 2.0×, got {len_2x}",
        );
    }
}
