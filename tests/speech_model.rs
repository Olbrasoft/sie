//! Explicit model smoke test, never part of unattended desktop-free tests.
use sie::{
    config::DictationConfig,
    speech::{SpeechEngine, read_wav},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[test]
#[ignore = "requires explicit SIE_TEST_MODEL and SIE_TEST_WAV; loads a real local model"]
fn repeated_inference_survives_cancellation() {
    let config = DictationConfig {
        model_path: std::env::var("SIE_TEST_MODEL")
            .expect("SIE_TEST_MODEL required")
            .into(),
        use_gpu: cfg!(feature = "cuda"),
        ..DictationConfig::default()
    };
    let pcm = read_wav(std::path::Path::new(
        &std::env::var("SIE_TEST_WAV").expect("SIE_TEST_WAV required"),
    ))
    .unwrap();
    let engine = SpeechEngine::new(config);
    let first = engine
        .transcribe(&pcm, Arc::new(AtomicBool::new(false)))
        .unwrap();
    assert!(!first.trim().is_empty());
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        flag.store(true, Ordering::SeqCst);
    });
    assert!(engine.transcribe(&pcm, cancelled).is_err());
    cancel.join().unwrap();
    for _ in 0..3 {
        let text = engine
            .transcribe(&pcm, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(text, first, "Cancellation must not poison later sessions");
    }
}
