//! Short-lived compositor input sessions and balanced key release.
use super::{check_binding, current_workspace, workspace_count};
use crate::macros::workspace_exists;
use anyhow::{Result, bail};
use std::time::Duration;
use zbus::{Connection, Proxy, zvariant::OwnedObjectPath};
const MUTTER: &str = "org.gnome.Mutter.RemoteDesktop";
const SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
// Mutter accepts evdev codes, not X11 keycodes.
const SUPER_L: u32 = 125;
const WORKSPACE_KEYS: [u32; 5] = [79, 80, 81, 75, 76];

/// A compositor-owned input session; disconnecting also releases virtual keys.
pub struct Desktop {
    pub(super) connection: Connection,
    session: OwnedObjectPath,
}

impl Desktop {
    pub async fn connect(connection: &Connection) -> Result<Self> {
        let root = Proxy::new(
            connection,
            MUTTER,
            "/org/gnome/Mutter/RemoteDesktop",
            MUTTER,
        )
        .await?;
        let session: OwnedObjectPath = root.call("CreateSession", &()).await?;
        let desktop = Self {
            connection: connection.clone(),
            session,
        };
        desktop
            .proxy()
            .await?
            .call::<_, _, ()>("Start", &())
            .await?;
        Ok(desktop)
    }

    async fn proxy(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(&self.connection, MUTTER, self.session.as_str(), SESSION).await?)
    }

    pub async fn switch(&self, index: u32) -> Result<bool> {
        if index >= 5 {
            bail!("Only workspace indices 0 through 4 are mapped");
        }
        if !workspace_exists(index, workspace_count(&self.connection).await?) {
            return Ok(false);
        }
        // Validate the actual configured shortcut without changing user settings.
        check_binding(index).await?;
        let proxy = self.proxy().await?;
        proxy
            .call::<_, _, ()>("NotifyKeyboardKeycode", &(SUPER_L, true))
            .await?;
        let press = proxy
            .call::<_, _, ()>(
                "NotifyKeyboardKeycode",
                &(WORKSPACE_KEYS[index as usize], true),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Attempt both releases even if pressing or releasing the numpad key fails.
        let release_key = proxy
            .call::<_, _, ()>(
                "NotifyKeyboardKeycode",
                &(WORKSPACE_KEYS[index as usize], false),
            )
            .await;
        let release_super = proxy
            .call::<_, _, ()>("NotifyKeyboardKeycode", &(SUPER_L, false))
            .await;
        press?;
        release_key?;
        release_super?;
        // Mutter queues input asynchronously. Keep the session alive until the
        // compositor confirms activation rather than dropping pending events.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if current_workspace(&self.connection).await? == index as i32 {
                return Ok(true);
            }
        }
        bail!("GNOME did not activate workspace {index}; check shortcut conflicts and Num Lock")
    }

    pub async fn stop(self) -> Result<()> {
        self.proxy().await?.call::<_, _, ()>("Stop", &()).await?;
        Ok(())
    }

    pub async fn send_keys(&self, keys: &[u32]) -> Result<()> {
        let proxy = self.proxy().await?;
        let mut failure = None;
        for key in keys {
            if let Err(error) = proxy
                .call::<_, _, ()>("NotifyKeyboardKeycode", &(*key, true))
                .await
            {
                failure = Some(error);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        for key in keys.iter().rev() {
            if let Err(error) = proxy
                .call::<_, _, ()>("NotifyKeyboardKeycode", &(*key, false))
                .await
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(error.into());
        }
        Ok(())
    }
}
