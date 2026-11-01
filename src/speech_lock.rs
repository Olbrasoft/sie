//! Optional local speech coordination while VirtualAssistant remains installed.
use std::{process::Stdio, sync::Arc};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub struct SpeechLock {
    enabled: bool,
    held: bool,
    local: Option<Arc<crate::notifications::Control>>,
}

impl SpeechLock {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            held: false,
            local: None,
        }
    }

    pub fn local(control: Arc<crate::notifications::Control>) -> Self {
        Self {
            enabled: false,
            held: false,
            local: Some(control),
        }
    }

    pub async fn acquire(&mut self, timeout_seconds: u64) -> Result<()> {
        if let Some(control) = &self.local {
            self.held = true;
            return control.recording(true).await;
        }
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
        if let Some(control) = &self.local {
            if self.held {
                let _ = control.recording(false).await;
            }
            self.held = false;
            return;
        }
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

impl Drop for SpeechLock {
    fn drop(&mut self) {
        if self.held
            && let Some(control) = &self.local
        {
            control.gate.send_modify(|g| g.recording = false);
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
