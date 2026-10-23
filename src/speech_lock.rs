//! Optional local speech coordination while VirtualAssistant remains installed.
use std::process::Stdio;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub struct SpeechLock {
    enabled: bool,
    held: bool,
}

impl SpeechLock {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            held: false,
        }
    }

    pub async fn acquire(&mut self, timeout_seconds: u64) -> Result<()> {
        if !self.enabled || self.held {
            return Ok(());
        }
        request(
            "start",
            &format!("{{\"timeoutSeconds\":{timeout_seconds}}}"),
        )
        .await?;
        self.held = true;
        Ok(())
    }

    pub async fn release(&mut self) {
        if self.held {
            match request("stop", "{}").await {
                Ok(()) => self.held = false,
                Err(error) => {
                    tracing::warn!(%error, "Could not release VirtualAssistant speech lock; it will expire")
                }
            }
        }
    }
}

async fn request(action: &str, data: &str) -> Result<()> {
    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--max-time",
            "1",
            "--noproxy",
            "*",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--data",
            data,
            &format!("http://127.0.0.1:5055/api/speech-lock/{action}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .context("Cannot coordinate VirtualAssistant speech")?;
    ensure!(
        status.success(),
        "Cannot coordinate VirtualAssistant speech"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn standalone_mode_never_contacts_virtual_assistant() {
        let mut lock = SpeechLock::new(false);
        lock.acquire(300).await.unwrap();
        assert!(!lock.held);
        lock.release().await;
    }
}
