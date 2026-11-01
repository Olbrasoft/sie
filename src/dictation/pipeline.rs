//! Local inference, correction history and guarded delivery for one recording.
use crate::{
    config::DictationConfig,
    corrections::Database,
    desktop::{self, FocusTarget},
    speech::SpeechEngine,
};
use anyhow::{Result, ensure};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use zbus::Connection;

pub(super) struct Completed {
    pub characters: usize,
    pub receipt: Option<desktop::PasteReceipt>,
}

#[derive(Clone)]
pub(super) struct Pipeline {
    config: DictationConfig,
    connection: Connection,
    engine: Arc<SpeechEngine>,
}
impl Pipeline {
    pub fn new(config: DictationConfig, connection: Connection) -> Self {
        Self {
            engine: Arc::new(SpeechEngine::new(config.clone())),
            config,
            connection,
        }
    }

    pub async fn select_model(&mut self, model: &Path, config_path: &Path) -> Result<()> {
        ensure!(model.is_file(), "Local Whisper model is missing");
        // No recording, pending paste or canceled native inference may still own
        // this engine. This also protects against a future missed job guard.
        ensure!(
            Arc::strong_count(&self.engine) == 1,
            "Speech engine is still in use"
        );
        let engine = self.engine.clone();
        let loaded = tokio::task::spawn_blocking(move || engine.unload()).await??;
        tracing::info!(released_model = loaded, "Previous Whisper context released");
        // On failure the previous selection remains valid and reloads lazily.
        crate::models::persist(config_path, model)?;
        self.config.model_path = model.to_owned();
        self.engine = Arc::new(SpeechEngine::new(self.config.clone()));
        tracing::info!("Whisper model selected; loading deferred until transcription");
        Ok(())
    }
    pub async fn complete(
        &self,
        pcm: Vec<i16>,
        duration: i64,
        target: FocusTarget,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Completed> {
        let engine = self.engine.clone();
        let abort = cancelled.clone();
        let raw = tokio::task::spawn_blocking(move || engine.transcribe(&pcm, abort)).await??;
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        let database = Database::connect(&self.config.database_url).await?;
        let processor = database.postprocessor().await?;
        let (corrected, used) = processor.apply(&raw);
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        if corrected.trim().is_empty() {
            return Ok(Completed {
                characters: 0,
                receipt: None,
            });
        }
        let pasted = desktop::paste_text_with_receipt(
            &self.connection,
            &target,
            &corrected,
            cancelled.clone(),
        )
        .await;
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        if let Err(error) = database
            .save(
                &raw,
                &corrected,
                &self
                    .config
                    .model_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                duration,
                pasted.is_ok(),
                &used,
            )
            .await
        {
            tracing::warn!(%error, "Could not save dictation history");
        }
        Ok(Completed {
            characters: corrected.chars().count(),
            receipt: pasted?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn model_selection_requires_exclusive_ownership_and_replaces_the_engine_lazily() {
        let bus = crate::test_bus::Bus::new();
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("new-model.bin");
        // Deliberately not a valid model: selecting must never initialize Whisper.
        std::fs::write(&model, b"not a Whisper model").unwrap();
        let config_path = directory.path().join("config.toml");
        let mut pipeline = Pipeline::new(DictationConfig::default(), bus.connection().await);
        let old_path = pipeline.config.model_path.clone();
        let old_engine = Arc::downgrade(&pipeline.engine);
        let in_flight = pipeline.clone();
        assert!(pipeline.select_model(&model, &config_path).await.is_err());
        assert_eq!(pipeline.config.model_path, old_path);
        assert!(!config_path.exists());
        drop(in_flight);
        pipeline.select_model(&model, &config_path).await.unwrap();
        assert!(old_engine.upgrade().is_none());
        assert_eq!(pipeline.config.model_path, model);
        assert_eq!(
            crate::config::AppConfig::load(Some(config_path))
                .unwrap()
                .dictation
                .model_path,
            model
        );
        assert!(!pipeline.engine.unload().unwrap());
    }

    #[tokio::test]
    async fn failed_persistence_keeps_the_previous_model_and_missing_files_never_change_config() {
        let bus = crate::test_bus::Bus::new();
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("new-model.bin");
        let config_path = directory.path().join("config.toml");
        std::fs::write(&config_path, "broken = [").unwrap();
        let mut pipeline = Pipeline::new(DictationConfig::default(), bus.connection().await);
        let original = pipeline.config.model_path.clone();
        assert!(pipeline.select_model(&model, &config_path).await.is_err());
        std::fs::write(&model, []).unwrap();
        assert!(pipeline.select_model(&model, &config_path).await.is_err());
        assert_eq!(pipeline.config.model_path, original);
        assert_eq!(std::fs::read_to_string(config_path).unwrap(), "broken = [");
        assert!(!pipeline.engine.unload().unwrap());
    }
}
