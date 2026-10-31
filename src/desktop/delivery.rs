//! Guarded clipboard delivery and receipts for deferred submission.
use super::{Desktop, FocusTarget, focused_window};
use anyhow::{Context, Result, ensure};
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};
use zbus::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    PasteOnly,
    Submit,
}

impl Desktop {
    /// The selection is already staged; Enter is a separate, guarded action.
    pub async fn paste_shortcut(
        &self,
        keys: &[u32],
        target: &FocusTarget,
        mode: DeliveryMode,
        cancelled: &AtomicBool,
    ) -> Result<PasteReceipt> {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        ensure!(
            focused_window(&self.connection).await?.id == target.id,
            "Target window changed; transcription was not pasted"
        );
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        self.send_keys(keys).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        if mode == DeliveryMode::Submit {
            let submitted: Result<()> = async {
                ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
                ensure!(
                    focused_window(&self.connection).await?.id == target.id,
                    "Target window changed before submission"
                );
                ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
                self.send_keys(&[28]).await?;
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(())
            }
            .await;
            submitted.context("Text pasted; submission was not sent")?;
        }
        Ok(PasteReceipt {
            target: target.clone(),
            pane: None,
            ready_at: tokio::time::Instant::now(),
        })
    }
}

/// Proof of a successful paste, retaining its exact target for deferred Enter.
pub struct PasteReceipt {
    pub(super) target: FocusTarget,
    pub(super) pane: Option<String>,
    pub(super) ready_at: tokio::time::Instant,
}

impl PasteReceipt {
    pub async fn submit(self, connection: &Connection, cancelled: &AtomicBool) -> Result<()> {
        let result = async {
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            tokio::time::sleep_until(self.ready_at).await;
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            if let Some(pane) = self.pane {
                crate::terminal::submit(connection, &self.target, &pane, cancelled).await
            } else {
                send_enter_to(connection, &self.target, cancelled).await
            }
        }
        .await;
        result.context("Text pasted; submission was not sent")
    }
}

/// A literal Enter at the current focus; no clipboard or dictation is involved.
pub async fn press_enter(connection: &Connection) -> Result<()> {
    let target = focused_window(connection).await?;
    send_enter_to(connection, &target, &AtomicBool::new(false)).await
}

pub async fn send_enter_to(
    connection: &Connection,
    target: &FocusTarget,
    cancelled: &AtomicBool,
) -> Result<()> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    let desktop = Desktop::connect(connection).await?;
    let result = async {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        ensure!(
            focused_window(connection).await?.id == target.id,
            "Target window changed before Enter"
        );
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        desktop.send_keys(&[28]).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        anyhow::Ok(())
    }
    .await;
    let stopped = desktop.stop().await;
    result?;
    stopped
}

async fn clipboard_read(primary: bool) -> Option<(String, Vec<u8>)> {
    let mut list = Command::new("wl-paste");
    if primary {
        list.arg("--primary");
    }
    let types = list.arg("--list-types").output().await.ok()?;
    if !types.status.success() {
        return None;
    }
    let types = String::from_utf8(types.stdout).ok()?;
    let mime = types
        .lines()
        .find(|name| name.starts_with("text/plain"))
        .or_else(|| types.lines().find(|name| *name == "image/png"))?
        .to_owned();
    let mut command = Command::new("wl-paste");
    if primary {
        command.arg("--primary");
    }
    let output = command
        .args(["--no-newline", "--type", &mime])
        .output()
        .await
        .ok()?;
    output.status.success().then_some((mime, output.stdout))
}

async fn restore_clipboard(primary: bool, snapshot: Option<(String, Vec<u8>)>) -> Result<()> {
    let mut command = Command::new("wl-copy");
    if primary {
        command.arg("--primary");
    }
    if let Some((mime, bytes)) = snapshot {
        let mut child = command
            .args(["--type", &mime])
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("Clipboard stdin unavailable")?
            .write_all(&bytes)
            .await?;
        ensure!(child.wait().await?.success(), "Cannot restore clipboard");
    } else {
        command.arg("--clear").status().await?;
    }
    Ok(())
}

async fn stage_selection(
    primary: bool,
    text: &str,
    cancelled: &AtomicBool,
) -> Result<tokio::process::Child> {
    // Clipboard managers can take ownership while copying the same selection,
    // or briefly restore the previous contents. Retry a bounded number of times.
    for _ in 0..4 {
        ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
        let mut command = Command::new("wl-copy");
        command.args(["--foreground", "--type", "text/plain;charset=utf-8"]);
        if primary {
            command.arg("--primary");
        }
        let mut owner = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = owner.stdin.take().context("Clipboard stdin unavailable")?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        for _ in 0..50 {
            ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
            if clipboard_read(primary)
                .await
                .is_some_and(|(_, bytes)| bytes == text.as_bytes())
            {
                return Ok(owner);
            }
            if owner.try_wait()?.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = owner.kill().await;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    anyhow::bail!("Cannot stage dictation text (primary: {primary})")
}

/// Stage Unicode text, verify focus again, paste, and restore the previous selection.
pub async fn paste_text(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_text_with_mode(connection, target, text, DeliveryMode::PasteOnly, cancelled).await
}

pub async fn paste_text_with_mode(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
) -> Result<()> {
    paste_with_receipt(connection, target, text, mode, cancelled)
        .await
        .map(|_| ())
}

pub async fn paste_text_with_receipt(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<PasteReceipt>> {
    paste_with_receipt(connection, target, text, DeliveryMode::PasteOnly, cancelled).await
}

async fn paste_with_receipt(
    connection: &Connection,
    target: &FocusTarget,
    text: &str,
    mode: DeliveryMode,
    cancelled: Arc<AtomicBool>,
) -> Result<Option<PasteReceipt>> {
    ensure!(!cancelled.load(Ordering::SeqCst), "Dictation canceled");
    if text.trim().is_empty() {
        return Ok(None);
    }
    let current = focused_window(connection).await?;
    ensure!(
        current.id == target.id,
        "Target window changed; transcription was not pasted"
    );
    let terminal = if current.is_terminal() {
        Some(crate::terminal::resolve(&current).await)
    } else {
        None
    };
    if let Some(pane) = terminal
        .as_ref()
        .and_then(|context| context.pane.as_deref())
    {
        crate::terminal::paste(connection, &current, pane, text, mode, cancelled).await?;
        return Ok(Some(PasteReceipt {
            target: current.clone(),
            pane: Some(pane.to_owned()),
            ready_at: tokio::time::Instant::now() + Duration::from_millis(200),
        }));
    }
    let primary = terminal.as_ref().is_some_and(|context| context.agent);
    let keys: &[u32] = if primary {
        &[42, 110]
    } else if terminal.is_some() {
        &[29, 42, 47]
    } else {
        &[29, 47]
    };
    let previous = clipboard_read(primary).await;
    // Complete compositor setup before staging so other clipboard owners have
    // the smallest possible interval in which to replace the selection.
    let desktop = Desktop::connect(connection).await?;
    let mut owner = None;
    let result: Result<PasteReceipt> = async {
        owner = Some(stage_selection(primary, text, &cancelled).await?);
        desktop.paste_shortcut(keys, target, mode, &cancelled).await
    }
    .await;
    let stopped = desktop.stop().await;
    // Do not overwrite a newer clipboard selection made by the user meanwhile.
    if owner.is_some()
        && clipboard_read(primary)
            .await
            .is_some_and(|(_, bytes)| bytes == text.as_bytes())
        && let Err(error) = restore_clipboard(primary, previous).await
    {
        tracing::warn!(%error, "Clipboard restoration failed");
    }
    if let Some(mut owner) = owner {
        let _ = owner.kill().await;
    }
    let receipt = result?;
    stopped?;
    Ok(Some(receipt))
}
