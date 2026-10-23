use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};

pub struct Recording {
    child: Child,
    reader: JoinHandle<std::io::Result<Vec<u8>>>,
    started: Instant,
}

impl Recording {
    pub async fn start(source: Option<&str>, max_seconds: u64) -> Result<Self> {
        let mute = Command::new("pactl")
            .args(["get-source-mute", "@DEFAULT_SOURCE@"])
            .env("LC_ALL", "C")
            .output()
            .await
            .context("Cannot inspect microphone state")?;
        ensure!(mute.status.success(), "Cannot inspect microphone state");
        ensure!(
            !String::from_utf8_lossy(&mute.stdout).contains("yes"),
            "Microphone is muted"
        );
        let mut command = Command::new("pw-record");
        command.args([
            "--rate",
            "16000",
            "--channels",
            "1",
            "--format",
            "s16",
            "--raw",
        ]);
        if let Some(source) = source {
            command.args(["--target", source]);
        }
        let mut child = command
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Cannot start PipeWire microphone recording")?;
        let stdout = child.stdout.take().context("No microphone audio stream")?;
        let reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take(max_seconds * 16000 * 2 + 2)
                .read_to_end(&mut bytes)
                .await?;
            Ok(bytes)
        });
        Ok(Self {
            child,
            reader,
            started: Instant::now(),
        })
    }

    pub async fn stop(mut self) -> Result<(Vec<i16>, i64)> {
        if let Some(id) = self.child.id() {
            let _ = kill(Pid::from_raw(id as i32), Signal::SIGINT);
        }
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            self.child.kill().await?;
        }
        let bytes = self.reader.await??;
        ensure!(bytes.len() >= 3200, "No microphone audio recorded");
        let samples = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Ok((samples, self.started.elapsed().as_millis() as i64))
    }

    pub async fn cancel(mut self) {
        let _ = self.child.kill().await;
        self.reader.abort();
    }
}
