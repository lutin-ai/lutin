//! whisper.cpp backend.
//!
//! Loads a `WhisperContext` from a file already on disk (CP handles
//! the download + atomic-rename). The worker borrows the context via
//! `Arc` and creates a fresh `WhisperState` per `transcribe` call, so
//! `&self` is enough — concurrent inferences off one factory are safe.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::backend::{MIN_INFERENCE_SAMPLES, SttBackendFactory, SttWorker, TranscribeParams};
use crate::SttError;

/// One-shot whisper.cpp log silencer. Process-global; call exactly
/// once at startup (CP does this in `Supervisor::spawn`).
pub fn install_log_callback() {
    unsafe {
        whisper_rs::set_log_callback(Some(whisper_log_noop), std::ptr::null_mut());
    }
}

unsafe extern "C" fn whisper_log_noop(
    _level: std::ffi::c_uint,
    _msg: *const std::ffi::c_char,
    _user_data: *mut std::ffi::c_void,
) {
}

/// Factory pinned to one model file. Cheap to construct — context
/// load happens on `create_worker`.
pub struct WhisperFactory {
    model_path: PathBuf,
}

impl WhisperFactory {
    pub fn new(model_path: PathBuf) -> Self {
        Self { model_path }
    }
}

impl SttBackendFactory for WhisperFactory {
    fn create_worker(&self) -> Result<Box<dyn SttWorker>, SttError> {
        let path_str = self.model_path.to_str().ok_or_else(|| {
            SttError::Load(format!(
                "non-utf8 model path: {}",
                self.model_path.display()
            ))
        })?;
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| SttError::Load(format!("load {}: {e}", self.model_path.display())))?;
        Ok(Box::new(WhisperWorker {
            ctx: Arc::new(ctx),
        }))
    }
}

pub struct WhisperWorker {
    ctx: Arc<WhisperContext>,
}

impl SttWorker for WhisperWorker {
    fn transcribe(
        &self,
        pcm: &[i16],
        params: &TranscribeParams,
        cancel_rx: &watch::Receiver<bool>,
    ) -> Result<String, SttError> {
        if pcm.len() < MIN_INFERENCE_SAMPLES {
            return Ok(String::new());
        }
        if *cancel_rx.borrow() {
            return Err(SttError::Cancelled);
        }
        let pcm_f32: Vec<f32> = pcm.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
        let mut state: WhisperState = self
            .ctx
            .create_state()
            .map_err(|e| SttError::Inference(format!("whisper create_state: {e}")))?;
        let mut full = FullParams::new(into_strategy(params.beam_size));
        full.set_single_segment(true);
        full.set_no_timestamps(true);
        full.set_temperature(0.0);
        full.set_no_context(true);
        full.set_suppress_nst(true);
        full.set_print_progress(false);
        full.set_print_special(false);
        full.set_print_realtime(false);
        if let Some(ref lang) = params.language {
            full.set_language(Some(lang.as_str()));
        }
        state
            .full(full, &pcm_f32)
            .map_err(|e| SttError::Inference(format!("full: {e}")))?;
        let mut out = String::with_capacity(128);
        for seg in state.as_iter() {
            let s = seg
                .to_str_lossy()
                .map_err(|e| SttError::Inference(format!("segment: {e}")))?;
            out.push_str(&s);
        }
        Ok(out.trim().to_owned())
    }
}

fn into_strategy(beam_size: u8) -> SamplingStrategy {
    match beam_size {
        0 | 1 => SamplingStrategy::Greedy { best_of: 1 },
        n => SamplingStrategy::BeamSearch {
            beam_size: n as i32,
            patience: -1.0,
        },
    }
}
