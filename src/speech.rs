//! Native, local Whisper inference. Audio and text never leave this process.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, ensure};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::config::DictationConfig;

pub struct SpeechEngine {
    config: DictationConfig,
    context: Mutex<Option<WhisperContext>>,
}

impl SpeechEngine {
    pub fn new(config: DictationConfig) -> Self {
        Self {
            config,
            context: Mutex::new(None),
        }
    }

    pub fn transcribe(&self, pcm: &[i16], cancelled: Arc<AtomicBool>) -> Result<String> {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        ensure!(pcm.len() >= 1600, "Recording is too short");
        let mut context = self
            .context
            .lock()
            .map_err(|_| anyhow::anyhow!("Speech engine lock unavailable"))?;
        if context.is_none() {
            let mut params = WhisperContextParameters::default();
            params.use_gpu(self.config.use_gpu && cfg!(feature = "cuda"));
            *context = Some(
                WhisperContext::new_with_params(&self.config.model_path, params)
                    .context("Cannot load local Whisper model")?,
            );
        }
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        let mut state = context.as_ref().unwrap().create_state()?;
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });
        params.set_language(Some(&self.config.language));
        params.set_no_context(true);
        params.set_temperature(0.0);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_n_threads(
            std::thread::available_parallelism()
                .map(|n| n.get().min(8) as i32)
                .unwrap_or(4),
        );
        // whisper-rs 0.16's safe wrapper double-boxes callback data but casts it
        // back to the closure type. Use the native callback with a stable atomic.
        // SAFETY: `cancelled` owns this allocation for the entire synchronous
        // state.full call. The callback only reads an AtomicBool, never the context.
        unsafe {
            params.set_abort_callback(Some(abort_requested));
            params.set_abort_callback_user_data(Arc::as_ptr(&cancelled).cast_mut().cast());
        }
        let samples: Vec<f32> = pcm
            .iter()
            .map(|sample| f32::from(*sample) / 32768.0)
            .collect();
        state
            .full(params, &samples)
            .context("Local Whisper inference failed")?;
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment.to_str_lossy()?.trim().to_owned();
            if !text.is_empty() {
                segments.push(text);
            }
        }
        Ok(segments.join(" "))
    }
}

pub fn read_wav(path: &std::path::Path) -> Result<Vec<i16>> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1
            && spec.sample_rate == 16000
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int,
        "Expected 16 kHz mono signed 16-bit WAV"
    );
    Ok(reader
        .into_samples::<i16>()
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

// SAFETY: user_data must point to a live AtomicBool until inference returns.
unsafe extern "C" fn abort_requested(user_data: *mut std::ffi::c_void) -> bool {
    unsafe { &*user_data.cast::<AtomicBool>() }.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_abort_callback_tracks_the_actual_atomic_across_threads() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let pointer = Arc::as_ptr(&cancelled).cast_mut().cast();
        // SAFETY: the Arc remains alive throughout both callback invocations.
        assert!(!unsafe { abort_requested(pointer) });
        let worker_flag = cancelled.clone();
        std::thread::spawn(move || worker_flag.store(true, Ordering::SeqCst))
            .join()
            .unwrap();
        assert!(unsafe { abort_requested(pointer) });
    }

    #[test]
    fn cancellation_is_checked_before_loading_a_model() {
        let engine = SpeechEngine::new(DictationConfig {
            model_path: "/missing-test-model.bin".into(),
            ..DictationConfig::default()
        });
        let result = engine.transcribe(&vec![0; 16000], Arc::new(AtomicBool::new(true)));
        assert_eq!(result.unwrap_err().to_string(), "Dictation canceled");
    }
}
