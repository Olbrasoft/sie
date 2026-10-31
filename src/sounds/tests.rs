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
    feedback.transition(Phase::Recording, false).await;
    feedback.transition(Phase::Transcribing, false).await;
    feedback.transition(Phase::Idle, true).await;
    assert!(feedback.playback.is_none());
}

#[test]
fn idle_prefix_preserves_every_sample_and_recent_playback_keeps_the_original_asset() {
    let mut original = hound::WavReader::new(Cursor::new(Cue::Recording.bytes())).unwrap();
    let spec = original.spec();
    let original_samples = original
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let bytes = cue_audio(Cue::Recording, RECORDING_WARMUP).unwrap();
    let mut padded = hound::WavReader::new(Cursor::new(bytes.as_ref())).unwrap();
    assert_eq!(padded.spec(), spec);
    let samples = padded
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let prefix = (u128::from(spec.sample_rate) * RECORDING_WARMUP.as_millis() / 1000) as usize
        * usize::from(spec.channels);
    assert!(samples[..prefix].iter().all(|sample| *sample == 0));
    assert_eq!(&samples[prefix..], original_samples);
    assert!(matches!(
        cue_audio(Cue::Recording, Duration::ZERO).unwrap(),
        Cow::Borrowed(_)
    ));
    for cue in [Cue::Transcribing, Cue::Cancel] {
        assert!(matches!(
            cue_audio(cue, RECORDING_WARMUP).unwrap(),
            Cow::Borrowed(_)
        ));
    }
}

#[test]
fn only_an_idle_recording_cue_gets_primed() {
    let activity = CueActivity::default();
    let now = Instant::now();
    assert_eq!(activity.warmup(Cue::Recording, now), RECORDING_WARMUP);
    assert_eq!(activity.warmup(Cue::Cancel, now), Duration::ZERO);
    assert_eq!(activity.warmup(Cue::Transcribing, now), Duration::ZERO);
    *activity.0.lock().unwrap() = Some(now);
    assert_eq!(
        activity.warmup(
            Cue::Recording,
            now + CUE_IDLE_THRESHOLD - Duration::from_millis(1)
        ),
        Duration::ZERO
    );
    assert_eq!(
        activity.warmup(Cue::Recording, now + CUE_IDLE_THRESHOLD),
        RECORDING_WARMUP
    );
}

fn capture_command(path: &std::path::Path) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", "cat > \"$1\"", "fixture"]).arg(path);
    command
}

#[tokio::test]
async fn actual_player_input_is_padded_once_and_other_cues_keep_recent_output_fast() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("audio");
    let activity = CueActivity::default();
    play_with_command(Cue::Recording, capture_command(&path), &activity)
        .await
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.duration(), 6575 + 7200);
    play_with_command(Cue::Recording, capture_command(&path), &activity)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), Cue::Recording.bytes());
    *activity.0.lock().unwrap() = Some(Instant::now() - CUE_IDLE_THRESHOLD);
    play_with_command(Cue::Transcribing, capture_command(&path), &activity)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), Cue::Transcribing.bytes());
    assert_eq!(
        activity.warmup(Cue::Recording, Instant::now()),
        Duration::ZERO
    );
}

#[tokio::test]
async fn failed_player_does_not_establish_recent_audio_activity() {
    let activity = CueActivity::default();
    let mut command = Command::new("sh");
    command.args(["-c", "exit 1"]);
    assert!(
        play_with_command(Cue::Recording, command, &activity)
            .await
            .is_err()
    );
    assert_eq!(
        activity.warmup(Cue::Recording, Instant::now()),
        RECORDING_WARMUP
    );
}

#[tokio::test]
async fn transition_joins_the_interrupted_owned_player_and_preserves_its_recent_activity() {
    let directory = tempfile::tempdir().unwrap();
    let audio = directory.path().join("audio");
    let pid = directory.path().join("pid");
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "cat > \"$1\"; printf '%s' \"$$\" > \"$2\"; exec sleep 20",
            "fixture",
        ])
        .arg(&audio)
        .arg(&pid);
    let mut feedback = Feedback::new(false, None, Phase::Recording);
    let activity = feedback.activity.clone();
    feedback.playback = Some(tokio::spawn(async move {
        let _ = play_with_command(Cue::Recording, command, &activity).await;
    }));
    let child_pid = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(&pid)
                && value.parse::<u32>().is_ok()
            {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    feedback.transition(Phase::Idle, false).await;
    assert!(feedback.playback.is_none());
    assert_eq!(
        feedback.activity.warmup(Cue::Recording, Instant::now()),
        Duration::ZERO
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::path::Path::new("/proc").join(&child_pid).exists() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
