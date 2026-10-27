//! Embedded nonverbal feedback, played by the existing PipeWire client.
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command, task::JoinHandle};

use crate::dictation::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cue {
    Recording,
    Transcribing,
    Cancel,
}

impl Cue {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Recording => include_bytes!("../assets/sounds/recording-start.wav"),
            Self::Transcribing => include_bytes!("../assets/sounds/write.mp3"),
            Self::Cancel => include_bytes!("../assets/sounds/paper-rip.mp3"),
        }
    }
}

pub(crate) fn cue_for_transition(before: Phase, after: Phase, cancelled: bool) -> Option<Cue> {
    if cancelled && matches!(before, Phase::Recording | Phase::Transcribing) {
        Some(Cue::Cancel)
    } else if after == Phase::Recording && before != after {
        Some(Cue::Recording)
    } else if after == Phase::Transcribing && before != after {
        Some(Cue::Transcribing)
    } else {
        None
    }
}

pub(crate) struct Feedback {
    enabled: bool,
    sink: Option<String>,
    phase: Phase,
    playback: Option<JoinHandle<()>>,
}

impl Feedback {
    pub fn new(enabled: bool, sink: Option<String>, phase: Phase) -> Self {
        Self {
            enabled,
            sink,
            phase,
            playback: None,
        }
    }

    pub fn transition(&mut self, after: Phase, cancelled: bool) {
        let cue = cue_for_transition(self.phase, after, cancelled);
        if (self.phase != after || cancelled)
            && let Some(task) = self.playback.take()
        {
            task.abort();
        }
        self.phase = after;
        if self.enabled
            && let Some(cue) = cue
        {
            let sink = self.sink.clone();
            self.playback = Some(tokio::spawn(async move {
                loop {
                    if let Err(error) = play(cue, sink.as_deref()).await {
                        tracing::warn!(%error, "Dictation sound playback unavailable");
                        break;
                    }
                    if cue != Cue::Transcribing {
                        break;
                    }
                }
            }));
        }
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        if let Some(task) = self.playback.take() {
            task.abort();
        }
    }
}

fn playback_command(sink: Option<&str>) -> Command {
    let mut command = Command::new("pw-play");
    command.args(["--latency", "20ms"]);
    if let Some(sink) = sink {
        command.args(["--target", sink]);
    }
    command
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn play(cue: Cue, sink: Option<&str>) -> anyhow::Result<()> {
    let mut child = playback_command(sink).spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Sound stdin unavailable"))?;
    stdin.write_all(cue.bytes()).await?;
    stdin.shutdown().await?;
    drop(stdin);
    anyhow::ensure!(
        child.wait().await?.success(),
        "PipeWire sound player failed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_cue_has_an_immediate_audible_onset() {
        let mut reader = hound::WavReader::new(std::io::Cursor::new(Cue::Recording.bytes()))
            .expect("Recording cue must be decoded PCM audio");
        let spec = reader.spec();
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.channels, 2);
        let onset = reader
            .samples::<i16>()
            .position(|sample| sample.unwrap().unsigned_abs() > 104)
            .expect("Recording cue must contain audible audio");
        let onset_frame = onset / usize::from(spec.channels);
        assert!(onset_frame * 1000 <= spec.sample_rate as usize * 15);
        let duration_ms = u64::from(reader.duration()) * 1000 / u64::from(spec.sample_rate);
        assert!((200..400).contains(&duration_ms));
    }

    #[test]
    fn playback_requests_low_latency_and_preserves_output_selection() {
        for sink in [None, Some("test.monitor.stereo")] {
            let command = playback_command(sink);
            let mut expected = vec!["--latency", "20ms"];
            if let Some(sink) = sink {
                expected.extend(["--target", sink]);
            }
            expected.push("-");
            assert_eq!(command.as_std().get_program(), "pw-play");
            assert_eq!(command.as_std().get_args().collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn cues_follow_real_state_changes_and_idle_cancel_is_silent() {
        assert_eq!(
            cue_for_transition(Phase::Idle, Phase::Recording, false),
            Some(Cue::Recording)
        );
        assert_eq!(
            cue_for_transition(Phase::Recording, Phase::Transcribing, false),
            Some(Cue::Transcribing)
        );
        for phase in [Phase::Recording, Phase::Transcribing] {
            assert_eq!(
                cue_for_transition(phase, Phase::Idle, true),
                Some(Cue::Cancel)
            );
        }
        for after in [Phase::Idle, Phase::Disabled, Phase::Error] {
            assert_eq!(cue_for_transition(Phase::Idle, after, false), None);
        }
        assert_eq!(cue_for_transition(Phase::Idle, Phase::Idle, true), None);
        assert_eq!(
            cue_for_transition(Phase::Recording, Phase::Recording, false),
            None
        );
    }

    #[tokio::test]
    async fn disabled_feedback_never_starts_an_audio_process() {
        let mut feedback = Feedback::new(false, None, Phase::Idle);
        feedback.transition(Phase::Recording, false);
        feedback.transition(Phase::Transcribing, false);
        feedback.transition(Phase::Idle, true);
        assert!(feedback.playback.is_none());
    }
}
