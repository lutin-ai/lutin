//! Microphone capture for hotkey-driven transcription.
//!
//! One global `Capture` instance owns the mic for the lifetime of the
//! app. `start()` arms the capture buffer; `stop()` disarms and drains
//! the accumulated 16 kHz mono samples. Concurrent triggers (two combos
//! held at once, or a re-press during the linger window) coalesce into
//! one logical session — the second `start()` is a no-op while the
//! buffer is already armed, and the matching `stop()` drains whatever
//! has accumulated since the first `start()`.
//!
//! cpal's `Stream` is `!Send`, so the stream lives on a dedicated
//! control thread. The hot path between hotkey press and capture is
//! just an atomic flip; play/pause runs in the background so the
//! keyboard thread never blocks on ALSA/PulseAudio cork latency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tracing::{debug, error, info, warn};

/// Whisper expects 16 kHz mono. Resample inline so callers always see
/// a uniform rate regardless of the device default.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Cap on accumulated samples per session. ~30s of 16 kHz mono = 480k
/// floats = ~2 MiB. Past that the user almost certainly forgot the key
/// is held; we stop appending rather than grow unbounded. Stop+drain
/// still returns whatever was captured.
const MAX_BUFFER_SAMPLES: usize = 30 * TARGET_SAMPLE_RATE as usize;

enum StreamCmd {
    Play,
    Pause,
}

/// Public handle. Cheap to clone if we ever need multiple owners; for
/// now there's exactly one, parked in `AppState`.
pub struct Capture {
    cmd_tx: mpsc::Sender<StreamCmd>,
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
}

impl Capture {
    /// Spawn the cpal stream-control thread. Returns once the stream
    /// has been built (or fails to build) so caller knows immediately
    /// whether mic input is available. Errors here mean no mic — the
    /// app stays usable, hotkeys just can't capture.
    pub fn new() -> Result<Self, CaptureError> {
        let recording = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::<f32>::with_capacity(TARGET_SAMPLE_RATE as usize)));
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), CaptureError>>();

        let cb_state = CallbackState {
            recording: recording.clone(),
            buffer: buffer.clone(),
        };

        std::thread::Builder::new()
            .name("audio-stream-ctrl".into())
            .spawn(move || run_stream_thread(cb_state, cmd_rx, ready_tx))
            .map_err(|_| CaptureError::ThreadInit)?;

        ready_rx.recv().map_err(|_| CaptureError::ThreadInit)??;

        Ok(Self {
            cmd_tx,
            recording,
            buffer,
        })
    }

    /// Arm the capture buffer. Idempotent — second call while already
    /// recording is a no-op (the buffer keeps accumulating). Sends Play
    /// to the control thread; if the stream is already running this is
    /// also a no-op at the cpal layer.
    pub fn start(&self) {
        if self.recording.swap(true, Ordering::AcqRel) {
            return;
        }
        self.buffer.lock().expect("audio buffer poisoned").clear();
        let _ = self.cmd_tx.send(StreamCmd::Play);
        debug!("audio capture started");
    }

    /// Disarm and drain. Returns 16 kHz mono f32 samples accumulated
    /// since the most recent `start()`. Always Pauses the stream so
    /// the mic LED clears even if nothing was captured.
    pub fn stop(&self) -> Vec<f32> {
        self.recording.store(false, Ordering::Release);
        let drained = std::mem::take(
            &mut *self.buffer.lock().expect("audio buffer poisoned"),
        );
        let _ = self.cmd_tx.send(StreamCmd::Pause);
        debug!(samples = drained.len(), "audio capture stopped");
        drained
    }
}

#[derive(Clone)]
struct CallbackState {
    recording: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<f32>>>,
}

fn run_stream_thread(
    cb: CallbackState,
    cmd_rx: mpsc::Receiver<StreamCmd>,
    ready_tx: mpsc::Sender<Result<(), CaptureError>>,
) {
    let stream = match build_stream(cb) {
        Ok(s) => {
            let _ = ready_tx.send(Ok(()));
            s
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    for cmd in cmd_rx {
        match cmd {
            StreamCmd::Play => {
                if let Err(e) = stream.play() {
                    error!(error = %e, "audio stream play failed");
                }
            }
            StreamCmd::Pause => {
                if let Err(e) = stream.pause() {
                    warn!(error = %e, "audio stream pause failed");
                }
            }
        }
    }
}

fn build_stream(cb: CallbackState) -> Result<cpal::Stream, CaptureError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(CaptureError::NoDevice)?;
    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels();
    let sample_format = config.sample_format();

    info!(
        device = %device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "unknown".into()),
        rate = sample_rate,
        channels,
        ?sample_format,
        "audio input opened"
    );

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            make_callback::<f32>(cb, channels, sample_rate),
            stream_err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            make_callback::<i16>(cb, channels, sample_rate),
            stream_err,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            make_callback::<u16>(cb, channels, sample_rate),
            stream_err,
            None,
        )?,
        other => return Err(CaptureError::UnsupportedFormat(format!("{other:?}"))),
    };
    Ok(stream)
}

fn stream_err(err: cpal::StreamError) {
    error!(error = %err, "audio stream error");
}

trait Sample: Copy + Send + 'static {
    fn to_f32(self) -> f32;
}
impl Sample for f32 {
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
}
impl Sample for i16 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32 / i16::MAX as f32
    }
}
impl Sample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        (self as f32 / u16::MAX as f32) * 2.0 - 1.0
    }
}

fn make_callback<T: Sample>(
    cb: CallbackState,
    channels: u16,
    sample_rate: u32,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static {
    let mut scratch = Vec::<f32>::new();
    let ratio = sample_rate as f32 / TARGET_SAMPLE_RATE as f32;
    move |data: &[T], _info| {
        if !cb.recording.load(Ordering::Acquire) {
            return;
        }
        resample_to_mono_16k(data, channels, ratio, &mut scratch);
        if scratch.is_empty() {
            return;
        }
        let mut buf = cb.buffer.lock().expect("audio buffer poisoned");
        let remaining = MAX_BUFFER_SAMPLES.saturating_sub(buf.len());
        if remaining == 0 {
            return;
        }
        let take = scratch.len().min(remaining);
        buf.extend_from_slice(&scratch[..take]);
    }
}

fn resample_to_mono_16k<T: Sample>(data: &[T], channels: u16, ratio: f32, out: &mut Vec<f32>) {
    out.clear();
    let mono_frames = data.len() / channels as usize;
    if mono_frames == 0 {
        return;
    }
    let out_len = (mono_frames as f32 / ratio) as usize;
    out.reserve(out_len);
    if channels > 1 {
        let ch = channels as usize;
        for i in 0..out_len {
            let frame_start = (i as f32 * ratio) as usize * ch;
            if frame_start + ch > data.len() {
                break;
            }
            let mut sum = 0.0;
            for c in 0..ch {
                sum += data[frame_start + c].to_f32();
            }
            out.push(sum / ch as f32);
        }
    } else {
        for i in 0..out_len {
            let src = (i as f32 * ratio) as usize;
            if src >= data.len() {
                break;
            }
            out.push(data[src].to_f32());
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no default audio input device")]
    NoDevice,
    #[error("unsupported sample format: {0}")]
    UnsupportedFormat(String),
    #[error("build stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("default config: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("stream control thread failed to initialize")]
    ThreadInit,
}
