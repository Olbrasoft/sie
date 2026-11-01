//! Route explicit device actions to the owning component without synthetic typing.
use sie::{
    desktop::Desktop, dictation::Command as DictationCommand, macros::InputAction,
    media::Status as MediaStatus, media_focus, switcher,
};
use std::time::Instant;
use tokio::sync::mpsc;
use zbus::Connection;

pub(super) struct Router {
    pub dictation: mpsc::Sender<DictationCommand>,
    pub dial: mpsc::Sender<switcher::Command>,
    pub focus: mpsc::Sender<media_focus::Request>,
}
impl Router {
    pub fn route(&self, action: InputAction, at: Instant, media: &MediaStatus) -> Option<u32> {
        match action {
            InputAction::Workspace(index) => return Some(index),
            InputAction::DialTurn(delta) => {
                let _ = self.dial.try_send(switcher::Command::Turn(delta));
            }
            InputAction::DialClick => {
                let _ = self.dial.try_send(switcher::Command::Click);
            }
            InputAction::DialDisconnected => {
                let _ = self.dial.try_send(switcher::Command::Cancel);
            }
            InputAction::MediaReveal => {
                if let Some(fallback) = media.target.clone() {
                    let _ = self.focus.try_send(media_focus::Request { at, fallback });
                }
            }
            InputAction::DictationToggle => {
                let _ = self.dictation.try_send(DictationCommand::Toggle);
            }
            InputAction::DictationCancel => {
                let _ = self.dictation.try_send(DictationCommand::Cancel);
            }
            InputAction::DictationSubmit => {
                let _ = self.dictation.try_send(DictationCommand::FinishAndSubmit);
            }
        }
        None
    }
}
pub(super) async fn switch_workspace(connection: &Connection, index: u32) {
    // Keep each remote-input session short and release it after the shortcut.
    let result = async {
        let desktop = Desktop::connect(connection).await?;
        let result = desktop.switch(index).await;
        let stopped = desktop.stop().await;
        let changed = result?;
        stopped?;
        anyhow::Ok(changed)
    }
    .await;
    match result {
        Ok(true) => tracing::info!(workspace = index, "Workspace shortcut sent"),
        Ok(false) => tracing::debug!(workspace = index, "Unavailable workspace ignored"),
        Err(error) => tracing::warn!("Workspace switch failed: {error:#}"),
    }
}
