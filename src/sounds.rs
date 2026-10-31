//! Embedded nonverbal feedback, played by the existing PipeWire client.
use std::{
    borrow::Cow,
    io::Cursor,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, process::Command, task::JoinHandle};

use crate::dictation::Phase;

const CUE_IDLE_THRESHOLD: Duration = Duration::from_secs(10);
const RECORDING_WARMUP: Duration = Duration::from_millis(300);

#[derive(Clone, Default)]
struct CueActivity(Arc<Mutex<Option<Instant>>>);

impl CueActivity {
    fn warmup(&self, cue: Cue, now: Instant) -> Duration {
        let last = *self.0.lock().unwrap_or_else(|error| error.into_inner());
        if cue == Cue::Recording
            && last.is_none_or(|at| now.saturating_duration_since(at) >= CUE_IDLE_THRESHOLD)
        {
            RECORDING_WARMUP
        } else {
            Duration::ZERO
        }
    }
}

struct ActiveOutput {
    activity: CueActivity,
    fed: bool,
}
impl Drop for ActiveOutput {
    fn drop(&mut self) {
        if self.fed {
            *self
                .activity
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
        }
    }
}

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
    activity: CueActivity,
}

impl Feedback {
    pub fn new(enabled: bool, sink: Option<String>, phase: Phase) -> Self {
        Self {
            enabled,
            sink,
            phase,
            playback: None,
            activity: CueActivity::default(),
        }
    }

    pub async fn transition(&mut self, after: Phase, cancelled: bool) {
        let cue = cue_for_transition(self.phase, after, cancelled);
        if (self.phase != after || cancelled)
            && let Some(task) = self.playback.take()
        {
            task.abort();
            // Finish dropping the previous player and its activity guard before
            // deciding whether the replacement cue needs output priming.
            let _ = task.await;
        }
        self.phase = after;
        if self.enabled
            && let Some(cue) = cue
        {
            let sink = self.sink.clone();
            let activity = self.activity.clone();
            self.playback = Some(tokio::spawn(async move {
                loop {
                    if let Err(error) = play(cue, sink.as_deref(), &activity).await {
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
    command.arg("-");
    command
}

fn cue_audio(cue: Cue, warmup: Duration) -> anyhow::Result<Cow<'static, [u8]>> {
    if cue != Cue::Recording || warmup.is_zero() {
        return Ok(Cow::Borrowed(cue.bytes()));
    }
    let mut original = hound::WavReader::new(Cursor::new(cue.bytes()))?;
    let spec = original.spec();
    anyhow::ensure!(
        spec.sample_format == hound::SampleFormat::Int && spec.bits_per_sample == 16,
        "Recording cue must be 16-bit PCM"
    );
    let silent_frames = u128::from(spec.sample_rate) * warmup.as_millis() / 1000;
    let mut buffer = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut buffer, spec)?;
    for _ in 0..silent_frames * u128::from(spec.channels) {
        writer.write_sample(0_i16)?;
    }
    for sample in original.samples::<i16>() {
        writer.write_sample(sample?)?;
    }
    writer.finalize()?;
    Ok(Cow::Owned(buffer.into_inner()))
}

async fn play(cue: Cue, sink: Option<&str>, activity: &CueActivity) -> anyhow::Result<()> {
    play_with_command(cue, playback_command(sink), activity).await
}

async fn play_with_command(
    cue: Cue,
    mut command: Command,
    activity: &CueActivity,
) -> anyhow::Result<()> {
    let warmup = activity.warmup(cue, Instant::now());
    let bytes = cue_audio(cue, warmup)?;
    if !warmup.is_zero() {
        tracing::info!(
            warmup_ms = warmup.as_millis(),
            "Priming recording cue after cue inactivity"
        );
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut active = ActiveOutput {
        activity: activity.clone(),
        fed: false,
    };
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Sound stdin unavailable"))?;
    let result = async {
        // Mark a stream active after its first chunk, even if a longer cue is
        // interrupted while the remaining bytes are still being written.
        let first = bytes.len().min(4096);
        stdin.write_all(&bytes[..first]).await?;
        active.fed = true;
        stdin.write_all(&bytes[first..]).await?;
        stdin.shutdown().await?;
        drop(stdin);
        anyhow::ensure!(
            child.wait().await?.success(),
            "PipeWire sound player failed"
        );
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        active.fed = false;
    }
    result
}

#[cfg(test)]
mod tests;
