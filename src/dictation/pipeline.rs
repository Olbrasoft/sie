//! Local inference, correction history and guarded delivery for one recording.
use crate::{
    config::DictationConfig,
    corrections::Database,
    desktop::{self, FocusTarget},
    speech::SpeechEngine,
};
use anyhow::{Result, ensure};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
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
