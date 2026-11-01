//! Workspace observation and validation of existing shortcut bindings.
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use std::time::Duration;
use tokio::process::Command;
use zbus::{Connection, Proxy};
const TRACKER: &str = "org.olbrasoft.Desktop";

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceState {
    pub index: i32,
    pub count: i32,
}

impl WorkspaceState {
    pub fn active_macro(&self) -> Option<i32> {
        (self.index >= 0 && self.index < 5 && self.index < self.count).then_some(self.index)
    }
}

pub async fn workspace_state(connection: &Connection) -> Result<WorkspaceState> {
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(TRACKER)?
        .path("/org/olbrasoft/Desktop")?
        .build()
        .await?;
    let mut values = properties.get_all(TRACKER.try_into()?).await?;
    Ok(WorkspaceState {
        index: i32::try_from(
            values
                .remove("CurrentWorkspace")
                .context("Missing current workspace")?,
        )?,
        count: i32::try_from(
            values
                .remove("TotalWorkspaces")
                .context("Missing workspace count")?,
        )?,
    })
}

/// Any workspace change triggers a fresh read, including mouse and other shortcuts.
pub async fn watch_workspaces(connection: Connection, changed: tokio::sync::mpsc::Sender<()>) {
    loop {
        let result: Result<()> = async {
            let proxy = Proxy::new(&connection, TRACKER, "/org/olbrasoft/Desktop", TRACKER).await?;
            let mut signals = proxy.receive_signal("WorkspaceChanged").await?;
            let _ = changed.try_send(());
            while signals.next().await.is_some() {
                if changed.is_closed() {
                    return Ok(());
                }
                // Coalesce rapid switches; the consumer reads the latest state.
                let _ = changed.try_send(());
            }
            bail!("Workspace signal stream closed")
        }
        .await;
        if changed.is_closed() {
            return;
        }
        if let Err(error) = result {
            tracing::debug!(%error, "Workspace signal subscription unavailable; retrying");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub async fn workspace_count(connection: &Connection) -> Result<i32> {
    workspace_property(connection, "TotalWorkspaces").await
}

pub async fn current_workspace(connection: &Connection) -> Result<i32> {
    workspace_property(connection, "CurrentWorkspace").await
}

async fn workspace_property(connection: &Connection, name: &str) -> Result<i32> {
    // Never cache this property: GNOME can add/remove dynamic workspaces at any time.
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(TRACKER)?
        .path("/org/olbrasoft/Desktop")?
        .build()
        .await?;
    let count = properties.get(TRACKER.try_into()?, name).await?;
    Ok(i32::try_from(count)?)
}

pub fn binding_matches(value: &str, index: u32) -> bool {
    index < 5
        && (value.contains(&format!("'<Super>KP_{}'", index + 1))
            || value.contains(&format!("\"<Super>KP_{}\"", index + 1)))
}

pub async fn check_binding(index: u32) -> Result<()> {
    let output = Command::new("gsettings")
        .args([
            "get",
            "org.gnome.desktop.wm.keybindings",
            &format!("switch-to-workspace-{}", index + 1),
        ])
        .kill_on_drop(true)
        .output()
        .await
        .context("Cannot execute gsettings")?;
    ensure!(
        output.status.success(),
        "Cannot read GNOME workspace keybinding"
    );
    let value = String::from_utf8(output.stdout)?;
    ensure!(
        binding_matches(&value, index),
        "Workspace {} requires the existing GNOME shortcut <Super>KP_{}; current value: {}",
        index,
        index + 1,
        value.trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_state_only_highlights_existing_first_five_targets() {
        assert_eq!(WorkspaceState { index: 4, count: 4 }.active_macro(), None);
        assert_eq!(
            WorkspaceState { index: 4, count: 5 }.active_macro(),
            Some(4)
        );
        assert_eq!(WorkspaceState { index: 5, count: 9 }.active_macro(), None);
        assert_eq!(
            WorkspaceState {
                index: -1,
                count: 5
            }
            .active_macro(),
            None
        );
    }

    #[test]
    fn bindings_require_the_exact_configured_accelerator() {
        assert!(binding_matches("['<Super>KP_1']\n", 0));
        assert!(binding_matches("['<Control>F5', '<Super>KP_5']", 4));
        assert!(!binding_matches("['<Super>KP_2']", 0));
        assert!(!binding_matches("['<Super>KP_10']", 0));
        assert!(!binding_matches("['<Shift><Super>KP_1']", 0));
        assert!(!binding_matches("@as []", 0));
        assert!(!binding_matches("['<Super>KP_6']", 5));
    }
}
