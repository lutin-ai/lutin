//! Microphone capture as a streaming chunk source.
//!
//! One global `Capture` instance owns the mic for the lifetime of the
//! app. `start()` arms capture and returns a receiver yielding i16
//! chunks (16 kHz mono PCM); `stop()` disarms and closes that
//! receiver. The dispatcher pumps each chunk into a CP
//! `TranscribeChunk` request as it arrives — there's no in-process
//! buffer accumulating the full clip, so PTT duration is bounded only
//! by user lung capacity and CP whisper memory.
//!
//! cpal's `Stream` is `!Send`, so the stream lives on a dedicated
//! control thread. The hot path between hotkey press and capture is
//! a quick mutex swap to install/remove the chunk sender; play/pause
//! runs in the background so the keyboard thread never blocks on
//! ALSA/PulseAudio cork latency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use lutin_control_protocol::MonoPcm16k;
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, error, info, warn};

/// Whisper expects 16 kHz mono. Resample inline so callers always see
/// a uniform rate regardless of the device default.
const TARGET_SAMPLE_RATE: u32 = 16_000;

enum StreamCmd {
    Play,
    Pause,
}

/// Public handle. One owner (`AppState.audio`) for the app's
/// lifetime; PTT press → `start()` → drain receiver → release →
/// `stop()`.
pub struct Capture {
    cmd_tx: mpsc::Sender<StreamCmd>,
    recording: Arc<AtomicBool>,
    /// `Some` while a PTT session is active. The cpal callback
    /// publishes each chunk through this sender; `stop()` clears it,
    /// which closes the receiver on the consumer side.
    chunk_tx: Arc<Mutex<Option<tokio_mpsc::UnboundedSender<MonoPcm16k>>>>,
}

impl Capture {
    pub fn new() -> Result<Self, CaptureError> {
        let recording = Arc::new(AtomicBool::new(false));
        let chunk_tx: Arc<Mutex<Option<tokio_mpsc::UnboundedSender<MonoPcm16k>>>> =
            Arc::new(Mutex::new(None));
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), CaptureError>>();

        let cb_state = CallbackState {
            recording: recording.clone(),
            chunk_tx: chunk_tx.clone(),
        };

        std::thread::Builder::new()
            .name("audio-stream-ctrl".into())
            .spawn(move || run_stream_thread(cb_state, cmd_rx, ready_tx))
            .map_err(|_| CaptureError::ThreadInit)?;

        ready_rx.recv().map_err(|_| CaptureError::ThreadInit)??;

        Ok(Self {
            cmd_tx,
            recording,
            chunk_tx,
        })
    }

    /// Arm the capture. Returns the chunk receiver — drop it (or call
    /// `stop()`) to release the mic. Idempotent: calling `start` while
    /// already recording closes the previous receiver and installs a
    /// fresh one, so a double-press doesn't silently mix two streams.
    pub fn start(&self) -> tokio_mpsc::UnboundedReceiver<MonoPcm16k> {
        let (tx, rx) = tokio_mpsc::unbounded_channel();
        // Replace any prior sender (closes the previous receiver) and
        // flip the recording flag so the cpal callback starts pushing.
        *self.chunk_tx.lock().expect("chunk_tx poisoned") = Some(tx);
        self.recording.store(true, Ordering::Release);
        let _ = self.cmd_tx.send(StreamCmd::Play);
        debug!("audio capture started");
        rx
    }

    /// Disarm. The chunk receiver returned by the matching `start()`
    /// closes; any chunk in flight on the audio thread is dropped on
    /// the next callback. Always Pauses the cpal stream so the mic LED
    /// clears even if nothing was captured.
    pub fn stop(&self) {
        self.recording.store(false, Ordering::Release);
        *self.chunk_tx.lock().expect("chunk_tx poisoned") = None;
        let _ = self.cmd_tx.send(StreamCmd::Pause);
        debug!("audio capture stopped");
    }
}

#[derive(Clone)]
struct CallbackState {
    recording: Arc<AtomicBool>,
    chunk_tx: Arc<Mutex<Option<tokio_mpsc::UnboundedSender<MonoPcm16k>>>>,
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
    fn to_i16(self) -> i16;
}
impl Sample for f32 {
    #[inline]
    fn to_i16(self) -> i16 {
        let clamped = self.clamp(-1.0, 1.0);
        (clamped * i16::MAX as f32) as i16
    }
}
impl Sample for i16 {
    #[inline]
    fn to_i16(self) -> i16 {
        self
    }
}
impl Sample for u16 {
    #[inline]
    fn to_i16(self) -> i16 {
        (self as i32 - 0x8000) as i16
    }
}

fn make_callback<T: Sample>(
    cb: CallbackState,
    channels: u16,
    sample_rate: u32,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static {
    let mut chunk = Vec::<i16>::new();
    let ratio = sample_rate as f32 / TARGET_SAMPLE_RATE as f32;
    move |data: &[T], _info| {
        if !cb.recording.load(Ordering::Acquire) {
            return;
        }
        resample_to_mono_16k_i16(data, channels, ratio, &mut chunk);
        if chunk.is_empty() {
            return;
        }
        let guard = cb.chunk_tx.lock().expect("chunk_tx poisoned");
        if let Some(tx) = guard.as_ref() {
            // `take` to move the chunk out without realloc — the
            // sender owns it from here. Wrap at the boundary so
            // every downstream consumer sees a `MonoPcm16k` (the
            // 16 kHz mono PCM invariant is satisfied by this
            // function's resampling, and the type carries that
            // proof). Sending into a closed receiver is the
            // documented signal that PTT is over and the cpal
            // callback may still fire once or twice afterward —
            // dropping the value silently is intentional.
            let send = std::mem::take(&mut chunk);
            let _ = tx.send(MonoPcm16k::from_samples(send));
        }
    }
}

fn resample_to_mono_16k_i16<T: Sample>(data: &[T], channels: u16, ratio: f32, out: &mut Vec<i16>) {
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
            // Average channels in i32 to avoid i16 overflow.
            let mut sum: i32 = 0;
            for c in 0..ch {
                sum += data[frame_start + c].to_i16() as i32;
            }
            out.push((sum / ch as i32) as i16);
        }
    } else {
        for i in 0..out_len {
            let src = (i as f32 * ratio) as usize;
            if src >= data.len() {
                break;
            }
            out.push(data[src].to_i16());
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
