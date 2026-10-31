//! Serialize dictation commands and publish recording/session status.
use super::{
    Command, Phase, Status,
    jobs::{Completion, Jobs},
};
use crate::{
    audio::Recording,
    config::DictationConfig,
    corrections::Database,
    desktop::{self, FocusTarget},
    ducking::Ducking,
    sounds::Feedback,
    speech_lock::SpeechLock,
};
use anyhow::{Result, ensure};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use zbus::Connection;

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
    let (owned_ducking, handle) = match &notification_control {
        Some(control) => (None, control.ducking.clone()),
        None => {
            let owner = Ducking::new(config.duck_sink.clone(), config.duck_volume_percent);
            let handle = owner.handle();
            (Some(owner), handle)
        }
    };
    let ducking = handle.recording();
    let mut speech_lock = match notification_control {
        Some(control) => SpeechLock::local(control),
        None => SpeechLock::new(config.virtual_assistant_speech_lock),
    };
    let mut recording: Option<(Recording, FocusTarget)> = None;
    let mut deadline: Option<Instant> = None;
    let mut jobs = Jobs::new(config.clone(), connection.clone());
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
                if command == Command::FinishAndSubmit && recording.is_none() && !jobs.is_active() {
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
                    feedback.transition(if enabled { Phase::Idle } else { Phase::Disabled }, true).await;
                    if let Some((recording, _)) = recording.take() { recording.cancel().await; }
                    jobs.cancel();
                    deadline = None;
                    speech_lock.release().await;
                    status.send_replace(Status::new(enabled, if enabled { Phase::Idle } else { Phase::Disabled }));
                    continue;
                }
                if matches!(command, Command::SetEnabled(true)) {
                    feedback.transition(Phase::Idle, false).await;
                    status.send_replace(Status::new(true, Phase::Idle));
                    continue;
                }
                if jobs.is_active() {
                    if command == Command::FinishAndSubmit { jobs.request_submit(); }
                    continue;
                }
                if let Some((capture, target)) = recording.take() {
                    // Restore media independently while capture drains; never
                    // wait for transcription to finish before requesting it.
                    ducking.set_recording(false);
                    deadline = None;
                    if let Err(error)=finish(capture,target,command==Command::FinishAndSubmit,&mut jobs,&mut feedback,&status).await {
                        speech_lock.release().await;publish_error(&status,&mut feedback,enabled,error).await;
                    }
                } else {
                    let result = async {
                        ensure!(config.model_path.is_file(), "Local Whisper model is missing");
                        // Fail before recording if postprocessing is unavailable.
                        Database::connect(&config.database_url).await?.postprocessor().await?;
                        let target = desktop::focused_window(&connection).await?;
                        // Transfer attenuation ownership before cancelling notification speech.
                        ducking.set_recording(true);
                        speech_lock.acquire(config.max_duration_seconds + 180).await?;
                        let capture = Recording::start(config.audio_source.as_deref(), config.max_duration_seconds).await?;
                        anyhow::Ok((capture, target))
                    }.await;
                    match result {
                        Ok(session) => {
                            recording = Some(session);
                            ducking.set_recording(true);
                            deadline = Some(Instant::now() + Duration::from_secs(config.max_duration_seconds));
                            feedback.transition(Phase::Recording, false).await;
                            status.send_replace(Status::new(true, Phase::Recording));
                        }
                        Err(error) => { ducking.set_recording(false); speech_lock.release().await; publish_error(&status, &mut feedback, enabled, error).await; },
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
                    if let Err(error)=finish(capture,target,false,&mut jobs,&mut feedback,&status).await {
                        speech_lock.release().await;publish_error(&status,&mut feedback,enabled,error).await;
                    }
                }
            },
            Some(result) = jobs.next(), if jobs.has_pending() => {
                match jobs.completed(result) {
                    Completion::Pending | Completion::Stale=>{},
                    Completion::Finished(result)=>{
                        feedback.transition(Phase::Idle,false).await;
                        speech_lock.release().await;
                        match result {
                            Ok(completed)=>{tracing::info!(characters=completed.characters,"Local dictation completed");status.send_replace(Status::new(enabled,Phase::Idle));},
                            Err(error)=>{ducking.set_recording(false);publish_error(&status,&mut feedback,enabled,error).await;},
                        }
                    }
                }
            },
        }
    }
    ducking.set_recording(false);
    if let Some((recording, _)) = recording.take() {
        recording.cancel().await;
    }
    jobs.cancel();
    speech_lock.release().await;
    drop(ducking);
    if let Some(ducking) = owned_ducking {
        ducking.shutdown().await;
    }
    jobs.shutdown().await;
}

async fn publish_error(
    status: &watch::Sender<Status>,
    feedback: &mut Feedback,
    enabled: bool,
    error: anyhow::Error,
) {
    feedback.transition(Phase::Error, false).await;
    tracing::warn!("Dictation failed: {error:#}");
    let reason = error.to_string();
    status.send_replace(Status {
        enabled,
        phase: Phase::Error,
        message: if reason.contains("submission was not sent") {
            "Text je vložený, ale odeslání se nezdařilo".into()
        } else if reason.contains("Microphone is muted") {
            "Mikrofon je ztlumený".into()
        } else if reason.contains("microphone state") {
            "Nastavený mikrofon není dostupný".into()
        } else if reason.contains("model is missing") {
            "Chybí model pro diktování".into()
        } else if reason.contains("focus") || reason.contains("window changed") {
            "Cílové okno se změnilo; text nebyl vložen".into()
        } else if reason.contains("VirtualAssistant speech") {
            "Nelze pozastavit hlas VirtualAssistanta".into()
        } else {
            "Diktování selhalo; podrobnosti jsou v protokolu".into()
        },
    });
}

async fn finish(
    capture: Recording,
    target: FocusTarget,
    submit: bool,
    jobs: &mut Jobs,
    feedback: &mut Feedback,
    status: &watch::Sender<Status>,
) -> Result<()> {
    let (pcm, duration) = capture.stop().await?;
    jobs.start(pcm, duration, target, submit);
    feedback.transition(Phase::Transcribing, false).await;
    status.send_replace(Status::new(true, Phase::Transcribing));
    Ok(())
}
