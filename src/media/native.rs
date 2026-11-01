//! Optional read-only GNOME media-key observation on a dedicated connection.
use super::{PLAYER_INTERFACE, PLAYER_PATH};
use anyhow::{Result, bail};
use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use zbus::{Connection, MatchRule, MessageStream, message::Type};

/// A dedicated connection observes only zero-argument native PlayPause calls
/// from GNOME's media-key service. No key events, titles or media URLs are read.
/// Monitoring permission is optional; state tracking works without it.
pub struct NativeObserver {
    messages: MessageStream,
    _connection: Connection,
}

impl NativeObserver {
    pub async fn new(connection: Connection) -> Result<Self> {
        let rule = MatchRule::builder()
            .msg_type(Type::MethodCall)
            .sender("org.gnome.SettingsDaemon.MediaKeys")?
            .path(PLAYER_PATH)?
            .interface(PLAYER_INTERFACE)?
            .member("PlayPause")?
            .build();
        let messages = MessageStream::from(&connection);
        zbus::fdo::MonitoringProxy::new(&connection)
            .await?
            .become_monitor(&[rule], 0)
            .await?;
        Ok(Self {
            messages,
            _connection: connection,
        })
    }
    pub async fn run(self, targets: mpsc::Sender<String>) -> Result<()> {
        self.run_with_hint(targets, None).await
    }
    pub async fn run_with_hint(
        mut self,
        targets: mpsc::Sender<String>,
        hints: Option<watch::Sender<Option<crate::media_focus::NativeHint>>>,
    ) -> Result<()> {
        while let Some(message) = self.messages.next().await {
            let message = message?;
            let header = message.header();
            if header.message_type() != Type::MethodCall
                || header.interface().map(|i| i.as_str()) != Some(PLAYER_INTERFACE)
                || header.member().map(|m| m.as_str()) != Some("PlayPause")
            {
                continue;
            }
            if let Some(destination) = header.destination() {
                if let Some(hints) = &hints {
                    hints.send_replace(Some(crate::media_focus::NativeHint {
                        at: std::time::Instant::now(),
                        destination: destination.to_string(),
                    }));
                }
                let _ = targets.try_send(destination.to_string());
            }
            if targets.is_closed() {
                return Ok(());
            }
        }
        bail!("Native media target observation ended")
    }
}

pub async fn observe_native_target(
    connection: Connection,
    targets: mpsc::Sender<String>,
) -> Result<()> {
    NativeObserver::new(connection).await?.run(targets).await
}
