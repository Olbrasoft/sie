//! Independent dictation actor: recording and inference never block tray actions.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, ensure};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use zbus::Connection;

use crate::{
    audio::Recording,
    config::DictationConfig,
    corrections::Database,
    desktop::{self, FocusTarget},
    ducking::Ducking,
    sounds::Feedback,
    speech::SpeechEngine,
    speech_lock::SpeechLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Toggle,
    FinishAndSubmit,
    Cancel,
    SetEnabled(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Disabled,
    Idle,
    Recording,
    Transcribing,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub enabled: bool,
    pub phase: Phase,
    pub message: String,
}

impl Status {
    pub fn new(enabled: bool, phase: Phase) -> Self {
        Self {
            enabled,
            phase,
            message: match phase {
                Phase::Disabled => "Diktování vypnuté",
                Phase::Idle => "Diktování připravené",
                Phase::Recording => "🔴 Nahrávám diktování",
                Phase::Transcribing => "Přepisuji lokálně…",
                Phase::Error => "Diktování nedostupné",
            }
            .into(),
        }
    }
}

struct AbortOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}
impl AbortOnDrop {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::SeqCst);
        }
    }
}

pub fn completion_is_current(active: Option<u64>, completed: u64, cancelled: bool) -> bool {
    active == Some(completed) && !cancelled
}

struct Delivery {
    target: FocusTarget,
}

struct Completed {
    characters: usize,
    receipt: Option<desktop::PasteReceipt>,
}

struct ActiveJob {
    id: u64,
    cancelled: Arc<AtomicBool>,
    submit_requested: bool,
}

impl ActiveJob {
    fn new(id: u64, cancelled: Arc<AtomicBool>, submit_requested: bool) -> Self {
        Self {
            id,
            cancelled,
            submit_requested,
        }
    }

    fn request_submit(&mut self) {
        self.submit_requested = true;
    }

    fn accepts(&self, completed: u64) -> bool {
        completion_is_current(
            Some(self.id),
            completed,
            self.cancelled.load(Ordering::SeqCst),
        )
    }
}

async fn complete(
    config: DictationConfig,
    connection: Connection,
    engine: Arc<SpeechEngine>,
    pcm: Vec<i16>,
    duration: i64,
    delivery: Delivery,
    cancelled: Arc<AtomicBool>,
) -> Result<Completed> {
    let abort = cancelled.clone();
    let raw = tokio::task::spawn_blocking(move || engine.transcribe(&pcm, abort)).await??;
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    let database = Database::connect(&config.database_url).await?;
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
        &connection,
        &delivery.target,
        &corrected,
        cancelled.clone(),
    )
    .await;
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    if let Err(error) = database
        .save(
            &raw,
            &corrected,
            &config
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

pub async fn run(
    config: DictationConfig,
    connection: Connection,
    mut commands: mpsc::Receiver<Command>,
    status: watch::Sender<Status>,
    notification_control: Option<Arc<crate::notifications::Control>>,
) {
    let mut enabled = config.enabled;
    let mut feedback = Feedback::new(
        config.sound_effects,
        config.sound_sink.clone(),
        if enabled {
            Phase::Idle
        } else {
            Phase::Disabled
        },
    );
    let mut speech_lock = match notification_control {
        Some(control) => SpeechLock::local(control),
        None => SpeechLock::new(config.virtual_assistant_speech_lock),
    };
    let ducking = Ducking::new(config.duck_sink.clone(), config.duck_volume_percent);
    let mut recording: Option<(Recording, FocusTarget)> = None;
    let mut deadline: Option<Instant> = None;
    let mut active: Option<ActiveJob> = None;
    let mut generation = 0;
    let engine = Arc::new(SpeechEngine::new(config.clone()));
    let mut jobs = JoinSet::new();
    status.send_replace(Status::new(
        enabled,
        if enabled {
            Phase::Idle
        } else {
            Phase::Disabled
        },
    ));
    loop {
        tokio::select! {
            // Process queued control requests before a simultaneous completion,
            // so a pending submit stays attached to the job that is finishing.
            biased;
            command = commands.recv() => {
                let Some(command) = command else { break; };
                if command == Command::FinishAndSubmit && recording.is_none() && active.is_none() {
                    if let Err(error) = desktop::press_enter(&connection).await {
                        tracing::warn!(%error, "Top side button could not send Enter");
                    } else {
                        tracing::info!("Top side button sent standalone Enter");
                    }
                    continue;
                }
                if let Command::SetEnabled(value) = command {
                    if value == enabled { continue; }
                    enabled = value;
                }
                if command == Command::Cancel || !enabled {
                    ducking.set_recording(false);
                    feedback.transition(if enabled { Phase::Idle } else { Phase::Disabled }, true);
                    if let Some((recording, _)) = recording.take() { recording.cancel().await; }
                    if let Some(job) = active.take() { job.cancelled.store(true, Ordering::SeqCst); }
                    deadline = None;
                    speech_lock.release().await;
                    status.send_replace(Status::new(enabled, if enabled { Phase::Idle } else { Phase::Disabled }));
                    continue;
                }
                if matches!(command, Command::SetEnabled(true)) {
                    feedback.transition(Phase::Idle, false);
                    status.send_replace(Status::new(true, Phase::Idle));
                    continue;
                }
                if let Some(job) = &mut active {
                    if command == Command::FinishAndSubmit {
                        job.request_submit();
                        tracing::info!(job=job.id, "Top side button queued current dictation submission");
                    }
                    continue;
                }
                if let Some((capture, target)) = recording.take() {
                    // Restore media independently while capture drains; never
                    // wait for transcription to finish before requesting it.
                    ducking.set_recording(false);
                    deadline = None;
                    match capture.stop().await {
                        Ok((pcm, duration)) => {
                            generation += 1;
                            let id = generation;
                            let cancelled = Arc::new(AtomicBool::new(false));
                            active = Some(ActiveJob::new(id, cancelled.clone(), command == Command::FinishAndSubmit));
                            let config = config.clone();
                            let connection = connection.clone();
                            let engine = engine.clone();
                            let delivery = Delivery {
                                target,
                            };
                            feedback.transition(Phase::Transcribing, false);
                            status.send_replace(Status::new(true, Phase::Transcribing));
                            jobs.spawn(async move {
                                let mut abort = AbortOnDrop::new(cancelled.clone());
                                let result = complete(config, connection, engine, pcm, duration, delivery, cancelled).await;
                                abort.disarm();
                                (id, result)
                            });
                        }
                        Err(error) => { speech_lock.release().await; publish_error(&status, &mut feedback, enabled, error); },
                    }
                } else {
                    let result = async {
                        ensure!(config.model_path.is_file(), "Local Whisper model is missing");
                        // Fail before recording if postprocessing is unavailable.
                        Database::connect(&config.database_url).await?.postprocessor().await?;
                        let target = desktop::focused_window(&connection).await?;
                        speech_lock.acquire(config.max_duration_seconds + 180).await?;
                        let capture = Recording::start(config.audio_source.as_deref(), config.max_duration_seconds).await?;
                        anyhow::Ok((capture, target))
                    }.await;
                    match result {
                        Ok(session) => {
                            recording = Some(session);
                            ducking.set_recording(true);
                            deadline = Some(Instant::now() + Duration::from_secs(config.max_duration_seconds));
                            feedback.transition(Phase::Recording, false);
                            status.send_replace(Status::new(true, Phase::Recording));
                        }
                        Err(error) => { speech_lock.release().await; publish_error(&status, &mut feedback, enabled, error); },
                    }
                }
            },
            _ = async {
                if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                else { std::future::pending::<()>().await; }
            } => {
                // Reuse the normal stop/transcribe path at the recording limit.
                if let Some((capture, target)) = recording.take() {
                    ducking.set_recording(false);
                    deadline = None;
                    match capture.stop().await {
                        Ok((pcm,duration)) => {
                            generation += 1;
                            let id = generation;
                            let cancelled = Arc::new(AtomicBool::new(false));
                            active = Some(ActiveJob::new(id,cancelled.clone(),false));
                            let config=config.clone(); let connection=connection.clone(); let engine=engine.clone();
                            feedback.transition(Phase::Transcribing, false);
                            status.send_replace(Status::new(true,Phase::Transcribing));
                            jobs.spawn(async move {
                                let mut abort=AbortOnDrop::new(cancelled.clone());
                                let delivery=Delivery{target};
                                let result=complete(config,connection,engine,pcm,duration,delivery,cancelled).await;
                                abort.disarm();
                                (id,result)
                            });
                        }
                        Err(error) => { speech_lock.release().await; publish_error(&status,&mut feedback,enabled,error); },
                    }
                }
            },
            Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                match result {
                    Ok((id, result)) if active.as_ref().is_some_and(|job| job.accepts(id)) => {
                        let result = match result {
                            Ok(mut completed) if active.as_ref().is_some_and(|job| job.submit_requested) && completed.receipt.is_some() => {
                                let receipt = completed.receipt.take().unwrap();
                                let cancelled = active.as_ref().unwrap().cancelled.clone();
                                let connection = connection.clone();
                                // Keep the job active and cancellable through deferred Enter.
                                // The consumed receipt prevents repeated presses from sending twice.
                                jobs.spawn(async move {
                                    let mut abort = AbortOnDrop::new(cancelled.clone());
                                    let result = receipt.submit(&connection, &cancelled).await.map(|()| completed);
                                    abort.disarm();
                                    (id, result)
                                });
                                continue;
                            }
                            result => result,
                        };
                        active = None;
                        feedback.transition(Phase::Idle, false);
                        speech_lock.release().await;
                        match result {
                            Ok(completed) => {
                                tracing::info!(characters=completed.characters, "Local dictation completed");
                                status.send_replace(Status::new(enabled, Phase::Idle));
                            }
                            Err(error) => { speech_lock.release().await; publish_error(&status, &mut feedback, enabled, error); },
                        }
                    }
                    Ok(_) => {},
                    Err(error) => {
                        if let Some(job)=active.take() { job.cancelled.store(true,Ordering::SeqCst); }
                        speech_lock.release().await;
                        publish_error(&status,&mut feedback,enabled,error.into());
                    },
                }
            },
        }
    }
    ducking.set_recording(false);
    if let Some((recording, _)) = recording.take() {
        recording.cancel().await;
    }
    if let Some(job) = active.take() {
        job.cancelled.store(true, Ordering::SeqCst);
    }
    speech_lock.release().await;
    ducking.shutdown().await;
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
}

fn publish_error(
    status: &watch::Sender<Status>,
    feedback: &mut Feedback,
    enabled: bool,
    error: anyhow::Error,
) {
    feedback.transition(Phase::Error, false);
    tracing::warn!("Dictation failed: {error:#}");
    status.send_replace(Status {
        enabled,
        phase: Phase::Error,
        message: if error.to_string().contains("submission was not sent") {
            "Text je vložený, ale odeslání se nezdařilo".into()
        } else if error.to_string().contains("Microphone is muted") {
            "Mikrofon je ztlumený".into()
        } else if error.to_string().contains("microphone state") {
            "Nastavený mikrofon není dostupný".into()
        } else if error.to_string().contains("model is missing") {
            "Chybí model pro diktování".into()
        } else if error.to_string().contains("focus")
            || error.to_string().contains("window changed")
        {
            "Cílové okno se změnilo; text nebyl vložen".into()
        } else if error.to_string().contains("VirtualAssistant speech") {
            "Nelze pozastavit hlas VirtualAssistanta".into()
        } else {
            "Diktování selhalo; podrobnosti jsou v protokolu".into()
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_submission_coalesces_and_cannot_cross_a_canceled_or_later_job() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut first = ActiveJob::new(7, cancelled.clone(), false);
        first.request_submit();
        first.request_submit();
        assert!(first.submit_requested);
        assert!(first.accepts(7));
        assert!(!first.accepts(8));
        cancelled.store(true, Ordering::SeqCst);
        assert!(!first.accepts(7));
        let next = ActiveJob::new(8, Arc::new(AtomicBool::new(false)), false);
        assert!(!next.submit_requested);
        assert!(!next.accepts(7));
        assert!(next.accepts(8));
    }
    #[test]
    fn canceled_and_stale_transcriptions_cannot_complete_a_new_session() {
        assert!(completion_is_current(Some(7), 7, false));
        assert!(!completion_is_current(Some(7), 7, true));
        assert!(!completion_is_current(None, 7, false));
        assert!(!completion_is_current(Some(8), 7, false));
    }

    #[test]
    fn shutdown_aborts_inference_but_normal_completion_does_not_cancel_it() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut guard = AbortOnDrop::new(cancelled.clone());
            guard.disarm();
        }
        assert!(!cancelled.load(Ordering::SeqCst));
        {
            let _guard = AbortOnDrop::new(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::SeqCst));
    }
}
