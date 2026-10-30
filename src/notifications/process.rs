//! Bounded owned child processes. Text is passed through stdin or private files.
use anyhow::{Result, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub const MAX_AUDIO: usize = 16 * 1024 * 1024;

pub async fn run(
    command: &mut Command,
    input: &[u8],
    seconds: u64,
    limit: usize,
) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(seconds), async {
        let mut child = command
            .spawn()
            .map_err(|_| anyhow::anyhow!("TTS helper could not start"))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap().take(limit as u64 + 1);
        let write = async move {
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            std::io::Result::Ok(())
        };
        let read = async {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await?;
            anyhow::Ok(bytes)
        };
        let (written, bytes) = tokio::join!(write, read);
        written.map_err(|_| anyhow::anyhow!("TTS helper input failed"))?;
        let bytes = bytes.map_err(|_| anyhow::anyhow!("TTS helper output failed"))?;
        ensure!(bytes.len() <= limit, "TTS helper output exceeds limit");
        ensure!(child.wait().await?.success(), "TTS helper failed");
        Ok(bytes)
    })
    .await
    .map_err(|_| anyhow::anyhow!("TTS helper timed out"))?
}

pub async fn read_audio(path: &Path) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(MAX_AUDIO as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_AUDIO,
        "Invalid TTS audio size"
    );
    Ok(bytes)
}

pub async fn play(audio: &[u8], sink: &str, ducking: &crate::ducking::Handle) -> Result<()> {
    let mut decode = Command::new("ffmpeg");
    decode.args([
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        "pipe:0",
        "-f",
        "wav",
        "-acodec",
        "pcm_s16le",
        "pipe:1",
    ]);
    let wav = run(&mut decode, audio, 30, MAX_AUDIO).await?;
    ensure!(
        wav.starts_with(b"RIFF") && wav.len() > 44,
        "TTS decoder produced invalid audio"
    );
    // Acquire only after decoding, but before any speech reaches the output.
    // The lease restores media on completion, errors and cancellation.
    let _media = ducking.notification().await?;
    let mut player = Command::new("pw-play");
    player.args(["--latency", "20ms", "--target", sink, "-"]);
    run(&mut player, &wav, 600, 1024).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn helpers_receive_eof_before_waiting_for_their_output() {
        let mut cat = Command::new("cat");
        assert_eq!(
            run(&mut cat, b"notification fixture", 2, 64).await.unwrap(),
            b"notification fixture"
        );
    }
    #[tokio::test]
    async fn helper_errors_and_output_limits_never_include_stderr() {
        let mut failed = Command::new("sh");
        failed.args(["-c", "printf 'fake-secret' >&2; exit 1"]);
        let error = run(&mut failed, b"", 2, 64).await.unwrap_err();
        assert!(!error.to_string().contains("fake-secret"));
        let mut oversized = Command::new("head");
        oversized.args(["-c", "128", "/dev/zero"]);
        assert!(run(&mut oversized, b"", 2, 64).await.is_err());
    }
    #[tokio::test]
    async fn cancellation_terminates_only_the_owned_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut child = Command::new("sh");
        child
            .args(["-c", "printf '%s' \"$$\" > \"$1\"; exec sleep 20", "test"])
            .arg(&pid_file);
        let task = tokio::spawn(async move { run(&mut child, b"", 20, 64).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let pid = tokio::fs::read_to_string(&pid_file).await.unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while Path::new(&format!("/proc/{}", pid.trim())).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
